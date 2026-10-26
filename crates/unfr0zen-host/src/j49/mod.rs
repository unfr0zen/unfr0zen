//! The OMAP boards' recovery flow: from their boot ROM, load x-loader and the NAND
//! flasher over USB, read the root slot the unit boots, patch it on the host
//! by appending JFFS2 nodes into its erased blocks, program and read back
//! those pages, and restart the unit. Port of unfr0zen-gen2.py.
//!
//! Every step leaves the NAND bootable when it stops: only erased pages of
//! the slot are programmed, and a write cut short leaves nodes whose CRCs
//! fail, which JFFS2 skips. So after a failure the unit is reset rather than
//! left in the flasher.

pub mod assets;
pub mod nand;
pub mod usb;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::Duration;

use unfr0zen_core::j49::flasher::{self, Flasher, Nand};
use unfr0zen_core::j49::{jffs2, kernel};
use unfr0zen_core::j49::nand::{self as layout, Environment, PAGES_PER_BLOCK, PAGE_RAW};
use unfr0zen_core::j49::omap;
use unfr0zen_core::patch::{self, changes_text};
use unfr0zen_core::{Device, Result, Slot};

use crate::device::{Unit, HOLD_BUTTON, WAIT_USB_BOOT_STAGE};
use crate::flow::{failure, private_dir, seconds, start_log, stored_pki, Config, HandBack};
use crate::progress::{stage, stage_with, Progress};
use crate::usb::{verify_shell, Id, Pipe};
use usb::Mode;

/// Brings the unit to its boot ROM's USB mode: (port, identity).
pub(crate) fn find_unit(progress: &mut dyn Progress, wait_usb: Duration, unit: Unit) -> Result<(String, Id)> {
    let Unit { device, port, id } = unit;
    if !matches!(device, Device::Diamond | Device::J49) {
        return Err(failure(format!("this needs an OMAP Nest; the unit on USB is a {}", device.name())));
    }
    let app = if device == Device::Diamond { usb::DIAMOND_APP } else { usb::APP };
    if usb::mode_for(id, app) == Some(Mode::App) {
        progress.waiting(HOLD_BUTTON);
        return stage(progress, WAIT_USB_BOOT_STAGE, || usb::wait_device_for(&[Mode::Rom], wait_usb, Some(&port), app));
    }
    Ok((port, id))
}

/// From the boot ROM to the flasher running from SDRAM: the ASIC ID,
/// x-loader into SRAM, then x-loader (re-enumerated) loads the flasher.
/// Returns the ASIC ID too.
pub(crate) fn load_flasher(device: Device) -> Result<(Vec<u8>, Flasher<Pipe>)> {
    let xloader = if device == Device::Diamond { assets::DIAMOND_XLOADER } else { assets::XLOADER };
    if xloader.is_empty() {
        return Err(failure(format!("this build has no {} x-loader (it needs Docker to build)", device.name())));
    }
    let (port, mut rom) = usb::open_rom(seconds(10))?;
    let asic = omap::probe(&mut rom)?;
    log::info!("{} boot ROM at {port}, ASIC ID {}", device.name(), asic.iter().map(|b| format!("{b:02x}")).collect::<String>());
    omap::load_first_stage(&mut rom, xloader)?;
    drop(rom);
    std::thread::sleep(Duration::from_millis(20));
    let (_, mut xloader) = usb::open_rom(seconds(60))?;
    let files = [omap::File { name: "nand-flasher", data: assets::FLASHER, address: flasher::BASE }];
    for message in omap::upload_files(&mut xloader, &files)? {
        log::info!("x-loader: {message}");
    }
    // x-loader is already waiting for its next file request's answer: the
    // jump must be the next bytes on the wire.
    omap::jump(&mut xloader, flasher::BASE)?;
    Ok((asic, Flasher::connect(xloader)?))
}

/// The U-Boot environment the unit boots with.
fn read_environment(nand: &mut dyn Nand) -> Result<Environment> {
    let mut copies = Vec::new();
    for offset in layout::ENV_COPIES {
        let raw = nand.read(offset / layout::PAGE, layout::ENV_SIZE / layout::PAGE)?;
        let mut data = Vec::with_capacity(layout::ENV_SIZE);
        for page in raw.chunks(PAGE_RAW) {
            data.extend(layout::correct_page(page)?.0);
        }
        let copy = Environment::parse(&data);
        log::info!("U-Boot environment at {offset:#x}: {}", copy.as_ref().map_or("invalid".into(), |c| {
            format!("generation {}, bootcmd {:?}", c.generation, c.get("bootcmd").unwrap_or(""))
        }));
        copies.push(copy);
    }
    Ok(layout::newest(&copies)?.1.clone())
}

/// The slot's raw pages, a block at a time.
fn read_slot(nand: &mut dyn Nand, progress: &mut dyn Progress, slot: Slot) -> Result<Vec<u8>> {
    let (offset, length) = layout::slot_partition(slot);
    let (first, pages) = (offset / layout::PAGE, length / layout::PAGE);
    let total = (pages * PAGE_RAW) as u64;
    let mut raw = Vec::with_capacity(pages * PAGE_RAW);
    for block in (0..pages).step_by(PAGES_PER_BLOCK) {
        progress.transfer(raw.len() as u64, layout::BLOCK_RAW as u64, total);
        raw.extend(nand.read(first + block, PAGES_PER_BLOCK)?);
    }
    progress.transfer(total, 0, total);
    Ok(raw)
}

/// Programs the patched slot in its plan's order (the pages before the erase,
/// the erase, the pages after), then reads every block it touched back and
/// checks each page that holds data, and that the erased ones are.
fn write_slot(nand: &mut dyn Nand, progress: &mut dyn Progress, slot: Slot, plan: &jffs2::Programming) -> Result<()> {
    let start = layout::slot_partition(slot).0 / layout::PAGE;
    let patched = &plan.raw;
    let pages = [&plan.before[..], &plan.after[..]].concat();
    let blocks: BTreeSet<usize> = pages.iter().map(|page| page / PAGES_PER_BLOCK).chain(plan.erase.iter().copied()).collect();
    let total = ((pages.len() + blocks.len() * PAGES_PER_BLOCK) * PAGE_RAW) as u64;
    let mut done = 0;
    let mut program = |nand: &mut dyn Nand, progress: &mut dyn Progress, pages: &[usize]| -> Result<()> {
        for &page in pages {
            progress.transfer(done, PAGE_RAW as u64, total);
            let status = nand.program(start + page, &patched[page * PAGE_RAW..(page + 1) * PAGE_RAW])?;
            if status != 0 {
                return Err(failure(format!("page {}: {}", start + page, flasher::status_text(status))));
            }
            done += PAGE_RAW as u64;
        }
        Ok(())
    };
    program(nand, progress, &plan.before)?;
    if !plan.erase.is_empty() {
        nand.unlock()?;
        for &block in &plan.erase {
            let status = nand.erase(start / PAGES_PER_BLOCK + block)?;
            if status != 0 {
                return Err(failure(format!("erasing block {}: {}", start / PAGES_PER_BLOCK + block, flasher::status_text(status))));
            }
        }
    }
    program(nand, progress, &plan.after)?;
    let mut corrected = 0;
    for &block in &blocks {
        progress.transfer(done, layout::BLOCK_RAW as u64, total);
        let first = block * PAGES_PER_BLOCK;
        let back = nand.read(start + first, PAGES_PER_BLOCK)?;
        for (index, got) in back.chunks(PAGE_RAW).enumerate() {
            let want = &patched[(first + index) * PAGE_RAW..(first + index + 1) * PAGE_RAW];
            if layout::erased(want) {
                if plan.erase.contains(&block) && !layout::erased(got) {
                    return Err(failure(format!("page {} is not erased after its block's erase", start + first + index)));
                }
                continue;
            }
            let (data, bits) = layout::correct_page(got)?;
            if data[..] != want[..layout::PAGE] {
                return Err(failure(format!("page {} reads back wrong", start + first + index)));
            }
            corrected += bits;
        }
        done += layout::BLOCK_RAW as u64;
    }
    progress.transfer(total, 0, total);
    log::info!("{slot}: {} pages programmed, {} blocks erased, read back ({corrected} bits corrected by ECC on readback)",
               pages.len(), plan.erase.len());
    Ok(())
}

/// What a run did, for its final line.
struct Outcome {
    slot: Slot,
    changed: bool,
}

/// From the NAND, by flasher or image, to the patched slot: everything but
/// finding the unit and restarting it.
fn route(progress: &mut dyn Progress, config: &Config, device: Device, nand: &mut dyn Nand, run_dir: &Path) -> Result<Outcome> {
    let options = &patch::Options {
        thread: config.options.thread.as_ref()
            .map(|thread| crate::assets::with_thread_radio(thread, assets::THREAD_RADIO, assets::THREAD_RADIO_IDENTITY)),
        ssh_server: config.options.ssh.then(assets::ssh_server),
        ..config.options.clone()
    };
    progress.status(&format!("NAND: {}", nand.chip()));
    let env = stage(progress, "Read installed boot order from NAND", || read_environment(nand))?;
    let order = env.boot_order()?;
    progress.status(&format!("Installed boot order: {} then {}", order[0], order[1]));
    let slot = config.slot.unwrap_or(order[0]);
    progress.status(&format!("Application root slot: {slot}"));
    fs::write(run_dir.join("selected-slot.txt"), format!("{slot}\n"))?;
    let raw = stage_with(progress, &format!("Read {slot} over USB"), |progress| read_slot(nand, progress, slot))?;
    if config.backup {
        let original = run_dir.join("original.raw");
        fs::write(&original, &raw)?;
        progress.status(&format!("Original root slot saved: {} (raw 2112-byte pages)", original.display()));
    }
    let mut volume = stage(progress, &format!("Scan {slot}'s JFFS2"), || jffs2::Slot::load(raw.clone()))?;
    let nestlocal_binary = if options.mqtt.is_some() || options.matter.is_some() || options.zigbee || options.doom.is_some() {
        crate::assets::nestlocal()
    } else {
        Vec::new()
    };
    let label = if config.dry_run { format!("Patch copies of {slot} files (dry run)") } else { format!("Patch {slot}") };
    let pki_dir = config.data_dir.join("nestlocal-pki");
    let patch = |volume: &mut jffs2::Slot| patch::patch_on_device(volume, device, options, &nestlocal_binary,
                                                                 crate::assets::CUSTOM_SCREEN, &mut || stored_pki(&pki_dir),
                                                                 &BTreeSet::new(), config.dry_run);
    let changes = stage(progress, &label, || match patch(&mut volume) {
        // Too little room beside the old nestlocal: again, from the same
        // read, with the old one's blocks erased for the new.
        Err(error) if volume.out_of_room() => {
            log::info!("{slot}: {error}; patching again with the old nestlocal's blocks erased to make room");
            volume = jffs2::Slot::load(raw.clone())?;
            volume.make_room_for_nestlocal();
            patch(&mut volume)
        }
        other => other,
    })?;
    fs::write(run_dir.join("changes.txt"), changes_text(&changes))?;
    if config.dry_run {
        if options.usb_shell {
            progress.status(&format!("Dry run: the kernel with a USB serial port would go in {slot}'s boot partition, \
                                      booted first"));
        }
        return Ok(Outcome { slot, changed: false });
    }
    let mut changed = false;
    if !changes.is_empty() {
        let plan = volume.programmed()?;
        let (free, used) = volume.usage();
        let pages = plan.before.len() + plan.after.len();
        let reclaimed = match plan.erase.len() {
            0 => String::new(),
            n => format!(" and {n} the old nestlocal's, erased to make room"),
        };
        progress.status(&format!("{} changes: {pages} pages in {used} blocks, of {free} erased{reclaimed}", changes.len()));
        stage_with(progress, &format!("Write and verify {slot} in NAND"), |progress| write_slot(nand, progress, slot, &plan))?;
        changed = true;
    }
    // The slot first: whatever stops after it leaves the unit booting the
    // stock kernel, until the environment names the new one.
    if options.usb_shell {
        let written = stage(progress, "Install the kernel with the USB serial port", || kernel::install(nand, slot, assets::KERNEL))?;
        progress.status(if written { "Kernel with the USB serial port installed" } else { "Kernel with the USB serial port already in place" });
        let first = stage(progress, "Have U-Boot boot it first", || kernel::boot_first(nand, slot))?;
        progress.status(if first { "U-Boot environment updated" } else { "U-Boot already boots it first" });
        changed |= written || first;
    }
    Ok(Outcome { slot, changed })
}

/// Refuses a run this build cannot do, before the unit is touched.
fn check(config: &Config) -> Result<()> {
    if config.omap == Some(Device::Diamond) && (config.options.thread.is_some() || config.options.zigbee) {
        return Err(failure("Diamond's CC2530 radio uses UART; --matter-thread and --zigbee need a separate radio implementation"));
    }
    if config.omap == Some(Device::Diamond) && config.options.usb_shell {
        return Err(failure("Diamond needs its own kernel for --usb-shell"));
    }
    if config.options.usb_shell && assets::KERNEL.is_empty() {
        return Err(failure("this build has no j49 kernel with a USB serial port (it needs Docker to build); drop --usb-shell"));
    }
    Ok(())
}

/// Runs the recovery from whatever mode the unit is in, as the diamond3's
/// does: run records under `work_dir`, the run log, the final status line.
pub(crate) fn run(config: &Config, progress: &mut dyn Progress, unit: Unit) -> Result<String> {
    let device = unit.device;
    check(config)?;
    if device == Device::Diamond && (config.options.thread.is_some() || config.options.zigbee || config.options.usb_shell) {
        return Err(failure("Diamond's UART radio and USB shell kernel are not available"));
    }
    let (port, _) = find_unit(progress, config.wait_usb, unit)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let run_dir = config.work_dir.join(&stamp);
    private_dir(&run_dir)?;
    let log_dir = start_log(progress, &stamp)?;
    let _hand_back = HandBack(vec![run_dir.clone(), log_dir]);
    let (asic, mut flasher) = stage(progress, "Load the NAND flasher over USB", || load_flasher(device))?;
    progress.device(&format!("{}: {}", device.description(), omap::silicon(&asic, flasher.idcode)));
    let outcome = route(progress, config, device, &mut flasher, &run_dir);
    // On failure too: whatever was written left the slot bootable.
    let reset = stage(progress, "Restart the unit", || flasher.reset());
    let outcome = outcome?;
    reset?;
    let app = if device == Device::Diamond { usb::DIAMOND_APP } else { usb::APP };
    stage(progress, "Wait for the unit to restart", || usb::wait_device_for(&[Mode::App], seconds(180), Some(&port), app))?;
    finish(progress, config, &run_dir, &outcome, Some(&port), device)
}

/// The same run against a raw NAND image (2112-byte pages) instead of a unit,
/// which it rewrites in place: for an emulator, or a backup.
pub fn run_image(config: &Config, image: &Path, progress: &mut dyn Progress) -> Result<String> {
    let device = config.omap.unwrap_or(Device::J49);
    check(config)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let run_dir = config.work_dir.join(&stamp);
    private_dir(&run_dir)?;
    let log_dir = start_log(progress, &stamp)?;
    let _hand_back = HandBack(vec![run_dir.clone(), log_dir]);
    let name = image.display().to_string();
    progress.device(&format!("{}: image", device.description()));
    let mut nand = flasher::Image::new(fs::read(image).map_err(|e| failure(format!("{name}: {e}")))?, &name)?;
    let outcome = route(progress, config, device, &mut nand, &run_dir)?;
    if outcome.changed {
        fs::write(image, &nand.raw).map_err(|e| failure(format!("{name}: {e}")))?;
    }
    finish(progress, config, &run_dir, &outcome, None, device)
}

fn finish(progress: &mut dyn Progress, config: &Config, run_dir: &Path, outcome: &Outcome, port: Option<&str>, device: Device) -> Result<String> {
    let slot = outcome.slot;
    if config.dry_run {
        return Ok(format!("Dry run done: {slot} left as it was; changes listed in {}", run_dir.join("changes.txt").display()));
    }
    let shell = match port.filter(|_| config.options.usb_shell) {
        Some(port) => Some(stage(progress, "Wait for USB root shell", || verify_shell(port, if device == Device::Diamond { usb::DIAMOND_APP } else { usb::APP }, config.wait_shell))?),
        None => None,
    };
    if let Some(matter) = &config.options.matter {
        let code = matter.pairing_code();
        fs::write(run_dir.join("matter-pairing-code.txt"), format!("{code}\n"))?;
        progress.status(&format!("Matter pairing code: {code} (Home Assistant: Add device, Matter, enter the code; \
                                  the unit must be on Wi-Fi)"));
    }
    Ok(match (shell, outcome.changed, port) {
        (Some(name), _, _) => format!("Root shell confirmed on {name} (UID 0)"),
        (None, false, _) => format!("{slot} has every change already; nothing written"),
        (None, true, Some(_)) => format!("Patched {slot}; the unit booted back to its app"),
        (None, true, None) => format!("Patched {slot} in the image"),
    })
}
