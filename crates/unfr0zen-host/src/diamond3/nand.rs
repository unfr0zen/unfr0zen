//! The whole 512 MiB NAND backed up, verified or restored through recovery
//! U-Boot. Port of scripts/backup_nand.py.
//!
//! A backup reads every chunk into the fastboot buffer and pulls it over USB,
//! checked against U-Boot's CRC32, then reads NAND a second time and compares
//! (the boot-control pages, whose bits flip between reads, are the only bytes
//! allowed to differ). Chunks already read are kept in OUT.parts/, so running
//! the same command again resumes an interrupted backup. Bad blocks are kept
//! in the image as erased, at their own offsets.
//!
//! A block with a page the ECC cannot correct does not stop a backup or a
//! verify: it is kept in the image as read, the rest of its pages intact, and
//! it reads differently each time, so a verify leaves it out. The .bad list
//! beside the image names these blocks and the bad ones; a backup adds any its
//! verify finds. A verify reports the ones it finds beyond the list, and a
//! restore's read-back fails on any.
//!
//! A restore writes such a backup back, beside its .sha256. Two areas are not
//! copied from the image: the FCB blocks, whose ROM encoding a BCH18 read
//! cannot capture, and the last four blocks, which hold this chip's own bad
//! block table. The current FCB is kept when it points at the image's U-Boot
//! copies; otherwise the FCB and DBBT are regenerated from the copies' IVTs.
//! This chip's bad blocks are skipped, and must be erased in the image.

use std::fs::{self, File};
use std::io;
use std::path::Path;

use unfr0zen_core::diamond3::nand::{all_chunks, fcb_matches, image_firmware, restore_chunks, BLOCK, BOOT_CONTROL, BOOT_SIZE};
pub use unfr0zen_core::diamond3::nand::NAND_SIZE;
use unfr0zen_core::shell::sha256_hex;
use unfr0zen_core::Result;

use super::usb;
use super::{EntryPoint, connect_recovery, find_unit, reach_recovery, report_silicon, Board};
use crate::device;
use crate::flow::{failure, seconds, start_log, HandBack};
use crate::nand::{add_bad_blocks, bad_blocks_path, checksum_path, file, read_at, read_bad_blocks, read_checked, recorded_sha256, write_bad_blocks, Job};
use crate::progress::{stage, stage_with, Progress};

/// Whether a whole-NAND image of `len` bytes is a diamond3's: its
/// ECC-corrected logical image.
pub fn is_image(len: u64) -> bool {
    len == u64::from(NAND_SIZE)
}

/// Recovery U-Boot, its USB port, and the chip's bad blocks.
struct Unit {
    board: Board,
    port: String,
    bad: Vec<u32>,
}

/// Runs `step` on each chunk but those `kept` from an earlier run, as one
/// stage with its progress in bytes. A failed chunk ends the stage; verified
/// backup parts remain available for the next run.
fn each_chunk(progress: &mut dyn Progress, verb: &str, chunks: &[(u32, u32)],
              kept: impl Fn(u32) -> bool, mut step: impl FnMut(u32, u32) -> Result<()>) -> Result<()> {
    let total: u64 = chunks.iter().map(|c| u64::from(c.1)).sum();
    stage_with(progress, &format!("{verb} NAND"), |progress| {
        let mut done = 0;
        for &(offset, size) in chunks {
            if kept(offset) {
                log::info!("NAND {offset:#x}+{size:#x} kept from an earlier run");
            } else {
                progress.transfer(done, size.into(), total);
                step(offset, size)?;
                log::info!("{verb} NAND {offset:#x}+{size:#x}: done");
            }
            done += u64::from(size);
        }
        progress.transfer(total, 0, total);
        Ok(())
    })
}

/// Runs the job from whatever mode the unit is in and boots it normally after.
/// `confirm` is asked before a restore writes anything. Returns the final
/// status line.
pub(crate) fn run(job: &Job, unit: device::Unit, wait_usb: std::time::Duration, entry_point: EntryPoint,
                  progress: &mut dyn Progress, confirm: &mut dyn FnMut(&str) -> bool,
                  cancel: &device::CancelFlag) -> Result<String> {
    let recorded = if let Job::Restore(image) = job { recorded_sha256(image)? } else { String::new() };
    let (port, id) = find_unit(progress, wait_usb, false, unit, cancel)?;
    // Checked while the boot ROM waits, before anything is loaded into it.
    let mut copies = Vec::new();
    if let Job::Restore(image) = job {
        let data = read_checked(progress, image, &recorded)?;
        copies = image_firmware(&data[..BOOT_SIZE as usize])?;
    }
    // The run log opens here: nothing has been sent to the unit yet.
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let mut hand_back = HandBack(vec![start_log(progress, &stamp)?]);
    if let Job::Restore(image) = job {
        log::info!("{} matches its SHA-256 {recorded}", image.display());
    }
    let port = reach_recovery(progress, port, id, entry_point)?;
    let mut board = connect_recovery(progress, &port)?;
    report_silicon(progress, &mut board);
    let bad = stage(progress, "Check for bad blocks", || board.bad_blocks())?;
    if bad.is_empty() {
        progress.status("NAND has no bad blocks");
    } else {
        log::info!("Bad blocks: {bad:#x?}");
        progress.status(&format!("NAND has {} bad blocks; they are skipped and kept as erased", bad.len()));
    }
    let mut unit = Unit { board, port, bad };
    let message = match job {
        Job::Backup(out) => {
            let parts = out.with_extension("parts");
            hand_back.0.push(parts.clone());
            let uncorrectable = backup(&mut unit, progress, out, &parts)?;
            let found = verify(&mut unit, progress, out)?;
            add_bad_blocks(&bad_blocks_path(out), &found)?;
            let checksum = checksum_path(out);
            let sha = stage(progress, "Hash the image", || file(out, fs::read(out)).map(|data| sha256_hex(&data)))?;
            let name = out.file_name().unwrap_or_default().to_string_lossy();
            file(&checksum, fs::write(&checksum, format!("{sha}  {name}\n")))?;
            file(&parts, fs::remove_dir_all(&parts))?;
            hand_back.0.pop();
            hand_back.0.extend([out.clone(), checksum, bad_blocks_path(out)]);
            let kept = match uncorrectable + found.len() {
                0 => String::new(),
                count => format!("; {count} blocks with uncorrectable pages kept as read and left out of the check"),
            };
            format!("NAND backup verified: {} ({NAND_SIZE} bytes, SHA-256 {sha}){kept}", out.display())
        }
        Job::Verify(image) => {
            let left_out = match verify(&mut unit, progress, image)?.len() {
                0 => String::new(),
                count => format!("; {count} blocks not listed beside it read uncorrectable and were left out of the check"),
            };
            format!("NAND matches {}{left_out}", image.display())
        }
        Job::Restore(image) => {
            restore(&mut unit, progress, image, &copies, confirm)?;
            format!("NAND restored from {}", image.display())
        }
    };
    let Unit { board, port, .. } = unit;
    stage(progress, "Chainload stock U-Boot from NAND", || board.boot_vendor())?;
    stage(progress, "Wait for the application firmware", || usb::wait_restored_application(seconds(120), &port))?;
    Ok(message)
}

/// Writes the image and its bad block list. Returns how many blocks have
/// uncorrectable pages.
fn backup(unit: &mut Unit, progress: &mut dyn Progress, out: &Path, parts: &Path) -> Result<usize> {
    file(parts, fs::create_dir_all(parts))?;
    let part = |offset: u32| parts.join(format!("{offset:08x}.bin"));
    // Parts are renamed into place only once read and checked, each after its list.
    each_chunk(progress, "Back up", &all_chunks(), |offset| part(offset).exists(), |offset, size| {
        let part = part(offset);
        let (data, uncorrectable) = unit.board.read_nand(offset, size, &unit.bad)?;
        if !uncorrectable.is_empty() {
            log::warn!("NAND blocks {uncorrectable:#x?} have uncorrectable pages; kept as read");
        }
        write_bad_blocks(&part.with_extension("bad"), &uncorrectable)?;
        let partial = part.with_extension("tmp");
        file(&partial, fs::write(&partial, &data))?;
        file(&part, fs::rename(&partial, &part))
    })?;
    let mut listed = unit.bad.clone();
    for (offset, _) in all_chunks() {
        listed.extend(read_bad_blocks(&part(offset).with_extension("bad"))?.unwrap_or_default());
    }
    let uncorrectable = listed.len() - unit.bad.len();
    listed.sort_unstable();
    write_bad_blocks(&bad_blocks_path(out), &listed)?;
    let partial = out.with_extension("tmp");
    file(&partial, File::create(&partial).and_then(|mut image| {
        for (offset, _) in all_chunks() {
            io::copy(&mut File::open(parts.join(format!("{offset:08x}.bin")))?, &mut image)?;
        }
        Ok(())
    }))?;
    file(out, fs::rename(&partial, out))?;
    Ok(uncorrectable)
}

/// Erases `blocks` in `data`, the part of an image at `offset`, as a read that
/// skips them leaves the buffer.
fn erase_blocks(data: &mut [u8], offset: u32, blocks: &[u32]) {
    for &block in blocks {
        if block >= offset && ((block - offset) as usize) < data.len() {
            let start = (block - offset) as usize;
            let end = data.len().min(start + BLOCK as usize);
            data[start..end].fill(0xff);
        }
    }
}

/// Compares NAND with the image but for the blocks its list names and those
/// that read uncorrectable now, which it returns.
fn verify(unit: &mut Unit, progress: &mut dyn Progress, image: &Path) -> Result<Vec<u32>> {
    let listed = read_bad_blocks(&bad_blocks_path(image))?.unwrap_or_default();
    let skipped = [unit.bad.as_slice(), listed.as_slice()].concat();
    let mut found = Vec::new();
    each_chunk(progress, "Verify", &all_chunks(), |_| false, |offset, size| {
        let mut expected = read_at(image, offset.into(), size as usize)?;
        erase_blocks(&mut expected, offset, &listed);
        if offset < BOOT_SIZE {
            let (mut actual, uncorrectable) = unit.board.read_nand(offset, size, &skipped)?;
            erase_blocks(&mut actual, offset, &uncorrectable);
            erase_blocks(&mut expected, offset, &uncorrectable);
            found.extend(uncorrectable);
            let control = BOOT_CONTROL as usize;
            if actual[control..] != expected[control..] {
                return Err(failure("the boot partition reads differently a second time outside the boot-control blocks"));
            }
            let flips: u32 = actual[..control].iter().zip(&expected[..control]).map(|(a, b)| (a ^ b).count_ones()).sum();
            log::info!("Boot-control blocks: {flips} bits differ between reads");
            return Ok(());
        }
        let (actual, uncorrectable) = unit.board.tolerant_nand_crc(offset, size, &skipped)?;
        erase_blocks(&mut expected, offset, &uncorrectable);
        found.extend(uncorrectable);
        let expected = crc32fast::hash(&expected);
        if actual != expected {
            return Err(failure(format!("NAND {offset:#x}+{size:#x} reads back as CRC32 {actual:08x}, \
                                        the image has {expected:08x}")));
        }
        Ok(())
    })?;
    if !found.is_empty() {
        log::warn!("NAND blocks {found:#x?} read uncorrectable in the verify; left out of it");
    }
    Ok(found)
}

fn restore(unit: &mut Unit, progress: &mut dyn Progress, image: &Path, copies: &[(u32, u32)],
           confirm: &mut dyn FnMut(&str) -> bool) -> Result<()> {
    let fcb = stage(progress, "Read the FCB", || unit.board.fcb_firmware())?;
    let keep = fcb.as_deref().is_some_and(|fcb| fcb_matches(fcb, copies));
    if keep {
        progress.status("The FCB in NAND points at the image's U-Boot copies; keeping it");
    } else if copies.len() != 2 {
        return Err(failure(format!("the FCB must be regenerated, but the image has {} U-Boot copies (need 2)",
                                   copies.len())));
    } else {
        progress.waiting("The FCB is missing or points elsewhere; it will be regenerated from the image");
    }
    let chunks = restore_chunks(keep);
    let (start, end) = (chunks[0].0, chunks.last().map_or(0, |c| c.0 + c.1));
    let mut lost = Vec::new();
    for &block in unit.bad.iter().filter(|&&block| (start..end).contains(&block)) {
        if read_at(image, block.into(), BLOCK as usize)?.iter().any(|&b| b != 0xff) {
            lost.push(block);
        }
    }
    if !lost.is_empty() {
        return Err(failure(format!("NAND blocks {lost:#x?} are bad on this chip but hold data in {}: \
                                    it is another unit's backup, or they went bad since", image.display())));
    }
    let listed = read_bad_blocks(&bad_blocks_path(image))?.unwrap_or_default();
    let damage = match listed.iter().filter(|block| (start..end).contains(*block) && !unit.bad.contains(*block)).count() {
        0 => String::new(),
        count => format!(". {count} blocks had uncorrectable pages when this backup was taken; they are written as read, any damage included"),
    };
    if !confirm(&format!("Erase and write NAND {start:#x}-{end:#x} from {}{damage}", image.display())) {
        return Err(failure("restore not confirmed; NAND untouched"));
    }
    each_chunk(progress, "Restore", &chunks, |_| false, |offset, size| {
        unit.board.write_nand(offset, &read_at(image, offset.into(), size as usize)?, &unit.bad)
    })?;
    if !keep {
        stage(progress, "Regenerate the FCB and DBBT", || unit.board.write_boot_control(copies))?;
    }
    stage(progress, "Check the FCB against the restored U-Boot copies", || match unit.board.fcb_firmware()? {
        None => Err(failure("no valid FCB after the restore; the boot ROM will not boot from NAND")),
        Some(fcb) if !fcb_matches(&fcb, copies) => {
            Err(failure(format!("the FCB points at {fcb:#x?}, the image has U-Boot at {copies:#x?}")))
        }
        Some(_) => Ok(()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use unfr0zen_core::Error;

    #[test]
    fn a_restore_whose_image_fails_its_check_opens_no_log() {
        #[derive(Default)]
        struct Seen { steps: Vec<String>, notes: Vec<String> }
        impl Progress for Seen {
            fn begin(&mut self, label: &str) { self.steps.push(label.to_string()); }
            fn end(&mut self, _: &str, _: Duration, _: bool) {}
            fn status(&mut self, message: &str) { self.notes.push(message.to_string()); }
            fn waiting(&mut self, _: &str) {}
        }
        let dir = std::env::temp_dir().join(format!("unfr0zen-restore-log-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let image = dir.join("nand.bin");
        fs::write(&image, b"image").unwrap();
        fs::write(checksum_path(&image), "sum  nand.bin\n").unwrap();
        let unit = device::Unit { device: device::Device::Diamond3, port: "1".into(), id: usb::SDP };
        let mut seen = Seen::default();
        assert!(run(&Job::Restore(image), unit, Duration::ZERO, EntryPoint::Auto, &mut seen, &mut |_| false, &Default::default()).is_err());
        assert_eq!(seen.steps, ["Check the image's SHA-256"]);
        assert!(!seen.notes.iter().any(|note| note.starts_with("Detailed log")), "{:?}", seen.notes);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_restore_asks_for_download_mode_before_it_hashes_the_image() {
        struct Steps(Vec<String>);
        impl Progress for Steps {
            fn begin(&mut self, label: &str) { self.0.push(label.to_string()); }
            fn end(&mut self, _: &str, _: Duration, _: bool) {}
            fn status(&mut self, _: &str) {}
            fn waiting(&mut self, _: &str) {}
        }
        let dir = std::env::temp_dir().join(format!("unfr0zen-restore-order-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let image = dir.join("nand.bin");
        fs::write(&image, b"image").unwrap();
        fs::write(checksum_path(&image), "sum  nand.bin\n").unwrap();
        let unit = device::Unit { device: device::Device::Diamond3, port: "1".into(), id: usb::APP_ACM };
        let cancel = device::CancelFlag::new(true.into());
        let mut steps = Steps(Vec::new());
        assert!(run(&Job::Restore(image), unit, Duration::ZERO, EntryPoint::Auto, &mut steps, &mut |_| false, &cancel).is_err());
        assert_eq!(steps.0, [device::WAIT_USB_BOOT_STAGE]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[derive(Default)]
    struct ProgressLog {
        transfers: Vec<(u64, u64, u64)>,
        outcomes: Vec<bool>,
    }

    impl Progress for ProgressLog {
        fn begin(&mut self, _: &str) {}
        fn end(&mut self, _: &str, _: Duration, ok: bool) { self.outcomes.push(ok); }
        fn status(&mut self, _: &str) {}
        fn waiting(&mut self, _: &str) {}
        fn transfer(&mut self, done: u64, next: u64, total: u64) {
            self.transfers.push((done, next, total));
        }
    }

    #[test]
    fn only_the_listed_blocks_of_a_chunk_are_erased() {
        let block = BLOCK as usize;
        let mut data = vec![1u8; 3 * block];
        erase_blocks(&mut data, BOOT_SIZE, &[0, BOOT_SIZE + BLOCK, BOOT_SIZE + 3 * BLOCK]);
        assert!(data[..block].iter().all(|&b| b == 1));
        assert!(data[block..2 * block].iter().all(|&b| b == 0xff));
        assert!(data[2 * block..].iter().all(|&b| b == 1));
    }

    #[test]
    fn a_disconnect_ends_the_stage_without_retrying_or_advancing() {
        for verb in ["Back up", "Verify", "Restore"] {
            let mut progress = ProgressLog::default();
            let mut attempted = Vec::new();
            let error = each_chunk(&mut progress, verb, &[(0, 4), (4, 4), (8, 4)], |_| false, |offset, _| {
                attempted.push(offset);
                if offset == 4 { Err(Error::Io("USB disconnected".into())) } else { Ok(()) }
            }).unwrap_err();
            assert!(matches!(error, Error::Io(ref message) if message == "USB disconnected"));
            assert_eq!(attempted, [0, 4]);
            assert_eq!(progress.transfers, [(0, 4, 12), (4, 4, 12)]);
            assert_eq!(progress.outcomes, [false]);
        }
    }

    #[test]
    fn a_resumed_backup_skips_saved_chunks_and_reports_completion() {
        let mut progress = ProgressLog::default();
        let mut attempted = Vec::new();
        each_chunk(&mut progress, "Back up", &[(0, 4), (4, 4)], |offset| offset == 0, |offset, _| {
            attempted.push(offset);
            Ok(())
        }).unwrap();
        assert_eq!(attempted, [4]);
        assert_eq!(progress.transfers, [(4, 4, 8), (8, 0, 8)]);
        assert_eq!(progress.outcomes, [true]);
    }
}
