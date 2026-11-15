//! What a page shows, laid out on the upright face: lines of text, then the
//! choices, the selected one inverted, all centred inside the face's circle.

use super::font;

pub const WHITE: u32 = 0xffffff;
pub const BLACK: u32 = 0x000000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    /// A code's size: eight digits fill more than 60% of the face's width.
    Large,
    Normal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub lines: Vec<(String, Size)>,
    pub choices: Vec<String>,
    pub selected: usize,
}

/// A line or a choice where it is drawn: its box, and its text at scale,
/// centred in it. A line's box is its text's; a choice's pads it.
#[derive(Debug)]
struct Block {
    text: String,
    scale: usize,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    inverted: bool,
}

/// A character's cell: the glyph and a pixel's gap, at scale 1.
const CELL: usize = font::WIDTH + 1;

/// How wide text is at scale.
fn text_width(characters: usize, scale: usize) -> usize {
    (characters * CELL).saturating_sub(1) * scale
}

/// Normal text's scale: three on the gen 3's 480-pixel face, two on the
/// gen 2's 320.
fn normal_scale(face: usize) -> usize {
    (face / 160).max(1)
}

/// The large scale: eight digits span at least 60% of the face.
fn large_scale(face: usize) -> usize {
    (face * 3 / 5).div_ceil(text_width(8, 1)).max(1)
}

/// Half the widest span centred on the face that stays inside its circle
/// over rows top..bottom.
fn half_chord(face: usize, top: usize, bottom: usize) -> usize {
    let r = face as f64 / 2.0;
    let d = (top as f64 - r).abs().max((bottom as f64 - r).abs());
    if d >= r { 0 } else { (r * r - d * d).sqrt() as usize }
}

/// Where each line, then each choice, is drawn. A block too wide for the
/// circle where it falls is drawn smaller, and at scale 1 cut short; one
/// with no room at all is not drawn.
fn layout(page: &Page, face: usize) -> Vec<Block> {
    let normal = normal_scale(face);
    let (gap, pad_x, pad_y) = (4 * normal, 3 * normal, 2 * normal);
    let lines = page.lines.iter().map(|(text, size)| {
        let scale = match size {
            Size::Large => large_scale(face),
            Size::Normal => normal,
        };
        (text, scale, false, font::HEIGHT * scale)
    });
    let choices = page.choices.iter().map(|text| (text, normal, true, font::HEIGHT * normal + 2 * pad_y));
    let items: Vec<_> = lines.chain(choices).collect();
    let height = items.iter().map(|item| item.3).sum::<usize>() + gap * items.len().saturating_sub(1);
    let mut y = face.saturating_sub(height) / 2;
    let mut blocks = Vec::new();
    for (index, (text, wanted, choice, h)) in items.into_iter().enumerate() {
        let pad = if choice { 2 * pad_x } else { 0 };
        let room = (2 * half_chord(face, y, y + h)).saturating_sub(pad);
        let length = text.chars().count();
        let scale = (1..=wanted).rev().find(|&s| text_width(length, s) <= room).unwrap_or(1);
        let text: String = text.chars().take((room + scale) / (CELL * scale)).collect();
        let w = if text.is_empty() { 0 } else { text_width(text.chars().count(), scale) + pad };
        let inverted = choice && index - page.lines.len() == page.selected;
        blocks.push(Block { text, scale, x: face.saturating_sub(w) / 2, y, w, h, inverted });
        y += h + gap;
    }
    blocks
}

/// The page drawn upright on a face×face square: 0xRRGGBB, row by row.
pub fn render(page: &Page, face: usize) -> Vec<u32> {
    let mut pixels = vec![BLACK; face * face];
    for block in layout(page, face) {
        let ink = if block.inverted {
            for y in block.y..(block.y + block.h).min(face) {
                pixels[y * face + block.x..y * face + (block.x + block.w).min(face)].fill(WHITE);
            }
            BLACK
        } else {
            WHITE
        };
        let s = block.scale;
        let left = block.x + (block.w - text_width(block.text.chars().count(), s)) / 2;
        let top = block.y + (block.h - font::HEIGHT * s) / 2;
        for (i, c) in block.text.chars().enumerate() {
            for column in 0..font::WIDTH {
                for row in 0..font::HEIGHT {
                    if !font::lit(c, column, row) {
                        continue;
                    }
                    let (x, y) = (left + (i * CELL + column) * s, top + row * s);
                    for dy in 0..s {
                        let start = (y + dy) * face + x;
                        pixels[start..start + s].fill(ink);
                    }
                }
            }
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code_page() -> Page {
        Page {
            lines: vec![("12345678".into(), Size::Large), ("192.168.1.50".into(), Size::Normal)],
            choices: Vec::new(),
            selected: 0,
        }
    }

    fn lit_columns(pixels: &[u32], face: usize) -> (usize, usize) {
        let columns: Vec<usize> = (0..face).filter(|&x| (0..face).any(|y| pixels[y * face + x] != 0)).collect();
        (columns[0], columns[columns.len() - 1])
    }

    #[test]
    fn the_code_page_fits_inside_the_round_face_at_both_sizes() {
        for face in [480, 320] {
            let pixels = render(&code_page(), face);
            assert_eq!(pixels.len(), face * face);
            let r = face as f64 / 2.0;
            let mut lit = 0;
            for (i, _) in pixels.iter().enumerate().filter(|(_, &p)| p != 0) {
                lit += 1;
                let (x, y) = ((i % face) as f64 + 0.5 - r, (i / face) as f64 + 0.5 - r);
                assert!(x * x + y * y <= r * r, "face {face}: ({}, {}) is outside the circle", i % face, i / face);
            }
            assert!(lit > 0, "face {face}: nothing drawn");
        }
    }

    #[test]
    fn large_digits_take_most_of_the_width() {
        for face in [480, 320] {
            let page = Page { lines: vec![("12345678".into(), Size::Large)], choices: Vec::new(), selected: 0 };
            let (left, right) = lit_columns(&render(&page, face), face);
            assert!((right - left + 1) * 10 >= face * 6, "face {face}: the digits span {left}..={right}");
        }
    }

    #[test]
    fn the_selected_choice_is_drawn_inverted() {
        let page = Page {
            lines: vec![("Update".into(), Size::Normal)],
            choices: vec!["Allow".into(), "Deny".into()],
            selected: 1,
        };
        let face = 480;
        let blocks = layout(&page, face);
        let (allow, deny) = (&blocks[1], &blocks[2]);
        assert!(!allow.inverted && deny.inverted);
        let pixels = render(&page, face);
        let lit = |b: &Block| (b.y..b.y + b.h).flat_map(|y| (b.x..b.x + b.w).map(move |x| (x, y)))
            .filter(|&(x, y)| pixels[y * face + x] != 0).count();
        // The selected choice is a lit box with dark letters; the other, lit
        // letters on the dark.
        assert!(lit(deny) * 2 > deny.w * deny.h && lit(deny) < deny.w * deny.h);
        assert!(lit(allow) > 0 && lit(allow) * 2 < allow.w * allow.h);
    }
}
