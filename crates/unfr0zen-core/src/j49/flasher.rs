//! The j49's NAND through j49/nand-flasher.c, which x-loader's USB loader
//! runs from SDRAM, or through a raw image standing in for the unit.
//!
//! The flasher's protocol (little-endian words on bulk EP1):
//!   'USBh' GPMC_CONFIG1_0 GPMC_CONFIG7_0 id[8] [CONTROL_IDCODE]   once, on entry
//!   'USBr' first count -> count raw pages         'USBe' page: a read stayed busy
//!   'USBp' page 0, 2112 raw bytes -> 'USBs' page status
//!   'USBE' block 0 -> 'USBs' block status
//!   'USBU' 'ALL!' 0 -> 'USBs' 0 status: lift the limits below
//!   'USBx' 0 0: reset the SoC
//! It programs only erased pages of the boot and root partitions and the
//! environment blocks, and erases only the environment blocks and the upper
//! halves of the boot partitions, until unlocked for a whole-NAND restore or
//! for a patch to erase the blocks of the nestlocal it replaces. Only erased
//! pages are programmed either way.

use std::time::Duration;

use super::nand::{self, BLOCK_RAW, PAGES_PER_BLOCK, PAGE_RAW};
use super::omap::{self, try_receive};
use crate::bulk::Bulk;
use crate::{device, Error, Result};

/// Where the flasher is loaded and entered.
pub const BASE: u32 = 0x8010_0000;
const CMD_HELLO: &[u8; 4] = b"USBh";
const CMD_READ: &[u8; 4] = b"USBr";
const CMD_PROGRAM: &[u8; 4] = b"USBp";
const CMD_STATUS: &[u8; 4] = b"USBs";
const CMD_RESET: &[u8; 4] = b"USBx";
const CMD_ERASE: &[u8; 4] = b"USBE";
const CMD_ERROR: &[u8; 4] = b"USBe";
const CMD_UNLOCK: &[u8; 4] = b"USBU";
const UNLOCK_MAGIC: &[u8; 4] = b"ALL!";
/// (NAND ID bytes 0-1, name, size, bus width)
const CHIPS: [([u8; 2], &str, u64, u32); 2] = [
    ([0x2c, 0xba], "Micron MT29F2G16", 0x1000_0000, 16),
    ([0xec, 0xdc], "Samsung K9F4G08U0C", 0x2000_0000, 8),
];
/// The data sizes of the chips a j49 has.
pub const CHIP_SIZES: [u64; 2] = [CHIPS[0].2, CHIPS[1].2];
const PROGRAM_SLOTS: std::ops::Range<usize> = 4 * 512..112 * 512;
const BOOT_TAILS: [std::ops::Range<usize>; 2] = [64..96, 496..528];

/// What a program or erase reported.
pub fn status_text(status: u32) -> String {
    match status {
        1 => "refused by the flasher (outside what it may write)".into(),
        2 => "the NAND reported a failure".into(),
        3 => "the NAND stayed busy".into(),
        4 => "refused by the flasher (not erased)".into(),
        other => format!("status {other}"),
    }
}

/// The NAND, by raw pages (2048 data + 64 OOB bytes).
pub trait Nand {
    /// `count` raw pages from `first`.
    fn read(&mut self, first: usize, count: usize) -> Result<Vec<u8>>;
    /// Programs one erased page; 0 when done, else the flasher's status.
    fn program(&mut self, page: usize, raw: &[u8]) -> Result<u32>;
    /// Erases one block; 0 when done, else the flasher's status.
    fn erase(&mut self, block: usize) -> Result<u32>;
    /// Lets every block be erased and every erased page programmed, for the
    /// rest of the session.
    fn unlock(&mut self) -> Result<()>;
    fn reset(&mut self) -> Result<()>;
    /// The chip, as the flasher identified it.
    fn chip(&self) -> &str;
    /// Its data bytes, without OOB.
    fn size(&self) -> u64;
}

fn erasable(block: usize) -> bool {
    nand::ENV_COPIES.iter().any(|&env| block == env / nand::BLOCK) || BOOT_TAILS.iter().any(|tail| tail.contains(&block))
}

fn programmable(page: usize) -> bool {
    PROGRAM_SLOTS.contains(&page) || erasable(page / PAGES_PER_BLOCK)
}

/// The flasher on the unit, over x-loader's bulk endpoints.
pub struct Flasher<B: Bulk> {
    bulk: B,
    chip: String,
    size: u64,
    /// CONTROL_IDCODE, from a flasher that sends it.
    pub idcode: Option<u32>,
}

impl<B: Bulk> Flasher<B> {
    /// Takes the flasher's hello, once it is entered, and refuses a GPMC or
    /// chip it does not know.
    pub fn connect(mut bulk: B) -> Result<Self> {
        // x-loader queues its next file request right after confirming the
        // flasher, and the jump answers it unread: skip it and any messages.
        let mut raw = Vec::new();
        for _ in 0..4 {
            raw = try_receive(&mut bulk, 512, Duration::from_secs(5))?;
            if raw.get(..4) != Some(b"USBf") && raw.get(..4) != Some(omap::CMD_MESSAGE) {
                break;
            }
        }
        if !(raw.len() == 20 || raw.len() == 24) || &raw[..4] != CMD_HELLO {
            return device(format!("no flasher hello (got {raw:02x?}); the flasher did not start, power-cycle the unit"));
        }
        let word = |at: usize| u32::from_le_bytes(raw[at..at + 4].try_into().unwrap());
        let (config1, config7) = (word(4), word(8));
        if (config1 >> 10) & 3 != 2 || config7 & (1 << 6) == 0 {
            return device(format!("GPMC CS0 is not an enabled NAND (CONFIG1 {config1:#x}, CONFIG7 {config7:#x})"));
        }
        let width = if (config1 >> 12) & 3 == 1 { 16 } else { 8 };
        let Some(&(_, name, size, chip_width)) = CHIPS.iter().find(|(id, ..)| raw[12..14] == *id) else {
            return device(format!("unknown NAND ID {:02x?}", &raw[12..20]));
        };
        if chip_width != width {
            return device(format!("{name} is x{chip_width} but GPMC CS0 is x{width}"));
        }
        let idcode = (raw.len() == 24).then(|| word(20));
        Ok(Flasher { bulk, chip: format!("{name}, {} MiB", size >> 20), size, idcode })
    }

    fn command(&mut self, op: &[u8; 4], a: u32, b: u32) -> Result<()> {
        omap::write(&mut self.bulk, &[&op[..], &a.to_le_bytes(), &b.to_le_bytes()].concat(), Duration::from_secs(2))
    }

    fn status(&mut self, what: &str, expected: usize) -> Result<u32> {
        let reply = self.bulk.receive(512, Duration::from_secs(10))?;
        if reply.len() != 12 || &reply[..4] != CMD_STATUS {
            return device(format!("no status for {what} {expected} (got {reply:02x?})"));
        }
        let word = |at: usize| u32::from_le_bytes(reply[at..at + 4].try_into().unwrap());
        if word(4) as usize != expected {
            return device(format!("status for {what} {}, expected {expected}", word(4)));
        }
        Ok(word(8))
    }
}

impl<B: Bulk> Nand for Flasher<B> {
    fn read(&mut self, first: usize, count: usize) -> Result<Vec<u8>> {
        self.command(CMD_READ, first as u32, count as u32)?;
        let want = count * PAGE_RAW;
        let mut data = Vec::with_capacity(want);
        while data.len() < want {
            let chunk = self.bulk.receive(want - data.len(), Duration::from_secs(10))?;
            // Whole 512-byte packets, or the short one that ends the transfer.
            if !chunk.is_empty() && (chunk.len() % 512 == 0 || data.len() + chunk.len() == want) {
                data.extend(chunk);
                continue;
            }
            if chunk.get(..4) == Some(CMD_ERROR) && chunk.len() == 8 {
                let page = u32::from_le_bytes(chunk[4..8].try_into().unwrap());
                return device(format!("the NAND stayed busy reading page {page}; power-cycle the unit"));
            }
            return device(format!("short read at page {first}: {} of {want} bytes", data.len() + chunk.len()));
        }
        Ok(data)
    }

    fn program(&mut self, page: usize, raw: &[u8]) -> Result<u32> {
        self.command(CMD_PROGRAM, page as u32, 0)?;
        // One transfer: the flasher takes it packet by packet, unpaced.
        self.bulk.send(raw, Duration::from_secs(5))?;
        self.status("page", page)
    }

    fn erase(&mut self, block: usize) -> Result<u32> {
        self.command(CMD_ERASE, block as u32, 0)?;
        self.status("block", block)
    }

    fn unlock(&mut self) -> Result<()> {
        self.command(CMD_UNLOCK, u32::from_le_bytes(*UNLOCK_MAGIC), 0)?;
        match self.status("unlock", 0)? {
            0 => Ok(()),
            status => device(format!("the flasher did not unlock: {}", status_text(status))),
        }
    }

    fn reset(&mut self) -> Result<()> {
        self.command(CMD_RESET, 0, 0)
    }

    fn chip(&self) -> &str {
        &self.chip
    }

    fn size(&self) -> u64 {
        self.size
    }
}

/// A raw NAND image (2112-byte pages) in place of the unit: programming ANDs
/// bits in, as the chip does, and only where the flasher would.
pub struct Image {
    pub raw: Vec<u8>,
    name: String,
    unlocked: bool,
}

impl Image {
    pub fn new(raw: Vec<u8>, name: &str) -> Result<Image> {
        if !raw.len().is_multiple_of(BLOCK_RAW) {
            return device(format!("{name} is not a whole number of {BLOCK_RAW}-byte raw blocks"));
        }
        Ok(Image { raw, name: format!("image {name}"), unlocked: false })
    }
}

impl Nand for Image {
    fn read(&mut self, first: usize, count: usize) -> Result<Vec<u8>> {
        self.raw.get(first * PAGE_RAW..(first + count) * PAGE_RAW).map(<[u8]>::to_vec)
            .ok_or_else(|| Error::Device(format!("pages {first}+{count} are past the image's end")))
    }

    fn program(&mut self, page: usize, raw: &[u8]) -> Result<u32> {
        if !self.unlocked && !programmable(page) {
            return Ok(1);
        }
        let old = &mut self.raw[page * PAGE_RAW..(page + 1) * PAGE_RAW];
        if !nand::erased(old) {
            return Ok(4);
        }
        old.iter_mut().zip(raw).for_each(|(old, new)| *old &= new);
        Ok(0)
    }

    fn erase(&mut self, block: usize) -> Result<u32> {
        if !self.unlocked && !erasable(block) {
            return Ok(1);
        }
        self.raw[block * BLOCK_RAW..(block + 1) * BLOCK_RAW].fill(0xff);
        Ok(0)
    }

    fn unlock(&mut self) -> Result<()> {
        self.unlocked = true;
        Ok(())
    }

    fn reset(&mut self) -> Result<()> {
        Ok(())
    }

    fn chip(&self) -> &str {
        &self.name
    }

    fn size(&self) -> u64 {
        (self.raw.len() / PAGE_RAW * nand::PAGE) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::j49::omap::tests::Scripted;
    use std::collections::VecDeque;

    fn hello(id: [u8; 2], config1: u32) -> Vec<u8> {
        [&b"USBh"[..], &config1.to_le_bytes(), &(1u32 << 6).to_le_bytes(), &id, &[0; 6]].concat()
    }

    #[test]
    fn the_hello_names_a_known_chip_at_its_width() {
        let x16 = (2 << 10) | (1 << 12);
        let flasher = Flasher::connect(Scripted { replies: VecDeque::from([b"USBf".to_vec(), hello([0x2c, 0xba], x16)]),
                                                  ..Default::default() }).unwrap();
        assert_eq!((flasher.chip(), flasher.idcode), ("Micron MT29F2G16, 256 MiB", None));
        let with_idcode = [hello([0x2c, 0xba], x16), 0x2b89_102fu32.to_le_bytes().to_vec()].concat();
        let flasher = Flasher::connect(Scripted { replies: VecDeque::from([with_idcode]), ..Default::default() }).unwrap();
        assert_eq!(flasher.idcode, Some(0x2b89_102f));
        let wrong = Flasher::connect(Scripted { replies: VecDeque::from([hello([0x2c, 0xba], 2 << 10)]), ..Default::default() });
        assert!(wrong.err().unwrap().to_string().contains("x16 but GPMC CS0 is x8"));
    }

    #[test]
    fn a_read_takes_whole_packets_and_the_short_one_that_ends_it() {
        let mut flasher = Flasher { idcode: None, size: 0, bulk: Scripted { replies: VecDeque::from([vec![1; 2048], vec![2; 64]]), ..Default::default() },
                                    chip: String::new() };
        let page = flasher.read(5, 1).unwrap();
        assert_eq!(page.len(), PAGE_RAW);
        assert_eq!(flasher.bulk.sent[0], [&b"USBr"[..], &5u32.to_le_bytes(), &1u32.to_le_bytes()].concat());
        let mut busy = Flasher { idcode: None, size: 0, bulk: Scripted { replies: VecDeque::from([[&b"USBe"[..], &9u32.to_le_bytes()].concat()]),
                                                  ..Default::default() }, chip: String::new() };
        assert!(busy.read(9, 1).unwrap_err().to_string().contains("busy reading page 9"));
    }

    #[test]
    fn the_image_writes_only_where_the_flasher_would() {
        let mut image = Image::new(vec![0xff; 2048 * BLOCK_RAW], "test").unwrap();
        let mut raw = vec![0xff; PAGE_RAW];
        raw[0] = 0;
        assert_eq!(image.program(0, &raw).unwrap(), 1, "x-loader's blocks");
        assert_eq!(image.program(12 * 512, &raw).unwrap(), 0, "root0");
        assert_eq!(image.program(12 * 512, &raw).unwrap(), 4, "programmed twice");
        assert_eq!(image.erase(12 * 512 / PAGES_PER_BLOCK).unwrap(), 1, "root0 is never erased");
        assert_eq!(image.erase(nand::ENV_COPIES[1] / nand::BLOCK).unwrap(), 0);
        assert_eq!(image.erase(64).unwrap(), 0, "boot0's upper half");
        assert_eq!(image.erase(63).unwrap(), 1, "boot0's stock kernel");
        image.unlock().unwrap();
        assert_eq!(image.erase(0).unwrap(), 0, "x-loader's blocks, unlocked");
        assert_eq!(image.program(0, &raw).unwrap(), 0);
        assert_eq!(image.program(0, &raw).unwrap(), 4, "still only erased pages");
    }

    #[test]
    fn a_page_goes_out_in_one_transfer_after_its_command() {
        let status = [&b"USBs"[..], &7u32.to_le_bytes(), &0u32.to_le_bytes()].concat();
        let mut flasher = Flasher { idcode: None, size: 0, chip: String::new(),
                                    bulk: Scripted { replies: VecDeque::from([status]), ..Default::default() } };
        assert_eq!(flasher.program(7, &[0x5a; PAGE_RAW]).unwrap(), 0);
        assert_eq!(flasher.bulk.sent, [[&b"USBp"[..], &7u32.to_le_bytes(), &0u32.to_le_bytes()].concat(), vec![0x5a; PAGE_RAW]]);
    }

    #[test]
    fn unlocking_sends_the_magic_word_and_needs_the_flasher_to_agree() {
        let status = |status: u32| [&b"USBs"[..], &0u32.to_le_bytes(), &status.to_le_bytes()].concat();
        let flasher = |reply| Flasher { idcode: None, size: 0, chip: String::new(),
                                        bulk: Scripted { replies: VecDeque::from([reply]), ..Default::default() } };
        let mut unlocked = flasher(status(0));
        unlocked.unlock().unwrap();
        assert_eq!(unlocked.bulk.sent[0], [&b"USBU"[..], b"ALL!", &0u32.to_le_bytes()].concat());
        assert!(flasher(status(1)).unlock().unwrap_err().to_string().contains("did not unlock"));
    }
}
