//! The settings menu's entries: Zigbee, Matter, Themes and Doom after its
//! first entry, each only with its feature on, and UNFR0ZEN in Legal Info's
//! place. Each takes the name of a stock screen whose factory the screen
//! object replaces (custom_root in platform/common/nlclient/src/custom_flow.h);
//! those stock screens (Nest app, Software, Nest Pro, Lock) leave the menu.
//!
//! The menu is a static list of screen names, between two "temperature"
//! entries, that nlclient reads in a loop of fixed length: the list is
//! rewritten in place and the loop's bound, an `add Rend, Rbase, #4*count`
//! after the list's address is loaded, shortened to match. The slots past the
//! bound keep valid names, since nlclient's lookup of a screen by name walks
//! the whole list. SCHEDULE uses the unused cloud-setup child registration;
//! a validated native insertion adds it before Doom or the first stock
//! entry at runtime, retaining every stock slot and both sentinels without
//! relocating the pointer table.
use super::{
    arm::Arm,
    discovery,
    elf::{word, Elf},
};
use crate::{device, Result};

/// The stock screens the entries take the place of, in the order of
/// [`Section`], then UNFR0ZEN's.
pub const HOSTS: [&str; 5] = ["account", "update", "proinfoscreen", "templock", "legalinfo"];
const LEGACY_HOSTS: [&str; 4] = ["account", "proinfoscreen", "templock", "legalinfo"];

pub(super) fn hosts(source: &[u8]) -> &'static [&'static str] {
    if super::legacy_abi(source) {
        &LEGACY_HOSTS
    } else {
        &HOSTS
    }
}

/// The unused cloud-setup child supplies SCHEDULE's distinct registration.
pub const SCHEDULE_HOST: &str = "appsetup";

pub(super) fn schedule_host(source: &[u8]) -> &'static str {
    if super::legacy_abi(source) {
        "update"
    } else {
        SCHEDULE_HOST
    }
}

/// A native menu insertion site. Modern clients use a non-owning pointer
/// vector; older ones use bounded inline name and screen arrays.
#[derive(serde::Serialize)]
pub(super) struct Insertion {
    pub call: u32,
    pub lookup: u32,
    pub grow: u32,
    pub object_register: u32,
    pub names_register: u32,
    pub screens_register: u32,
    pub next_register: u32,
    /// Whether `grow` appends (an older libstdc++'s `_M_emplace_back_aux`,
    /// taking the value's address) rather than inserting at a position.
    pub appends: bool,
}

pub(super) fn insertion(elf: &Elf, sections: &[Section]) -> Result<Insertion> {
    let (list, names) = discovery::settings(elf)?;
    if names.len() < 3 || names[names.len() - 3..] != ["legalinfo", "reset", "temperature"] {
        return device("settings menu insertion requires Legal Info before Reset and Temperature");
    }
    let arm = Arm::new(elf)?;
    let (_, mut found) = menu_loop(elf, &arm, list, names.len())?;
    if found.len() != 1 {
        return device(format!("settings menu insertion: expected one safe shape, found {}", found.len()));
    }
    let found = found.pop().unwrap();
    let arranged = arranged_with_abi(&names, sections, super::legacy_abi(elf.data))?;
    if super::legacy_abi(elf.data) && arranged.len() + 1 > 27 {
        return device("5.6-7 settings name capacity: choose at most two of Zigbee, Matter and Doom");
    }
    if found.grow == 0 && arranged.len() > 32 {
        return device("settings menu insertion exceeds legacy name capacity");
    }
    if !(4..12).contains(&found.object_register) {
        return device("settings menu object is not in a preserved register");
    }
    // A successful lookup returns the hash table's borrowed pointer directly.
    let lookup = arm.code(found.lookup, None)?;
    let imports = discovery::imported(elf)?;
    let first = lookup
        .iter()
        .position(|i| i.name == "bl")
        .ok_or_else(|| crate::Error::Device("menu lookup has no call".into()))?;
    if lookup[first].ops[0].imm() != imports.get("PL_HashTableLookupConst").copied()
        || lookup.get(first + 1).map(|i| i.word) != Some(0xe2504000)
        || lookup.get(first + 2).map(|i| i.word >> 24) != Some(0x0a)
        || lookup.get(first + 3).map(|i| i.word) != Some(0xe1a00004)
        || !lookup.get(first + 4).is_some_and(|i| [0xe8bd8070, 0xe89da830, 0xe8bd8038].contains(&i.word))
    {
        return device("settings menu lookup does not return a borrowed screen");
    }
    if found.grow != 0 {
        let grow = arm.code(found.grow, None)?;
        let calls = arm.calls(found.grow)?;
        // Allocates, moves the old elements, frees them and stores the new
        // buffer's begin, end and capacity: inserting the value at the
        // position, or (appending) reading it from the second argument.
        let shape: &[u32] = if found.appends {
            &[0xe5993000, 0x17873004, 0xe5867000, 0xe9860030]
        } else {
            &[0xe8900180, 0xe585b000, 0xe5858004, 0xe5854008]
        };
        if !shape.iter().all(|w| grow.iter().any(|i| i.word == *w))
            || !imports.get("_Znwj").is_some_and(|address| calls.contains(address))
            || !["_ZdlPv", "_ZdlPvj"]
                .iter()
                .any(|name| imports.get(*name).is_some_and(|address| calls.contains(address)))
        {
            return device("settings menu growth does not have the native pointer-vector ABI");
        }
    }
    Ok(found)
}

/// The native insertion sites in the code at `at`: the menu's loop calls
/// the screen lookup once per name and keeps each screen it finds.
fn sites(arm: &Arm, at: u32) -> Result<Vec<Insertion>> {
    let code = arm.code(at, None)?;
    let words: Vec<u32> = code.iter().map(|i| i.word).collect();
    let mut found = Vec::new();
    for (i, ins) in code.iter().enumerate() {
        if ins.name != "bl" || i < 7 {
            continue;
        }
        let Some(lookup) = ins.ops.first().and_then(|o| o.imm()) else { continue };
        let after = &words[i + 1..];
        let before = &words[i - 7..i];
        if after.len() >= 9 && after[0] == 0xe3500000 && after[1] == 0xe58d0008 {
            let object = (after[3] >> 16) & 15;
            if after[2] >> 24 != 0x0a
                || after[3] != 0xe5901074 | (object << 16)
                || after[4] != 0xe5903078 | (object << 16)
                || after[5] != 0xe1510003
                || after[6] >> 24 != 0x0a
                || after[7] != 0xe4810004
                || after[8] != 0xe5801074 | (object << 16)
            {
                continue;
            }
            let target = code[i + 7].ops[0].imm().unwrap();
            let grow = arm.code(target, Some(12))?;
            if grow.len() != 3 || grow[0].word != 0xe59d0004 || grow[1].word != 0xe28d2008 || grow[2].name != "bl" {
                continue;
            }
            found.push(Insertion {
                call: ins.address,
                lookup,
                grow: grow[2].ops[0].imm().unwrap(),
                object_register: object,
                names_register: 0,
                screens_register: 0,
                next_register: 0,
                appends: false,
            });
        } else if after.len() >= 12 && after[0] == 0xe3500000 && after[1] == 0xe58d0000 {
            // An older libstdc++'s push_back: the screen stored at the end
            // when there is room (and the end is not null), appended by
            // _M_emplace_back_aux(vector, &screen) when there is not.
            let object = (after[3] >> 16) & 15;
            if after[2] >> 24 != 0x0a
                || after[3] != 0xe5903074 | (object << 16)
                || after[4] != 0xe5902078 | (object << 16)
                || after[5] != 0xe1530002
                || after[6] >> 24 != 0x0a
                || after[7] != 0xe3530000
                || after[8] != 0x15830000
                || after[9] != 0x15903074 | (object << 16)
                || after[10] != 0xe2833004
                || after[11] != 0xe5803074 | (object << 16)
            {
                continue;
            }
            let target = code[i + 7].ops[0].imm().unwrap();
            let grow = arm.code(target, Some(12))?;
            if grow.len() != 3
                || grow[0].word != 0xe1a0100d
                || grow[1].word != 0xe2800070 | (object << 16)
                || grow[2].name != "bl"
            {
                continue;
            }
            found.push(Insertion {
                call: ins.address,
                lookup,
                grow: grow[2].ops[0].imm().unwrap(),
                object_register: object,
                names_register: 0,
                screens_register: 0,
                next_register: 0,
                appends: true,
            });
        } else if after.len() >= 3 && after[0] & 0xfff0ffff == 0xe280303c && after[2] & 0xfff0ffff == 0xe7800103 {
            let screens = (after[0] >> 16) & 15;
            let object = (after[2] >> 16) & 15;
            let next = after[1] & 15;
            if after[1] != 0xe1a00000 | (screens << 12) | next
                || !before.contains(&(0xe2800001 | (screens << 16) | (next << 12)))
                || !before.contains(&(0xe5900060 | (object << 16)))
            {
                continue;
            }
            for names in 4..12 {
                if before.contains(&(0xe2800001 | (names << 16) | (names << 12)))
                    && before.contains(&(0xe280301c | (names << 16)))
                {
                    found.push(Insertion {
                        call: ins.address,
                        lookup,
                        grow: 0,
                        object_register: object,
                        names_register: names,
                        screens_register: screens,
                        next_register: next,
                        appends: false,
                    });
                }
            }
        }
    }
    Ok(found)
}

/// The menu's loop: of the loops bounded by the list's end, the one holding
/// an insertion site, and its sites. Others (6.0-6 also asks each listed
/// screen in turn) walk the whole list and keep their bound.
fn menu_loop(elf: &Elf, arm: &Arm, list: u32, count: usize) -> Result<(u32, Vec<Insertion>)> {
    let mut found = Vec::new();
    for at in bounds(elf, list, count) {
        let mut sites = sites(arm, at)?;
        // The reviewed 5.6-7 build fills bounded inline name and screen
        // arrays (names at +0x68, screens at +0xd4) in its own loop shape.
        if super::legacy_abi(elf.data) && at == 0x100b88 && sites.is_empty() {
            sites.push(Insertion {
                call: 0x100c54,
                lookup: 0x4d098,
                grow: 0,
                object_register: 5,
                names_register: 8,
                screens_register: 7,
                next_register: 4,
                appends: false,
            });
        }
        if !sites.is_empty() {
            found.push((at, sites));
        }
    }
    match found.len() {
        1 => Ok(found.pop().unwrap()),
        n => device(format!("settings menu loop bound: expected one, found {n}")),
    }
}

/// An entry shown only with its feature on, in the menu's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Zigbee,
    Matter,
    Themes,
    Doom,
}

impl Section {
    /// Every section, in the menu's order.
    pub const ALL: [Section; 4] = [Section::Zigbee, Section::Matter, Section::Themes, Section::Doom];

    /// The 5.6-7 ABI has no reviewed theme preload and holds two optional tiles.
    pub const LEGACY: [Section; 2] = [Section::Zigbee, Section::Matter];

    fn host(self) -> &'static str {
        HOSTS[self as usize]
    }
}

/// The menu's names as nlclient reads them: the stock list without the
/// screens the entries replace, the sections in order after its first
/// entry, and Legal Info's name (UNFR0ZEN) where it was.
pub fn arranged(stock: &[String], sections: &[Section]) -> Result<Vec<String>> {
    arranged_with_abi(stock, sections, false)
}

fn arranged_with_abi(stock: &[String], sections: &[Section], legacy: bool) -> Result<Vec<String>> {
    if legacy && sections.contains(&Section::Themes) {
        return device("5.6-7 has no reviewed theme preload");
    }
    let hosts: &[&str] = if legacy { &LEGACY_HOSTS } else { &HOSTS };
    if stock.first().map(String::as_str) != Some("temperature")
        || stock.last().map(String::as_str) != Some("temperature")
    {
        return device("settings menu does not run between temperature entries");
    }
    for &host in hosts {
        if stock.iter().filter(|n| *n == host).count() != 1 {
            return device(format!("settings menu does not hold {host} once"));
        }
    }
    let mut names: Vec<String> =
        stock.iter().filter(|n| !hosts[..hosts.len() - 1].contains(&n.as_str())).cloned().collect();
    for (at, section) in Section::ALL.iter().filter(|s| sections.contains(s)).enumerate() {
        let name = if legacy && *section == Section::Matter { "proinfoscreen" } else { section.host() };
        names.insert(1 + at, name.to_string());
    }
    Ok(names)
}

/// Rewrites `output`'s settings menu, read from `source`, to show
/// `sections`: the list's address, its loop bound's and the names it shows.
pub fn arrange(source: &[u8], output: &mut [u8], sections: &[Section]) -> Result<serde_json::Value> {
    let elf = Elf::parse(source)?;
    let (address, stock) = discovery::settings(&elf)?;
    let names = arranged_with_abi(&stock, sections, super::legacy_abi(source))?;
    let at = elf.offset(address)?;
    let words: Vec<u32> = (0..stock.len()).map(|i| word(source, at + i * 4)).collect::<Result<_>>()?;
    let (bound, _) = menu_loop(&elf, &Arm::new(&elf)?, address, stock.len())?;
    let bound_at = elf.offset(bound)?;
    for (offset, length) in [(at, stock.len() * 4), (bound_at, 4)] {
        if output.get(offset..offset + length) != source.get(offset..offset + length) {
            return device("settings menu already changed");
        }
    }
    let pointer = |name: &str| words[stock.iter().position(|n| n == name).unwrap()];
    for i in 0..stock.len() {
        // Past the new end: the last entry's name again.
        let name = names.get(i).unwrap_or(&stock[stock.len() - 1]);
        output[at + i * 4..at + i * 4 + 4].copy_from_slice(&pointer(name).to_le_bytes());
    }
    let add = word(source, bound_at)?;
    let shorter = (add & !0xfff) | (names.len() as u32 * 4);
    output[bound_at..bound_at + 4].copy_from_slice(&shorter.to_le_bytes());
    Ok(serde_json::json!({"address": address, "bound": bound, "names": names}))
}

/// The loops' bounds: each `add Rd, Rn, #4*count` (AL, unrotated) whose Rn
/// holds the list's address, or the word before it, loaded from a literal
/// (and on a position-independent build, added to pc) a few instructions
/// before.
fn bounds(elf: &Elf, list: u32, count: usize) -> Vec<u32> {
    let mut found = Vec::new();
    for section in elf.sections.iter().filter(|s| s.kind == 1 && s.flags & 4 != 0) {
        let code = elf.contents(section);
        let at = |i: usize| code.get(i * 4..i * 4 + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
        for i in 0..code.len() / 4 {
            let w = at(i).unwrap();
            // LDR Rt, [pc, #±imm12]
            if w & 0xff7f_0000 != 0xe51f_0000 {
                continue;
            }
            let pc = section.address + i as u32 * 4 + 8;
            let literal = if w & (1 << 23) != 0 { pc.wrapping_add(w & 0xfff) } else { pc.wrapping_sub(w & 0xfff) };
            let Ok(value) = elf.read(literal) else { continue };
            let (mut register, mut base) = ((w >> 12) & 0xf, value);
            for j in i + 1..(i + 16).min(code.len() / 4) {
                let x = at(j).unwrap();
                let address = section.address + j as u32 * 4;
                // ADD Rd, pc, Rm
                if x & 0xffff_0ff0 == 0xe08f_0000 && x & 0xf == register {
                    (register, base) = ((x >> 12) & 0xf, value.wrapping_add(address + 8));
                    continue;
                }
                // ADD Rd, Rn, #imm8 (no rotation)
                if x & 0xfff0_0f00 == 0xe280_0000
                    && (x >> 16) & 0xf == register
                    && x & 0xff == count as u32 * 4
                    && (base == list || base == list.wrapping_sub(4))
                {
                    found.push(address);
                    break;
                }
                // The register written by anything else: no longer the base.
                if (x >> 12) & 0xf == register && x >> 28 == 0xe {
                    break;
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn the_sections_follow_the_first_entry_and_unfr0zen_takes_legal_infos_place() {
        let stock = names(&[
            "temperature",
            "schedule",
            "account",
            "wifi",
            "templock",
            "proinfoscreen",
            "update",
            "systeminfo",
            "legalinfo",
            "reset",
            "temperature",
        ]);
        assert_eq!(
            arranged(&stock, &[Section::Doom, Section::Zigbee]).unwrap(),
            names(&[
                "temperature",
                "account",
                "templock",
                "schedule",
                "wifi",
                "systeminfo",
                "legalinfo",
                "reset",
                "temperature"
            ])
        );
        assert_eq!(
            arranged(&stock, &[]).unwrap(),
            names(&["temperature", "schedule", "wifi", "systeminfo", "legalinfo", "reset", "temperature"])
        );
        assert_eq!(
            arranged(&stock, &[Section::Matter, Section::Themes, Section::Doom, Section::Zigbee]).unwrap()[1..5],
            names(&["account", "update", "proinfoscreen", "templock"])
        );
        let mut missing = stock.clone();
        missing.retain(|n| n != "templock");
        assert!(arranged(&missing, &[]).is_err());
    }
}
