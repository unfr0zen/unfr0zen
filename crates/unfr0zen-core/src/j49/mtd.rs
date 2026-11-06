//! The NAND's partition table as its own kernel has it: the board file's
//! `struct mtd_partition` array, in the compressed kernel a slot holds. Nothing
//! is assumed of where the kernel lies or of what the table says.

use std::io::Read;

use super::nand::{self, BLOCK, PAGE_RAW};

/// A legacy uImage's magic, at the start of its header.
const UIMAGE_MAGIC: [u8; 4] = [0x27, 0x05, 0x19, 0x56];
const UIMAGE_HEADER: usize = 64;
const GZIP_MAGIC: [u8; 3] = [0x1f, 0x8b, 0x08];
/// A `struct mtd_partition` on 32-bit ARM: name, then size and offset as
/// 8-aligned 64-bit fields, then mask_flags and ecclayout.
const ROW: usize = 32;
/// MTDPART_OFS_APPEND: right after the partition before.
const APPEND: u64 = u64::MAX;
/// Fewer than this is not a NAND's table.
const FEWEST_ROWS: usize = 4;
const LONGEST_NAME: usize = 31;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Partition {
    pub name: String,
    pub offset: usize,
    /// In bytes; None for MTDPART_SIZ_FULL, the rest of the chip.
    pub size: Option<usize>,
}

fn name_at(kernel: &[u8], offset: usize) -> Option<String> {
    let text = kernel.get(offset..)?;
    let len = text.iter().take(LONGEST_NAME + 1).position(|&b| b == 0)?;
    let name = &text[..len];
    (!name.is_empty() && name.iter().all(|b| b.is_ascii_graphic() || *b == b' '))
        .then(|| String::from_utf8_lossy(name).into_owned())
}

fn u32_at(kernel: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(kernel.get(offset..offset + 4)?.try_into().ok()?))
}

fn u64_at(kernel: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(kernel.get(offset..offset + 8)?.try_into().ok()?))
}

/// The table starting at `start`, as far as its rows read as one. A name's
/// pointer is taken back to the kernel by its link address: the load address
/// in the kernel's mapping, whose start is the pointer's top bits.
fn rows_at(kernel: &[u8], start: usize, load: u32) -> Vec<Partition> {
    let mut rows = Vec::new();
    let mut next = 0u64;
    for row in (start..).step_by(ROW) {
        let (Some(pointer), Some(size), Some(offset)) = (u32_at(kernel, row), u64_at(kernel, row + 8), u64_at(kernel, row + 16)) else {
            break;
        };
        let base = (pointer & 0xf000_0000).wrapping_add(load & 0x0fff_ffff);
        let offset = if offset == APPEND { next } else { offset };
        let aligned = size.is_multiple_of(BLOCK as u64) && offset.is_multiple_of(BLOCK as u64);
        let name = pointer.checked_sub(base).and_then(|at| name_at(kernel, at as usize));
        let (true, Some(name)) = (aligned, name) else {
            break;
        };
        rows.push(Partition { name, offset: offset as usize, size: (size != 0).then_some(size as usize) });
        next = offset + size;
    }
    rows
}

/// The partition table in a decompressed kernel loaded at `load`.
pub fn table_in(kernel: &[u8], load: u32) -> Option<Vec<Partition>> {
    (0..kernel.len()).step_by(4).find_map(|start| {
        if u64_at(kernel, start + 16)? != 0 {
            return None;
        }
        let rows = rows_at(kernel, start, load);
        (rows.len() >= FEWEST_ROWS).then_some(rows)
    })
}

/// A whole uImage's decompressed kernel and its load address.
fn unpack(image: &[u8]) -> Option<(Vec<u8>, u32)> {
    let load = u32::from_be_bytes(image.get(16..20)?.try_into().ok()?);
    let payload = image.get(UIMAGE_HEADER..)?;
    let gzip = payload.windows(GZIP_MAGIC.len()).position(|window| window == GZIP_MAGIC)?;
    let mut kernel = Vec::new();
    flate2::read::GzDecoder::new(&payload[gzip..]).read_to_end(&mut kernel).ok()?;
    Some((kernel, load))
}

/// Finds the table in an image read block by block: raw blocks in, each
/// kernel met is tried until one has it. A kernel skips bad blocks, as here.
#[derive(Default)]
pub struct TableFinder {
    table: Option<Vec<Partition>>,
    image: Vec<u8>,
    /// The bytes of the kernel being gathered, 0 when none is.
    wanted: usize,
}

impl TableFinder {
    pub fn block(&mut self, raw: &[u8]) {
        if self.table.is_some() || nand::factory_bad(raw) {
            return;
        }
        if self.wanted == 0 {
            if !raw.starts_with(&UIMAGE_MAGIC) {
                return;
            }
            let Some(size) = raw.get(12..16).map(|size| u32::from_be_bytes(size.try_into().unwrap())) else {
                return;
            };
            self.wanted = UIMAGE_HEADER + size as usize;
            self.image.clear();
        }
        for page in raw.chunks(PAGE_RAW) {
            match nand::correct_page(page) {
                Ok((data, _)) => self.image.extend(data),
                Err(_) => {
                    self.wanted = 0;
                    return;
                }
            }
        }
        if self.image.len() >= self.wanted {
            self.table = unpack(&self.image[..self.wanted]).and_then(|(kernel, load)| table_in(&kernel, load));
            self.wanted = 0;
            self.image = Vec::new();
        }
    }

    pub fn into_table(self) -> Option<Vec<Partition>> {
        self.table
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::j49::nand::{encode, BLOCK_RAW, PAGE};

    const LOAD: u32 = 0x8000_8000;
    /// Where a kernel loaded at `LOAD` is linked.
    const LINKED: u32 = 0xc000_8000;

    /// A kernel with this table after some code, its names after that.
    fn kernel(rows: &[(&str, u64, u64)]) -> Vec<u8> {
        let table = 4096;
        let names = table + (rows.len() + 1) * ROW;
        let mut kernel = vec![0x11u8; table];
        let mut text = Vec::new();
        for (name, size, offset) in rows {
            kernel.extend((LINKED + (names + text.len()) as u32).to_le_bytes());
            kernel.extend([0; 4]);
            kernel.extend(size.to_le_bytes());
            kernel.extend(offset.to_le_bytes());
            kernel.extend([0; 8]);
            text.extend(name.as_bytes());
            text.push(0);
        }
        kernel.extend([0; ROW]);
        kernel.extend(text);
        kernel
    }

    fn uimage(payload: &[u8]) -> Vec<u8> {
        let mut image = UIMAGE_MAGIC.to_vec();
        image.resize(UIMAGE_HEADER, 0);
        image[12..16].copy_from_slice(&(payload.len() as u32).to_be_bytes());
        image[16..20].copy_from_slice(&LOAD.to_be_bytes());
        image.extend(payload);
        image
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut zip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        zip.write_all(data).unwrap();
        zip.finish().unwrap()
    }

    /// Raw blocks holding `data`, padded with erased pages.
    fn raw_blocks(data: &[u8]) -> Vec<u8> {
        let mut raw = encode(data);
        raw.resize(data.len().div_ceil(BLOCK).max(1) * BLOCK_RAW, 0xff);
        raw
    }

    const ROWS: [(&str, u64, u64); 4] = [("boot", BLOCK as u64, 0), ("root0", 2 * BLOCK as u64, APPEND),
                                         ("system-config", BLOCK as u64, 8 * BLOCK as u64), ("data", BLOCK as u64, APPEND)];

    fn expected() -> Vec<Partition> {
        [("boot", BLOCK, 0), ("root0", 2 * BLOCK, BLOCK), ("system-config", BLOCK, 8 * BLOCK), ("data", BLOCK, 9 * BLOCK)]
            .map(|(name, size, offset)| Partition { name: name.into(), offset, size: Some(size) }).to_vec()
    }

    #[test]
    fn a_kernels_table_reads_its_names_and_appended_offsets() {
        assert_eq!(table_in(&kernel(&ROWS), LOAD), Some(expected()));
    }

    #[test]
    fn a_size_of_zero_is_the_rest_of_the_chip() {
        let rows = [("nand0", 0, 0), ("boot", BLOCK as u64, 0), ("root0", BLOCK as u64, APPEND), ("data", BLOCK as u64, APPEND)];
        let table = table_in(&kernel(&rows), LOAD).unwrap();
        assert_eq!(table[0], Partition { name: "nand0".into(), offset: 0, size: None });
        assert_eq!(table[2], Partition { name: "root0".into(), offset: BLOCK, size: Some(BLOCK) });
    }

    #[test]
    fn code_that_is_not_a_table_is_none() {
        assert_eq!(table_in(&vec![0x11; 1 << 16], LOAD), None);
        assert_eq!(table_in(&vec![0; 1 << 16], LOAD), None);
    }

    #[test]
    fn the_table_is_found_past_a_loader_without_one_and_a_bad_block_inside_the_kernel() {
        // Filler that gzip cannot shrink keeps the kernel across blocks.
        let mut filler = kernel(&ROWS);
        let mut state = 1u32;
        filler.extend((0..3 * BLOCK).map(|_| { state = state.wrapping_mul(1_664_525).wrapping_add(1); (state >> 24) as u8 }));
        let image = uimage(&gzip(&filler));
        assert!(image.len() > 2 * BLOCK);
        let mut raw = raw_blocks(&uimage(b"a boot loader, no gzip in it"));
        let blocks = raw_blocks(&image);
        raw.extend(&blocks[..BLOCK_RAW]);
        let mut bad = vec![0xffu8; BLOCK_RAW];
        bad[PAGE] = 0;
        raw.extend(bad);
        raw.extend(&blocks[BLOCK_RAW..]);
        let mut finder = TableFinder::default();
        for block in raw.chunks(BLOCK_RAW) {
            finder.block(block);
        }
        assert_eq!(finder.into_table(), Some(expected()));
    }

    #[test]
    fn an_image_with_no_kernel_has_no_table() {
        let mut finder = TableFinder::default();
        finder.block(&vec![0xff; BLOCK_RAW]);
        finder.block(&raw_blocks(b"not a kernel"));
        assert_eq!(finder.into_table(), None);
    }
}
