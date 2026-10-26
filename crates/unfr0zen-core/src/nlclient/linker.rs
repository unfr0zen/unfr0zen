//! Link the embedded ARM object in memory. This is the payload.ld contract,
//! not a general linker: unknown sections, relocations and veneers fail closed.
use super::elf::{align, bytes, half, string, word, Elf};
use crate::{device, Result};
use std::collections::BTreeMap;
pub struct Linked {
    pub bytes: Vec<u8>,
    pub entry: u32,
    pub symbols: BTreeMap<String, u32>,
}
pub fn link(object: &[u8], address: u32, externals: &BTreeMap<String, u32>) -> Result<Linked> {
    let elf = Elf::parse(object)?;
    if half(object, 16)? != 1 || address & 3 != 0 {
        return device("payload must be an ARM relocatable object at an aligned address");
    }
    if elf
        .sections
        .iter()
        .any(|s| s.flags & 2 != 0 && (s.size > 1 << 20 || s.align > 0x10000))
    {
        return device("payload section exceeds supported size or alignment");
    }
    let mut offsets = BTreeMap::new();
    let mut output = Vec::new();
    // ld.lld -O0 keeps SHF_MERGE strings in input order, including duplicates.
    for prefix in [".text", ".rodata"] {
        let sections: Vec<_> = elf
            .sections
            .iter()
            .enumerate()
            .filter(|(_, s)| s.name.starts_with(prefix))
            .collect();
        let group_align = sections.iter().map(|(_, s)| s.align).max().unwrap_or(1);
        output.resize(align(output.len(), group_align)?, 0);
        for (index, s) in sections {
            if s.kind != 1 || s.flags & 1 != 0 {
                return device("writable or unsupported payload section");
            }
            output.resize(align(output.len(), s.align)?, 0);
            offsets.insert(index, output.len());
            output.extend(elf.contents(s));
        }
    }
    for (i, s) in elf.sections.iter().enumerate() {
        if s.size != 0
            && s.flags & 2 != 0
            && !offsets.contains_key(&i)
            && !s.name.starts_with(".ARM.exidx")
            && !s.name.starts_with(".ARM.extab")
        {
            return device(format!("unsupported allocated payload section {}", s.name));
        }
    }
    if output.is_empty()
        || address
            .checked_add(
                u32::try_from(output.len())
                    .map_err(|_| crate::Error::Device("payload too large".into()))?,
            )
            .is_none()
    {
        return device("invalid payload address range");
    }
    let table = elf.section(".symtab")?;
    let strings = elf
        .sections
        .get(table.link)
        .ok_or_else(|| crate::Error::Device("invalid payload symbol strings".into()))?;
    if table.size % 16 != 0 {
        return device("invalid payload symbol table");
    }
    let mut values = Vec::new();
    let mut symbols = externals.clone();
    for o in (table.offset..table.offset + table.size).step_by(16) {
        let name = string(elf.contents(strings), word(object, o)? as usize)?;
        let value = word(object, o + 4)?;
        let section = half(object, o + 14)? as usize;
        let value = match section {
            0 if name.is_empty() => 0,
            0 => *externals
                .get(&name)
                .ok_or_else(|| crate::Error::Device(format!("unresolved payload symbol {name}")))?,
            0xfff1 => value,
            _ => match offsets.get(&section) {
                Some(offset) => {
                    let section = &elf.sections[section];
                    if value as usize > section.size {
                        return device("payload symbol outside its section");
                    }
                    address
                        .checked_add(*offset as u32)
                        .and_then(|v| v.checked_add(value))
                        .ok_or_else(|| {
                            crate::Error::Device("payload symbol address overflow".into())
                        })?
                }
                None => {
                    values.push(None);
                    continue;
                }
            },
        };
        values.push(Some(value));
        // lld drops assembler .L* and anonymous section symbols from .symtab.
        if !name.is_empty() && !name.starts_with(".L") {
            symbols.insert(name, value);
        }
    }
    for s in &elf.sections {
        if s.kind != 9 && s.kind != 4 {
            continue;
        }
        let Some(&base) = offsets.get(&s.info) else {
            continue;
        };
        if s.kind != 9
            || s.link
                != elf
                    .sections
                    .iter()
                    .position(|s| s.name == ".symtab")
                    .unwrap()
            || s.size % 8 != 0
        {
            return device("unsupported payload relocation table");
        }
        for o in (s.offset..s.offset + s.size).step_by(8) {
            let relative = word(object, o)? as usize;
            let info = word(object, o + 4)?;
            let symbol = values
                .get((info >> 8) as usize)
                .and_then(|s| *s)
                .ok_or_else(|| {
                    crate::Error::Device("relocation to a discarded or invalid symbol".into())
                })?;
            let target = elf
                .sections
                .get(s.info)
                .ok_or_else(|| crate::Error::Device("invalid relocation destination".into()))?;
            bytes(elf.contents(target), relative, 4)?;
            let at = base + relative;
            let old = word(&output, at)?;
            let place = address + at as u32;
            let value = match info & 255 {
                2 => symbol.wrapping_add(old),                     // ABS32
                3 => symbol.wrapping_add(old).wrapping_sub(place), // REL32
                28 | 29 => {
                    // CALL / JUMP24: ELF addend already contains -8.
                    if old & 0x0e000000 != 0x0a000000 || symbol & 3 != 0 {
                        return device("non-ARM payload branch relocation");
                    }
                    let addend = ((old << 8) as i32 >> 6) as i64;
                    let delta = symbol as i64 + addend - place as i64;
                    if delta & 3 != 0 || !(-(1 << 25)..(1 << 25)).contains(&delta) {
                        return device("payload branch needs an unsupported veneer");
                    }
                    (old & 0xff000000) | ((delta >> 2) as u32 & 0xffffff)
                }
                kind => return device(format!("unsupported ARM payload relocation {kind}")),
            };
            output[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
    }
    let entry = *symbols
        .get("custom_factory")
        .ok_or_else(|| crate::Error::Device("payload has no custom_factory".into()))?;
    if !(address..address + output.len() as u32).contains(&entry) {
        return device("payload factory outside executable bytes");
    }
    Ok(Linked {
        bytes: output,
        entry,
        symbols,
    })
}
