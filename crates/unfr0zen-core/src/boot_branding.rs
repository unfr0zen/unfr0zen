//! Recolor recognized Google boot artwork without changing shapes or frame data.
use crate::{device, shell::sha256_hex, Result};
use std::collections::BTreeMap;

pub const GRADIENT: [[u8; 3]; 8] = [
    [225, 235, 239],
    [197, 219, 229],
    [152, 191, 211],
    [124, 182, 209],
    [94, 172, 209],
    [64, 146, 185],
    [18, 90, 126],
    [5, 81, 120],
];
pub(crate) const COLORS: [(u32, u32); 4] = [
    (0xffea4335, 0xff125a7e),
    (0xfffbbc04, 0xffe1ebef),
    (0xff34a853, 0xff5eacd1),
    (0xff4285f4, 0xff98bfd3),
];
pub(crate) const FILES: [(&str, &str); 3] = [
    (
        "nestlabs/share/images/G_splash_88px.png",
        "etc/unfr0zen/boot/splash.original",
    ),
    (
        "nestlabs/share/images/G_animation_168frames_94px.png",
        "etc/unfr0zen/boot/animation.original",
    ),
    (
        "boot/kernel_fdt.itb",
        "etc/unfr0zen/boot/kernel_fdt.itb.original",
    ),
];

fn color(rgb: &[u8]) -> [u8; 3] {
    let max = *rgb.iter().max().unwrap() as f64;
    if max == 0.0 {
        return [0; 3];
    }
    let (old, new) = COLORS
        .iter()
        .min_by(|(a, _), (b, _)| {
            let distance = |v: u32| {
                let c = [(v >> 16) as u8, (v >> 8) as u8, v as u8];
                let peak = *c.iter().max().unwrap() as f64;
                (0..3)
                    .map(|i| (rgb[i] as f64 / max - c[i] as f64 / peak).powi(2))
                    .sum::<f64>()
            };
            distance(*a).total_cmp(&distance(*b))
        })
        .unwrap();
    let primary = [(old >> 16) as u8, (old >> 8) as u8, *old as u8];
    let intensity = (max / *primary.iter().max().unwrap() as f64).min(1.0);
    [(new >> 16) as u8, (new >> 8) as u8, *new as u8].map(|v| (v as f64 * intensity).round() as u8)
}

/// Unknown or unsupported artwork is left untouched, including already branded files.
pub fn recolor(name: &str, source: &[u8]) -> Result<Option<Vec<u8>>> {
    if name == FILES[2].0 {
        return match kernel_fit(source) {
            Ok(output) => Ok(output),
            Err(error) => {
                log::info!("Boot logo skipped: {error}");
                Ok(None)
            }
        };
    }
    let expected = match name {
        "nestlabs/share/images/G_splash_88px.png" => {
            "80db15241d616de65c871e56b2ca1114c6b8016863619dede8e2ecb814059da0"
        }
        "nestlabs/share/images/G_animation_168frames_94px.png" => {
            "136f4fdeb96076a7e41f496773a0c2d78c1a985f10e367f326cd7da32e79c414"
        }
        _ => return Ok(None),
    };
    if sha256_hex(source) != expected {
        return Ok(None);
    }
    let mut output = source.to_vec();
    let mut at = 8;
    while at + 12 <= output.len() {
        let n = be(&output, at)? as usize;
        let end = at
            .checked_add(12 + n)
            .ok_or_else(|| crate::Error::Device("PNG range overflow".into()))?;
        if end > output.len() {
            return device("truncated PNG");
        }
        if &output[at + 4..at + 8] == b"PLTE" {
            for rgb in output[at + 8..at + 8 + n].as_chunks_mut::<3>().0 {
                *rgb = color(rgb);
            }
            let crc = crc32fast::hash(&output[at + 4..at + 8 + n]);
            output[at + 8 + n..end].copy_from_slice(&crc.to_be_bytes());
            return Ok(Some(output));
        }
        at = end;
    }
    device("recognized PNG has no palette")
}

fn range(data: &[u8], at: usize, n: usize) -> Result<&[u8]> {
    data.get(
        at..at
            .checked_add(n)
            .ok_or_else(|| crate::Error::Device("boot image range overflow".into()))?,
    )
    .ok_or_else(|| crate::Error::Device("truncated boot image".into()))
}
fn be(data: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(range(data, at, 4)?.try_into().unwrap()))
}
fn le(data: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(range(data, at, 4)?.try_into().unwrap()))
}
fn cstr(data: &[u8], at: usize) -> Result<&str> {
    let tail = data
        .get(at..)
        .ok_or_else(|| crate::Error::Device("invalid FIT string offset".into()))?;
    let n = tail
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| crate::Error::Device("unterminated FIT string".into()))?;
    std::str::from_utf8(&tail[..n]).or_else(|_| device("invalid FIT string"))
}
/// Property ranges retain the original FIT layout, including image lengths.
fn properties(data: &[u8]) -> Result<BTreeMap<(String, String), (usize, usize)>> {
    if be(data, 0)? != 0xd00dfeed || be(data, 4)? as usize != data.len() {
        return device("unsupported FIT");
    }
    let mut at = be(data, 8)? as usize;
    let strings = range(data, be(data, 12)? as usize, be(data, 32)? as usize)?;
    let end = at
        .checked_add(be(data, 36)? as usize)
        .filter(|&v| v <= data.len())
        .ok_or_else(|| crate::Error::Device("invalid FIT structure range".into()))?;
    let mut stack = Vec::new();
    let mut props = BTreeMap::new();
    while at < end {
        let token = be(data, at)?;
        at += 4;
        match token {
            1 => {
                let name = cstr(&data[..end], at)?;
                at = (at + name.len() + 4) & !3;
                stack.push(name);
            }
            2 => {
                if stack.pop().is_none() {
                    return device("unbalanced FIT nodes");
                }
            }
            3 => {
                let size = be(data, at)? as usize;
                let name = cstr(strings, be(data, at + 4)? as usize)?;
                at += 8;
                range(&data[..end], at, size)?;
                if props
                    .insert((stack.join("/"), name.into()), (at, size))
                    .is_some()
                {
                    return device("duplicate FIT property");
                }
                at = (at + size + 3) & !3;
            }
            4 => {}
            9 if stack.is_empty() => return Ok(props),
            _ => return device("unsupported FIT token"),
        }
    }
    device("unterminated FIT structure")
}

struct Block {
    packed: std::ops::Range<usize>,
    raw: Vec<u8>,
}
struct Lzop {
    start: usize,
    header_end: usize,
    end: usize,
    blocks: Vec<Block>,
}
fn lzop(data: &[u8], start: usize) -> Result<Lzop> {
    // Supported lzop headers use Adler-32 on uncompressed blocks, no filter or extras.
    if range(data, start + 15, 2)? != [3, 9] || be(data, start + 17)? != 0x0300000d {
        return device("unsupported kernel LZOP header");
    }
    let mut at = start + 34 + range(data, start + 33, 1)?[0] as usize;
    if adler2::adler32_slice(range(data, start + 9, at - start - 9)?) != be(data, at)? {
        return device("bad LZOP header checksum");
    }
    at += 4;
    let header_end = at;
    let mut blocks = Vec::new();
    let mut total = 0usize;
    loop {
        let block_start = at;
        let n = be(data, at)? as usize;
        at += 4;
        if n == 0 {
            break;
        }
        total += n;
        if n > 256 * 1024 || total > 32 * 1024 * 1024 {
            return device("unsupported kernel block size");
        }
        let packed = be(data, at)? as usize;
        let checksum = be(data, at + 4)?;
        at += 8;
        if packed == 0 || packed > n {
            return device("invalid LZOP block size");
        }
        let input = range(data, at, packed)?;
        at += packed;
        let mut raw = vec![0; n];
        if n == packed {
            raw.copy_from_slice(input);
        } else if lzokay::decompress::decompress(input, &mut raw) != Ok(n) {
            return device("invalid kernel LZO block");
        }
        if adler2::adler32_slice(&raw) != checksum {
            return device("bad kernel block checksum");
        }
        blocks.push(Block {
            packed: block_start..at,
            raw,
        });
    }
    if blocks.is_empty() {
        return device("empty kernel LZOP stream");
    }
    Ok(Lzop {
        start,
        header_end,
        end: at,
        blocks,
    })
}

fn logo_palette(kernel: &[u8]) -> Result<Option<(usize, usize)>> {
    let mut found = None;
    for at in (0..kernel.len().saturating_sub(24)).step_by(4) {
        if le(kernel, at)? != 3 || le(kernel, at + 4)? != 480 || le(kernel, at + 8)? != 480 {
            continue;
        }
        let count = le(kernel, at + 12)? as usize;
        if count != 166 {
            continue;
        }
        let Some(palette) = le(kernel, at + 16)?
            .checked_sub(0xc0008000)
            .map(|v| v as usize)
        else {
            continue;
        };
        let Some(pixels) = le(kernel, at + 20)?
            .checked_sub(0xc0008000)
            .map(|v| v as usize)
        else {
            continue;
        };
        let (Ok(colors), Ok(indexes)) = (
            range(kernel, palette, count * 3),
            range(kernel, pixels, 480 * 480),
        ) else {
            continue;
        };
        if palette + count * 3 > pixels
            || indexes.iter().any(|&v| v < 32 || v as usize >= 32 + count)
        {
            continue;
        }
        if sha256_hex(&[colors, indexes].concat())
            != "e0a36e8a9069b4a4d302ebf938e3ab293c875c347c03dc29d725e4a04d6a0777"
        {
            continue;
        }
        if found.replace((palette, count * 3)).is_some() {
            return device("ambiguous kernel logo");
        }
    }
    Ok(found)
}

fn kernel_fit(source: &[u8]) -> Result<Option<Vec<u8>>> {
    let props = properties(source)?;
    let Some(&(at, size)) = props.get(&("/images/kernel@1".into(), "data".into())) else {
        return Ok(None);
    };
    let hash_path = "/images/kernel@1/hash@1".to_string();
    let Some(&(hash_at, 32)) = props.get(&(hash_path.clone(), "value".into())) else {
        return Ok(None);
    };
    let Some(&(algo_at, algo_size)) = props.get(&(hash_path, "algo".into())) else {
        return Ok(None);
    };
    let zimage = range(source, at, size)?;
    if range(source, algo_at, algo_size)? != b"sha256\0"
        || sha256_hex(zimage) != sha256_hex_bytes(range(source, hash_at, 32)?)
    {
        return device("kernel FIT hash does not match");
    }
    if le(zimage, 36)? != 0x016f2818 {
        return Ok(None);
    }
    let mut streams = Vec::new();
    for (i, bytes) in zimage.windows(9).enumerate() {
        if bytes == b"\x89LZO\0\r\n\x1a\n" {
            if let Ok(stream) = lzop(zimage, i) {
                streams.push(stream);
            }
        }
    }
    if streams.len() != 1 {
        return Ok(None);
    }
    let stream = streams.pop().unwrap();
    let kernel: Vec<u8> = stream
        .blocks
        .iter()
        .flat_map(|b| b.raw.iter().copied())
        .collect();
    let Some((palette, n)) = logo_palette(&kernel)? else {
        return Ok(None);
    };
    let mut modified = kernel.clone();
    for rgb in modified[palette..palette + n].as_chunks_mut::<3>().0 {
        *rgb = color(rgb);
    }
    let mut packed = zimage[stream.start..stream.header_end].to_vec();
    let mut offset = 0;
    for block in &stream.blocks {
        let raw = &modified[offset..offset + block.raw.len()];
        offset += raw.len();
        if raw == block.raw {
            packed.extend_from_slice(&zimage[block.packed.clone()]);
        } else {
            let compressed = lzokay::compress::compress(raw).map_err(|error| {
                crate::Error::Device(format!("kernel LZO compression: {error}"))
            })?;
            let data = if compressed.len() < raw.len() {
                &compressed
            } else {
                raw
            };
            packed.extend_from_slice(&(raw.len() as u32).to_be_bytes());
            packed.extend_from_slice(&(data.len() as u32).to_be_bytes());
            packed.extend_from_slice(&adler2::adler32_slice(raw).to_be_bytes());
            packed.extend_from_slice(data);
        }
    }
    packed.extend_from_slice(&[0; 4]);
    let capacity = stream.end - stream.start;
    if packed.len() > capacity {
        return device(format!(
            "recolored logo exceeds kernel compression space: {} > {capacity}",
            packed.len()
        ));
    }
    packed.resize(capacity, 0);
    let mut patched = zimage.to_vec();
    patched[stream.start..stream.end].copy_from_slice(&packed);
    let round_trip = lzop(&patched, stream.start)?;
    if round_trip
        .blocks
        .iter()
        .flat_map(|b| b.raw.iter())
        .copied()
        .collect::<Vec<_>>()
        != modified
    {
        return device("kernel compression round trip failed");
    }
    let mut output = source.to_vec();
    output[at..at + size].copy_from_slice(&patched);
    use sha2::Digest;
    output[hash_at..hash_at + 32].copy_from_slice(&sha2::Sha256::digest(&patched));
    Ok(Some(output))
}
fn sha256_hex_bytes(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixtures() -> Option<std::path::PathBuf> {
        std::env::var_os("BOOT_BRANDING_FIXTURES").map(Into::into)
    }

    #[test]
    fn lzop_decodes_lzo1x_blocks_and_requires_the_declared_size() {
        let raw = b"kernel LZO compatibility block\n".repeat(64);
        // LZO1X-999 encoding of the synthetic text above.
        let reference = vec![
            48, 107, 101, 114, 110, 101, 108, 32, 76, 90, 79, 32, 99, 111, 109, 112, 97, 116, 105,
            98, 105, 108, 105, 116, 121, 32, 98, 108, 111, 99, 107, 10, 32, 0, 0, 0, 0, 0, 0, 0,
            135, 120, 0, 17, 0, 0,
        ];
        for compressed in [reference, lzokay::compress::compress(&raw).unwrap()] {
            for size in [raw.len(), raw.len() - 1, raw.len() + 1] {
                let mut data = vec![0; 34];
                data[..9].copy_from_slice(b"\x89LZO\0\r\n\x1a\n");
                data[15..17].copy_from_slice(&[3, 9]);
                data[17..21].copy_from_slice(&0x0300000du32.to_be_bytes());
                data.extend_from_slice(&adler2::adler32_slice(&data[9..]).to_be_bytes());
                data.extend_from_slice(&(size as u32).to_be_bytes());
                data.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
                let mut expected = raw.clone();
                expected.resize(size, 0);
                data.extend_from_slice(&adler2::adler32_slice(&expected).to_be_bytes());
                data.extend_from_slice(&compressed);
                data.extend_from_slice(&[0; 4]);
                if size == raw.len() {
                    assert_eq!(lzop(&data, 0).unwrap().blocks[0].raw, raw);
                } else {
                    assert!(lzop(&data, 0).is_err());
                }
            }
        }
    }

    #[test]
    fn primary_colors_match_the_shared_gradient() {
        for ((old, new), stop) in COLORS.iter().zip([6, 0, 4, 2]) {
            assert_eq!(
                color(&[(old >> 16) as u8, (old >> 8) as u8, *old as u8]),
                GRADIENT[stop]
            );
            assert_eq!(
                *new & 0xffffff,
                (GRADIENT[stop][0] as u32) << 16
                    | (GRADIENT[stop][1] as u32) << 8
                    | GRADIENT[stop][2] as u32
            );
        }
        assert_eq!(color(&[0, 0, 0]), [0, 0, 0]);
    }

    #[test]
    fn unmatched_and_truncated_images_are_ignored() {
        for (name, _) in FILES {
            for size in 0..128 {
                assert!(recolor(name, &vec![0xff; size]).unwrap().is_none());
            }
        }
        assert!(recolor("other.png", b"custom art").unwrap().is_none());
    }

    fn png_chunks(data: &[u8]) -> Vec<(&[u8], &[u8])> {
        let mut at = 8;
        let mut chunks = Vec::new();
        while at < data.len() {
            let n = be(data, at).unwrap() as usize;
            assert_eq!(
                be(data, at + 8 + n).unwrap(),
                crc32fast::hash(&data[at + 4..at + 8 + n])
            );
            chunks.push((&data[at + 4..at + 8], &data[at + 8..at + 8 + n]));
            at += 12 + n;
        }
        chunks
    }

    #[test]
    fn researched_pngs_keep_pixels_transparency_and_frame_dimensions() {
        let Some(dir) = fixtures() else {
            return;
        };
        for ((name, _), fixture) in FILES[..2]
            .iter()
            .zip(["splash-stock.png", "animation-stock.png"])
        {
            let source = std::fs::read(dir.join(fixture)).unwrap();
            let output = recolor(name, &source).unwrap().expect("recognized PNG");
            assert_eq!(source.len(), output.len());
            for ((kind, before), (after_kind, after)) in
                png_chunks(&source).into_iter().zip(png_chunks(&output))
            {
                assert_eq!(kind, after_kind);
                if kind == b"PLTE" {
                    assert_ne!(before, after);
                } else {
                    assert_eq!(before, after);
                }
            }
            assert!(recolor(name, &output).unwrap().is_none());
            let mut custom = source.clone();
            *custom.last_mut().unwrap() ^= 1;
            assert!(recolor(name, &custom).unwrap().is_none());
        }
    }

    #[test]
    fn researched_kernel_keeps_pixels_decompressor_and_device_trees() {
        let Some(dir) = fixtures() else {
            return;
        };
        let source = std::fs::read(dir.join("kernel-stock.itb")).unwrap();
        let output = kernel_fit(&source).unwrap().expect("recognized kernel");
        assert_eq!(source.len(), output.len());
        assert!(kernel_fit(&output).unwrap().is_none());
        let props = properties(&source).unwrap();
        let (at, size) = props[&("/images/kernel@1".into(), "data".into())];
        for ((path, key), &(start, len)) in &props {
            if path != "/images/kernel@1" && path != "/images/kernel@1/hash@1" {
                assert_eq!(
                    &source[start..start + len],
                    &output[start..start + len],
                    "{path}/{key}"
                );
            }
        }
        let zimage = &source[at..at + size];
        let stream = zimage
            .windows(9)
            .enumerate()
            .filter(|(_, b)| *b == b"\x89LZO\0\r\n\x1a\n")
            .find_map(|(i, _)| lzop(zimage, i).ok())
            .unwrap();
        let kernel: Vec<u8> = stream
            .blocks
            .iter()
            .flat_map(|b| b.raw.iter().copied())
            .collect();
        let (palette, count) = logo_palette(&kernel).unwrap().unwrap();
        let new_zimage = &output[at..at + size];
        let modified: Vec<u8> = lzop(new_zimage, stream.start)
            .unwrap()
            .blocks
            .into_iter()
            .flat_map(|b| b.raw)
            .collect();
        assert_eq!(&kernel[..palette], &modified[..palette]);
        assert_eq!(&kernel[palette + count..], &modified[palette + count..]);
        assert_eq!(&zimage[..stream.start], &new_zimage[..stream.start]);
        assert_eq!(&zimage[stream.end..], &new_zimage[stream.end..]);
        assert_ne!(
            &kernel[palette..palette + count],
            &modified[palette..palette + count]
        );
        use sha2::Digest;
        let (hash, _) = props[&("/images/kernel@1/hash@1".into(), "value".into())];
        assert_eq!(
            &output[hash..hash + 32],
            sha2::Sha256::digest(new_zimage).as_slice()
        );
        let mut damaged = source.clone();
        damaged[at + size / 2] ^= 1;
        assert!(recolor(FILES[2].0, &damaged).unwrap().is_none());
    }
}
