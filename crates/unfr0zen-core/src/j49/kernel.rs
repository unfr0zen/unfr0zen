//! The j49's kernel with a USB serial port, beside the stock one: in the
//! unused upper half of the slot's boot partition, booted first through
//! nandbootNacm, a copy of the stock nandbootN loading from there. Should it
//! not load, U-Boot goes on to the stock kernel. Port of unfr0zen-gen2.py.

use std::sync::LazyLock;

use regex::Regex;

use super::flasher::{status_text, Nand};
use super::nand::{self, Environment, BLOCK, BLOCK_RAW, KERNEL_TAIL, PAGES_PER_BLOCK, PAGE_RAW};
use crate::{device, Result, Slot};

const UIMAGE_MAGIC: u32 = 0x2705_1956;

fn check(status: u32, what: String) -> Result<()> {
    if status == 0 { Ok(()) } else { device(format!("{what}: {}", status_text(status))) }
}

/// Programs raw pages from `first`, skipping the all-0xff ones.
fn program(nand: &mut dyn Nand, first: usize, raw: &[u8]) -> Result<()> {
    for (index, page) in raw.chunks(PAGE_RAW).enumerate() {
        if page.iter().any(|&b| b != 0xff) {
            check(nand.program(first + index, page)?, format!("page {}", first + index))?;
        }
    }
    Ok(())
}

/// Whether a block's raw pages hold `want` (raw pages too): its data pages
/// read as written, the rest erased.
fn holds(have: &[u8], want: &[u8]) -> Result<bool> {
    for (index, got) in have.chunks(PAGE_RAW).enumerate() {
        match want.get(index * PAGE_RAW..(index + 1) * PAGE_RAW).filter(|page| page.iter().any(|&b| b != 0xff)) {
            None if !nand::erased(got) => return Ok(false),
            None => {}
            Some(page) if nand::erased(got) || nand::correct_page(got)?.0 != page[..nand::PAGE] => return Ok(false),
            Some(_) => {}
        }
    }
    Ok(true)
}

/// Puts `image`, a U-Boot kernel image, in the upper half of the slot's
/// boot partition as U-Boot's nboot reads it (bad blocks skipped), and reads
/// it back; false when it was there already.
pub fn install(nand: &mut dyn Nand, slot: Slot, image: &[u8]) -> Result<bool> {
    let word = |data: &[u8], at: usize| u32::from_be_bytes(data[at..at + 4].try_into().unwrap());
    if image.len() < 64 || word(image, 0) != UIMAGE_MAGIC {
        return device("the kernel is not a U-Boot image");
    }
    let (start, size) = nand::boot_partition(slot);
    let stock = nand::correct_page(&nand.read(start / nand::PAGE, 1)?)?.0;
    if word(&stock, 0) != UIMAGE_MAGIC {
        return device(format!("no stock kernel at the start of {slot}'s boot partition"));
    }
    if 64 + word(&stock, 12) as usize > KERNEL_TAIL {
        return device("the stock kernel reaches into the boot partition's upper half");
    }
    let first = (start + KERNEL_TAIL) / BLOCK;
    let blocks = (size - KERNEL_TAIL) / BLOCK;
    let raw = nand.read(first * PAGES_PER_BLOCK, blocks * PAGES_PER_BLOCK)?;
    let good: Vec<usize> = (0..blocks).filter(|b| !nand::factory_bad(&raw[b * BLOCK_RAW..(b + 1) * BLOCK_RAW])).collect();
    let pages = nand::encode(image);
    let chunks: Vec<&[u8]> = pages.chunks(BLOCK_RAW).collect();
    if chunks.len() > good.len() {
        return device(format!("the kernel needs {} blocks; {} are good", chunks.len(), good.len()));
    }
    let placed = |raw: &[u8]| -> Result<bool> {
        for (&block, chunk) in good.iter().zip(&chunks) {
            if !holds(&raw[block * BLOCK_RAW..(block + 1) * BLOCK_RAW], chunk)? {
                return Ok(false);
            }
        }
        Ok(true)
    };
    if placed(&raw)? {
        return Ok(false);
    }
    for (&block, chunk) in good.iter().zip(&chunks) {
        if !raw[block * BLOCK_RAW..(block + 1) * BLOCK_RAW].chunks(PAGE_RAW).all(nand::erased) {
            check(nand.erase(first + block)?, format!("erasing block {}", first + block))?;
        }
        program(nand, (first + block) * PAGES_PER_BLOCK, chunk)?;
    }
    if !placed(&nand.read(first * PAGES_PER_BLOCK, blocks * PAGES_PER_BLOCK)?)? {
        return device("the kernel reads back wrong");
    }
    log::info!("kernel ({} bytes) in {} blocks from {:#x}", image.len(), chunks.len(), first * BLOCK);
    Ok(true)
}

/// Both environment copies, as read (None where invalid).
fn environments(nand: &mut dyn Nand) -> Result<Vec<Option<Environment>>> {
    nand::ENV_COPIES.iter().map(|&offset| {
        let raw = nand.read(offset / nand::PAGE, nand::ENV_SIZE / nand::PAGE)?;
        let mut data = Vec::with_capacity(nand::ENV_SIZE);
        for page in raw.chunks(PAGE_RAW) {
            data.extend(nand::correct_page(page)?.0);
        }
        Ok(Environment::parse(&data))
    }).collect()
}

/// Has U-Boot try the kernel `install` placed first: nandbootNacm, the
/// stock nandbootN loading from the upper half, ahead in bootcmd. Written
/// as U-Boot does, into the other copy one generation on, and read back;
/// false when the environment has it already.
pub fn boot_first(nand: &mut dyn Nand, slot: Slot) -> Result<bool> {
    static NBOOT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"nboot\.i \$\{loadaddr\} 0 (0x[0-9a-fA-F]+)").unwrap());
    let copies = environments(nand)?;
    let (active, env) = nand::newest(&copies)?;
    let n = slot.number();
    let stock = env.get(&format!("nandboot{n}")).unwrap_or("").to_string();
    let start = nand::boot_partition(slot).0;
    let Some(found) = NBOOT.captures(&stock).filter(|c| usize::from_str_radix(&c[1][2..], 16).ok() == Some(start)) else {
        return device(format!("nandboot{n} does not load {slot}'s kernel: {stock:?}"));
    };
    let acm = format!("nandboot{n}acm");
    let mut new = env.clone();
    new.set(&acm, stock.replacen(&found[0], &format!("nboot.i ${{loadaddr}} 0 {:#x}", start + KERNEL_TAIL), 1));
    let bootcmd = new.get("bootcmd").unwrap_or("").to_string();
    if !bootcmd.starts_with(&format!("run {acm} || ")) {
        new.set("bootcmd", format!("run {acm} || {bootcmd}"));
    }
    if new.values == env.values {
        return Ok(false);
    }
    new.generation = env.generation.wrapping_add(1);
    let target = nand::ENV_COPIES[1 - active];
    let block = target / BLOCK;
    let head = nand.read(block * PAGES_PER_BLOCK, 2)?;
    if nand::factory_bad(&[head, vec![0xff; BLOCK_RAW - 2 * PAGE_RAW]].concat()) {
        return device(format!("the environment block at {target:#x} is bad"));
    }
    check(nand.erase(block)?, format!("erasing the environment block at {target:#x}"))?;
    program(nand, target / nand::PAGE, &nand::encode(&new.to_bytes()?))?;
    if environments(nand)?[1 - active].as_ref() != Some(&new) {
        return device("the environment reads back wrong");
    }
    log::info!("environment written at {target:#x}, generation {}: bootcmd {:?}", new.generation, new.get("bootcmd"));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::j49::flasher::Image;

    /// The first 100 blocks of a j49: boot0 with a stock kernel, and both
    /// environment copies, the second one generation on.
    fn unit() -> Image {
        let mut image = Image::new(vec![0xff; 100 * BLOCK_RAW], "test").unwrap();
        let mut stock = vec![0u8; 64 + 3000];
        stock[..4].copy_from_slice(&UIMAGE_MAGIC.to_be_bytes());
        stock[12..16].copy_from_slice(&3000u32.to_be_bytes());
        let boot = nand::encode(&stock);
        image.raw[nand::boot_partition(Slot::Root0).0 / nand::PAGE * PAGE_RAW..][..boot.len()].copy_from_slice(&boot);
        for (index, offset) in nand::ENV_COPIES.into_iter().enumerate() {
            let env = Environment { generation: index as u8, values: vec![
                ("bootcmd".into(), "run nandboot0 || run nandboot1".into()),
                ("nandboot0".into(), "nboot.i ${loadaddr} 0 0x400000 && bootm".into()),
            ] };
            let raw = nand::encode(&env.to_bytes().unwrap());
            image.raw[offset / nand::PAGE * PAGE_RAW..][..raw.len()].copy_from_slice(&raw);
        }
        image
    }

    #[test]
    fn the_kernel_goes_in_the_upper_half_once() {
        let mut image = unit();
        let mut kernel = vec![7u8; 300_000];
        kernel[..4].copy_from_slice(&UIMAGE_MAGIC.to_be_bytes());
        assert!(install(&mut image, Slot::Root0, &kernel).unwrap());
        assert!(!install(&mut image, Slot::Root0, &kernel).unwrap(), "already in place");
        let at = (4 << 20) + KERNEL_TAIL;
        let first = nand::correct_page(&image.raw[at / nand::PAGE * PAGE_RAW..][..PAGE_RAW]).unwrap().0;
        assert_eq!(first[..8], kernel[..8]);
        kernel[100] = 8;
        assert!(install(&mut image, Slot::Root0, &kernel).unwrap(), "a new kernel replaces it");
        assert!(install(&mut image, Slot::Root0, b"not a kernel").is_err());
    }

    #[test]
    fn the_environment_boots_it_first_from_the_other_copy() {
        let mut image = unit();
        assert!(boot_first(&mut image, Slot::Root0).unwrap());
        let copies = environments(&mut image).unwrap();
        let (active, env) = nand::newest(&copies).unwrap();
        assert_eq!((active, env.generation), (0, 2));
        assert_eq!(env.get("bootcmd"), Some("run nandboot0acm || run nandboot0 || run nandboot1"));
        assert_eq!(env.get("nandboot0acm"), Some("nboot.i ${loadaddr} 0 0x800000 && bootm"));
        assert_eq!(env.boot_order().unwrap(), [Slot::Root0, Slot::Root1]);
        assert!(!boot_first(&mut image, Slot::Root0).unwrap(), "already first");
        assert!(boot_first(&mut image, Slot::Root1).is_err(), "no nandboot1 to copy");
    }
}
