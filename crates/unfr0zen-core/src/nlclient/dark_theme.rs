//! Gen 2's settings screens in the gen 3's dark palette: black backgrounds,
//! white text, the gen 3's blue selection and grey unselected rows. The colours
//! are the ones the gen 3's nlclient sets in the same functions.
use super::elf::{word, Elf};
use crate::{device, shell::sha256_hex, Result};

const STOCK_SHA: &str = "67596bb71d0225a3b54966e96cc19bc469c0e593ee72e37bfe96fc30a3741bb7";

/// Each light screen's enter passes `0xffeff1e7` to the background layer's
/// SetColor (0x17d838) as a `movw`/`movt r1` pair.
const BACKGROUNDS: [(u32, u32); 32] = [
    (0xa5f78, 0xa5f7c), (0xb0990, 0xb0994), (0xc53b4, 0xc53b8), (0xcbec8, 0xcbecc),
    (0xd2368, 0xd236c), (0xd51a8, 0xd51ac), (0xdcc20, 0xdcc24), (0xdd750, 0xdd754),
    (0xe16b0, 0xe16b4), (0xe3d6c, 0xe3d70), (0xe6af4, 0xe6af8), (0xe9a40, 0xe9a44),
    (0xea09c, 0xea0a4), (0xfbcec, 0xfbcf0), (0x1018e0, 0x1018e4), (0x102fc0, 0x102fc4),
    (0x116d70, 0x116d74), (0x116e50, 0x116e54), (0x119894, 0x119898), (0x123a44, 0x123a4c),
    (0x12da28, 0x12da2c), (0x135e20, 0x135e24), (0x144bdc, 0x144be0), (0x14fb60, 0x14fb64),
    (0x152de4, 0x152de8), (0x153530, 0x153534), (0x15eecc, 0x15eed0), (0x16d0c4, 0x16d0c8),
    (0x174dc0, 0x174dc4), (0x193508, 0x19350c), (0x194740, 0x194744), (0x19a540, 0x19a544),
];

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

fn movw(rd: u32, value: u32) -> u32 { 0xe3000000 | (value >> 12) << 16 | rd << 12 | value & 0xfff }
fn movt(rd: u32, value: u32) -> u32 { 0xe3400000 | (value >> 12) << 16 | rd << 12 | value & 0xfff }
fn store(rd: u32, slot: u32) -> u32 { 0xe5840000 | rd << 12 | slot }

/// A colour as the `movw`/`movt` pair at `low` and `high`.
fn colour(low: u32, high: u32, rd: u32, argb: u32) -> [(u32, u32); 2] {
    [(low, movw(rd, argb & 0xffff)), (high, movt(rd, argb >> 16))]
}

/// The layer palette's slots (layer + offset) by role, as the gen 3 sets them.
/// 0x2d7860 sets all of them and then their alpha bytes, so only RGB counts.
fn palette() -> Vec<u32> {
    let mut code = vec![
        0xe1a05001, // mov r5, r1 (the fade, read after the alpha stores)
        WHITE_R0, store(R0, 0x28), store(R0, 0x34), store(R0, 0x40), // text, pill text, field text
        movw(R3, 0xc4c4), movt(R3, 0xffc4), store(R3, 0x2c), // secondary text
        movw(R3, 0x9999), movt(R3, 0xff99), store(R3, 0x30),
        movw(R3, 0x6666), movt(R3, 0xff66), store(R3, 0x38), store(R3, 0x54), // unselected, field outline
        movw(R3, 0xa3d9), movt(R3, 0xff00), store(R3, 0x3c), store(R3, 0x50), store(R3, 0x5c), // selection, accent
        movw(R3, 0x4157), movt(R3, 0xff00), store(R3, 0x44), // field being edited
        BLACK_R12, store(R12, 0x48), store(R12, 0x4c), store(R12, 0x58), // field fills, dialog
        movw(R3, 0xe0e0), movt(R3, 0xffe0), store(R3, 0x60),
    ];
    code.resize(31, NOP);
    code
}

/// The same palette from the nlLayer constructor, which also clears its other
/// fields here and returns `this` in r0; r1 is 0 and r2 is 1 on entry.
fn constructor() -> Vec<u32> {
    vec![
        store(R2, 0x68),
        store(R1, 0x14), store(R1, 0x10), store(R1, 0x18), store(R1, 0x64),
        store(R1, 0x1c), store(R1, 0x6c), store(R1, 0x24),
        BLACK_R12, store(R12, 0x48), store(R12, 0x58),
        WHITE_R1, store(R1, 0x28), store(R1, 0x34), store(R1, 0x40),
        movw(R3, 0xc4c4), movt(R3, 0xffc4), store(R3, 0x2c),
        movw(R3, 0x9999), movt(R3, 0xff99), store(R3, 0x30),
        movw(R3, 0x6666), movt(R3, 0xff66), store(R3, 0x38), store(R3, 0x54),
        movw(R3, 0xa3d9), movt(R3, 0xff00), store(R3, 0x3c), store(R3, 0x50),
        movw(R3, 0x4157), movt(R3, 0xff00), store(R3, 0x44),
    ]
}

/// Every word replaced, as (address, after). The stock word at each address
/// is checked before any is written.
fn edits() -> Vec<(u32, u32)> {
    let mut edits = Vec::new();
    for (low, high) in BACKGROUNDS {
        edits.extend(colour(low, high, R1, 0xff000000));
    }
    // The carousel's tiles: the gen 3's lighter blue.
    edits.extend(colour(0x1139e0, 0x1139e4, R1, 0xff00a1f2));
    // Equipment and Proview fade their background in from computed bytes:
    // the colour is cleared and only the alpha byte is inserted.
    for first in [0xbd50c, 0xfc4e4] {
        edits.extend([(first, 0xe3a01000), (first + 8, NOP), (first + 16, NOP)]); // mov r1, #0
    }
    edits.extend(palette().into_iter().enumerate().map(|(i, code)| (0x2d7878 + 4 * i as u32, code)));
    edits.extend(constructor().into_iter().enumerate().map(|(i, code)| (0x2d7aec + 4 * i as u32, code)));
    // The letter wheel's selected-letter bubble.
    edits.extend(colour(0xb41ac, 0xb41b0, R3, 0xff00a3d9));
    // Equipment detection sets its title to black after the palette.
    edits.push((0xbb670, WHITE_R2));
    // The message screen's list: text, secondary text, pill text, pill and
    // unselected rows.
    edits.push((0xe3e60, WHITE_R2));
    edits.extend(colour(0xe3e90, 0xe3e98, R2, 0xffc4c4c4));
    edits.extend(colour(0xe3e9c, 0xe3ea4, R3, 0xffffffff));
    edits.extend(colour(0xe3ea8, 0xe3eb0, R2, 0xff00a3d9));
    edits.extend(colour(0xe3eb4, 0xe3ebc, R3, 0xff666666));
    // The schedule's New/Cancel/Done menu: white text on the gen 3's blue.
    edits.extend(colour(0x1075ec, 0x107600, R3, 0xffffffff));
    edits.extend(colour(0x107610, 0x107618, R2, 0xff00a3d9));
    // The carousel tiles' title, white, and subtitle, light blue.
    edits.extend(colour(0x113014, 0x113018, R0, 0xffffffff));
    edits.extend(colour(0x11302c, 0x113030, R0, 0xffbfeaff));
    edits.extend(colour(0x1136d8, 0x1136dc, R0, 0xffbfeaff));
    // The wiring diagram's selected terminal.
    edits.extend(colour(0x165700, 0x165708, R0, 0xff00a3d9));
    // The progress ring: a black track, and its two blues in the gen 3's order.
    edits.extend(colour(0x2d9e78, 0x2d9e7c, R12, 0xff000000));
    edits.push((0x2d9ed8, movw(R3, 0x7cec)));
    edits.push((0x2d9f20, movw(R12, 0xa3d9)));
    // Nest Pro's information colours.
    edits.extend(colour(0xf93a8, 0xf93b0, R1, 0xff30b4e9));
    edits.extend(colour(0xf93ac, 0xf93b4, R3, 0xff103b4d));
    // nlChapterLayer's unselected rows keep the palette's grey.
    edits.extend(colour(0x18c63c, 0x18c648, R3, 0xff666666));
    // The disc behind the boot logo and the restarting message.
    edits.push((0x40648, BLACK_R1));
    edits
}

/// Whether `stock` is the nlclient the fixed addresses were taken from.
pub fn reviewed(stock: &[u8]) -> bool {
    sha256_hex(stock) == STOCK_SHA
}

/// Apply the dark palette to the screen builder's output. The stock hash
/// guards the fixed addresses; unknown builds fail closed, and an output whose
/// words at these addresses differ from stock is refused before any write.
pub fn patch(stock: &[u8], output: &mut [u8]) -> Result<()> {
    if !reviewed(stock) {
        return device("j49 dark theme requires the reviewed nlclient 5.9.4-5 binary");
    }
    let elf = Elf::parse(stock)?;
    let mut writes = Vec::new();
    for (address, after) in edits() {
        let offset = elf.offset(address)?;
        let before = word(stock, offset)?;
        if before == after || word(output, offset)? != before {
            return device("j49 dark theme patch site changed");
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

    #[test]
    fn every_site_is_edited_once() {
        let mut addresses: Vec<u32> = edits().iter().map(|&(address, _)| address).collect();
        addresses.sort();
        addresses.dedup();
        assert_eq!(addresses.len(), edits().len());
    }

    #[test]
    fn edits_match_the_independent_golden() {
        let golden: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../platform/common/nlclient/tests/dark-theme/j49-5.9.4-5.json")).unwrap();
        assert_eq!(STOCK_SHA, golden["stock_sha256"].as_str().unwrap());
        let hex = |value: &serde_json::Value| u32::from_str_radix(value.as_str().unwrap().trim_start_matches("0x"), 16).unwrap();
        let expected: Vec<(u32, u32)> = golden["edits"].as_array().unwrap().iter()
            .map(|edit| (hex(&edit[0]), hex(&edit[2]))).collect();
        let mut emitted = edits();
        emitted.sort();
        let mut expected = expected;
        expected.sort();
        assert_eq!(emitted, expected);
    }
}
