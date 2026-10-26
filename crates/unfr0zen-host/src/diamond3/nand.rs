//! The whole 512 MiB NAND backed up, verified or restored through recovery
//! U-Boot. Port of scripts/backup_nand.py.
//!
//! A backup reads every chunk into the fastboot buffer and pulls it over USB,
//! checked against U-Boot's CRC32, then reads NAND a second time and compares
//! (the boot-control pages, whose bits flip between reads, are the only bytes
//! allowed to differ). Chunks already read are kept in OUT.parts/, so running
//! the same command again resumes an interrupted backup. A chip with bad
//! blocks is refused: `nand read` skips them and would shift everything after.
//!
//! A restore writes such a backup back, beside its .sha256. Two areas are not
//! copied from the image: the FCB blocks, whose ROM encoding a BCH18 read
//! cannot capture, and the last four blocks, which hold this chip's own bad
//! block table. The current FCB is kept when it points at the image's U-Boot
//! copies; otherwise the FCB and DBBT are regenerated from the copies' IVTs.

use std::fs::{self, File};
use std::io;
use std::path::Path;

use unfr0zen_core::diamond3::nand::{all_chunks, fcb_matches, image_firmware, restore_chunks, BOOT_CONTROL, BOOT_SIZE};
pub use unfr0zen_core::diamond3::nand::NAND_SIZE;
use unfr0zen_core::shell::sha256_hex;
use unfr0zen_core::{Error, Result};

use super::usb::{self, Mode};
use super::{connect_recovery, find_unit, reach_recovery, report_silicon, Board, WATCHDOG_SECONDS};
use crate::device;
use crate::flow::{failure, seconds, start_log, HandBack};
use crate::nand::{checksum_path, file, read_at, Job};
use crate::progress::{stage, stage_with, Progress};

/// Whether a whole-NAND image of `len` bytes is a diamond3's: its
/// ECC-corrected logical image.
pub fn is_image(len: u64) -> bool {
    len == u64::from(NAND_SIZE)
}

/// Recovery U-Boot and the port it is on, which a reload may change.
struct Unit {
    board: Board,
    port: String,
}

impl Unit {
    /// Runs `step` on each chunk but those `kept` from an earlier run, as one
    /// stage with its progress in bytes. Recovery U-Boot sometimes dies after
    /// large transfers and resets to SDP: then it is reloaded and the chunk
    /// done again, once.
    fn each_chunk(&mut self, progress: &mut dyn Progress, verb: &str, chunks: &[(u32, u32)],
                  kept: impl Fn(u32) -> bool, mut step: impl FnMut(&mut Board, u32, u32) -> Result<()>) -> Result<()> {
        let total: u64 = chunks.iter().map(|c| u64::from(c.1)).sum();
        stage_with(progress, &format!("{verb} NAND"), |progress| {
            let mut done = 0;
            for &(offset, size) in chunks {
                if kept(offset) {
                    log::info!("NAND {offset:#x}+{size:#x} kept from an earlier run");
                } else {
                    progress.transfer(done, size.into(), total);
                    match step(&mut self.board, offset, size) {
                        Err(Error::Io(dropped)) => {
                            log::warn!("Recovery dropped at NAND {offset:#x} ({dropped}); reloading it");
                            self.reload().map_err(|e| failure(format!("{dropped}; recovery did not come back: {e}")))?;
                            step(&mut self.board, offset, size)
                        }
                        other => other,
                    }?;
                    log::info!("{verb} NAND {offset:#x}+{size:#x}: done");
                }
                done += u64::from(size);
            }
            progress.transfer(total, 0, total);
            Ok(())
        })
    }

    fn reload(&mut self) -> Result<()> {
        let (port, _) = usb::wait_device(&[Mode::Sdp], seconds(180), Some(&self.port))?;
        usb::load_recovery(WATCHDOG_SECONDS)?;
        let (port, _) = usb::wait_device(&[Mode::Recovery], seconds(45), Some(&port))?;
        self.board = connect_recovery(&port)?;
        self.port = port;
        Ok(())
    }
}

/// Runs the job from whatever mode the unit is in and boots it normally after.
/// `confirm` is asked before a restore writes anything. Returns the final
/// status line.
pub(crate) fn run(job: &Job, unit: device::Unit, wait_usb: std::time::Duration, progress: &mut dyn Progress,
                  confirm: &mut dyn FnMut(&str) -> bool) -> Result<String> {
    // Everything that can be checked before the unit is touched.
    let mut copies = Vec::new();
    if let Job::Restore(image) = job {
        let checksum = checksum_path(image);
        let Ok(expected) = fs::read_to_string(&checksum) else {
            return Err(failure(format!("{} is missing; restore only a verified backup", checksum.display())));
        };
        let data = stage(progress, "Check the image's SHA-256", || file(image, fs::read(image)))?;
        if expected.split_whitespace().next() != Some(sha256_hex(&data).as_str()) {
            return Err(failure(format!("{} does not match {}", image.display(), checksum.display())));
        }
        copies = image_firmware(&data[..BOOT_SIZE as usize])?;
    }

    let (port, id) = find_unit(progress, wait_usb, false, unit)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let mut hand_back = HandBack(vec![start_log(progress, &stamp)?]);
    let port = reach_recovery(progress, port, id)?;
    let mut board = stage(progress, "Connect to recovery console", || connect_recovery(&port))?;
    report_silicon(progress, &mut board);
    let mut unit = Unit { board, port };
    let bad = stage(progress, "Check for bad blocks", || unit.board.bad_blocks())?;
    if !bad.is_empty() {
        return Err(failure(format!("NAND has bad blocks at {bad:#x?}; `nand read` skips them, so an image would shift")));
    }
    progress.status("NAND has no bad blocks");
    let message = match job {
        Job::Backup(out) => {
            let parts = out.with_extension("parts");
            hand_back.0.push(parts.clone());
            backup(&mut unit, progress, out, &parts)?;
            verify(&mut unit, progress, out)?;
            let checksum = checksum_path(out);
            let sha = stage(progress, "Hash the image", || file(out, fs::read(out)).map(|data| sha256_hex(&data)))?;
            let name = out.file_name().unwrap_or_default().to_string_lossy();
            file(&checksum, fs::write(&checksum, format!("{sha}  {name}\n")))?;
            file(&parts, fs::remove_dir_all(&parts))?;
            hand_back.0.pop();
            hand_back.0.extend([out.clone(), checksum]);
            format!("NAND backup verified: {} ({NAND_SIZE} bytes, SHA-256 {sha})", out.display())
        }
        Job::Verify(image) => {
            verify(&mut unit, progress, image)?;
            format!("NAND matches {}", image.display())
        }
        Job::Restore(image) => {
            restore(&mut unit, progress, image, &copies, confirm)?;
            format!("NAND restored from {}", image.display())
        }
    };
    let Unit { board, port } = unit;
    stage(progress, "Chainload stock U-Boot from NAND", || board.boot_vendor())?;
    stage(progress, "Wait for the application firmware", || usb::wait_device(&[Mode::App], seconds(120), Some(&port)))?;
    Ok(message)
}

fn backup(unit: &mut Unit, progress: &mut dyn Progress, out: &Path, parts: &Path) -> Result<()> {
    file(parts, fs::create_dir_all(parts))?;
    let part = |offset: u32| parts.join(format!("{offset:08x}.bin"));
    // Parts are renamed into place only once read and checked.
    unit.each_chunk(progress, "Back up", &all_chunks(), |offset| part(offset).exists(), |board, offset, size| {
        let part = part(offset);
        let data = board.read_nand(offset, size)?;
        let partial = part.with_extension("tmp");
        file(&partial, fs::write(&partial, &data))?;
        file(&part, fs::rename(&partial, &part))
    })?;
    let partial = out.with_extension("tmp");
    file(&partial, File::create(&partial).and_then(|mut image| {
        for (offset, _) in all_chunks() {
            io::copy(&mut File::open(parts.join(format!("{offset:08x}.bin")))?, &mut image)?;
        }
        Ok(())
    }))?;
    file(out, fs::rename(&partial, out))
}

fn verify(unit: &mut Unit, progress: &mut dyn Progress, image: &Path) -> Result<()> {
    unit.each_chunk(progress, "Verify", &all_chunks(), |_| false, |board, offset, size| {
        let expected = read_at(image, offset.into(), size as usize)?;
        if offset < BOOT_SIZE {
            let actual = board.read_nand(offset, size)?;
            let control = BOOT_CONTROL as usize;
            if actual[control..] != expected[control..] {
                return Err(failure("the boot partition reads differently a second time outside the boot-control blocks"));
            }
            let flips: u32 = actual[..control].iter().zip(&expected[..control]).map(|(a, b)| (a ^ b).count_ones()).sum();
            log::info!("Boot-control blocks: {flips} bits differ between reads");
            return Ok(());
        }
        let (actual, expected) = (board.nand_crc(offset, size)?, crc32fast::hash(&expected));
        if actual != expected {
            return Err(failure(format!("NAND {offset:#x}+{size:#x} reads back as CRC32 {actual:08x}, \
                                        the image has {expected:08x}")));
        }
        Ok(())
    })
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
    if !confirm(&format!("Erase and write NAND {start:#x}-{end:#x} from {}", image.display())) {
        return Err(failure("restore not confirmed; NAND untouched"));
    }
    unit.each_chunk(progress, "Restore", &chunks, |_| false, |board, offset, size| {
        board.write_nand(offset, &read_at(image, offset.into(), size as usize)?)
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
