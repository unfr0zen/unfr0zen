//! The j49 kernel's boot logo, Nest's cream disc with the blue house
//! (CONFIG_LOGO_DIAMOND_CLUT224), recoloured onto black for the dark theme:
//! only its palette changes, inside the gzip-compressed zImage of a U-Boot
//! image.
//!
//! The 2.6.37 decompressor reads the kernel's size from the last 4 bytes of
//! the compressed data, at a linked address, and its gunzip skips a header
//! file name but checks no CRC. The recompressed stream is therefore padded
//! to the original length through that file name.
use std::num::NonZeroU64;

use flate2::{Decompress, FlushDecompress, Status};

use crate::{device, shell::sha256_hex, Result};

const UIMAGE_MAGIC: u32 = 0x2705_1956;
const HEADER: usize = 64;
/// Where the decompressed kernel is linked: PAGE_OFFSET + TEXT_OFFSET.
const KERNEL_BASE: u32 = 0xc000_8000;
const LINUX_LOGO_CLUT224: u32 = 3;
const SIZE: u32 = 320;
/// Nest's logo palette, and the same recoloured by `dark`.
const STOCK_CLUT: &str = "9f9b45c9a37930b4f58eeb000f0c4ea0629b337fb05c3971af2953bebb6f9516";
const DARK_CLUT: &str = "95a5f652c640aee831584c110e0daf01d02fb719b4c236d9cbd6222538bb0687";
/// The disc, the corners and the house the logo's colours blend.
const DISC: [f64; 3] = [239.0, 241.0, 231.0];
const CORNER: [f64; 3] = [20.0, 20.0, 20.0];
const HOUSE: [f64; 3] = [62.0, 187.0, 201.0];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Logo {
    Stock,
    Dark,
}

/// A kernel as its parts: the U-Boot image, where the gzip stream sits in
/// it, the decompressed kernel, and the logo's palette in that.
struct Kernel<'a> {
    image: &'a [u8],
    gzip: std::ops::Range<usize>,
    inflated: Vec<u8>,
    clut: std::ops::Range<usize>,
}

fn be(data: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(data[at..at + 4].try_into().unwrap())
}

fn le(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}

/// The raw deflate data from `at` inflated, and how many bytes it took;
/// None when it is not one whole stream.
fn inflate(data: &[u8], hint: usize) -> Option<(Vec<u8>, usize)> {
    let mut inflater = Decompress::new(false);
    let mut out = Vec::with_capacity(hint);
    loop {
        if out.len() == out.capacity() {
            out.reserve(out.len().max(1 << 20));
        }
        let before = (inflater.total_in(), inflater.total_out());
        match inflater.decompress_vec(&data[inflater.total_in() as usize..], &mut out, FlushDecompress::Finish) {
            Ok(Status::StreamEnd) => return Some((out, inflater.total_in() as usize)),
            Ok(_) if (inflater.total_in(), inflater.total_out()) == before && out.len() < out.capacity() => return None,
            Ok(_) => {}
            Err(_) => return None,
        }
    }
}

/// The gzip header's length at `at`: 10 bytes and the file name, the only
/// field the kernel's gunzip skips.
fn gzip_header(data: &[u8], at: usize) -> Option<usize> {
    let header = data.get(at..at + 10)?;
    if header[..3] != [0x1f, 0x8b, 0x08] || header[3] & !0x08 != 0 {
        return None;
    }
    if header[3] & 0x08 == 0 {
        return Some(10);
    }
    Some(10 + data[at + 10..].iter().position(|&b| b == 0)? + 1)
}

fn parse(image: &[u8]) -> Result<Option<Kernel<'_>>> {
    if image.len() < HEADER || be(image, 0) != UIMAGE_MAGIC {
        return device("the kernel is not a U-Boot image");
    }
    let end = HEADER + be(image, 12) as usize;
    if end > image.len() || crc32fast::hash(&image[HEADER..end]) != be(image, 24) {
        return device("the kernel's U-Boot image is incomplete");
    }
    // The zImage's piggy: the first gzip stream that inflates whole to the
    // size its last 4 bytes give.
    let body = &image[..end];
    let mut at = HEADER;
    let (gzip, inflated) = loop {
        let Some(found) = body[at..].windows(3).position(|w| w == [0x1f, 0x8b, 0x08]) else { return Ok(None) };
        at += found;
        if let Some(length) = gzip_header(body, at) {
            if let Some((inflated, used)) = inflate(&body[at + length..], 8 << 20) {
                let stream = at..at + length + used + 8;
                if stream.end <= end && le(body, stream.end - 4) as usize == inflated.len() {
                    break (stream, inflated);
                }
            }
        }
        at += 1;
    };
    // struct linux_logo { type, width, height, clutsize, clut, data }.
    let mut found = None;
    for at in (0..inflated.len().saturating_sub(24)).step_by(4) {
        let word = |i: usize| le(&inflated, at + 4 * i);
        if word(0) != LINUX_LOGO_CLUT224 || word(1) != SIZE || word(2) != SIZE || word(3) > 224 {
            continue;
        }
        let clut = word(4).wrapping_sub(KERNEL_BASE) as usize;
        let range = clut..clut + 3 * word(3) as usize;
        if range.end <= inflated.len() && matches!(sha256_hex(&inflated[range.clone()]).as_str(), STOCK_CLUT | DARK_CLUT) {
            if found.is_some() {
                return device("the kernel has two boot logos");
            }
            found = Some(range);
        }
    }
    Ok(found.map(|clut| Kernel { image, gzip, inflated, clut }))
}

/// Which of the two logos the kernel boots with; None for any other kernel.
pub fn logo(image: &[u8]) -> Result<Option<Logo>> {
    Ok(parse(image)?.map(|kernel| match sha256_hex(&kernel.inflated[kernel.clut]).as_str() {
        STOCK_CLUT => Logo::Stock,
        _ => Logo::Dark,
    }))
}

/// A colour of the logo as a blend of the disc, the corners and the house,
/// with the disc and the corners black: the house keeps its share.
fn dark(rgb: &[u8]) -> [u8; 3] {
    let c: Vec<f64> = rgb.iter().map(|&v| v as f64).collect();
    // c - corner = a (disc - corner) + b (house - corner), least squares.
    let u: Vec<f64> = (0..3).map(|i| DISC[i] - CORNER[i]).collect();
    let v: Vec<f64> = (0..3).map(|i| HOUSE[i] - CORNER[i]).collect();
    let w: Vec<f64> = (0..3).map(|i| c[i] - CORNER[i]).collect();
    let dot = |x: &[f64], y: &[f64]| (0..3).map(|i| x[i] * y[i]).sum::<f64>();
    let (uu, uv, vv, uw, vw) = (dot(&u, &u), dot(&u, &v), dot(&v, &v), dot(&u, &w), dot(&v, &w));
    let b = ((uu * vw - uv * uw) / (uu * vv - uv * uv)).clamp(0.0, 1.0);
    HOUSE.map(|h| (h * b).round() as u8)
}

/// The kernel with the dark logo: None when it has no stock logo, or when
/// the recompressed kernel does not fit the original stream.
pub fn darken(image: &[u8]) -> Result<Option<Vec<u8>>> {
    let Some(kernel) = parse(image)? else { return Ok(None) };
    if sha256_hex(&kernel.inflated[kernel.clut.clone()]) != STOCK_CLUT {
        return Ok(None);
    }
    let mut inflated = kernel.inflated.clone();
    for rgb in inflated[kernel.clut.clone()].chunks_mut(3) {
        let colour = dark(rgb);
        rgb.copy_from_slice(&colour);
    }
    let options = zopfli::Options { iteration_count: NonZeroU64::new(1).unwrap(), ..Default::default() };
    let mut deflated = Vec::new();
    zopfli::compress(options, zopfli::Format::Deflate, &inflated[..], &mut deflated)
        .map_err(|e| crate::Error::Io(e.to_string()))?;
    let length = kernel.gzip.len();
    let Some(name) = length.checked_sub(10 + 1 + deflated.len() + 8) else {
        log::warn!("the recompressed kernel is {} bytes over its stream", 10 + 1 + deflated.len() + 8 - length);
        return Ok(None);
    };
    let original = &kernel.image[kernel.gzip.clone()];
    let mut gzip = Vec::with_capacity(length);
    gzip.extend([0x1f, 0x8b, 0x08, 0x08]);
    gzip.extend(&original[4..10]); // modification time, extra flags, OS
    gzip.extend(b"unfr0zen dark boot logo ".iter().cycle().take(name));
    gzip.push(0);
    gzip.extend(&deflated);
    gzip.extend(crc32fast::hash(&inflated).to_le_bytes());
    gzip.extend((inflated.len() as u32).to_le_bytes());
    let mut image = kernel.image.to_vec();
    image[kernel.gzip.clone()].copy_from_slice(&gzip);
    let end = HEADER + be(&image, 12) as usize;
    let data = crc32fast::hash(&image[HEADER..end]);
    image[24..28].copy_from_slice(&data.to_be_bytes());
    image[4..8].fill(0);
    let header = crc32fast::hash(&image[..HEADER]);
    image[4..8].copy_from_slice(&header.to_be_bytes());
    let check = parse(&image)?.filter(|k| k.gzip == kernel.gzip && k.inflated == inflated);
    if check.is_none() {
        return device("the kernel with the dark boot logo does not read back");
    }
    Ok(Some(image))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_disc_and_corners_go_black_and_the_house_keeps_its_share() {
        assert_eq!(dark(&[239, 241, 231]), [0, 0, 0]);
        assert_eq!(dark(&[235, 237, 227]), [0, 0, 0]);
        assert_eq!(dark(&[20, 20, 20]), [0, 0, 0]);
        assert_eq!(dark(&[62, 187, 201]), [62, 187, 201]);
        // About halfway between the disc and the house: about half the house.
        assert_eq!(dark(&[151, 214, 216]), [31, 93, 100]);
    }

    #[test]
    fn the_gzip_header_takes_only_a_file_name() {
        assert_eq!(gzip_header(&[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 2, 3, 0xaa], 0), Some(10));
        assert_eq!(gzip_header(&[0x1f, 0x8b, 8, 8, 0, 0, 0, 0, 2, 3, b'a', b'b', 0, 0xaa], 0), Some(13));
        assert_eq!(gzip_header(&[0x1f, 0x8b, 8, 4, 0, 0, 0, 0, 2, 3, 0, 0], 0), None, "an extra field");
        assert_eq!(gzip_header(&[0x1f, 0x8b, 8, 8, 0, 0, 0, 0, 2, 3, b'a'], 0), None, "an unterminated name");
    }

    #[test]
    fn only_u_boot_images_are_read() {
        assert!(logo(b"not a kernel").is_err());
        let mut image = vec![0u8; HEADER + 4];
        image[..4].copy_from_slice(&UIMAGE_MAGIC.to_be_bytes());
        image[12..16].copy_from_slice(&4u32.to_be_bytes());
        image[24..28].copy_from_slice(&crc32fast::hash(&[0; 4]).to_be_bytes());
        assert_eq!(logo(&image).unwrap(), None, "no zImage, no logo");
        assert_eq!(darken(&image).unwrap(), None);
    }
}
