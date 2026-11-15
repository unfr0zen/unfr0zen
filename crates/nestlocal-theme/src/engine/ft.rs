//! client-ng sets text with the unit's FreeType where it can load it; a
//! static binary loads no library, so here there is never a face and
//! fontdue sets every run (typeset.rs's other path).

use std::rc::Rc;

/// A glyph's coverage as FreeType would set it.
pub struct Glyph {
    pub width: usize,
    pub rows: usize,
    pub left: i32,
    pub top: i32,
    pub advance: f32,
    pub coverage: Vec<u8>,
}

/// A FreeType face: never made.
pub struct Face(());

impl Face {
    pub fn new(_bytes: Rc<[u8]>) -> Option<Face> {
        None
    }

    pub fn glyph(&self, _ch: char, _px: f32) -> Option<Glyph> {
        None
    }
}
