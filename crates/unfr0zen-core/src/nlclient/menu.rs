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
//! the whole list.
use super::{
    discovery,
    elf::{word, Elf},
};
use crate::{device, Result};

/// The stock screens the entries take the place of, in the order of
/// [`Section`], then UNFR0ZEN's.
pub const HOSTS: [&str; 5] = ["account", "update", "proinfoscreen", "templock", "legalinfo"];

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

    fn host(self) -> &'static str {
        HOSTS[self as usize]
    }
}

/// The menu's names as nlclient reads them: the stock list without the
/// screens the entries replace, the sections in order after its first
/// entry, and Legal Info's name (UNFR0ZEN) where it was.
pub fn arranged(stock: &[String], sections: &[Section]) -> Result<Vec<String>> {
    if stock.first().map(String::as_str) != Some("temperature")
        || stock.last().map(String::as_str) != Some("temperature")
    {
        return device("settings menu does not run between temperature entries");
    }
    for host in HOSTS {
        if stock.iter().filter(|n| *n == host).count() != 1 {
            return device(format!("settings menu does not hold {host} once"));
        }
    }
    let mut names: Vec<String> = stock.iter().filter(|n| !HOSTS[..4].contains(&n.as_str())).cloned().collect();
    for (at, section) in Section::ALL.iter().filter(|s| sections.contains(s)).enumerate() {
        names.insert(1 + at, section.host().to_string());
    }
    Ok(names)
}

/// Rewrites `output`'s settings menu, read from `source`, to show
/// `sections`: the list's address, its loop bound's and the names it shows.
pub fn arrange(source: &[u8], output: &mut [u8], sections: &[Section]) -> Result<serde_json::Value> {
    let elf = Elf::parse(source)?;
    let (address, stock) = discovery::settings(&elf)?;
    let names = arranged(&stock, sections)?;
    let at = elf.offset(address)?;
    let words: Vec<u32> = (0..stock.len()).map(|i| word(source, at + i * 4)).collect::<Result<_>>()?;
    let bound = bound(&elf, address, stock.len())?;
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

/// The loop's bound: the one `add Rd, Rn, #4*count` (AL, unrotated) whose
/// Rn holds the list's address, or the word before it, loaded from a literal
/// (and on a position-independent build, added to pc) a few instructions
/// before.
fn bound(elf: &Elf, list: u32, count: usize) -> Result<u32> {
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
    match found.as_slice() {
        [one] => Ok(*one),
        _ => device(format!("settings menu loop bound: expected one, found {}", found.len())),
    }
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
