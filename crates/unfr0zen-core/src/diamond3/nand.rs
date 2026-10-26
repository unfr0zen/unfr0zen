//! The whole NAND as an ECC-corrected logical image, through recovery U-Boot:
//! 2048-byte pages, no OOB, byte offset equal to NAND offset. Reads go through
//! `nand read` and the fastboot buffer; writes through `nand write.trimffs`
//! (erased pages stay erased for UBI). Port of scripts/backup_nand.py.

use std::time::Duration;

use crate::console::Serial;
use crate::diamond3::recovery::{fcb_fields, is_ivt, Fastboot, Recovery, RAM_BASE};
use crate::{device, Result};

pub const NAND_SIZE: u32 = 0x2000_0000;
/// The boot partition. The ROM's boot-control pages in its first blocks are
/// not BCH18, so ECC errors there are expected and its pages kept as read.
pub const BOOT_SIZE: u32 = 0x40_0000;
/// The FCB/DBBT copies in the first four erase blocks.
pub const BOOT_CONTROL: u32 = 0x8_0000;
const CHUNK: u32 = 0x400_0000;
const PAGE: u32 = 0x800;
const BLOCK: u32 = 0x2_0000;
/// The first U-Boot copy may start here, after the FCB and DBBT search areas.
pub const FIRMWARE: u32 = 0x10_0000;
/// Stock U-Boot and Linux keep their flash bad block table in the last 4 blocks.
const BBT_AREA: u32 = 4 * BLOCK;

fn seconds(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// The boot partition, then the rest in fastboot-buffer-sized pieces:
/// (offset, size) covering `start..end`.
pub fn chunks(start: u32, end: u32) -> Vec<(u32, u32)> {
    let mut chunks = Vec::new();
    if start < BOOT_SIZE {
        chunks.push((start, BOOT_SIZE - start));
    }
    let mut offset = start.max(BOOT_SIZE);
    while offset < end {
        chunks.push((offset, CHUNK.min(end - offset)));
        offset += CHUNK;
    }
    chunks
}

/// Every chunk of the whole NAND.
pub fn all_chunks() -> Vec<(u32, u32)> {
    chunks(0, NAND_SIZE)
}

/// What restore writes: the FCB blocks only when they must be regenerated
/// anyway, and never the chip's own bad block table at the end.
pub fn restore_chunks(keep_fcb: bool) -> Vec<(u32, u32)> {
    chunks(if keep_fcb { BOOT_CONTROL } else { FIRMWARE }, NAND_SIZE - BBT_AREA)
}

/// The blocks `nand bad` lists, as U-Boot prints them ("  %08llx"), up to
/// the prompt. Output without the list, or with any other line in it, is an error.
pub fn bad_blocks(response: &str) -> Result<Vec<u32>> {
    let Some((_, list)) = response.split_once("bad blocks:") else {
        return device(format!("nand bad printed no bad block list: {}", response.trim()));
    };
    let list = list.trim_end();
    list.strip_suffix("=>").unwrap_or(list).lines().map(str::trim).filter(|line| !line.is_empty())
        .map(|line| match u32::from_str_radix(line, 16) {
            Ok(offset) if line.bytes().all(|b| b.is_ascii_hexdigit()) => Ok(offset),
            _ => device(format!("nand bad printed {line:?}, not a block offset")),
        })
        .collect()
}

/// (offset, size) of each U-Boot copy in an image's boot partition, from its IVT.
pub fn image_firmware(boot: &[u8]) -> Result<Vec<(u32, u32)>> {
    let word = |at: usize| boot.get(at..at + 4).map(|w| u32::from_le_bytes(w.try_into().unwrap()));
    let mut copies = Vec::new();
    for offset in (FIRMWARE..BOOT_SIZE).step_by(BLOCK as usize) {
        let ivt = (offset + 0x400) as usize;
        let (Some(header), Some(boot_data), Some(own)) = (word(ivt), word(ivt + 16), word(ivt + 20)) else {
            return device("the image's boot partition is cut short");
        };
        if !is_ivt(header) {
            continue;
        }
        // boot_data is an address in the loaded image, which starts 0x400 before the IVT.
        let length = boot_data.checked_sub(own).and_then(|d| word(ivt + d as usize + 4));
        let size = length.map(|l| l.div_ceil(PAGE) * PAGE);
        match size {
            Some(size) if size > 0 && size <= BOOT_SIZE - offset => copies.push((offset, size)),
            _ => return device(format!("U-Boot IVT at {offset:#x} gives an impossible length {length:?}")),
        }
    }
    Ok(copies)
}

/// Whether each U-Boot copy the FCB names is one of the image's, with room for it.
pub fn fcb_matches(fcb: &[(u32, u32)], copies: &[(u32, u32)]) -> bool {
    fcb.iter().all(|(offset, size)| copies.iter().any(|(o, s)| o == offset && size >= s))
}

impl<S: Serial, F: Fastboot> Recovery<S, F> {
    /// The bad blocks recovery's in-memory table found.
    pub fn bad_blocks(&mut self) -> Result<Vec<u32>> {
        bad_blocks(&self.console.command("nand bad", seconds(60))?)
    }

    /// Reads NAND into the fastboot buffer. ECC errors are allowed in the
    /// boot-control blocks only, which are not BCH18; they are read on their
    /// own, as U-Boot names only the start of a read that fails.
    fn nand_to_ram(&mut self, offset: u32, size: u32) -> Result<()> {
        if offset < BOOT_CONTROL && offset + size > BOOT_CONTROL {
            let head = BOOT_CONTROL - offset;
            self.nand_to_address(RAM_BASE, offset, head)?;
            return self.nand_to_address(RAM_BASE + head, BOOT_CONTROL, size - head);
        }
        self.nand_to_address(RAM_BASE, offset, size)
    }

    fn nand_to_address(&mut self, address: u32, offset: u32, size: u32) -> Result<()> {
        let response = self.console.command_allow_failure(&format!("nand read {address:#x} {offset:#x} {size:#x}"),
                                                          seconds(120))?;
        let ecc = offset + size <= BOOT_CONTROL
            && response.contains(&format!("NAND read from offset {offset:x} failed -74"));
        if !response.contains(&format!("{size} bytes read: OK")) && !ecc {
            return device(format!("nand read at {offset:#x} failed: {}", response.trim()));
        }
        Ok(())
    }

    pub fn read_nand(&mut self, offset: u32, size: u32) -> Result<Vec<u8>> {
        self.nand_to_ram(offset, size)?;
        self.upload_ram(size as usize)
    }

    /// The CRC32 of NAND as it reads now, computed on the unit.
    pub fn nand_crc(&mut self, offset: u32, size: u32) -> Result<u32> {
        self.nand_to_ram(offset, size)?;
        self.checked_crc(RAM_BASE, size as usize)
    }

    /// Erases NAND at `offset` and writes `data` there, checked in RAM first and
    /// read back after.
    pub fn write_nand(&mut self, offset: u32, data: &[u8]) -> Result<()> {
        let (size, expected) = self.stage(data)?;
        let response = self.console.command(&format!("nand erase {offset:#x} {size:#x}"), seconds(120))?;
        if !response.contains("OK") {
            return device(format!("nand erase at {offset:#x} failed: {}", response.trim()));
        }
        let response = self.console.command(&format!("nand write.trimffs {RAM_BASE:#x} {offset:#x} {size:#x}"),
                                            seconds(300))?;
        if !response.contains(&format!("{size} bytes written: OK")) {
            return device(format!("nand write at {offset:#x} failed: {}", response.trim()));
        }
        let actual = self.nand_crc(offset, size as u32)?;
        if actual != expected {
            return device(format!("NAND {offset:#x}+{size:#x} reads back as CRC32 {actual:08x}, \
                                   the image has {expected:08x}"));
        }
        Ok(())
    }

    /// The U-Boot copies the FCB in NAND names, or None without a valid FCB.
    pub fn fcb_firmware(&mut self) -> Result<Option<Vec<(u32, u32)>>> {
        let response = self.console.command_allow_failure("nandbcb dump 0", seconds(60))?;
        Ok(fcb_fields(&response).map(|f| {
            let page = |key: &str| f[key] as u32 * PAGE;
            vec![(page("fw1_start"), page("fw1_pages")), (page("fw2_start"), page("fw2_pages"))]
        }))
    }

    /// Writes a new FCB and DBBT for the two U-Boot copies.
    pub fn write_boot_control(&mut self, copies: &[(u32, u32)]) -> Result<()> {
        let &[(fw1, size1), (fw2, size2)] = copies else {
            return device(format!("the FCB needs two U-Boot copies; the image has {}", copies.len()));
        };
        let response = self.console.command(&format!("nand erase 0 {FIRMWARE:#x}"), seconds(60))?;
        if !response.contains("OK") {
            return device(format!("boot control erase failed: {}", response.trim()));
        }
        let response = self.console.command_allow_failure(
            &format!("nandbcb bcbonly 0 {fw1:#x} {size1:#x} {fw2:#x} {size2:#x}"), seconds(60))?;
        if response.contains("ERROR") {
            return device(format!("nandbcb bcbonly failed: {}", response.trim()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diamond3::recovery::tests::{board, uboot, Unit};

    #[test]
    fn chunks_cover_the_nand_once() {
        let all = all_chunks();
        assert_eq!(all[0], (0, BOOT_SIZE));
        assert_eq!(all.len(), 9);
        assert_eq!(all.iter().map(|c| c.1 as u64).sum::<u64>(), NAND_SIZE as u64);
        assert!(all.windows(2).all(|w| w[0].0 + w[0].1 == w[1].0));
        let restore = restore_chunks(true);
        assert_eq!(restore[0], (BOOT_CONTROL, BOOT_SIZE - BOOT_CONTROL));
        assert_eq!(restore.last().map(|c| c.0 + c.1), Some(NAND_SIZE - BBT_AREA));
        assert_eq!(restore_chunks(false)[0].0, FIRMWARE);
    }

    #[test]
    fn every_listed_bad_block_counts() {
        let response = "nand bad\n\nDevice 0 bad blocks:\n  00080000\n  04400000\n  1ff00000\n";
        assert_eq!(bad_blocks(response).unwrap(), [0x8_0000, 0x440_0000, 0x1ff0_0000]);
        assert!(bad_blocks("nand bad\n\nDevice 0 bad blocks:\n").unwrap().is_empty());
        assert!(bad_blocks("nand bad\r\n\r\nDevice 0 bad blocks:\r\n=> ").unwrap().is_empty());
        assert_eq!(bad_blocks("nand bad\r\n\r\nDevice 0 bad blocks:\r\n  00080000\r\n=> ").unwrap(), [0x8_0000]);
    }

    #[test]
    fn bad_block_output_that_does_not_parse_is_an_error() {
        assert!(bad_blocks("nand bad\n").is_err());
        assert!(bad_blocks("nand bad\n\nno devices available\n").is_err());
        assert!(bad_blocks("nand bad\n\nDevice 0 bad blocks:\n  00080000\n  garbled\n").is_err());
        assert!(bad_blocks("nand bad\n\nDevice 0 bad blocks:\n  +0080000\n").is_err());
    }

    #[test]
    fn firmware_copies_come_from_their_ivts() {
        let mut boot = vec![0xffu8; BOOT_SIZE as usize];
        for offset in [0x10_0000, 0x20_0000] {
            let image = uboot(0x66010, 0x4020_00d1);
            boot[offset..offset + image.len()].copy_from_slice(&image);
        }
        assert_eq!(image_firmware(&boot).unwrap(), [(0x10_0000, 0x66800), (0x20_0000, 0x66800)]);
        boot[0x20_0424..0x20_0428].copy_from_slice(&0x30_0000u32.to_le_bytes());
        assert!(image_firmware(&boot).is_err());
    }

    #[test]
    fn a_boot_partition_cut_short_or_without_ivts_gives_no_copies() {
        assert!(image_firmware(&vec![0xffu8; FIRMWARE as usize]).unwrap_err().to_string().contains("cut short"));
        assert_eq!(image_firmware(&vec![0xffu8; BOOT_SIZE as usize]).unwrap(), []);
    }

    #[test]
    fn boot_control_is_rewritten_only_for_two_copies() {
        let (mut board, unit) = board(Unit::default());
        let error = board.write_boot_control(&[(0x10_0000, 0x66800)]).unwrap_err();
        assert!(error.to_string().contains("two U-Boot copies"), "{error}");
        assert!(!unit.borrow().lines.iter().any(|l| l.starts_with("nand")));
        board.write_boot_control(&[(0x10_0000, 0x66800), (0x20_0000, 0x66800)]).unwrap();
        let lines = &unit.borrow().lines;
        assert_eq!(&lines[lines.len() - 2..], ["nand erase 0 0x100000", "nandbcb bcbonly 0 0x100000 0x66800 0x200000 0x66800"]);
    }

    #[test]
    fn the_fcb_names_its_copies_in_bytes() {
        let mut unit = Unit::default();
        unit.nand.insert(0x10_0000, Vec::new());
        let (mut board, _) = board(unit);
        assert_eq!(board.fcb_firmware().unwrap(), Some(vec![(0x10_0000, 0x8_0000), (0x18_0000, 0x8_0000)]));
    }

    #[test]
    fn the_fcb_must_name_the_images_copies() {
        let copies = [(0x10_0000, 0x66800), (0x20_0000, 0x66800)];
        assert!(fcb_matches(&[(0x10_0000, 0x80000), (0x20_0000, 0x80000)], &copies));
        assert!(!fcb_matches(&[(0x10_0000, 0x80000), (0x30_0000, 0x80000)], &copies));
        assert!(!fcb_matches(&[(0x10_0000, 0x1000), (0x20_0000, 0x80000)], &copies));
    }

    #[test]
    fn ecc_errors_pass_only_in_the_boot_partition() {
        let mut unit = Unit::default();
        unit.ecc_errors = true;
        let (mut board, _) = board(unit);
        assert_eq!(board.read_nand(0, 0x1000).unwrap().len(), 0x1000);
        assert!(board.read_nand(BOOT_SIZE, 0x1000).is_err());
    }

    #[test]
    fn ecc_errors_pass_only_in_the_boot_control_blocks() {
        let mut unit = Unit::default();
        unit.ecc_errors = true;
        let (mut board, _) = board(unit);
        assert!(board.read_nand(0, BOOT_SIZE).is_err());
        assert!(board.read_nand(BOOT_CONTROL, 0x1000).is_err());
        assert!(board.read_nand(BOOT_CONTROL - 0x1000, 0x2000).is_err());
        assert_eq!(board.read_nand(0, BOOT_CONTROL).unwrap().len(), BOOT_CONTROL as usize);
    }

    #[test]
    fn the_boot_partition_is_read_in_two_parts_into_one_buffer() {
        let mut unit = Unit::default();
        unit.nand.insert(0, vec![1u8; BOOT_CONTROL as usize]);
        unit.nand.insert(BOOT_CONTROL, vec![2u8; (BOOT_SIZE - BOOT_CONTROL) as usize]);
        let (mut board, unit) = board(unit);
        let data = board.read_nand(0, BOOT_SIZE).unwrap();
        assert_eq!(data.len(), BOOT_SIZE as usize);
        assert!(data[..BOOT_CONTROL as usize].iter().all(|&b| b == 1));
        assert!(data[BOOT_CONTROL as usize..].iter().all(|&b| b == 2));
        let reads: Vec<String> = unit.borrow().lines.iter().filter(|l| l.starts_with("nand read")).cloned().collect();
        assert_eq!(reads, ["nand read 0x88000000 0x0 0x80000", "nand read 0x88080000 0x80000 0x380000"]);
    }

    #[test]
    fn writes_are_erased_first_and_read_back() {
        let (mut board, unit) = board(Unit::default());
        let data: Vec<u8> = (0..0x2000u32).map(|i| i as u8).collect();
        board.write_nand(BOOT_SIZE, &data).unwrap();
        assert_eq!(unit.borrow().nand[&BOOT_SIZE], data);
        {
            let lines = &unit.borrow().lines;
            let erase = lines.iter().position(|l| l.starts_with("nand erase 0x400000 0x2000")).unwrap();
            let write = lines.iter().position(|l| l.starts_with("nand write.trimffs")).unwrap();
            assert!(erase < write);
        }
        unit.borrow_mut().corrupt_writes = true;
        assert!(board.write_nand(BOOT_SIZE, &data).unwrap_err().to_string().contains("reads back"));
    }
}
