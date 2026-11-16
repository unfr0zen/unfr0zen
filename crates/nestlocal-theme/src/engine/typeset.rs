//! Text set on the CPU as a grey-scale bitmap per run of text, which the
//! painters draw as one tinted image (one quad a frame on the GPU).
//!
//! Where FreeType is (`ft`: the unit's own, which stock nlclient sets its
//! text with) runs are set as stock's are, matched on nest-qemu to the
//! pixel: each glyph unhinted at a whole number of pixels a em (FreeType
//! rounds the size), its coverage made at a whole pixel and laid at its
//! pen's fraction of one as a GPU samples a texture there (bilinearly),
//! one over the other; pens advance by FreeType's unrounded advances and
//! the letter spacing, unkerned, from the whole pixel nearest its start the
//! painters start the run on, as stock's start (or, for a text stock does not
//! start on one, the fraction of a pixel it starts at). Elsewhere fontdue sets them, with
//! stock's look approximated: glyphs on whole pixels, coverage lifted and
//! smeared.

use fontdue::FontSettings;
use log::{debug, warn};

/// Coverage is raised to this power and each pixel takes this share of
/// its left neighbour's: stock's text has its edges lifted and its stems a
/// third of a pixel heavier on the right than plain coverage gives.
const GAMMA: f32 = 0.8;
const SMEAR: f32 = 0.33;

fn lift(a: u8) -> u8 {
    (255.0 * (a as f32 / 255.0).powf(GAMMA)).round() as u8
}

/// A run of text as coverage, one byte a pixel, and where it sits: `left`
/// pixels right of the pen's start and `top` pixels above the baseline.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub width: usize,
    pub height: usize,
    pub left: i32,
    pub top: i32,
    /// Where the pen ends, in pixels.
    pub advance: f32,
    pub alpha: Vec<u8>,
}

/// A face: fontdue's, for metrics and where FreeType is not, and FreeType's
/// where it is.
pub struct Font {
    fd: fontdue::Font,
    ft: Option<super::ft::Face>,
}

impl Font {
    pub fn lookup_glyph_index(&self, ch: char) -> u16 {
        self.fd.lookup_glyph_index(ch)
    }

    fn horizontal_line_metrics(&self, px: f32) -> Option<fontdue::LineMetrics> {
        self.fd.horizontal_line_metrics(px)
    }

    /// Whether FreeType sets this face's text, which can then start at a
    /// fraction of a pixel.
    pub fn subpixel(&self) -> bool {
        self.ft.is_some()
    }
}

/// Bundled with the binary, so the fallback face can always draw.
const FALLBACK_FONT: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/fonts/Inter-Regular.ttf"));

/// A theme's fonts by name, with the fallback. A file named more than once
/// is loaded once: a parsed face holds every glyph's outline, megabytes the
/// gen 2's 64 MB cannot spare.
pub struct Fonts {
    named: std::collections::HashMap<String, std::rc::Rc<Font>>,
    fallback: std::rc::Rc<Font>,
    /// Names asked for that the theme does not have, warned of once each.
    missing: std::cell::RefCell<std::collections::HashSet<String>>,
}

impl Fonts {
    pub fn new() -> Result<Fonts, String> {
        Ok(Fonts {
            named: std::collections::HashMap::new(),
            fallback: std::rc::Rc::new(load(FALLBACK_FONT).map_err(|e| format!("fallback font: {e}"))?),
            missing: Default::default(),
        })
    }

    /// Loads a theme's fonts, replacing the previous theme's names.
    pub fn use_theme(&mut self, fonts: &[(String, std::path::PathBuf)]) -> Result<(), String> {
        let mut by_bytes: Vec<(Vec<u8>, std::rc::Rc<Font>)> = vec![(FALLBACK_FONT.to_vec(), self.fallback.clone())];
        let mut named = std::collections::HashMap::new();
        for (name, file) in fonts {
            let bytes = std::fs::read(file).map_err(|e| format!("font {name} ({}): {e}", file.display()))?;
            let font = match by_bytes.iter().find(|(b, _)| *b == bytes) {
                Some((_, font)) => {
                    debug!("font: {name} ({}) shares a face already loaded", file.display());
                    font.clone()
                }
                None => {
                    let font =
                        std::rc::Rc::new(load(&bytes).map_err(|e| format!("font {name} ({}): {e}", file.display()))?);
                    debug!(
                        "font: {name} ({}, {} bytes) loaded, set by {}",
                        file.display(),
                        bytes.len(),
                        if font.subpixel() { "FreeType" } else { "fontdue" }
                    );
                    by_bytes.push((bytes, font.clone()));
                    font
                }
            };
            named.insert(name.clone(), font);
        }
        self.named = named;
        self.missing.borrow_mut().clear();
        Ok(())
    }

    /// The face of that name, or the fallback.
    pub fn get(&self, name: &str) -> &Font {
        self.named.get(name).unwrap_or_else(|| {
            if self.missing.borrow_mut().insert(name.to_string()) {
                warn!("font: {name:?} is not the theme's; drawing with the fallback");
            }
            &self.fallback
        })
    }

    /// client-ng's CJK face, for texts its translations give: the face's
    /// words are English, so there is none.
    pub fn cjk(&self, _text: &str) -> Option<&Font> {
        None
    }
}

pub fn load(bytes: &[u8]) -> Result<Font, String> {
    let fd = fontdue::Font::from_bytes(bytes, FontSettings::default()).map_err(str::to_string)?;
    let ft = if cfg!(test) { None } else { super::ft::Face::new(std::rc::Rc::from(bytes)) };
    Ok(Font { fd, ft })
}

/// The font a character is drawn with: `font`, or `extra` (the CJK
/// fallback) for a character only it has.
fn pick<'a>(font: &'a Font, extra: Option<&'a Font>, ch: char) -> &'a Font {
    match extra {
        Some(e) if font.lookup_glyph_index(ch) == 0 && e.lookup_glyph_index(ch) != 0 => e,
        _ => font,
    }
}

/// Each glyph's pen position and font, spaced (and kerned by fontdue), and
/// where the pen ends.
fn pens<'a>(
    cache: &mut Glyphs,
    font: &'a Font,
    extra: Option<&'a Font>,
    text: &str,
    px: f32,
    spacing: f32,
) -> (Vec<(char, f32, &'a Font)>, f32) {
    let mut pen = 0.0;
    let mut out = Vec::new();
    let mut prev: Option<(char, &Font)> = None;
    for ch in text.chars() {
        let f = pick(font, extra, ch);
        if f.ft.is_none() {
            // Kerning only within one font.
            if let Some((p, _)) = prev.filter(|(_, pf)| std::ptr::eq(*pf, f)) {
                pen += f.fd.horizontal_kern(p, ch, px).unwrap_or(0.0);
            }
        }
        out.push((ch, pen, f));
        pen += match &f.ft {
            Some(_) => cache.ft(f, ch, px).map_or(0.0, |g| g.advance),
            None => f.fd.metrics(ch, px).advance_width,
        } + spacing;
        prev = Some((ch, f));
    }
    if !out.is_empty() {
        pen -= spacing;
    }
    (out, pen)
}

/// How far the pen moves over `text`.
pub fn advance(cache: &mut Glyphs, font: &Font, extra: Option<&Font>, text: &str, px: f32, spacing: f32) -> f32 {
    pens(cache, font, extra, text, px, spacing).1
}

/// The font's ascent and descent at `px`, both as distances from the baseline.
pub fn line(font: &Font, px: f32) -> (f32, f32) {
    font.horizontal_line_metrics(px).map_or((px * 0.8, px * 0.2), |m| (m.ascent, -m.descent))
}

/// A glyph's placement and coverage, shared by the runs that use it.
type Glyph = std::rc::Rc<(fontdue::Metrics, Vec<u8>)>;

/// Rasterised glyphs, by font, character and size: a run of new text is
/// mostly glyphs already set.
#[derive(Default)]
pub struct Glyphs {
    cache: std::collections::HashMap<(usize, char, u32), Glyph>,
    ft: std::collections::HashMap<(usize, char, u32), Option<std::rc::Rc<super::ft::Glyph>>>,
}

impl Glyphs {
    fn get(&mut self, font: &Font, ch: char, px: f32) -> Glyph {
        let key = (font as *const Font as usize, ch, px.to_bits());
        self.cache.entry(key).or_insert_with(|| std::rc::Rc::new(font.fd.rasterize(ch, px))).clone()
    }

    fn ft(&mut self, font: &Font, ch: char, px: f32) -> Option<std::rc::Rc<super::ft::Glyph>> {
        let key = (font as *const Font as usize, ch, px.to_bits());
        self.ft
            .entry(key)
            .or_insert_with(|| font.ft.as_ref().and_then(|f| f.glyph(ch, px)).map(std::rc::Rc::new))
            .clone()
    }

    /// Forgets every glyph, as when the fonts change.
    pub fn clear(&mut self) {
        self.cache.clear();
        self.ft.clear();
    }
}

#[cfg(test)]
pub fn rasterize(font: &Font, text: &str, px: f32, spacing: f32) -> Run {
    rasterize_with(&mut Glyphs::default(), font, None, text, px, spacing)
}

pub fn rasterize_with(cache: &mut Glyphs, font: &Font, extra: Option<&Font>, text: &str, px: f32, spacing: f32) -> Run {
    rasterize_at(cache, font, extra, text, px, spacing, (0.0, 0.0))
}

/// The run, its pen starting `frac` (each 0 to 1) of a pixel right of and
/// below a whole pixel, where FreeType sets it.
pub fn rasterize_at(
    cache: &mut Glyphs,
    font: &Font,
    extra: Option<&Font>,
    text: &str,
    px: f32,
    spacing: f32,
    frac: (f32, f32),
) -> Run {
    let (pens, advance) = pens(cache, font, extra, text, px, spacing);
    if pens.iter().any(|(_, _, f)| f.ft.is_some()) {
        return set_like_stock(cache, &pens, px, advance, frac);
    }
    let glyphs: Vec<_> = pens
        .iter()
        .map(|&(ch, pen, f)| {
            let glyph = cache.get(f, ch, px);
            let m = glyph.0;
            (pen.round() as i32 + m.xmin, m.ymin + m.height as i32, m, glyph)
        })
        .filter(|(_, _, m, _)| m.width > 0 && m.height > 0)
        .collect();
    let Some(left) = glyphs.iter().map(|g| g.0).min() else {
        return Run { width: 0, height: 0, left: 0, top: 0, advance, alpha: Vec::new() };
    };
    let right = glyphs.iter().map(|(x, _, m, _)| x + m.width as i32).max().unwrap_or(left);
    let top = glyphs.iter().map(|g| g.1).max().unwrap_or(0);
    let bottom = glyphs.iter().map(|(_, t, m, _)| t - m.height as i32).min().unwrap_or(0);
    // One column more on the right for the smear.
    let (width, height) = ((right - left) as usize + 1, (top - bottom) as usize);
    let mut alpha = vec![0u8; width * height];
    for (x, t, m, glyph) in &glyphs {
        let (ox, oy) = ((x - left) as usize, (top - t) as usize);
        let bitmap = &glyph.1;
        for row in 0..m.height {
            for col in 0..m.width {
                let a = &mut alpha[(oy + row) * width + ox + col];
                *a = a.saturating_add(lift(bitmap[row * m.width + col]));
            }
        }
    }
    for row in alpha.chunks_mut(width) {
        for x in (1..width).rev() {
            row[x] = (row[x] as f32 + SMEAR * row[x - 1] as f32).min(255.0) as u8;
        }
    }
    Run { width, height, left, top, advance, alpha }
}

/// Stock's run (see the module's notes): each glyph's whole-pixel coverage
/// laid bilinearly at its pen's fraction, one over the other.
fn set_like_stock(cache: &mut Glyphs, pens: &[(char, f32, &Font)], px: f32, advance: f32, (fx, fy): (f32, f32)) -> Run {
    // Each glyph's coverage and its top-left in run pixels from the pen's
    // start (x right, y down from the baseline).
    let glyphs: Vec<(std::rc::Rc<super::ft::Glyph>, f32, f32)> = pens
        .iter()
        .filter_map(|&(ch, pen, f)| cache.ft(f, ch, px).map(|g| (g, pen, f)))
        .filter(|(g, _, _)| g.width > 0 && g.rows > 0)
        .map(|(g, pen, _)| {
            let (x, y) = (pen + fx + g.left as f32, fy - g.top as f32);
            (g, x, y)
        })
        .collect();
    if glyphs.is_empty() {
        return Run { width: 0, height: 0, left: 0, top: 0, advance, alpha: Vec::new() };
    }
    let left = glyphs.iter().map(|(_, x, _)| x.floor() as i32).min().unwrap_or(0);
    let right = glyphs.iter().map(|(g, x, _)| x.floor() as i32 + g.width as i32 + 1).max().unwrap_or(left);
    let top = glyphs.iter().map(|(_, _, y)| y.floor() as i32).min().unwrap_or(0);
    let bottom = glyphs.iter().map(|(g, _, y)| y.floor() as i32 + g.rows as i32 + 1).max().unwrap_or(top);
    let (width, height) = ((right - left) as usize, (bottom - top) as usize);
    let mut cover = vec![0f32; width * height];
    let mut glyph = Vec::new();
    for (g, x, y) in &glyphs {
        let (ix, iy) = (x.floor() as i32, y.floor() as i32);
        let (dx, dy) = (x - ix as f32, y - iy as f32);
        // The glyph sampled bilinearly a fraction (dx, dy) off its pixels.
        let (w, h) = (g.width + 1, g.rows + 1);
        glyph.clear();
        glyph.resize(w * h, 0f32);
        for r in 0..g.rows {
            for c in 0..g.width {
                let v = g.coverage[r * g.width + c] as f32 / 255.0;
                if v == 0.0 {
                    continue;
                }
                glyph[r * w + c] += v * (1.0 - dx) * (1.0 - dy);
                glyph[r * w + c + 1] += v * dx * (1.0 - dy);
                glyph[(r + 1) * w + c] += v * (1.0 - dx) * dy;
                glyph[(r + 1) * w + c + 1] += v * dx * dy;
            }
        }
        let (ox, oy) = ((ix - left) as usize, (iy - top) as usize);
        for r in 0..h {
            for c in 0..w {
                let s = glyph[r * w + c];
                let d = &mut cover[(oy + r) * width + ox + c];
                *d = *d + s - *d * s;
            }
        }
    }
    let alpha = cover.iter().map(|&v| (v * 255.0).round().clamp(0.0, 255.0) as u8).collect();
    Run { width, height, left, top: -top, advance, alpha }
}

/// The run grown by `radius` pixels all round: each pixel takes the most
/// coverage within that distance, which outlines the letters.
pub fn dilate(run: &Run, radius: f32) -> Run {
    let r = radius.ceil().max(0.0) as i32;
    let (w, h) = (run.width + 2 * r as usize, run.height + 2 * r as usize);
    let mut alpha = vec![0u8; w * h];
    let offsets: Vec<(i32, i32, f32)> = (-r..=r)
        .flat_map(|dy| (-r..=r).map(move |dx| (dx, dy)))
        .filter_map(|(dx, dy)| {
            let d = ((dx * dx + dy * dy) as f32).sqrt();
            // Partial weight at the rim keeps the outline's edge soft.
            let weight = (radius + 0.5 - d).clamp(0.0, 1.0);
            (weight > 0.0).then_some((dx, dy, weight))
        })
        .collect();
    for y in 0..run.height as i32 {
        for x in 0..run.width as i32 {
            let a = run.alpha[y as usize * run.width + x as usize];
            if a == 0 {
                continue;
            }
            for &(dx, dy, weight) in &offsets {
                let (ox, oy) = ((x + dx + r) as usize, (y + dy + r) as usize);
                let v = (a as f32 * weight) as u8;
                let cell = &mut alpha[oy * w + ox];
                *cell = (*cell).max(v);
            }
        }
    }
    Run { width: w, height: h, left: run.left - r, top: run.top + r, advance: run.advance, alpha }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font() -> Font {
        load(FALLBACK_FONT).unwrap()
    }

    #[test]
    fn a_run_covers_its_glyphs_from_the_baseline() {
        let f = font();
        let run = rasterize(&f, "HI", 40.0, 0.0);
        // Capitals stand on the baseline: the run's bottom is at it.
        assert_eq!(run.top as usize, run.height);
        assert!(run.width > 20 && run.height > 25 && run.height < 35, "{}x{}", run.width, run.height);
        assert!(run.alpha.contains(&255));
        let spaced = rasterize(&f, "HI", 40.0, 10.0);
        assert_eq!(spaced.width, run.width + 10);
        assert!((spaced.advance - run.advance - 10.0).abs() < 1e-3);
        assert_eq!(advance(&mut Glyphs::default(), &f, None, "HI", 40.0, 0.0), run.advance);
    }

    #[test]
    fn an_outline_grows_the_run_and_keeps_its_letters() {
        let run = rasterize(&font(), "I", 40.0, 0.0);
        let grown = dilate(&run, 2.0);
        assert_eq!((grown.width, grown.height), (run.width + 4, run.height + 4));
        assert_eq!((grown.left, grown.top), (run.left - 2, run.top + 2));
        let inked = |r: &Run| r.alpha.iter().filter(|&&a| a > 128).count();
        assert!(inked(&grown) > inked(&run));
    }

    #[test]
    fn cached_glyphs_set_the_same_run() {
        let f = font();
        let mut cache = Glyphs::default();
        let plain = rasterize(&f, "MONDAY 15:54", 36.0, 0.0);
        assert_eq!(rasterize_with(&mut cache, &f, None, "MONDAY 15:54", 36.0, 0.0), plain);
        assert_eq!(rasterize_with(&mut cache, &f, None, "MONDAY 15:54", 36.0, 0.0), plain);
    }

    #[test]
    fn empty_text_is_an_empty_run() {
        let run = rasterize(&font(), "", 40.0, 0.0);
        assert_eq!((run.width, run.height, run.advance), (0, 0, 0.0));
        assert_eq!(rasterize(&font(), " ", 40.0, 0.0).width, 0);
    }
}
