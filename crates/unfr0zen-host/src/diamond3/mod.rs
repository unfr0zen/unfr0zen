//! The diamond3's recovery flow: from whatever USB mode the unit is in, load
//! recovery U-Boot, boot the slot's kernel with the unfr0zen initramfs, patch
//! the volume from Linux on the unit, and confirm the USB root shell.

pub mod assets;
pub mod boot;
pub mod nand;
pub mod usb;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

use unfr0zen_core::console::UBoot;
use unfr0zen_core::diamond3::fastboot::Protocol;
use unfr0zen_core::diamond3::initramfs;
use unfr0zen_core::diamond3::recovery::Recovery;
use unfr0zen_core::bundle::{self, Settings};
use unfr0zen_core::patch;
use unfr0zen_core::shell::InitramfsShell;
use unfr0zen_core::{Error, Result};

use crate::device::{CancelFlag, Device, Unit, HOLD_BUTTON, WAIT_USB_BOOT_STAGE};
use crate::flow::{failure, private_dir, seconds, start_log, stored_pki, Config, HandBack};
use crate::progress::{stage, Phase, Progress};
use crate::usb::{open_acm, verify_shell, Id, Pipe, Port};
use usb::Mode;
pub(crate) use usb::EntryPoint;

/// The boot wrapper's watchdog while recovery runs.
pub(crate) const WATCHDOG_SECONDS: u32 = 128;

pub(crate) type Board = Recovery<Port, Protocol<Pipe>>;

pub(crate) fn connect_recovery(progress: &mut dyn Progress, port: &str) -> Result<Board> {
    connect_with_retry(progress, || {
        let console = UBoot::connect(open_acm(usb::RECOVERY, seconds(45))?, seconds(20))?;
        Ok(Recovery::new(console, usb::open_fastboot(port)?))
    }, || {
        let (_, id) = usb::wait_device(&[Mode::Recovery, Mode::Sdp, Mode::App, Mode::Initramfs], seconds(10), Some(port), &Default::default())?;
        if id == usb::SDP {
            usb::load_recovery(WATCHDOG_SECONDS, EntryPoint::Auto)?;
            usb::wait_device(&[Mode::Recovery], seconds(45), Some(port), &Default::default())?;
        } else if id != usb::RECOVERY {
            return Err(failure(format!("the unit restarted in {} mode", usb::mode(id).unwrap().name())));
        }
        Ok(())
    })
}

/// Only establishing the connection is retried; no operation on NAND is replayed.
fn connect_with_retry<T>(progress: &mut dyn Progress, mut connect: impl FnMut() -> Result<T>,
                         mut reconnect: impl FnMut() -> Result<()>) -> Result<T> {
    stage(progress, "Connect to recovery console", || {
        for attempt in 1..=3 {
            log::info!("Recovery connection attempt {attempt}/3");
            match connect() {
                Ok(board) => return Ok(board),
                Err(error @ (Error::Io(_) | Error::Timeout(_))) if attempt < 3 => {
                    log::warn!("Recovery connection attempt {attempt}/3 failed: {error}; reconnecting");
                    reconnect().map_err(|retry| failure(format!("{error}; recovery reconnect failed: {retry}")))?;
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!()
    })
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
fn load_recovery(progress: &mut dyn Progress, port: &str, entry_point: EntryPoint) -> Result<(String, Id)> {
    if let Some(chip) = stage(progress, "Load recovery U-Boot", || usb::load_recovery(WATCHDOG_SECONDS, entry_point))? {
        progress.device(&format!("{}: {chip}", Device::Diamond3.description()));
    }
    stage(progress, "Wait for recovery USB", || usb::wait_device(&[Mode::Recovery], seconds(45), Some(port), &Default::default()))
}

/// Brings the unit from whatever mode it is in to SDP or recovery, or leaves
/// it in the application firmware with `stop_at_app`: (port, identity).
pub(crate) fn find_unit(progress: &mut dyn Progress, wait_usb: Duration, stop_at_app: bool, unit: Unit,
                        cancel: &CancelFlag) -> Result<(String, Id)> {
    let Unit { device, mut port, mut id } = unit;
    if device == Device::Onyx {
        return Err(crate::onyx::unsupported(progress));
    }
    if device != Device::Diamond3 {
        return Err(failure(format!("this needs a diamond3; the unit on USB is a {}", device.name())));
    }
    if id == usb::INITRAMFS {
        // An interrupted run left the unit in its initramfs shell.
        (port, id) = stage(progress, "Restart the unit from an earlier initramfs shell", || {
            let shell = InitramfsShell::connect(open_acm(usb::INITRAMFS, seconds(45))?, seconds(30))?;
            shell.restart()?;
            usb::wait_device(&[Mode::App, Mode::Sdp], seconds(180), Some(&port), &Default::default())
        })?;
    }
    if usb::mode(id) == Some(Mode::App) && !stop_at_app {
        progress.waiting(HOLD_BUTTON);
        (port, id) = stage(progress, WAIT_USB_BOOT_STAGE, || usb::wait_device(&[Mode::Sdp], wait_usb, Some(&port), cancel))?;
    }
    Ok((port, id))
}

/// From what find_unit reached to recovery U-Boot: its port.
pub(crate) fn reach_recovery(progress: &mut dyn Progress, port: String, id: Id, entry_point: EntryPoint) -> Result<String> {
    let (port, id) = if id == usb::SDP { load_recovery(progress, &port, entry_point)? } else { (port, id) };
    if id != usb::RECOVERY {
        return Err(failure("recovery U-Boot did not enumerate"));
    }
    Ok(port)
}

/// Runs the recovery from whatever mode the unit is in to a confirmed root
/// shell, writing run records under `work_dir` and the timestamped run log.
/// With `backup`, the whole NAND is backed up there first, from the same
/// recovery U-Boot. Returns the final status line.
pub(crate) fn run(config: &Config, progress: &mut dyn Progress, unit: Unit,
                  cancel: &CancelFlag, backup: Option<&Path>) -> Result<String> {
    let (port, id) = find_unit(progress, config.wait_usb, false, unit, cancel)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let run_dir = config.work_dir.join(&stamp);
    private_dir(&run_dir)?;
    let log_dir = start_log(progress, &stamp)?;
    let mut hand_back = HandBack(vec![run_dir.clone(), log_dir]);
    progress.phase(Phase::Recovery);
    let port = reach_recovery(progress, port, id, config.entry_point)?;
    let mut board = connect_recovery(progress, &port)?;
    report_silicon(progress, &mut board);
    if let Some(out) = backup {
        progress.phase(Phase::Backup);
        let line;
        (board, line) = nand::back_up_then(board, &port, progress, out, &mut hand_back)?;
        progress.status(&line);
    }
    route(progress, config, board, &port, &run_dir)
}

/// Where the initramfs holds the bundle, the run's options, and what
/// unfr0zen-apply leaves for the host.
const BUNDLE: &str = "bundle.tgz";
const OPTIONS: &str = "options.json";
const OUT: &str = "/unfr0zen";

/// Runs the bundle's unfr0zen-apply on the volume the initramfs mounted, and
/// keeps what it reports in the run directory: nlclient.json, and the unit's
/// stock nlclient when no review covers it. Returns initramfs-changes.txt.
fn apply(shell: &mut InitramfsShell<Port>, run_dir: &Path) -> Result<String> {
    // No input: the shell's own (the host's nudges among it) stays out of the hook scripts.
    let run = shell.run_with(&format!("tar -xzf /{BUNDLE} -C / {} && /{} /{BUNDLE} /{OPTIONS} /mnt {OUT} < /dev/null 2>&1",
                                      bundle::APPLY, bundle::APPLY), seconds(600), false);
    match &run {
        Ok(output) => output.lines().for_each(|line| log::info!("unit: {line}")),
        Err(error) => log::error!("unfr0zen-apply failed: {error}"),
    }
    let stock = shell.read_file(&format!("{OUT}/nlclient.stock"))?.map(|(_, _, data)| data);
    if let Some(data) = &stock {
        fs::write(run_dir.join("nlclient.stock"), data)?;
    }
    let saved = || format!("The unit's stock nlclient is saved as {}: attach it to a bug report",
                           run_dir.join("nlclient.stock").display());
    if let Err(error) = run {
        return Err(if stock.is_some() { failure(format!("{error}. {}", saved())) } else { error });
    }
    let (_, _, result) = shell.read_file(&format!("{OUT}/result.json"))?
        .ok_or_else(|| failure("unfr0zen-apply left no result.json"))?;
    let result: serde_json::Value = serde_json::from_slice(&result)
        .map_err(|e| failure(format!("unfr0zen-apply's result.json: {e}")))?;
    if !result["nlclient"].is_null() {
        fs::write(run_dir.join("nlclient.json"), format!("{:#}\n", result["nlclient"]))?;
    }
    if stock.is_some() {
        log::info!("{}", saved());
    }
    let changes = result["changes"].as_array().ok_or_else(|| failure("unfr0zen-apply's result.json lists no changes"))?;
    Ok(changes.iter().map(|change| {
        let field = |key: &str| change[key].as_str().unwrap_or("-").to_string();
        format!("{} {}: {} -> {}\n", field("action"), field("name"), field("old"), field("new"))
    }).collect())
}

/// From recovery U-Boot to the confirmed root shell.
fn route(progress: &mut dyn Progress, config: &Config, mut board: Board, port: &str, run_dir: &Path) -> Result<String> {
    let options = &patch::Options {
        thread: config.options.thread.as_ref()
            .map(|thread| crate::assets::with_thread_radio(thread, assets::THREAD_RADIO, assets::THREAD_RADIO_IDENTITY)),
        ..config.options.clone()
    };
    progress.phase(Phase::Check);
    let environment = stage(progress, "Read installed boot order from NAND", || {
        board.attach_ubi()?;
        board.read_installed_environment()
    })?;
    progress.status(&format!("Installed boot order: {} then {}", environment.order[0], environment.order[1]));
    let slot = stage(progress, "Find the application root volume", || board.select_slot(environment.order))?;
    progress.status(&format!("Application root volume: {slot}"));
    fs::write(run_dir.join("selected-slot.txt"), format!("{slot}\n"))?;

    // The patch bundle and the run's options travel in the initramfs, over
    // fastboot; unfr0zen-apply patches the volume from them on the unit.
    progress.phase(Phase::Prepare);
    let initramfs = stage(progress, "Build the patch bundle and the initramfs", || -> Result<Vec<u8>> {
        let nestlocal = options.mqtt.is_some() || options.matter.is_some() || options.zigbee || options.doom.is_some()
            || options.external_temperature;
        let pki = if nestlocal { Some(stored_pki(&config.data_dir.join("nestlocal-pki"))?) } else { None };
        let settings = serde_json::to_vec_pretty(&Settings::new(options, pki.as_ref())).expect("settings serialize");
        let files = BTreeMap::from([
            (BUNDLE.to_string(), crate::bundle::pack(Device::Diamond3, &options.version, None)?),
            (OPTIONS.to_string(), settings),
        ]);
        let image = initramfs::build(&crate::assets::busybox(), &files);
        fs::write(run_dir.join("initramfs.cpio.gz"), &image)?;
        Ok(image)
    })?;
    let (length, _) = stage(progress, "Upload the initramfs to RAM", || board.stage(&initramfs))?;
    stage(progress, &format!("Boot the {slot} kernel with the initramfs"), || board.boot_initramfs(slot, &environment, length))?;

    let (port, mut shell) = stage(progress, "Wait for the initramfs USB shell", || {
        let (port, id) = usb::wait_device(&[Mode::Initramfs, Mode::Sdp, Mode::App], seconds(120), Some(port), &Default::default())?;
        if id != usb::INITRAMFS {
            let mode = usb::mode(id).map_or("unknown", Mode::name);
            return Err(failure(format!("the unit came back in {mode} mode, not the initramfs shell; {slot} is \
                                        unchanged. The kernel UART (ttymxc0) shows why")));
        }
        let shell = InitramfsShell::connect(open_acm(usb::INITRAMFS, seconds(45))?, seconds(30))?;
        Ok((port, shell))
    })?;
    let probe = shell.run("uname -a; cat /proc/mtd; grep \" /mnt \" /proc/mounts")?;
    if let Some(release) = probe.lines().find(|l| l.starts_with("Linux ")).and_then(|l| l.split_whitespace().nth(2)) {
        let firmware = release.split_once('-').map_or(release, |(_, build)| build);
        log::info!("Firmware {firmware} (kernel release {release})");
    }
    progress.phase(Phase::Patch);
    let changes = stage(progress, &format!("Patch {slot} on the unit"), || apply(&mut shell, run_dir))?;
    fs::write(run_dir.join("initramfs-changes.txt"), changes)?;
    progress.phase(Phase::Restart);
    shell.restart()?;

    let (port, id) = stage(progress, "Wait for the unit to restart", || usb::wait_device(&[Mode::App, Mode::Sdp], seconds(180), Some(&port), &Default::default()))?;
    if id == usb::SDP {
        let (port, _) = load_recovery(progress, &port, config.entry_point)?;
        let mut board = connect_recovery(progress, &port)?;
        board.attach_ubi()?;
        stage(progress, "Chainload stock U-Boot from NAND", || board.boot_vendor())?;
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
    }
    Ok(match shell {
        Some(name) => format!("Root shell confirmed on {name} (UID 0)"),
        None if id == usb::SDP => format!("Patched {slot}; the unit was started from its stock U-Boot. Its boot is not \
                                           confirmed: the USB root shell is off"),
        None => format!("Patched {slot}; the unit booted back to its app"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct ProgressLog(Vec<bool>);

    impl Progress for ProgressLog {
        fn begin(&mut self, _: &str) {}
        fn end(&mut self, _: &str, _: Duration, ok: bool) { self.0.push(ok); }
        fn status(&mut self, _: &str) { panic!("connection retries must only be logged"); }
        fn waiting(&mut self, _: &str) { panic!("connection retries must only be logged"); }
    }

    #[test]
    fn a_dropped_connection_can_succeed_on_retry() {
        let mut progress = ProgressLog::default();
        let (mut attempts, mut reconnects) = (0, 0);
        let board = connect_with_retry(&mut progress, || {
            attempts += 1;
            match attempts {
                1 => Err(Error::Io("Broken pipe".into())),
                2 => Err(Error::Timeout("no prompt".into())),
                _ => Ok("connected"),
            }
        }, || { reconnects += 1; Ok(()) }).unwrap();
        assert_eq!(board, "connected");
        assert_eq!((attempts, reconnects), (3, 2));
        assert_eq!(progress.0, [true]);
    }

    #[test]
    fn connection_retries_stop_after_three_attempts() {
        let (mut attempts, mut reconnects) = (0, 0);
        let result: Result<()> = connect_with_retry(&mut ProgressLog::default(), || {
            attempts += 1;
            Err(Error::Io("Broken pipe".into()))
        }, || { reconnects += 1; Ok(()) });
        assert!(matches!(result, Err(Error::Io(_))));
        assert_eq!((attempts, reconnects), (3, 2));
    }

    #[test]
    fn an_absent_device_stops_retries_and_keeps_the_original_error() {
        let mut attempts = 0;
        let result: Result<()> = connect_with_retry(&mut ProgressLog::default(), || {
            attempts += 1;
            Err(Error::Io("Broken pipe".into()))
        }, || Err(Error::Timeout("USB device absent".into())));
        let error = result.unwrap_err().to_string();
        assert!(error.contains("Broken pipe") && error.contains("USB device absent"), "{error}");
        assert_eq!(attempts, 1);
    }

    #[test]
    fn device_errors_are_not_retried() {
        let result: Result<()> = connect_with_retry(&mut ProgressLog::default(), || {
            Err(Error::Device("invalid interface".into()))
        }, || panic!("device errors must not reconnect"));
        assert!(matches!(result, Err(Error::Device(_))));
    }
}
