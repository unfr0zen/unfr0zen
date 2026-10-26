//! The j49's whole NAND, backed up, verified or restored through the
//! flasher: raw pages with their OOB (2112 bytes each), so bad blocks and the
//! ECC are kept as they are. Port of scripts/backup_nand.py.
//!
//! A backup keeps a block from one read when the ECC finds every page clean,
//! and otherwise (bits flipped, or x-loader's blocks, which the boot ROM's
//! own ECC covers) reads it until two reads agree (a third breaks a tie;
//! raw reads can differ by flipped bits the ECC corrects). It keeps each chunk
//! in OUT.parts/ once read, so running the same command again resumes, and
//! writes its SHA-256 beside it. A verify compares each page's ECC-corrected
//! data, and that erased pages are still erased.
//!
//! A restore writes such a backup back, beside its .sha256, once the image's
//! factory-bad blocks are found to be the chip's: each block that does not
//! already verify is erased, its pages programmed raw as the image has them,
//! and read back. The boot ROM's USB boot does not depend on the NAND, so an
//! interrupted restore is run again, and writes only what still differs.

use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use unfr0zen_core::j49::flasher::{self, Flasher, Nand, CHIP_SIZES};
use unfr0zen_core::j49::nand::{self, BLOCK_RAW, PAGES_PER_BLOCK, PAGE_RAW};
use unfr0zen_core::j49::omap;
use unfr0zen_core::shell::sha256_hex;
use unfr0zen_core::Result;

use super::{find_unit, load_flasher};
use crate::device::Unit;
use crate::flow::{failure, start_log, HandBack};
use crate::nand::{checksum_path, file, read_at, Job};
use crate::progress::{stage, stage_with, Progress};
use crate::usb::Pipe;

/// Whether a whole-NAND image of `len` bytes is a j49's: raw pages with their OOB.
pub fn is_image(len: u64) -> bool {
    CHIP_SIZES.iter().any(|&size| len == size / nand::PAGE as u64 * PAGE_RAW as u64)
}

/// Blocks per part of a backup: 8 MiB of data.
const CHUNK_BLOCKS: usize = 64;

/// Every page erased, or holding data whose ECC finds no bit to fix: a read
/// that needs no second one to confirm it.
fn clean(block_raw: &[u8]) -> bool {
    block_raw.chunks(PAGE_RAW).all(|page| nand::erased(page) || matches!(nand::correct_page(page), Ok((_, 0))))
}

/// One block, kept from one read when it is clean, else read until two reads
/// agree; (raw, whether it was clean or any two did).
fn stable_block(nand: &mut dyn Nand, block: usize) -> Result<(Vec<u8>, bool)> {
    let mut reads: Vec<Vec<u8>> = Vec::new();
    for _ in 0..3 {
        let data = nand.read(block * PAGES_PER_BLOCK, PAGES_PER_BLOCK)?;
        if reads.is_empty() && clean(&data) {
            return Ok((data, true));
        }
        if reads.contains(&data) {
            return Ok((data, true));
        }
        reads.push(data);
    }
    Ok((reads.swap_remove(0), false))
}

fn blocks(nand: &dyn Nand) -> usize {
    (nand.size() / nand::BLOCK as u64) as usize
}

/// Reports `block` under way, of the NAND's.
fn at_block(progress: &mut dyn Progress, nand: &dyn Nand, block: usize) {
    progress.transfer((block * BLOCK_RAW) as u64, BLOCK_RAW as u64, (blocks(nand) * BLOCK_RAW) as u64);
}

/// Runs `step` on each chunk of blocks but those `kept` from an earlier run,
/// logging each chunk's time.
fn each_chunk(nand: &mut dyn Nand, progress: &mut dyn Progress, verb: &str, kept: impl Fn(usize) -> bool,
              mut step: impl FnMut(&mut dyn Nand, &mut dyn Progress, usize, usize) -> Result<()>) -> Result<()> {
    let blocks = blocks(nand);
    let total = (blocks * BLOCK_RAW) as u64;
    stage_with(progress, &format!("{verb} NAND"), |progress| {
        for first in (0..blocks).step_by(CHUNK_BLOCKS) {
            let count = CHUNK_BLOCKS.min(blocks - first);
            if kept(first) {
                log::info!("{verb} NAND: blocks {first}-{} kept from an earlier run", first + count - 1);
                continue;
            }
            let started = Instant::now();
            step(nand, progress, first, count)?;
            log::info!("{verb} NAND: blocks {first}-{} of {blocks} ({:.1}s)", first + count - 1, started.elapsed().as_secs_f64());
        }
        progress.transfer(total, 0, total);
        Ok(())
    })
}

/// Whether the unit's page reads as the image's: both erased, or the same
/// data once each is ECC-corrected (raw where the ECC cannot correct, as on a
/// bad block).
fn same_page(want: &[u8], got: &[u8]) -> bool {
    match (nand::erased(want), nand::erased(got)) {
        (true, true) => true,
        (false, false) => match (nand::correct_page(want), nand::correct_page(got)) {
            (Ok((want, _)), Ok((got, _))) => want == got,
            _ => want == got,
        },
        _ => false,
    }
}

/// The first page of a block that does not read as the image's.
fn differing_page(want: &[u8], got: &[u8]) -> Option<usize> {
    want.chunks(PAGE_RAW).zip(got.chunks(PAGE_RAW)).position(|(want, got)| !same_page(want, got))
}

fn backup(nand: &mut dyn Nand, progress: &mut dyn Progress, out: &Path, parts: &Path) -> Result<(usize, usize)> {
    file(parts, fs::create_dir_all(parts))?;
    let part = |first: usize| parts.join(format!("{first:05}.raw"));
    let (mut bad, mut unstable) = (0, 0);
    each_chunk(nand, progress, "Back up", |first| part(first).exists(), |nand, progress, first, count| {
        let mut data = Vec::with_capacity(count * BLOCK_RAW);
        for block in first..first + count {
            at_block(progress, nand, block);
            let (raw, stable) = stable_block(nand, block)?;
            if nand::factory_bad(&raw) {
                bad += 1;
            } else if !stable {
                unstable += 1;
                log::warn!("block {block}: no two raw reads agreed, nor did the ECC find it clean; the first is kept");
            }
            data.extend(raw);
        }
        let partial = part(first).with_extension("tmp");
        file(&partial, fs::write(&partial, &data))?;
        file(&part(first), fs::rename(&partial, part(first)))
    })?;
    let partial = out.with_extension("tmp");
    let mut names: Vec<_> = file(parts, fs::read_dir(parts))?.filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "raw")).collect();
    names.sort();
    file(&partial, File::create(&partial).and_then(|mut image| {
        for name in &names {
            io::copy(&mut File::open(name)?, &mut image)?;
        }
        Ok(())
    }))?;
    file(out, fs::rename(&partial, out))?;
    Ok((bad, unstable))
}

fn verify(nand: &mut dyn Nand, progress: &mut dyn Progress, image: &Path) -> Result<()> {
    each_chunk(nand, progress, "Verify", |_| false, |nand, progress, first, count| {
        for block in first..first + count {
            at_block(progress, nand, block);
            let expected = read_at(image, (block * BLOCK_RAW) as u64, BLOCK_RAW)?;
            let actual = nand.read(block * PAGES_PER_BLOCK, PAGES_PER_BLOCK)?;
            if let Some(index) = differing_page(&expected, &actual) {
                return Err(failure(format!("NAND page {} differs from the image", block * PAGES_PER_BLOCK + index)));
            }
        }
        Ok(())
    })
}

/// Writes `image` back (see the module's description); `confirm` is asked
/// once everything is checked and before anything is erased. Returns how
/// many blocks were written.
fn restore(nand: &mut dyn Nand, progress: &mut dyn Progress, image: &Path,
           confirm: &mut dyn FnMut(&str) -> bool) -> Result<usize> {
    let blocks = blocks(nand);
    let len = file(image, fs::metadata(image))?.len();
    if len != (blocks * BLOCK_RAW) as u64 {
        return Err(failure(format!("{} is a {} MiB NAND's image; this unit has the {}", image.display(),
                                   (len / PAGE_RAW as u64 * nand::PAGE as u64) >> 20, nand.chip())));
    }
    // Erasing a factory-bad block would lose its marker; one bad in the image
    // only, or on the chip only, is not this chip's backup or cannot hold it.
    let differ = stage_with(progress, "Compare bad blocks with the image's", |progress| {
        let mut differ = Vec::new();
        for block in 0..blocks {
            at_block(progress, nand, block);
            let chip = nand.read(block * PAGES_PER_BLOCK, 2)?;
            let image = read_at(image, (block * BLOCK_RAW) as u64, 2 * PAGE_RAW)?;
            if nand::factory_bad(&chip) != nand::factory_bad(&image) {
                differ.push(block);
            }
        }
        Ok(differ)
    })?;
    if !differ.is_empty() {
        return Err(failure(format!("bad blocks {differ:?} differ between the chip and {}: it is not this unit's \
                                    backup, or a block has gone bad since; NAND untouched", image.display())));
    }
    if !confirm(&format!("Erase and write the whole NAND ({}) from {}", nand.chip(), image.display())) {
        return Err(failure("restore not confirmed; NAND untouched"));
    }
    nand.unlock()?;
    let mut written = 0;
    each_chunk(nand, progress, "Restore", |_| false, |nand, progress, first, count| {
        for block in first..first + count {
            at_block(progress, nand, block);
            let want = read_at(image, (block * BLOCK_RAW) as u64, BLOCK_RAW)?;
            if nand::factory_bad(&want) || differing_page(&want, &nand.read(block * PAGES_PER_BLOCK, PAGES_PER_BLOCK)?).is_none() {
                continue;
            }
            match nand.erase(block)? {
                0 => {}
                status => return Err(failure(format!("erasing block {block}: {}", flasher::status_text(status)))),
            }
            for (index, raw) in want.chunks(PAGE_RAW).enumerate() {
                // Blank pages stay erased; any other is programmed whole, so
                // a JFFS2 clean marker in the OOB comes back too.
                if raw.iter().all(|&b| b == 0xff) {
                    continue;
                }
                let page = block * PAGES_PER_BLOCK + index;
                match nand.program(page, raw)? {
                    0 => {}
                    status => return Err(failure(format!("programming page {page}: {}", flasher::status_text(status)))),
                }
            }
            if let Some(index) = differing_page(&want, &nand.read(block * PAGES_PER_BLOCK, PAGES_PER_BLOCK)?) {
                return Err(failure(format!("page {} reads back different after its restore", block * PAGES_PER_BLOCK + index)));
            }
            written += 1;
        }
        Ok(())
    })?;
    Ok(written)
}

/// Runs the job and restarts the unit after, on failure too. `confirm` is
/// asked before a restore writes anything.
pub(crate) fn run(job: &Job, unit: Unit, wait_usb: Duration, progress: &mut dyn Progress,
                  confirm: &mut dyn FnMut(&str) -> bool) -> Result<String> {
    if let Job::Restore(image) = job {
        let checksum = checksum_path(image);
        let Ok(expected) = fs::read_to_string(&checksum) else {
            return Err(failure(format!("{} is missing; restore only a verified backup", checksum.display())));
        };
        let sha = stage(progress, "Check the image's SHA-256", || file(image, fs::read(image)).map(|data| sha256_hex(&data)))?;
        if expected.split_whitespace().next() != Some(sha.as_str()) {
            return Err(failure(format!("{} does not match {}", image.display(), checksum.display())));
        }
    }
    let device = unit.device;
    find_unit(progress, wait_usb, unit)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let mut hand_back = HandBack(vec![start_log(progress, &stamp)?]);
    let (asic, mut flasher): (Vec<u8>, Flasher<Pipe>) = stage(progress, "Load the NAND flasher over USB", || load_flasher(device))?;
    progress.device(&format!("{}: {}", device.description(), omap::silicon(&asic, flasher.idcode)));
    progress.status(&format!("NAND: {}", flasher.chip()));
    let result = (|| -> Result<String> {
        Ok(match job {
            Job::Backup(out) => {
                let parts = out.with_extension("parts");
                hand_back.0.push(parts.clone());
                let (bad, unstable) = backup(&mut flasher, progress, out, &parts)?;
                let checksum = checksum_path(out);
                let sha = stage(progress, "Hash the image", || file(out, fs::read(out)).map(|data| sha256_hex(&data)))?;
                let name = out.file_name().unwrap_or_default().to_string_lossy();
                file(&checksum, fs::write(&checksum, format!("{sha}  {name}\n")))?;
                file(&parts, fs::remove_dir_all(&parts))?;
                hand_back.0.pop();
                hand_back.0.extend([out.clone(), checksum]);
                progress.status(&format!("{bad} factory-bad blocks; {unstable} blocks whose raw reads never agreed"));
                format!("NAND backup: {} (raw 2112-byte pages, SHA-256 {sha})", out.display())
            }
            Job::Verify(image) => {
                verify(&mut flasher, progress, image)?;
                format!("NAND matches {}", image.display())
            }
            Job::Restore(image) => {
                let written = restore(&mut flasher, progress, image, confirm)?;
                format!("NAND restored from {} ({written} blocks written, the rest already matched)", image.display())
            }
        })
    })();
    let reset = stage(progress, "Restart the unit", || flasher.reset());
    let message = result?;
    reset?;
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unfr0zen_core::j49::flasher::Image;

    /// Reads that differ until the given read of each block.
    struct Flaky {
        image: Image,
        reads: usize,
        settle: usize,
    }

    impl Nand for Flaky {
        fn read(&mut self, first: usize, count: usize) -> Result<Vec<u8>> {
            self.reads += 1;
            let mut data = self.image.read(first, count)?;
            if self.reads < self.settle {
                data[0] ^= self.reads as u8;
            }
            Ok(data)
        }
        fn program(&mut self, page: usize, raw: &[u8]) -> Result<u32> { self.image.program(page, raw) }
        fn erase(&mut self, block: usize) -> Result<u32> { self.image.erase(block) }
        fn unlock(&mut self) -> Result<()> { self.image.unlock() }
        fn reset(&mut self) -> Result<()> { Ok(()) }
        fn chip(&self) -> &str { "flaky" }
        fn size(&self) -> u64 { self.image.size() }
    }

    struct Silent;
    impl Progress for Silent {
        fn begin(&mut self, _: &str) {}
        fn end(&mut self, _: &str, _: Duration, _: bool) {}
        fn status(&mut self, _: &str) {}
        fn waiting(&mut self, _: &str) {}
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("unfr0zen-j49-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Marks a block factory-bad, as the chip's maker does.
    fn mark_bad(raw: &mut [u8], block: usize) {
        raw[block * BLOCK_RAW + nand::PAGE] = 0;
    }

    #[test]
    fn a_block_is_kept_once_two_reads_agree() {
        let image = Image::new(vec![0xff; BLOCK_RAW], "one block").unwrap();
        let mut erased = Flaky { image, reads: 0, settle: 0 };
        let (data, stable) = stable_block(&mut erased, 0).unwrap();
        assert!(stable && data[0] == 0xff && erased.reads == 1, "a clean block takes one read");
        // A page whose ECC fixes a bit, the same on every read.
        let mut raw = vec![0xff; BLOCK_RAW];
        raw[..PAGE_RAW].copy_from_slice(&nand::encode_page(&[0x5a; nand::PAGE], &[0xff; nand::OOB]));
        raw[100] ^= 0x10;
        let mut flipped = Flaky { image: Image::new(raw.clone(), "one block").unwrap(), reads: 0, settle: 0 };
        let (data, stable) = stable_block(&mut flipped, 0).unwrap();
        assert!(stable && data == raw && flipped.reads == 2, "a corrected one is confirmed by a second");
        let image = Image::new(vec![0xff; BLOCK_RAW], "one block").unwrap();
        let (data, stable) = stable_block(&mut Flaky { image, reads: 0, settle: 9 }, 0).unwrap();
        assert!(!stable && data[0] == 0xff ^ 1, "the first read when none agree");
    }

    #[test]
    fn a_backup_verifies_against_its_unit() {
        let dir = temp_dir("verify");
        let mut image = Image::new(vec![0xff; 70 * BLOCK_RAW], "unit").unwrap();
        image.raw[5 * BLOCK_RAW + 7] = 0x12;
        let out = dir.join("nand.raw");
        backup(&mut image, &mut Silent, &out, &dir.join("nand.parts")).unwrap();
        assert_eq!(fs::read(&out).unwrap(), image.raw);
        verify(&mut image, &mut Silent, &out).unwrap();
        image.raw[6 * BLOCK_RAW] = 0;
        assert!(verify(&mut image, &mut Silent, &out).unwrap_err().to_string().contains("page 384"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_restore_writes_back_only_the_blocks_that_differ_anywhere_on_the_chip() {
        let dir = temp_dir("restore");
        let mut backup = vec![0xff; 70 * BLOCK_RAW];
        mark_bad(&mut backup, 40);
        // x-loader's block, which the locked flasher never writes, a root
        // slot's page with a clean marker in its OOB, and one more.
        backup[7] = 0x12;
        backup[20 * BLOCK_RAW + 3 * PAGE_RAW + 5] = 0x34;
        backup[20 * BLOCK_RAW + 3 * PAGE_RAW + nand::PAGE + 8] = 0x85;
        backup[66 * BLOCK_RAW + 100] = 0x56;
        let path = dir.join("nand.raw");
        fs::write(&path, &backup).unwrap();
        let mut unit = Image::new(backup.clone(), "unit").unwrap();
        unit.raw[7] = 0x10;
        unit.raw[20 * BLOCK_RAW + 3 * PAGE_RAW + 5] = 0x00;
        unit.raw[30 * BLOCK_RAW + 9] = 0x00;
        let mut asked = Vec::new();
        let written = restore(&mut unit, &mut Silent, &path, &mut |what: &str| { asked.push(what.to_string()); true }).unwrap();
        assert_eq!(written, 3, "blocks 0, 20 and 30");
        assert!(unit.raw == backup, "the unit's NAND is the backup, OOB and bad-block marker included");
        assert_eq!(asked.len(), 1);
        assert_eq!(restore(&mut unit, &mut Silent, &path, &mut |_: &str| true).unwrap(), 0, "a second run writes nothing");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_restore_leaves_the_nand_untouched_unless_everything_agrees() {
        let dir = temp_dir("restore-refused");
        let mut backup = vec![0xff; 70 * BLOCK_RAW];
        backup[30 * BLOCK_RAW] = 0;
        let path = dir.join("nand.raw");
        fs::write(&path, &backup).unwrap();
        let mut unit = Image::new(vec![0xff; 70 * BLOCK_RAW], "unit").unwrap();
        mark_bad(&mut unit.raw, 12);
        let before = unit.raw.clone();
        let mut asked = false;
        let error = restore(&mut unit, &mut Silent, &path, &mut |_: &str| { asked = true; true }).unwrap_err().to_string();
        assert!(error.contains("bad blocks [12]") && !asked, "{error}");
        unit.raw[12 * BLOCK_RAW + nand::PAGE] = 0xff;
        let error = restore(&mut unit, &mut Silent, &path, &mut |_: &str| false).unwrap_err().to_string();
        assert!(error.contains("not confirmed"), "{error}");
        unit.raw[12 * BLOCK_RAW + nand::PAGE] = 0;
        assert!(unit.raw == before);
        fs::write(&path, &backup[..69 * BLOCK_RAW]).unwrap();
        let error = restore(&mut unit, &mut Silent, &path, &mut |_: &str| true).unwrap_err().to_string();
        assert!(error.contains("NAND's image"), "{error}");
        fs::remove_dir_all(dir).unwrap();
    }
}
