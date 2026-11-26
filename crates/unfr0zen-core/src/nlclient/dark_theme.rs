//! The gen 1's and gen 2's settings screens in the gen 3's dark palette:
//! black backgrounds, white text, the gen 3's blue selection and grey
//! unselected rows. The colours are the ones the gen 3's nlclient sets in the
//! same functions.
use super::elf::{word, Elf};
use crate::{device, shell::sha256_hex, Result};

const R0: u32 = 0;
const R1: u32 = 1;
const R2: u32 = 2;
const R3: u32 = 3;
const R12: u32 = 12;
const NOP: u32 = 0xe1a00000; // mov r0, r0
const WHITE_R0: u32 = 0xe3e004ff; // mvn r0, #0xff000000
const WHITE_R1: u32 = 0xe3e01000; // mvn r1, #0
const WHITE_R2: u32 = 0xe3e024ff; // mvn r2, #0xff000000
const BLACK_R1: u32 = 0xe3a014ff; // mov r1, #0xff000000
const BLACK_R12: u32 = 0xe3a0c4ff; // mov r12, #0xff000000

fn movw(rd: u32, value: u32) -> u32 {
    0xe3000000 | (value >> 12) << 16 | rd << 12 | value & 0xfff
}
fn movt(rd: u32, value: u32) -> u32 {
    0xe3400000 | (value >> 12) << 16 | rd << 12 | value & 0xfff
}
fn store(rd: u32, slot: u32) -> u32 {
    0xe5840000 | rd << 12 | slot
}

/// A colour as the `movw`/`movt` pair at `low` and `high`.
fn colour(low: u32, high: u32, rd: u32, argb: u32) -> [(u32, u32); 2] {
    [(low, movw(rd, argb & 0xffff)), (high, movt(rd, argb >> 16))]
}

/// The layer palette's slots (layer + offset) by role, as the gen 3 sets them.
/// 0x2d7860 sets all of them and then their alpha bytes, so only RGB counts.
fn palette() -> Vec<u32> {
    let mut code = vec![
        0xe1a05001, // mov r5, r1 (the fade, read after the alpha stores)
        WHITE_R0,
        store(R0, 0x28),
        store(R0, 0x34),
        store(R0, 0x40), // text, pill text, field text
        movw(R3, 0xc4c4),
        movt(R3, 0xffc4),
        store(R3, 0x2c), // secondary text
        movw(R3, 0x9999),
        movt(R3, 0xff99),
        store(R3, 0x30),
        movw(R3, 0x6666),
        movt(R3, 0xff66),
        store(R3, 0x38),
        store(R3, 0x54), // unselected, field outline
        movw(R3, 0xa3d9),
        movt(R3, 0xff00),
        store(R3, 0x3c),
        store(R3, 0x50),
        store(R3, 0x5c), // selection, accent
        movw(R3, 0x4157),
        movt(R3, 0xff00),
        store(R3, 0x44), // field being edited
        BLACK_R12,
        store(R12, 0x48),
        store(R12, 0x4c),
        store(R12, 0x58), // field fills, dialog
        movw(R3, 0xe0e0),
        movt(R3, 0xffe0),
        store(R3, 0x60),
    ];
    code.resize(31, NOP);
    code
}

/// The same palette from the nlLayer constructor, which also clears its other
/// fields here and returns `this` in r0; r1 is 0 and r2 is 1 on entry.
fn constructor() -> Vec<u32> {
    vec![
        store(R2, 0x68),
        store(R1, 0x14),
        store(R1, 0x10),
        store(R1, 0x18),
        store(R1, 0x64),
        store(R1, 0x1c),
        store(R1, 0x6c),
        store(R1, 0x24),
        BLACK_R12,
        store(R12, 0x48),
        store(R12, 0x58),
        WHITE_R1,
        store(R1, 0x28),
        store(R1, 0x34),
        store(R1, 0x40),
        movw(R3, 0xc4c4),
        movt(R3, 0xffc4),
        store(R3, 0x2c),
        movw(R3, 0x9999),
        movt(R3, 0xff99),
        store(R3, 0x30),
        movw(R3, 0x6666),
        movt(R3, 0xff66),
        store(R3, 0x38),
        store(R3, 0x54),
        movw(R3, 0xa3d9),
        movt(R3, 0xff00),
        store(R3, 0x3c),
        store(R3, 0x50),
        movw(R3, 0x4157),
        movt(R3, 0xff00),
        store(R3, 0x44),
    ]
}

/// Find a palette operation by its stock instructions and surrounding context.
/// Only the displacement of a direct branch varies with the link address.
/// The key is the first word to edit, two words into the matched region.
fn regions(elf: &Elf, key: u32, words: usize, digest: &str) -> Result<Vec<u32>> {
    let text = elf.section(".text")?;
    let mut matches = Vec::new();
    for (i, bytes) in elf.contents(text).windows(words * 4).step_by(4).enumerate() {
        if word(bytes, 8)? != key {
            continue;
        }
        let mut normalized = Vec::with_capacity(bytes.len());
        for &instruction in bytes.as_chunks::<4>().0 {
            let mut w = u32::from_le_bytes(instruction);
            if w & 0x0e000000 == 0x0a000000 {
                w &= 0xff000000;
            }
            normalized.extend(w.to_le_bytes());
        }
        if sha256_hex(&normalized) == digest {
            matches.push(text.address + (i * 4 + 8) as u32);
        }
    }
    Ok(matches)
}

// Screen entry methods load the light background into r1 and pass it to
// SetColor, a setter for the background layer's ARGB field.
fn backgrounds(elf: &Elf, carousel: u32) -> Result<Vec<(u32, u32)>> {
    let text = elf.section(".text")?;
    let arm = super::arm::Arm::new(elf)?;
    let setter = arm.code(carousel + 8, Some(4))?;
    let setter = setter
        .first()
        .filter(|i| i.name == "bl")
        .and_then(|i| i.ops[0].imm())
        .ok_or_else(|| crate::Error::Device("dark theme: missing carousel background setter".into()))?;
    if !arm.body(setter)?.iter().map(|i| i.word).eq([0xe1a0c00d, 0xe92dd800, 0xe24cb004, 0xe580109c, 0xe89da800]) {
        return device("dark theme: unknown background colour setter");
    }
    let mut found = Vec::new();
    for (i, &w) in elf.contents(text).as_chunks::<4>().0.iter().enumerate() {
        if u32::from_le_bytes(w) != movw(R1, 0xf1e7) {
            continue;
        }
        let low = text.address + i as u32 * 4;
        let code = arm.code(low, None)?;
        let mut high = None;
        for ins in code.iter().skip(1) {
            if high.is_none() && ins.word == movt(R1, 0xffef) {
                high = Some(ins.address);
                continue;
            }
            if matches!(ins.name.as_str(), "bl" | "b") {
                if let (Some(high), Some(target)) = (high, ins.ops[0].imm()) {
                    if target == setter {
                        found.extend(colour(low, high, R1, 0xff000000));
                    }
                }
                break;
            }
            if ins.name.starts_with('b') || ins.writes.iter().any(|r| r == "r1" || r == "pc") {
                break;
            }
        }
    }
    if found.is_empty() {
        return device("dark theme: no light screen backgrounds");
    }
    Ok(found)
}

fn edits(elf: &Elf) -> Result<Vec<(u32, u32)>> {
    let one = |role: &str, key, words, digest: &str| -> Result<u32> {
        let found = regions(elf, key, words, digest)?;
        match found.as_slice() {
            [address] => Ok(*address),
            _ => device(format!("dark theme: expected one {role}, found {}", found.len())),
        }
    };
    // The carousel's tiles: the gen 3's lighter blue.
    let carousel = one("carousel", 0xe3061380, 6, "983891ada931fe4998344fc819e9022c71a32e8cc45035f15632c56545a2a785")?;
    let mut edits = backgrounds(elf, carousel)?;
    edits.extend(colour(carousel, carousel + 0x4, R1, 0xff00a1f2));
    // Equipment and Proview fade their background in from computed bytes:
    // the colour is cleared and only the alpha byte is inserted.
    let fades = regions(elf, 0xe7d71812, 9, "c1028b156a1d8ce8ab0314378a75521666a2d16736bac6080f73af790c1dcecd")?;
    if fades.len() != 2 {
        return device(format!("dark theme: expected Equipment and Proview fades, found {}", fades.len()));
    }
    for first in fades {
        edits.extend([(first, 0xe3a01000), (first + 8, NOP), (first + 16, NOP)]);
    }
    let palette_at =
        one("palette", 0xe3053050, 35, "f495cf76037a231a13238bbe96e95d16400fe0d42641a0d129258580002367d3")?;
    edits.extend(palette().into_iter().enumerate().map(|(i, code)| (palette_at + 4 * i as u32, code)));
    let constructor_at =
        one("constructor", 0xe3093999, 35, "2fd9e8a6e07c313adf51be694919c6df03c995b45ca0c1181bbb887cabbc448c")?;
    edits.extend(constructor().into_iter().enumerate().map(|(i, code)| (constructor_at + 4 * i as u32, code)));
    // The letter wheel's selected-letter bubble.
    let letter = one("letter", 0xe30b31c0, 6, "7576bfa90969d7d3fa5135852b1b8e4ff19370d1669bb54929317ba19839572d")?;
    edits.extend(colour(letter, letter + 0x4, R3, 0xff00a3d9));
    // Equipment detection sets its title to black after the palette.
    let equipment_title =
        one("equipment title", 0xe3a024ff, 5, "0e35c665a366a8fc0676c4fb2547e013f467789336e5a615d62ba0d82858f1e3")?;
    edits.push((equipment_title, WHITE_R2));
    // The message screen's list: text, secondary text, pill text, pill and
    // unselected rows.
    let message = one("message", 0xe3a024ff, 28, "0d06a26e1e744728e5c69a6e733bbb54c050b9442c62187f8b74d9396582d463")?;
    edits.push((message, WHITE_R2));
    edits.extend(colour(message + 0x30, message + 0x38, R2, 0xffc4c4c4));
    edits.extend(colour(message + 0x3c, message + 0x44, R3, 0xffffffff));
    edits.extend(colour(message + 0x48, message + 0x50, R2, 0xff00a3d9));
    edits.extend(colour(message + 0x54, message + 0x5c, R3, 0xff666666));
    // The schedule's New/Cancel/Done menu: white text on the gen 3's blue.
    let schedule = one("schedule", 0xe30f31e7, 16, "44815dfda80ab9665c6246e67eff47227553342b8eb497cfd553f81807ee6ef9")?;
    edits.extend(colour(schedule, schedule + 0x14, R3, 0xffffffff));
    edits.extend(colour(schedule + 0x24, schedule + 0x2c, R2, 0xff00a3d9));
    // The carousel tiles' title, white, and subtitle, light blue.
    let title = one("title", 0xe30f01e7, 12, "d5ee5eefed112a0a14eebf654b52128119712bfc92c2cffcedb8e11fe8d22907")?;
    edits.extend(colour(title, title + 0x4, R0, 0xffffffff));
    edits.extend(colour(title + 0x18, title + 0x1c, R0, 0xffbfeaff));
    let subtitle = one("subtitle", 0xe30f01e7, 6, "1eb3f730048bb49f39d95134aec49160ce194ebf3f2556f51e95ddb872b14a41")?;
    edits.extend(colour(subtitle, subtitle + 0x4, R0, 0xffbfeaff));
    // The wiring diagram's selected terminal.
    let wiring = one("wiring", 0xe3060380, 7, "4f349d84d22a6714d2f244e3a1797e3383f1cbd71925698ff3878e976e6d11a5")?;
    edits.extend(colour(wiring, wiring + 0x8, R0, 0xff00a3d9));
    // The progress ring: a black track, and its two blues in the gen 3's order.
    let progress = one("progress", 0xe30fc1e7, 47, "c5c86600b7a275d77db1bf132ca914f45fd94219421667b77be232e180b3b831")?;
    edits.extend(colour(progress, progress + 0x4, R12, 0xff000000));
    edits.push((progress + 0x60, movw(R3, 0x7cec)));
    edits.push((progress + 0xa8, movw(R12, 0xa3d9)));
    // Nest Pro's information colours.
    let pro = one("pro", 0xe3a01cbb, 8, "6f501ad6ab997876960dd6308b70c72352671c42cd247af44ecf09a585bd105a")?;
    edits.extend(colour(pro, pro + 0x8, R1, 0xff30b4e9));
    edits.extend(colour(pro + 0x4, pro + 0xc, R3, 0xff103b4d));
    // nlChapterLayer's unselected rows keep the palette's grey.
    let chapter = one("chapter", 0xe30b38b8, 8, "fff7b29cc2ffad696f412b89347df961130c9db36c4d6388d2b4cd91b63da3f5")?;
    edits.extend(colour(chapter, chapter + 0xc, R3, 0xff666666));
    // The disc behind the boot logo and the restarting message.
    let boot = one("boot", 0xe1a01005, 5, "2011ee9107ccb91e3322abce68808b5c54036b265531fdbce5996f83a21dcded")?;
    edits.push((boot, BLACK_R1));
    Ok(edits)
}

/// Whether the executable contains the supported stock palette operations.
pub fn reviewed(stock: &[u8]) -> bool {
    Elf::parse(stock).and_then(|elf| edits(&elf)).is_ok()
}

/// Validate every edit against the screen builder's output before writing.
pub fn patch(stock: &[u8], output: &mut [u8]) -> Result<()> {
    let elf = Elf::parse(stock)?;
    let mut writes = Vec::new();
    for (address, after) in edits(&elf)? {
        let offset = elf.offset(address)?;
        let before = word(stock, offset)?;
        if before == after || word(output, offset)? != before {
            return device("the dark theme patch site changed");
        }
        writes.push((offset, after));
    }
    for (offset, after) in writes {
        output[offset..offset + 4].copy_from_slice(&after.to_le_bytes());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_firmware_is_rejected_without_changing_output() {
        let mut output = b"linked screen".to_vec();
        assert!(!reviewed(b"unknown firmware"));
        assert!(patch(b"unknown firmware", &mut output).is_err());
        assert_eq!(output, b"linked screen");
    }

    #[test]
    fn rewritten_palettes_fill_their_stock_regions_exactly() {
        // 0x2d7878..=0x2d78f0 and 0x2d7aec..=0x2d7b68.
        assert_eq!(palette().len(), 31);
        assert_eq!(constructor().len(), 32);
        assert_eq!(palette()[0], 0xe1a05001);
    }

    #[test]
    fn instruction_encodings_match_the_assembler() {
        assert_eq!(movw(R1, 0xf1e7), 0xe30f11e7);
        assert_eq!(movt(R1, 0xffef), 0xe34f1fef);
        assert_eq!(store(R3, 0x2c), 0xe584302c);
    }
}
