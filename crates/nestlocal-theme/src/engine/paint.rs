//! What a theme can draw. The GPU painter (render.rs) draws it with femtovg;
//! the recording painter here lets tests run a theme without a display.

/// A colour, components from 0 to 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const WHITE: Color = Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };
    pub const BLACK: Color = Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };

    /// `#rgb`, `#rrggbb` or `#rrggbbaa`.
    pub fn parse(text: &str) -> Result<Color, String> {
        let hex = text.strip_prefix('#').filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| format!("colour {text:?} is not #rgb, #rrggbb or #rrggbbaa"))?;
        let digit = |i: usize, n: usize| u8::from_str_radix(&hex[i..i + n], 16).unwrap() as f32;
        let (r, g, b, a) = match hex.len() {
            3 => (digit(0, 1) * 17.0, digit(1, 1) * 17.0, digit(2, 1) * 17.0, 255.0),
            6 => (digit(0, 2), digit(2, 2), digit(4, 2), 255.0),
            8 => (digit(0, 2), digit(2, 2), digit(4, 2), digit(6, 2)),
            _ => return Err(format!("colour {text:?} is not #rgb, #rrggbb or #rrggbbaa")),
        };
        Ok(Color { r: r / 255.0, g: g / 255.0, b: b / 255.0, a: a / 255.0 })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Baseline {
    Top,
    Middle,
    Alphabetic,
    Bottom,
}

/// How a run of text is set.
#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    /// A font the theme registered, by its name.
    pub font: String,
    pub size: f32,
    pub color: Color,
    pub align: Align,
    pub baseline: Baseline,
    /// Extra space between letters.
    pub spacing: f32,
    /// A stroke round the letters, drawn under them: colour and width.
    pub outline: Option<(Color, f32)>,
    /// Whether the text starts on the nearest whole pixel, as stock starts
    /// most of its texts; if not, where it falls, as stock draws its large
    /// target.
    pub snap: bool,
}

/// Everything a theme draws goes through this, in its design units.
#[allow(clippy::too_many_arguments)]
pub trait Painter {
    fn fill_circle(&mut self, x: f32, y: f32, r: f32, color: Color);
    fn stroke_circle(&mut self, x: f32, y: f32, r: f32, color: Color, width: f32);
    /// An arc around (x, y), angles in radians clockwise from 12 o'clock.
    fn arc(&mut self, x: f32, y: f32, r: f32, from: f32, to: f32, color: Color, width: f32);
    /// A colour that moves from `start` to `end` along an arc.
    fn gradient_arc(&mut self, x: f32, y: f32, r: f32, from: f32, to: f32, start: Color, end: Color, width: f32) {
        let n = ((to - from).abs() / 3.0_f32.to_radians()).ceil().max(2.0) as usize;
        for i in 0..n {
            let a = (i as f32 + 0.5) / n as f32;
            let mix = |v: f32, w: f32| v + (w - v) * a;
            let color = Color { r: mix(start.r, end.r), g: mix(start.g, end.g), b: mix(start.b, end.b), a: mix(start.a, end.a) };
            let b0 = from + (to - from) * i as f32 / n as f32;
            let b1 = from + (to - from) * (i + 1) as f32 / n as f32;
            self.arc(x, y, r, b0.min(b1) - 0.3_f32.to_radians(), b0.max(b1) + 0.3_f32.to_radians(), color, width);
        }
    }
    /// A white glow whose faint discs build toward the centre.
    fn radial_glow(&mut self, x: f32, y: f32, radius: f32, steps: u32) {
        for i in 0..steps {
            let f = i as f32 / steps as f32;
            self.fill_circle(x, y, radius * (1.0 - f).powf(1.4), Color { a: 5.0 / 255.0, ..Color::WHITE });
        }
    }
    fn line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, color: Color, width: f32, round: bool);
    /// Many segments in one stroke: far cheaper than as many lines.
    fn lines(&mut self, segments: &[[f32; 4]], color: Color, width: f32);
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, color: Color);
    /// A closed polygon, filled.
    fn polygon(&mut self, points: &[(f32, f32)], color: Color);
    /// Draws text and returns its advance width.
    fn text(&mut self, text: &str, x: f32, y: f32, style: &TextStyle) -> f32;
    /// The advance width and height text would take.
    fn measure(&mut self, text: &str, style: &TextStyle) -> (f32, f32);
    /// Whether the style's font has every character of `text` (a unit
    /// font's symbols, which a stand-in lacks).
    fn has_glyphs(&mut self, _text: &str, _style: &TextStyle) -> bool {
        true
    }
    /// An image from the theme, by its path in the theme.
    fn image(&mut self, path: &str, x: f32, y: f32, w: f32, h: f32, alpha: f32) -> Result<(), String>;
    /// One frame of a sprite sheet: the `frame`th of `columns` a row, each
    /// `w` by `h`, drawn 1:1 at (x, y). Returns the sheet's frame count.
    fn sprite(&mut self, path: &str, frame: u32, columns: u32, w: f32, h: f32, x: f32, y: f32) -> Result<u32, String>;
    fn save(&mut self);
    fn restore(&mut self);
    fn translate(&mut self, x: f32, y: f32);
    /// Clockwise, in radians.
    fn rotate(&mut self, angle: f32);
    fn scale(&mut self, x: f32, y: f32);
    /// Multiplies the alpha of everything drawn until the next restore.
    fn alpha(&mut self, alpha: f32);
    /// Fills the whole panel with a colour, whatever the transform.
    fn clear(&mut self, color: Color);
    /// Draws `draw` into the offscreen layer `name`, `size` design units
    /// from the origin (the theme when none), unless the layer already holds
    /// what `key` names.
    fn layer(&mut self, name: &str, key: &str, size: Option<(f32, f32)>, draw: &mut dyn FnMut(&mut dyn Painter) -> Result<(), String>) -> Result<(), String>;
    /// Draws layer `name` from the origin, its colours multiplied by
    /// `tint`, only inside `clip` when one is given.
    fn paint_layer(&mut self, name: &str, tint: Color, clip: Option<&[(f32, f32)]>) -> Result<(), String>;
    /// Draws all of layer `name` onto the quadrilateral `corners` (its
    /// top-left, top-right, bottom-right and bottom-left), in perspective:
    /// as the theme of a flat card turned in space would show.
    fn warp_layer(&mut self, name: &str, corners: &[(f32, f32); 4]) -> Result<(), String>;
}

/// The texture weights of a quadrilateral's corners for a perspective
/// mapping, from where its diagonals cross: (u·q, v·q, q) interpolated
/// linearly and divided by q samples the card as perspective shows it.
/// All ones when the corners make no convex quadrilateral.
pub fn perspective_weights(corners: &[(f32, f32); 4]) -> [f32; 4] {
    let [p0, p1, p2, p3] = *corners;
    let (d1, d2) = ((p2.0 - p0.0, p2.1 - p0.1), (p3.0 - p1.0, p3.1 - p1.1));
    let cross = d1.0 * d2.1 - d1.1 * d2.0;
    if cross.abs() < 1e-6 {
        return [1.0; 4];
    }
    let (wx, wy) = (p1.0 - p0.0, p1.1 - p0.1);
    let (s, t) = ((wx * d2.1 - wy * d2.0) / cross, (wx * d1.1 - wy * d1.0) / cross);
    if !(0.0..=1.0).contains(&s) || !(0.0..=1.0).contains(&t) || s * t * (1.0 - s) * (1.0 - t) == 0.0 {
        return [1.0; 4];
    }
    // Distances from the crossing as fractions of each diagonal.
    let (a0, a2, a1, a3) = (s, 1.0 - s, t, 1.0 - t);
    [(a0 + a2) / a2, (a1 + a3) / a3, (a2 + a0) / a0, (a3 + a1) / a1]
}

/// The homography from the unit square's corners (0,0), (1,0), (1,1),
/// (0,1) to `corners`, as a row-major 3×3 matrix.
pub fn square_to_quad(corners: &[(f32, f32); 4]) -> [f32; 9] {
    let [(x0, y0), (x1, y1), (x2, y2), (x3, y3)] = *corners;
    let (sx, sy) = (x0 - x1 + x2 - x3, y0 - y1 + y2 - y3);
    let (dx1, dx2, dy1, dy2) = (x1 - x2, x3 - x2, y1 - y2, y3 - y2);
    let den = dx1 * dy2 - dx2 * dy1;
    let (g, h) = if den.abs() < 1e-9 { (0.0, 0.0) } else { ((sx * dy2 - dx2 * sy) / den, (dx1 * sy - sx * dy1) / den) };
    [x1 - x0 + g * x1, x3 - x0 + h * x3, x0,
     y1 - y0 + g * y1, y3 - y0 + h * y3, y0,
     g, h, 1.0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_parse_in_their_three_lengths() {
        assert_eq!(Color::parse("#fff").unwrap(), Color::WHITE);
        let c = Color::parse("#ec3100").unwrap();
        assert!((c.r - 236.0 / 255.0).abs() < 1e-6 && (c.g - 49.0 / 255.0).abs() < 1e-6 && c.b == 0.0 && c.a == 1.0);
        assert!((Color::parse("#00000080").unwrap().a - 128.0 / 255.0).abs() < 1e-6);
        for bad in ["fff", "#ff", "#gggggg", "#1234567"] {
            assert!(Color::parse(bad).is_err(), "{bad}");
        }
    }

    /// A unit card at depth 2, turned about its upright axis by `angle`,
    /// seen through a lens of focal length 2: its corners on screen, and
    /// where its point (u, v) lands.
    fn turned_card(angle: f32, u: f32, v: f32) -> (f32, f32) {
        let depth = 2.0 - u * angle.sin();
        (2.0 * u * angle.cos() / depth, 2.0 * v / depth)
    }

    #[test]
    fn a_turned_card_maps_as_perspective_shows_it() {
        let angle = 1.1f32;
        let corners = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].map(|(u, v)| turned_card(angle, u, v));
        let h = square_to_quad(&corners);
        for (s, t) in [(0.25f32, 0.5f32), (0.8, 0.1), (0.5, 0.5)] {
            let want = turned_card(angle, 2.0 * s - 1.0, 2.0 * t - 1.0);
            let w = h[6] * s + h[7] * t + 1.0;
            let got = ((h[0] * s + h[1] * t + h[2]) / w, (h[3] * s + h[4] * t + h[5]) / w);
            assert!((got.0 - want.0).abs() < 1e-4 && (got.1 - want.1).abs() < 1e-4, "{got:?} {want:?}");
        }
    }
}
