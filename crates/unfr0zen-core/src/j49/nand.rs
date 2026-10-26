//! The j49 NAND as the flasher sees it: raw pages of 2048 data bytes then 64
//! OOB bytes, its partitions, the GPMC's BCH4 code and the U-Boot environment.
//!
//! Each 512-byte sector is protected by BCH t=4 over GF(2^13)
//! (x^13 + x^4 + x^3 + x + 1), as the OMAP3 GPMC computes it for x-loader,
//! U-Boot and Linux: 52 parity bits, right-aligned in 7 bytes, most
//! significant byte first, at OOB 36 + 7 * sector. A sector whose stored code
//! is all 0xff was never programmed. OOB bytes 0-1 are the bad-block marker
//! (x16 part), 2-35 are free (JFFS2 keeps its cleanmarker there).

use std::sync::LazyLock;

use crate::{device, Result, Slot};

pub const PAGE: usize = 2048;
pub const OOB: usize = 64;
pub const PAGE_RAW: usize = PAGE + OOB;
pub const SECTOR: usize = 512;
pub const PAGES_PER_BLOCK: usize = 64;
pub const BLOCK: usize = PAGE * PAGES_PER_BLOCK;
pub const BLOCK_RAW: usize = PAGE_RAW * PAGES_PER_BLOCK;
const ECC_OFFSET: usize = 36;
const ECC_BYTES: usize = 7;
const PARITY_BITS: u32 = 52;
const PARITY: u64 = (1 << PARITY_BITS) - 1;
const CODE_BITS: usize = SECTOR * 8 + PARITY_BITS as usize;
const FIELD: usize = 8191;

/// The kernel's upper half of each boot partition, unused by stock (its
/// kernel is the 2.2 MB at their start), where a kernel of our own goes.
pub const KERNEL_TAIL: usize = 4 << 20;
pub const ENV_COPIES: [usize; 2] = [0x34_0000, 0x3a_0000];
pub const ENV_SIZE: usize = 0x2_0000;

/// A root slot's JFFS2 partition: (offset, length) in bytes.
pub fn slot_partition(slot: Slot) -> (usize, usize) {
    match slot {
        Slot::Root0 => (12 << 20, 46 << 20),
        Slot::Root1 => (66 << 20, 46 << 20),
    }
}

/// The slot's kernel partition, which nandbootN loads from its start.
pub fn boot_partition(slot: Slot) -> (usize, usize) {
    match slot {
        Slot::Root0 => (4 << 20, 8 << 20),
        Slot::Root1 => (58 << 20, 8 << 20),
    }
}

struct Field {
    exp: Vec<u16>,
    log: Vec<u16>,
    table: [u64; 256],
}

static GF: LazyLock<Field> = LazyLock::new(|| {
    let (mut exp, mut log) = (vec![0u16; FIELD * 2], vec![0u16; FIELD + 1]);
    let mut x = 1u16;
    for i in 0..FIELD {
        exp[i] = x;
        exp[i + FIELD] = x;
        log[x as usize] = i as u16;
        x <<= 1;
        if x & 0x2000 != 0 {
            x ^= 0x201b;
        }
    }
    // g(x): the product of the minimal polynomials of alpha^1, 3, 5, 7.
    let mut g = vec![1u8];
    let mut done = std::collections::BTreeSet::new();
    for r in [1usize, 3, 5, 7] {
        if done.contains(&r) {
            continue;
        }
        let (mut m, mut j) = (vec![1u16], r);
        while done.insert(j) {
            m.insert(0, 0);
            for k in 0..m.len() - 1 {
                if m[k + 1] != 0 {
                    m[k] ^= exp[log[m[k + 1] as usize] as usize + j];
                }
            }
            j = j * 2 % FIELD;
        }
        let mut product = vec![0u8; g.len() + m.len() - 1];
        for (a, &ga) in g.iter().enumerate() {
            for (b, &mb) in m.iter().enumerate() {
                product[a + b] ^= ga & (mb & 1) as u8;
            }
        }
        g = product;
    }
    let generator: u64 = g.iter().take(PARITY_BITS as usize).enumerate().map(|(i, &c)| u64::from(c) << i).sum();
    let mut table = [0u64; 256];
    for (byte, entry) in table.iter_mut().enumerate() {
        let mut r = 0u64;
        for b in (0..8).rev() {
            let feedback = (r >> (PARITY_BITS - 1)) & 1 != (byte as u64 >> b) & 1;
            r = (r << 1) & PARITY;
            if feedback {
                r ^= generator;
            }
        }
        *entry = r;
    }
    Field { exp, log, table }
});

impl Field {
    fn mul(&self, a: u16, b: u16) -> u16 {
        if a == 0 || b == 0 { 0 } else { self.exp[self.log[a as usize] as usize + self.log[b as usize] as usize] }
    }

    fn syndromes(&self, remainder: u64) -> [u16; 9] {
        let mut s = [0u16; 9];
        for j in [1usize, 3, 5, 7] {
            s[j] = (0..PARITY_BITS as usize).filter(|k| remainder >> k & 1 != 0)
                .fold(0, |v, k| v ^ self.exp[j * k % FIELD]);
        }
        s[2] = self.mul(s[1], s[1]);
        s[4] = self.mul(s[2], s[2]);
        s[6] = self.mul(s[3], s[3]);
        s[8] = self.mul(s[4], s[4]);
        s
    }

    /// Berlekamp-Massey: the error locator, low degree first.
    fn locator(&self, s: &[u16; 9]) -> Vec<u16> {
        let (mut c, mut b) = ([0u16; 9], [0u16; 9]);
        (c[0], b[0]) = (1, 1);
        let (mut length, mut m, mut last) = (0usize, 1usize, 1u16);
        for n in 0..8 {
            let mut d = s[n + 1];
            for i in 1..=length {
                d ^= self.mul(c[i], s[n + 1 - i]);
            }
            if d == 0 {
                m += 1;
                continue;
            }
            let scale = self.exp[(self.log[d as usize] as usize + FIELD - self.log[last as usize] as usize) % FIELD];
            let previous = c;
            for i in 0..9 - m {
                c[i + m] ^= self.mul(scale, b[i]);
            }
            if 2 * length <= n {
                (length, b, last, m) = (n + 1 - length, previous, d, 1);
            } else {
                m += 1;
            }
        }
        c[..=length].to_vec()
    }
}

/// The 52-bit BCH remainder of a 512-byte sector.
pub fn parity(sector: &[u8]) -> u64 {
    let gf = &*GF;
    sector.iter().fold(0, |r, &byte| ((r << 8) & PARITY) ^ gf.table[((r >> 44) as u8 ^ byte) as usize])
}

/// One sector corrected against its 7 stored ECC bytes: (data, bits fixed),
/// or None when uncorrectable. An erased sector (code all 0xff) reads as
/// 0xff when no more than 4 bits are stuck.
pub fn correct_sector(data: &[u8], stored: &[u8]) -> Option<(Vec<u8>, u32)> {
    if stored.iter().all(|&b| b == 0xff) {
        let flips: u32 = data.iter().map(|b| b.count_zeros()).sum();
        return (flips <= 4).then(|| (vec![0xff; SECTOR], flips));
    }
    let code = stored.iter().fold(0u64, |v, &b| v << 8 | u64::from(b)) & PARITY;
    let remainder = parity(data) ^ code;
    if remainder == 0 {
        return Some((data.to_vec(), 0));
    }
    let gf = &*GF;
    let locator = gf.locator(&gf.syndromes(remainder));
    let count = locator.len() - 1;
    if count > 4 || locator[count] == 0 {
        return None;
    }
    let (mut fixed, mut found) = (data.to_vec(), 0);
    for e in 0..CODE_BITS {
        // x^e is an error position when locator(alpha^-e) == 0.
        let power = (FIELD - e % FIELD) % FIELD;
        let v = locator.iter().enumerate().filter(|(_, &c)| c != 0)
            .fold(0, |v, (i, &c)| v ^ gf.exp[(gf.log[c as usize] as usize + power * i) % FIELD]);
        if v != 0 {
            continue;
        }
        found += 1;
        if e >= PARITY_BITS as usize {
            let bit = CODE_BITS - 1 - e; // 0 is the MSB of byte 0
            fixed[bit / 8] ^= 0x80 >> (bit % 8);
        }
    }
    (found == count).then_some((fixed, count as u32))
}

/// A raw page's 2048 data bytes, ECC-corrected, and the bits fixed.
pub fn correct_page(raw: &[u8]) -> Result<(Vec<u8>, u32)> {
    let (mut out, mut fixed) = (Vec::with_capacity(PAGE), 0);
    for s in 0..PAGE / SECTOR {
        let at = PAGE + ECC_OFFSET + s * ECC_BYTES;
        let Some((data, bits)) = correct_sector(&raw[s * SECTOR..(s + 1) * SECTOR], &raw[at..at + ECC_BYTES]) else {
            return device(format!("uncorrectable NAND sector {s}"));
        };
        out.extend_from_slice(&data);
        fixed += bits;
    }
    Ok((out, fixed))
}

/// A page as the kernel writes it: data, then oob with every sector's code.
pub fn encode_page(data: &[u8], oob: &[u8]) -> Vec<u8> {
    let mut raw = [data, oob].concat();
    for s in 0..PAGE / SECTOR {
        let at = PAGE + ECC_OFFSET + s * ECC_BYTES;
        raw[at..at + ECC_BYTES].copy_from_slice(&parity(&data[s * SECTOR..(s + 1) * SECTOR]).to_be_bytes()[1..]);
    }
    raw
}

/// Raw pages for data, padded to whole pages, with 0xff OOB and its ECC; a
/// page of all 0xff stays erased.
pub fn encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len().div_ceil(PAGE) * PAGE_RAW);
    for chunk in data.chunks(PAGE) {
        let mut page = chunk.to_vec();
        page.resize(PAGE, 0xff);
        if page.iter().all(|&b| b == 0xff) {
            out.resize(out.len() + PAGE_RAW, 0xff);
        } else {
            out.extend(encode_page(&page, &[0xff; OOB]));
        }
    }
    out
}

/// Never programmed since its erase: every data and ECC bit still 1.
pub fn erased(raw: &[u8]) -> bool {
    raw[..PAGE].iter().chain(&raw[PAGE + ECC_OFFSET..PAGE_RAW]).all(|&b| b == 0xff)
}

/// The bad-block marker: OOB bytes 0-1 of page 0 or 1 not 0xff.
pub fn factory_bad(block_raw: &[u8]) -> bool {
    [0, 1].iter().any(|p| block_raw[p * PAGE_RAW + PAGE..p * PAGE_RAW + PAGE + 2] != [0xff, 0xff])
}

/// A redundant U-Boot environment copy: its generation and its variables,
/// in the order U-Boot keeps them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Environment {
    pub generation: u8,
    pub values: Vec<(String, String)>,
}

impl Environment {
    /// One copy's ENV_SIZE bytes; None when its CRC32 does not match.
    pub fn parse(data: &[u8]) -> Option<Environment> {
        if data.len() < ENV_SIZE || crc32fast::hash(&data[5..ENV_SIZE]) != u32::from_le_bytes(data[..4].try_into().unwrap()) {
            return None;
        }
        let body = &data[5..ENV_SIZE];
        let end = body.windows(2).position(|w| w == [0, 0]).unwrap_or(body.len());
        let values = body[..end].split(|&b| b == 0).filter(|e| !e.is_empty())
            .filter_map(|e| String::from_utf8_lossy(e).split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
            .collect();
        Some(Environment { generation: data[4], values })
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    /// Sets a variable, in place when it exists, else at the end.
    pub fn set(&mut self, key: &str, value: String) {
        match self.values.iter_mut().find(|(k, _)| k == key) {
            Some((_, v)) => *v = value,
            None => self.values.push((key.to_string(), value)),
        }
    }

    /// The copy's ENV_SIZE bytes: CRC32 of the variables, generation, variables.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut body: Vec<u8> = self.values.iter().flat_map(|(k, v)| format!("{k}={v}\0").into_bytes()).collect();
        body.push(0);
        if body.len() > ENV_SIZE - 5 {
            return device("U-Boot environment too large");
        }
        body.resize(ENV_SIZE - 5, 0);
        Ok([&crc32fast::hash(&body).to_le_bytes()[..], &[self.generation], &body].concat())
    }

    /// The slots bootcmd boots, in its order.
    pub fn boot_order(&self) -> Result<[Slot; 2]> {
        let bootcmd = self.get("bootcmd").unwrap_or("");
        let numbers: Vec<u8> = bootcmd.match_indices("run nandboot").filter_map(|(at, _)| {
            let rest = &bootcmd.as_bytes()[at + 12..];
            let boundary = rest.get(1).is_none_or(|c| !c.is_ascii_alphanumeric() && *c != b'_');
            matches!(rest.first(), Some(b'0' | b'1')).then_some(rest[0] - b'0').filter(|_| boundary)
        }).collect();
        if numbers.len() < 2 || numbers[0] == numbers[1] {
            return device("bootcmd does not boot both root slots");
        }
        Ok([numbers[0], numbers[1]].map(|n| if n == 0 { Slot::Root0 } else { Slot::Root1 }))
    }
}

/// The copy U-Boot boots with: the valid one of the newer generation, and
/// its index in ENV_COPIES.
pub fn newest(copies: &[Option<Environment>]) -> Result<(usize, &Environment)> {
    let mut best: Option<(usize, &Environment)> = None;
    for (index, copy) in copies.iter().enumerate() {
        let Some(copy) = copy else { continue };
        best = match best {
            Some((_, newest)) if !(1..128).contains(&copy.generation.wrapping_sub(newest.generation)) => best,
            _ => Some((index, copy)),
        };
    }
    best.map_or_else(|| device("neither U-Boot environment copy is valid"), Ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(seed: u32) -> Vec<u8> {
        let mut x = seed.wrapping_mul(2_654_435_761) | 1;
        (0..PAGE).map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        }).collect()
    }

    #[test]
    fn up_to_four_flipped_bits_per_sector_are_corrected() {
        let data = sample(1);
        let raw = encode_page(&data, &[0xff; OOB]);
        assert_eq!(correct_page(&raw).unwrap(), (data.clone(), 0));
        // In the data and in the stored code alike.
        for (n, bits) in [(1, vec![0]), (2, vec![7, 4095]), (3, vec![100, 2000, 4100]), (4, vec![1, 2, 3, 4140])] {
            let mut damaged = raw.clone();
            for bit in bits {
                if bit < SECTOR * 8 {
                    damaged[bit / 8] ^= 0x80 >> (bit % 8);
                } else {
                    let code = bit - SECTOR * 8 + 4; // the 52 bits sit right-aligned in 56
                    damaged[PAGE + ECC_OFFSET + code / 8] ^= 0x80 >> (code % 8);
                }
            }
            assert_eq!(correct_page(&damaged).unwrap(), (data.clone(), n), "{n} bits");
        }
    }

    #[test]
    fn an_erased_page_reads_as_erased() {
        let mut raw = vec![0xff; PAGE_RAW];
        assert!(erased(&raw));
        raw[10] = 0xfe;
        assert_eq!(correct_page(&raw).unwrap(), (vec![0xff; PAGE], 1));
        assert!(!erased(&raw));
    }

    #[test]
    fn encode_leaves_blank_pages_erased() {
        let mut data = sample(2);
        data.extend([0xff; PAGE]);
        data.extend([1, 2, 3]);
        let raw = encode(&data);
        assert_eq!(raw.len(), 3 * PAGE_RAW);
        assert!(erased(&raw[PAGE_RAW..2 * PAGE_RAW]));
        assert_eq!(correct_page(&raw[2 * PAGE_RAW..]).unwrap().0[..4], [1, 2, 3, 0xff]);
    }

    #[test]
    fn the_environment_round_trips_in_order() {
        let env = Environment { generation: 7, values: vec![
            ("bootcmd".into(), "run nandboot1 || run nandboot0".into()), ("a".into(), "b=c".into())] };
        let data = env.to_bytes().unwrap();
        assert_eq!(data.len(), ENV_SIZE);
        assert_eq!(Environment::parse(&data).unwrap(), env);
        assert_eq!(env.boot_order().unwrap(), [Slot::Root1, Slot::Root0]);
        let mut broken = data.clone();
        broken[100] ^= 1;
        assert_eq!(Environment::parse(&broken), None);
    }

    #[test]
    fn the_newer_generation_wins_across_the_wrap() {
        let copy = |generation| Some(Environment { generation, values: vec![] });
        assert_eq!(newest(&[copy(255), copy(0)]).unwrap().0, 1);
        assert_eq!(newest(&[copy(3), copy(2)]).unwrap().0, 0);
        assert_eq!(newest(&[None, copy(2)]).unwrap().0, 1);
        assert!(newest(&[None, None]).is_err());
    }

    #[test]
    fn boot_order_needs_both_slots() {
        let env = |cmd: &str| Environment { generation: 0, values: vec![("bootcmd".into(), cmd.into())] };
        assert_eq!(env("run nandboot0 || run nandboot1").boot_order().unwrap(), [Slot::Root0, Slot::Root1]);
        assert!(env("run nandboot0acm || run nandboot0").boot_order().is_err());
        assert_eq!(env("run nandboot1acm || run nandboot1 || run nandboot0").boot_order().unwrap(), [Slot::Root1, Slot::Root0]);
    }
}
