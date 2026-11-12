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
use unfr0zen_core::j49::{jffs2, kernel, logo::{self, Logo}};
use unfr0zen_core::j49::nand::{self as layout, Environment, PAGES_PER_BLOCK, PAGE_RAW};
use unfr0zen_core::j49::omap;
use unfr0zen_core::patch::{self, changes_text};
use unfr0zen_core::{Device, Result, Slot};

use crate::device::{CancelFlag, Unit, HOLD_BUTTON, WAIT_USB_BOOT_STAGE};
use crate::flow::{failure, private_dir, screen_builder, seconds, start_log, stored_pki, Config, HandBack};
use crate::progress::{stage, stage_with, Phase, Progress};
use crate::usb::{verify_shell, Id, Pipe};
use usb::Mode;

/// Brings the unit to its boot ROM's USB mode: (port, identity).
pub(crate) fn find_unit(progress: &mut dyn Progress, wait_usb: Duration, unit: Unit,
                        cancel: &CancelFlag) -> Result<(String, Id)> {
    let Unit { device, port, id } = unit;
    if !matches!(device, Device::Diamond | Device::J49) {
        return Err(failure(format!("this needs an OMAP Nest; the unit on USB is a {}", device.name())));
    }
    let app = app_id(device);
    if usb::mode_for(id, app) == Some(Mode::App) {
        progress.waiting(HOLD_BUTTON);
        return stage(progress, WAIT_USB_BOOT_STAGE, || usb::wait_device_for(&[Mode::Rom], wait_usb, Some(&port), app, cancel));
    }
    Ok((port, id))
}

/// The generation's application gadget identity.
fn app_id(device: Device) -> Id {
    if device == Device::Diamond { usb::DIAMOND_APP } else { usb::APP }
}

/// The generation a run is for: the one asked for, else the one the NAND's
/// U-Boot environment names (the boards share their boot ROM's USB identity;
/// Nest's U-Boot tells them apart the same way), else the USB identity's.
fn generation(wanted: Option<Device>, env: &Environment, usb: Device) -> Device {
    wanted.or_else(|| env.device()).unwrap_or(usb)
}

/// The x-loader for the unit's generation as known before the flasher runs:
/// the gen 1's own (crate::diamond) when its application gadget or
/// `--device` named it, else the j49's.
fn xloader(device: Device) -> &'static [u8] {
    if device == Device::Diamond { crate::diamond::XLOADER } else { assets::XLOADER }
}

/// From the boot ROM to the flasher running from SDRAM: the ASIC ID,
/// x-loader into SRAM, then x-loader (re-enumerated) loads the flasher.
/// Returns the ASIC ID too.
pub(crate) fn load_flasher(device: Device) -> Result<(Vec<u8>, Flasher<Pipe>)> {
    let xloader = xloader(device);
    if xloader.is_empty() {
        return Err(failure("this build has no x-loader (it needs Docker to build)"));
    }
    let (port, mut rom) = usb::open_rom(seconds(10))?;
    let asic = omap::probe(&mut rom)?;
    log::info!("OMAP boot ROM at {port}, ASIC ID {}", asic.iter().map(|b| format!("{b:02x}")).collect::<String>());
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
pub(crate) fn read_environment(nand: &mut dyn Nand) -> Result<Environment> {
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
    device: Device,
    slot: Slot,
    changed: bool,
}

/// From the NAND, by flasher or image, to the patched slot: everything but
/// finding the unit and restarting it. `usb` is the generation the USB
/// identity gave, which the environment's nlmodel settles; `silicon` names
/// the SoC for the progress.
fn route(progress: &mut dyn Progress, config: &Config, usb: Device, silicon: &str, nand: &mut dyn Nand, run_dir: &Path)
         -> Result<Outcome> {
    let options = &patch::Options {
        thread: config.options.thread.as_ref()
            .map(|thread| crate::assets::with_thread_radio(thread, assets::THREAD_RADIO, assets::THREAD_RADIO_IDENTITY)),
        ssh_server: config.options.ssh.then(assets::ssh_server),
        ..config.options.clone()
    };
    progress.phase(Phase::Check);
    progress.status(&format!("NAND: {}", nand.chip()));
    let env = stage(progress, "Read installed boot order from NAND", || read_environment(nand))?;
    let device = generation(config.omap, &env, usb);
    progress.device(&format!("{}: {silicon}", device.description()));
    progress.status(&format!("Generation: {} (nlmodel {})", device.description(), env.get("nlmodel").unwrap_or("unset")));
    unsupported(device, &config.options)?;
    let order = env.boot_order()?;
    progress.status(&format!("Installed boot order: {} then {}", order[0], order[1]));
    let slot = order[0];
    progress.status(&format!("Application root slot: {slot}"));
    fs::write(run_dir.join("selected-slot.txt"), format!("{slot}\n"))?;
    let raw = stage_with(progress, &format!("Read {slot} over USB"), |progress| read_slot(nand, progress, slot))?;
    let mut volume = stage(progress, &format!("Scan {slot}'s JFFS2"), || jffs2::Slot::load(raw.clone()))?;
    let nestlocal_binary = if options.mqtt.is_some() || options.matter.is_some() || options.zigbee || options.doom.is_some() {
        crate::assets::nestlocal()
    } else {
        Vec::new()
    };
    // The patched volume is built in memory, then written.
    progress.phase(Phase::Prepare);
    let label = format!("Patch {slot}");
    let pki_dir = config.data_dir.join("nestlocal-pki");
    let patch = |volume: &mut jffs2::Slot| patch::patch_on_device(volume, device, options, &nestlocal_binary,
                                                                 &mut || stored_pki(&pki_dir),
                                                                 false, &screen_builder(run_dir));
    let changes = stage(progress, &label, || match patch(&mut volume) {
        // Too little room beside the old nestlocal or nlclient: again, from
        // the same read, with the old ones' blocks erased for the new.
        Err(error) if volume.out_of_room() => {
            log::info!("{slot}: {error}; patching again with the old nestlocal's and nlclient's blocks erased to make room");
            volume = jffs2::Slot::load(raw.clone())?;
            volume.make_room_in_place();
            patch(&mut volume)
        }
        other => other,
    })?;
    fs::write(run_dir.join("changes.txt"), changes_text(&changes))?;
    let upper = stage(progress, "Choose the kernel booted first", || upper_kernel(nand, slot, options))?;
    progress.phase(Phase::Patch);
    let mut changed = false;
    if !changes.is_empty() {
        let plan = volume.programmed()?;
        let (free, used) = volume.usage();
        let pages = plan.before.len() + plan.after.len();
        let reclaimed = match plan.erase.len() {
            0 => String::new(),
            n => format!(" and {n} of old copies, erased to make room"),
        };
        progress.status(&format!("{} changes: {pages} pages in {used} blocks, of {free} erased{reclaimed}", changes.len()));
        stage_with(progress, &format!("Write and verify {slot} in NAND"), |progress| write_slot(nand, progress, slot, &plan))?;
        changed = true;
    }
    // The slot first: whatever stops after it leaves the unit booting the
    // stock kernel, until the environment names the new one.
    if let Some((image, what)) = upper {
        let written = stage(progress, &format!("Install {what}"), || kernel::install(nand, slot, &image))?;
        let line = format!("{what} {}", if written { "installed" } else { "already in place" });
        progress.status(&format!("{}{}", line[..1].to_uppercase(), &line[1..]));
        let first = stage(progress, "Have U-Boot boot it first", || kernel::boot_first(nand, slot))?;
        progress.status(if first { "U-Boot environment updated" } else { "U-Boot already boots it first" });
        changed |= written || first;
    }
    Ok(Outcome { device, slot, changed })
}

/// The kernel for the upper half of the slot's boot partition, booted first,
/// and what it is: the one with the USB serial port, either kernel with the
/// dark boot logo for the dark theme, or without either option the stock
/// kernel in place of a dark one there. None when the upper half stays.
fn upper_kernel(nand: &mut dyn Nand, slot: Slot, options: &patch::Options) -> Result<Option<(Vec<u8>, String)>> {
    let usb = "the kernel with the USB serial port";
    let stock = |nand: &mut dyn Nand| kernel::read(nand, slot, false)?
        .ok_or_else(|| failure(format!("no stock kernel in {slot}'s boot partition")));
    if options.dark_theme {
        let source = if options.usb_shell { assets::KERNEL.to_vec() } else { stock(nand)? };
        if let Some(dark) = logo::darken(&source)? {
            let what = if options.usb_shell { format!("{usb} and the dark boot logo") } else { "the kernel with the dark boot logo".into() };
            return Ok(Some((dark, what)));
        }
        log::warn!("{slot}: the kernel's boot logo is not Nest's, or does not fit recoloured; left as it is");
    }
    if options.usb_shell {
        return Ok(Some((assets::KERNEL.to_vec(), usb.into())));
    }
    match kernel::read(nand, slot, true)? {
        Some(upper) if logo::logo(&upper)? == Some(Logo::Dark) => Ok(Some((stock(nand)?, "the stock kernel's boot logo".into()))),
        _ => Ok(None),
    }
}

/// Refuses what a Diamond cannot run: its CC2530 radio uses UART, and it has
/// no kernel with a USB serial port.
fn unsupported(device: Device, options: &patch::Options) -> Result<()> {
    if device != Device::Diamond {
        return Ok(());
    }
    if options.thread.is_some() || options.zigbee {
        return Err(failure("Diamond's CC2530 radio uses UART; --matter-thread and --zigbee need a separate radio implementation"));
    }
    if options.usb_shell {
        return Err(failure("Diamond needs its own kernel for --usb-shell"));
    }
    if options.dark_theme {
        return Err(failure("--dark-theme is only for the 2nd generation"));
    }
    Ok(())
}

/// Refuses a run this build cannot do, before the unit is touched.
fn check(config: &Config) -> Result<()> {
    if let Some(device) = config.omap {
        unsupported(device, &config.options)?;
    }
    if config.options.usb_shell && assets::KERNEL.is_empty() {
        return Err(failure("this build has no j49 kernel with a USB serial port (it needs Docker to build); drop --usb-shell"));
    }
    Ok(())
}

/// Runs the recovery from whatever mode the unit is in, as the diamond3's
/// does: run records under `work_dir`, the run log, the final status line.
/// With `backup`, the whole NAND is backed up there first, through the same
/// flasher.
pub(crate) fn run(config: &Config, progress: &mut dyn Progress, unit: Unit,
                  cancel: &CancelFlag, backup: Option<&Path>) -> Result<String> {
    let usb = unit.device;
    check(config)?;
    unsupported(usb, &config.options)?;
    let (port, _) = find_unit(progress, config.wait_usb, unit, cancel)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let run_dir = config.work_dir.join(&stamp);
    private_dir(&run_dir)?;
    let log_dir = start_log(progress, &stamp)?;
    let mut hand_back = HandBack(vec![run_dir.clone(), log_dir]);
    progress.phase(Phase::Recovery);
    let (asic, mut flasher) = stage(progress, "Load the NAND flasher over USB", || load_flasher(usb))?;
    let silicon = omap::silicon(&asic, flasher.idcode);
    let outcome = match backup {
        Some(out) => {
            progress.phase(Phase::Backup);
            nand::back_up_whole(&mut flasher, progress, out, &mut hand_back).and_then(|line| {
                progress.status(&line);
                route(progress, config, usb, &silicon, &mut flasher, &run_dir)
            })
        }
        None => route(progress, config, usb, &silicon, &mut flasher, &run_dir),
    };
    // On failure too: whatever was written left the slot bootable. A
    // failure stays the phase it happened in.
    if outcome.is_ok() {
        progress.phase(Phase::Restart);
    }
    let reset = stage(progress, "Restart the unit", || flasher.reset());
    let outcome = outcome?;
    reset?;
    let app = app_id(outcome.device);
    stage(progress, "Wait for the unit to restart", || usb::wait_device_for(&[Mode::App], seconds(180), Some(&port), app, &Default::default()))?;
    finish(progress, config, &run_dir, &outcome, Some(&port))
}

/// The same run against a raw NAND image (2112-byte pages) instead of a unit,
/// which it rewrites in place: for an emulator, or a backup.
pub fn run_image(config: &Config, image: &Path, progress: &mut dyn Progress) -> Result<String> {
    check(config)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let run_dir = config.work_dir.join(&stamp);
    private_dir(&run_dir)?;
    let log_dir = start_log(progress, &stamp)?;
    let _hand_back = HandBack(vec![run_dir.clone(), log_dir]);
    let name = image.display().to_string();
    let mut nand = flasher::Image::new(fs::read(image).map_err(|e| failure(format!("{name}: {e}")))?, &name)?;
    let outcome = route(progress, config, Device::J49, "image", &mut nand, &run_dir)?;
    if outcome.changed {
        fs::write(image, &nand.raw).map_err(|e| failure(format!("{name}: {e}")))?;
    }
    finish(progress, config, &run_dir, &outcome, None)
}

fn finish(progress: &mut dyn Progress, config: &Config, run_dir: &Path, outcome: &Outcome, port: Option<&str>) -> Result<String> {
    let slot = outcome.slot;
    let shell = match port.filter(|_| config.options.usb_shell) {
        Some(port) => Some(stage(progress, "Wait for USB root shell", || verify_shell(port, app_id(outcome.device), config.wait_shell))?),
        None => None,
    };
    if let Some(matter) = &config.options.matter {
        let code = matter.pairing_code();
        fs::write(run_dir.join("matter-pairing-code.txt"), format!("{code}\n"))?;
    }
    Ok(match (shell, outcome.changed, port) {
        (Some(name), _, _) => format!("Root shell confirmed on {name} (UID 0)"),
        (None, false, _) => format!("{slot} has every change already; nothing written"),
        (None, true, Some(_)) => format!("Patched {slot}; the unit booted back to its app"),
        (None, true, None) => format!("Patched {slot} in the image"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use unfr0zen_core::j49::flasher::Image;

    /// boot0 with `stock` in its lower half, the upper half erased.
    fn boot0(stock: &[u8]) -> Image {
        let (start, size) = layout::boot_partition(Slot::Root0);
        let mut image = Image::new(vec![0xff; (start + size) / layout::PAGE * PAGE_RAW], "test").unwrap();
        let pages = layout::encode(stock);
        image.raw[start / layout::PAGE * PAGE_RAW..][..pages.len()].copy_from_slice(&pages);
        image
    }

    #[test]
    fn the_dark_logo_goes_first_and_without_the_option_comes_back_off() {
        let Ok(stock) = fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build/j49/uImage")) else {
            eprintln!("No build/j49/uImage; skipping");
            return;
        };
        let mut nand = boot0(&stock);
        let neither = patch::Options::default();
        let dark = patch::Options { dark_theme: true, ..Default::default() };
        assert!(upper_kernel(&mut nand, Slot::Root0, &neither).unwrap().is_none(), "nothing to undo");
        let (image, what) = upper_kernel(&mut nand, Slot::Root0, &dark).unwrap().unwrap();
        assert_eq!((logo::logo(&image).unwrap(), what.as_str()), (Some(Logo::Dark), "the kernel with the dark boot logo"));
        kernel::install(&mut nand, Slot::Root0, &image).unwrap();
        let (image, what) = upper_kernel(&mut nand, Slot::Root0, &neither).unwrap().unwrap();
        assert_eq!((image, what.as_str()), (stock, "the stock kernel's boot logo"));
    }

    #[test]
    fn a_gen_1_gets_its_own_x_loader_and_a_gen_2_the_j49s() {
        if crate::diamond::XLOADER.is_empty() || assets::XLOADER.is_empty() {
            eprintln!("No x-loaders in this build; skipping");
            return;
        }
        assert_ne!(crate::diamond::XLOADER, assets::XLOADER);
        assert_eq!(xloader(Device::Diamond), crate::diamond::XLOADER);
        assert_eq!(xloader(Device::J49), assets::XLOADER);
    }
}
