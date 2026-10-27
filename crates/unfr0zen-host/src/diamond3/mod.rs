//! The diamond3's recovery flow: from whatever USB mode the unit is in, load
//! recovery U-Boot, boot the slot's kernel with the unfr0zen initramfs, patch
//! the volume from Linux on the unit, and confirm the USB root shell.

pub mod assets;
pub mod boot;
pub mod nand;
pub mod usb;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::time::Duration;

use unfr0zen_core::console::UBoot;
use unfr0zen_core::diamond3::fastboot::Protocol;
use unfr0zen_core::diamond3::initramfs;
use unfr0zen_core::diamond3::recovery::Recovery;
use unfr0zen_core::patch::{self, changes_text};
use unfr0zen_core::shell::{sha256_hex, InitramfsShell};
use unfr0zen_core::Result;

use crate::device::{Device, Unit, HOLD_BUTTON, WAIT_USB_BOOT_STAGE};
use crate::flow::{failure, private_dir, seconds, start_log, stored_pki, Config, HandBack};
use crate::progress::{stage, Progress};
use crate::usb::{open_acm, verify_shell, Id, Pipe, Port};
use usb::Mode;

/// The boot wrapper's watchdog while recovery runs.
pub(crate) const WATCHDOG_SECONDS: u32 = 128;

pub(crate) type Board = Recovery<Port, Protocol<Pipe>>;

pub(crate) fn connect_recovery(port: &str) -> Result<Board> {
    let console = UBoot::connect(open_acm(usb::RECOVERY, seconds(45))?, seconds(20))?;
    Ok(Recovery::new(console, usb::open_fastboot(port)?))
}

/// The SoC and its silicon revision, as recovery reads them, for the front
/// end; a failed read is only logged.
pub(crate) fn report_silicon(progress: &mut dyn Progress, board: &mut Board) {
    match board.silicon() {
        Ok(silicon) => progress.device(&format!("{}: {silicon}", Device::Diamond3.description())),
        Err(error) => log::warn!("Silicon revision not read: {error}"),
    }
}

/// SDP to recovery U-Boot on the same port.
fn load_recovery(progress: &mut dyn Progress, port: &str) -> Result<(String, Id)> {
    stage(progress, "Load recovery U-Boot", || usb::load_recovery(WATCHDOG_SECONDS))?;
    stage(progress, "Wait for recovery USB", || usb::wait_device(&[Mode::Recovery], seconds(45), Some(port)))
}

/// Brings the unit from whatever mode it is in to SDP or recovery, or leaves
/// it in the application firmware with `stop_at_app`: (port, identity).
pub(crate) fn find_unit(progress: &mut dyn Progress, wait_usb: Duration, stop_at_app: bool, unit: Unit)
                        -> Result<(String, Id)> {
    let Unit { device, mut port, mut id } = unit;
    if device != Device::Diamond3 {
        return Err(failure(format!("this needs a diamond3; the unit on USB is a {}", device.name())));
    }
    if id == usb::INITRAMFS {
        // An interrupted run left the unit in its initramfs shell.
        (port, id) = stage(progress, "Restart the unit from an earlier initramfs shell", || {
            let shell = InitramfsShell::connect(open_acm(usb::INITRAMFS, seconds(45))?, seconds(30))?;
            shell.restart()?;
            usb::wait_device(&[Mode::App, Mode::Sdp], seconds(180), Some(&port))
        })?;
    }
    if usb::mode(id) == Some(Mode::App) && !stop_at_app {
        progress.waiting(HOLD_BUTTON);
        (port, id) = stage(progress, WAIT_USB_BOOT_STAGE, || usb::wait_device(&[Mode::Sdp], wait_usb, Some(&port)))?;
    }
    Ok((port, id))
}

/// From what find_unit reached to recovery U-Boot: its port.
pub(crate) fn reach_recovery(progress: &mut dyn Progress, port: String, id: Id) -> Result<String> {
    let (port, id) = if id == usb::SDP { load_recovery(progress, &port)? } else { (port, id) };
    if id != usb::RECOVERY {
        return Err(failure("recovery U-Boot did not enumerate"));
    }
    Ok(port)
}

/// Runs the recovery from whatever mode the unit is in to a confirmed root
/// shell, writing run records under `work_dir` and the timestamped run log.
/// Returns the final status line.
pub(crate) fn run(config: &Config, progress: &mut dyn Progress, unit: Unit) -> Result<String> {
    let (port, id) = find_unit(progress, config.wait_usb, false, unit)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let run_dir = config.work_dir.join(&stamp);
    private_dir(&run_dir)?;
    let log_dir = start_log(progress, &stamp)?;
    let _hand_back = HandBack(vec![run_dir.clone(), log_dir]);
    let port = reach_recovery(progress, port, id)?;
    route(progress, config, &port, &run_dir)
}

/// From recovery U-Boot to the confirmed root shell.
fn route(progress: &mut dyn Progress, config: &Config, port: &str, run_dir: &Path) -> Result<String> {
    let options = &patch::Options {
        thread: config.options.thread.as_ref()
            .map(|thread| crate::assets::with_thread_radio(thread, assets::THREAD_RADIO, assets::THREAD_RADIO_IDENTITY)),
        ..config.options.clone()
    };
    let mut board = stage(progress, "Connect to recovery console", || connect_recovery(port))?;
    report_silicon(progress, &mut board);
    let environment = stage(progress, "Read installed boot order from NAND", || {
        board.attach_ubi()?;
        board.read_installed_environment()
    })?;
    progress.status(&format!("Installed boot order: {} then {}", environment.order[0], environment.order[1]));
    let slot = stage(progress, "Find the application root volume", || board.select_slot(environment.order, config.slot))?;
    progress.status(&format!("Application root volume: {slot}"));
    fs::write(run_dir.join("selected-slot.txt"), format!("{slot}\n"))?;
    if config.backup {
        let original = run_dir.join("original.ubifs");
        stage(progress, &format!("Read and download {slot} over USB"), || -> Result<()> {
            let image = board.retrieve(slot)?;
            fs::write(&original, &image)?;
            log::info!("Original {slot} saved: {} SHA-256 {}, CRC32 {:08x}", original.display(), sha256_hex(&image),
                       crc32fast::hash(&image));
            Ok(())
        })?;
        progress.status(&format!("Original root filesystem saved: {}", original.display()));
    }

    // Contents known before boot travel in the initramfs, over fastboot.
    let nestlocal_binary = if options.mqtt.is_some() || options.matter.is_some() || options.zigbee || options.doom.is_some()
        || options.client_ng.is_some() {
        crate::assets::nestlocal()
    } else {
        Vec::new()
    };
    let mut payload: BTreeMap<String, Vec<u8>> = options.overlay.iter()
        .flat_map(|overlay| overlay.files.iter().map(|(_, _, data)| (sha256_hex(data), data.clone()))).collect();
    for bundle in options.thread.iter().chain(&options.doom).chain(&options.client_ng) {
        for (_, _, data) in &bundle.files { payload.insert(sha256_hex(data), data.clone()); }
    }
    if !nestlocal_binary.is_empty() {
        payload.insert(sha256_hex(&nestlocal_binary), nestlocal_binary.clone());
    }
    let initramfs = stage(progress, "Build the initramfs", || -> Result<Vec<u8>> {
        let image = initramfs::build(&crate::assets::busybox(), &payload);
        fs::write(run_dir.join("initramfs.cpio.gz"), &image)?;
        Ok(image)
    })?;
    let (length, _) = stage(progress, "Upload the initramfs to RAM", || board.stage(&initramfs))?;
    stage(progress, &format!("Boot the {slot} kernel with the initramfs"), || board.boot_initramfs(slot, &environment, length))?;

    let (port, mut shell) = stage(progress, "Wait for the initramfs USB shell", || {
        let (port, id) = usb::wait_device(&[Mode::Initramfs, Mode::Sdp, Mode::App], seconds(120), Some(port))?;
        if id != usb::INITRAMFS {
            let mode = usb::mode(id).map_or("unknown", Mode::name);
            return Err(failure(format!("the unit came back in {mode} mode, not the initramfs shell; {slot} is \
                                        unchanged. The kernel UART (ttymxc0) shows why")));
        }
        let shell = InitramfsShell::connect(open_acm(usb::INITRAMFS, seconds(45))?, seconds(30))?;
        Ok((port, shell))
    })?;
    shell.run("uname -a; cat /proc/mtd; grep \" /mnt \" /proc/mounts")?;
    let label = if config.dry_run { format!("Patch copies of {slot} files (dry run)") } else { format!("Patch {slot} in place") };
    let payload: BTreeSet<String> = payload.into_keys().collect();
    let pki_dir = config.data_dir.join("nestlocal-pki");
    let changes = stage(progress, &label, || {
        patch::patch_on_device(&mut shell, Device::Diamond3, options, &nestlocal_binary, crate::assets::CUSTOM_SCREEN, &mut || stored_pki(&pki_dir), &payload, config.dry_run)
    })?;
    fs::write(run_dir.join("initramfs-changes.txt"), changes_text(&changes))?;
    shell.restart()?;

    let (port, id) = stage(progress, "Wait for the unit to restart", || usb::wait_device(&[Mode::App, Mode::Sdp], seconds(180), Some(&port)))?;
    if id == usb::SDP {
        let (port, _) = load_recovery(progress, &port)?;
        let mut board = stage(progress, "Reconnect to recovery console", || connect_recovery(&port))?;
        board.attach_ubi()?;
        stage(progress, "Chainload stock U-Boot from NAND", || board.boot_vendor())?;
    }
    if config.dry_run {
        return Ok(format!("Dry run done: {slot} left as it was; changes listed in {}",
                          run_dir.join("initramfs-changes.txt").display()));
    }
    // Without the USB root shell there is nothing to log in to: the patch's
    // writes were checked on the volume, and the unit coming back as its app
    // is all that is seen of the boot.
    let shell = if options.usb_shell {
        Some(stage(progress, "Wait for USB root shell", || verify_shell(&port, usb::APP_ACM, config.wait_shell))?)
    } else {
        None
    };
    if let Some(matter) = &options.matter {
        let code = matter.pairing_code();
        fs::write(run_dir.join("matter-pairing-code.txt"), format!("{code}\n"))?;
        progress.status(&format!("Matter pairing code: {code} (Home Assistant: Add device, Matter, enter the code; \
                                  the unit must be on Wi-Fi)"));
    }
    Ok(match shell {
        Some(name) => format!("Root shell confirmed on {name} (UID 0)"),
        None if id == usb::SDP => format!("Patched {slot}; the unit was started from its stock U-Boot. Its boot is not \
                                           confirmed: the USB root shell is off"),
        None => format!("Patched {slot}; the unit booted back to its app"),
    })
}
