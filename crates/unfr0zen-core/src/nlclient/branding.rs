//! Discover the procedural Google boot-animation palette by its ARM function shape.
use super::{
    arm::Arm,
    elf::{word, Elf},
};
use crate::{boot_branding::COLORS, Result};
use std::collections::BTreeMap;

pub(super) fn recolor(source: &[u8], output: &mut [u8]) -> Result<bool> {
    let elf = Elf::parse(source)?;
    let arm = Arm::new(&elf)?;
    let exidx = elf.section(".ARM.exidx")?;
    let mut starts = Vec::new();
    for at in (exidx.offset..exidx.offset + exidx.size).step_by(8) {
        let relative = ((word(source, at)? << 1) as i32) >> 1;
        starts.push((exidx.address + (at - exidx.offset) as u32).wrapping_add(relative as u32));
    }
    let mut candidates: BTreeMap<u32, Vec<(usize, usize)>> = BTreeMap::new();
    for section in elf.sections.iter().filter(|s| s.flags & 4 != 0) {
        for at in (section.offset..(section.offset + section.size).saturating_sub(7)).step_by(4) {
            let lo = word(source, at)?;
            let hi = word(source, at + 4)?;
            if lo & 0xfff00000 != 0xe3000000
                || hi & 0xfff00000 != 0xe3400000
                || lo & 0xf000 != hi & 0xf000
            {
                continue;
            }
            let value =
                (((hi >> 4) & 0xf000) | (hi & 0xfff)) << 16 | ((lo >> 4) & 0xf000) | (lo & 0xfff);
            let Some(index) = COLORS.iter().position(|&(before, _)| before == value) else {
                continue;
            };
            let address = section.address + (at - section.offset) as u32;
            let index_fn = starts.partition_point(|&s| s <= address);
            if index_fn > 0 {
                candidates
                    .entry(starts[index_fn - 1])
                    .or_default()
                    .push((at, index));
            }
        }
    }
    let candidates: Vec<_> = candidates
        .into_iter()
        .filter(|(_, hits)| {
            hits.len() == 4 && (0..4).all(|i| hits.iter().any(|&(_, index)| index == i))
        })
        .collect();
    let [(function, hits)] = candidates.as_slice() else {
        return Ok(false);
    };
    if arm.fingerprint(*function).ok().as_deref()
        != Some("12fec1b7876a0475d685bce6629e03e25268a8c73e17c1c9676af3870d96c844")
    {
        return Ok(false);
    }
    for &(at, index) in hits {
        let color = COLORS[index].1;
        for (delta, value) in [(0, color & 0xffff), (4, color >> 16)] {
            let original = word(source, at + delta)?;
            let patched = (original & !0xf0fff) | ((value & 0xf000) << 4) | (value & 0xfff);
            output[at + delta..at + delta + 4].copy_from_slice(&patched.to_le_bytes());
        }
    }
    log::info!("Recolored the recognized nlclient boot-animation palette");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recognized_palette_is_reversible_and_unknown_code_is_untouched() {
        let Some(path) = std::env::var_os("NLCLIENT_STOCK") else {
            return;
        };
        let source = std::fs::read(path).unwrap();
        let mut output = source.clone();
        assert!(recolor(&source, &mut output).unwrap());
        assert_eq!(source.len(), output.len());
        assert_eq!(
            source.iter().zip(&output).filter(|(a, b)| a != b).count(),
            15
        );
        let journal = super::super::Patch::journal(&source, &output).unwrap();
        assert_eq!(
            super::super::Patch::parse(&journal)
                .unwrap()
                .apply(&output, true)
                .unwrap(),
            source
        );
        let before = output.clone();
        assert!(!recolor(&before, &mut output).unwrap());
        assert_eq!(output, before);
        // The palette match alone is insufficient: the enclosing function must match too.
        let first = source
            .iter()
            .zip(&output)
            .position(|(a, b)| a != b)
            .unwrap()
            & !3;
        let mut unknown = source.clone();
        unknown[first - 3] ^= 0x10;
        let mut untouched = unknown.clone();
        assert!(!recolor(&unknown, &mut untouched).unwrap());
        assert_eq!(untouched, unknown);
    }
}
