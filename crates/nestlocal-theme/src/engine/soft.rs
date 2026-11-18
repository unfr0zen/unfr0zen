//! The software painter: themes' drawing on the CPU, through tiny-skia, for
//! units without a GPU (the gen 1's and gen 2's AM3703). It draws what the GPU painter
//! draws, into a pixmap the size of the framebuffer.

use std::collections::HashMap;
use std::f32::consts::{FRAC_PI_2, TAU};
use std::path::Path;
use std::time::Instant;

use tiny_skia::{
    FillRule, FilterQuality, LineCap, Paint, PathBuilder, Pattern, Pixmap, PixmapPaint, Rect, SpreadMode, Stroke,
    Transform,
};

use super::paint::{gradient_arcs, square_to_quad, Align, Baseline, Color, Painter, TextStyle};
use super::typeset;
use log::{debug, trace};

/// At most this many runs of text are kept.
const RUNS: usize = 96;
/// Images and layers not drawn in this many frames are dropped, checked
/// every so many frames.
const KEEP_FRAMES: u64 = 120;
/// Tinted copies kept of each layer.
const TINTS: usize = 4;
const EVICT_EVERY: u64 = 64;

/// An offscreen drawing of the theme, and what it was drawn for.
struct Layer {
    key: String,
    pixmap: Pixmap,
    /// Where the layer is not transparent, which alone is painted.
    runs: Runs,
    /// Its size in design units.
    size: (f32, f32),
    /// The layer in the tints it was last painted with, most recent first
    /// (a theme may paint one in two, alternately).
    tinted: Vec<(Color, Pixmap)>,
}

/// Each row's runs of pixels that are not transparent, as columns from
/// (inclusive) to (exclusive): painting a layer blends only these, as most
/// of a layer the dial's size is clear. Gaps shorter than RUN_GAP are kept
/// in a run, as a run costs more to start than a few clear pixels to skip.
type Runs = Vec<Vec<(u16, u16)>>;
const RUN_GAP: usize = 8;

fn runs(pixmap: &Pixmap) -> Runs {
    let width = pixmap.width() as usize;
    pixmap
        .data()
        .chunks_exact(width * 4)
        .map(|row| {
            let mut runs: Vec<(u16, u16)> = Vec::new();
            for (i, px) in row.as_chunks::<4>().0.iter().enumerate() {
                if px[3] == 0 {
                    continue;
                }
                match runs.last_mut() {
                    Some((_, end)) if i - usize::from(*end) < RUN_GAP => *end = i as u16 + 1,
                    _ => runs.push((i as u16, i as u16 + 1)),
                }
            }
            runs
        })
        .collect()
}

/// Whether painting is timed, for a profile: reading the clock is a system
/// call on an ARM kernel without vDSO clocks, which every operation would
/// make twice.
pub static PROFILING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A span of painting, when profiling: its operation and start.
type Span = Option<(&'static str, Instant)>;
fn span(operation: &'static str) -> Span {
    PROFILING.load(std::sync::atomic::Ordering::Relaxed).then(|| (operation, Instant::now()))
}

/// atan2 to about a thousandth of a radian, without the library's call.
fn atan2(y: f32, x: f32) -> f32 {
    use std::f32::consts::PI;
    if x == 0.0 && y == 0.0 {
        return 0.0;
    }
    let (ax, ay) = (x.abs(), y.abs());
    let a = ax.min(ay) / ax.max(ay);
    let s = a * a;
    let mut r = ((-0.046_496_47 * s + 0.159_314_22) * s - 0.327_622_76) * s * a + a;
    if ay > ax {
        r = FRAC_PI_2 - r;
    }
    if x < 0.0 {
        r = PI - r;
    }
    if y < 0.0 {
        -r
    } else {
        r
    }
}

/// A run of text as an image, its placement in pixels from the pen.
struct CachedRun {
    pixmap: Option<Pixmap>,
    left: i32,
    top: i32,
    used: u64,
}

/// A pixmap the size of the framebuffer and what it has loaded for the
/// current theme.
pub struct Renderer {
    pub pixmap: Pixmap,
    fonts: typeset::Fonts,
    images: HashMap<String, Option<Pixmap>>,
    /// Images scaled to the panel's pixels at the size last drawn, so that
    /// drawing them again is a copy.
    scaled: HashMap<String, Pixmap>,
    layers: HashMap<String, Layer>,
    /// The frame each image and layer was last drawn in: those not drawn
    /// for a while are dropped, for the gen 2's 64 MB.
    used: HashMap<(bool, String), u64>,
    frame: u64,
    runs: HashMap<String, CachedRun>,
    tick: u64,
    glyphs: typeset::Glyphs,
    advances: HashMap<String, f32>,
    /// Shapes filled or stroked since the count was last taken.
    pub shapes: u32,
    /// Time spent in the painter since last taken, and by operation, while
    /// profiling.
    pub painting: std::time::Duration,
    pub profile: std::collections::BTreeMap<&'static str, (u32, std::time::Duration)>,
    /// The frame's design units, pixels to a design unit, and the
    /// transform from design units to the framebuffer.
    design: f32,
    scale: f32,
    base: Transform,
}

impl Renderer {
    pub fn new(width: u32, height: u32) -> Result<Renderer, String> {
        Ok(Renderer {
            pixmap: Pixmap::new(width, height).ok_or_else(|| format!("a {width}x{height} pixmap"))?,
            fonts: typeset::Fonts::new()?,
            images: HashMap::new(),
            scaled: HashMap::new(),
            layers: HashMap::new(),
            used: HashMap::new(),
            frame: 0,
            runs: HashMap::new(),
            tick: 0,
            glyphs: typeset::Glyphs::default(),
            advances: HashMap::new(),
            shapes: 0,
            painting: std::time::Duration::ZERO,
            profile: std::collections::BTreeMap::new(),
            design: 480.0,
            scale: 1.0,
            base: Transform::identity(),
        })
    }

    /// Loads a theme's fonts, replacing the previous theme's names.
    pub fn use_fonts(&mut self, fonts: &[(String, std::path::PathBuf)]) -> Result<(), String> {
        self.fonts.use_theme(fonts)?;
        self.images.clear();
        self.scaled.clear();
        self.layers.clear();
        self.used.clear();
        self.runs.clear();
        self.glyphs.clear();
        self.advances.clear();
        debug!("paint: {} fonts in use; images, layers and text dropped", fonts.len());
        Ok(())
    }

    /// Starts a frame: clears the pixmap and sets the transform from the
    /// theme's upright design units to the theme's pixels, upright.
    pub fn begin(&mut self, theme: f32, design: f32) {
        self.pixmap.fill(tiny_skia::Color::BLACK);
        self.frame += 1;
        if self.frame.is_multiple_of(EVICT_EVERY) {
            let frame = self.frame;
            self.used.retain(|_, at| frame - *at <= KEEP_FRAMES);
            let used = &self.used;
            self.images.retain(|k, _| used.contains_key(&(false, k.clone())));
            let images = &self.images;
            self.scaled.retain(|k, _| images.contains_key(k));
            self.layers.retain(|k, _| used.contains_key(&(true, k.clone())));
            trace!(
                "paint: {} images, {} scaled and {} layers kept after frame {frame}",
                self.images.len(),
                self.scaled.len(),
                self.layers.len()
            );
        }
        self.design = design;
        self.scale = theme / design;
        self.base = Transform::from_scale(self.scale, self.scale);
    }

    /// Counts a span of painting, when profiling, under its operation or
    /// `operation` when given.
    fn timed(&mut self, span: Span) {
        self.timed_as(span, None);
    }
    fn timed_as(&mut self, span: Span, operation: Option<&'static str>) {
        if let Some((name, started)) = span {
            let took = started.elapsed();
            self.painting += took;
            let total = self.profile.entry(operation.unwrap_or(name)).or_default();
            total.0 += 1;
            total.1 += took;
        }
    }

    pub fn painter<'a>(&'a mut self, theme_dir: &'a Path) -> SoftPainter<'a> {
        let base = self.base;
        SoftPainter { r: self, dir: theme_dir, stack: vec![State { transform: base, alpha: 1.0 }], layer: None }
    }
}

#[derive(Clone, Copy)]
struct State {
    transform: Transform,
    alpha: f32,
}

pub struct SoftPainter<'a> {
    r: &'a mut Renderer,
    dir: &'a Path,
    /// The transform and alpha, one per save level.
    stack: Vec<State>,
    /// The pixmap of the layer being drawn, in place of the frame's.
    layer: Option<Pixmap>,
}

fn paint(c: Color, alpha: f32) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8((c.r * 255.0) as u8, (c.g * 255.0) as u8, (c.b * 255.0) as u8, (c.a * alpha * 255.0) as u8);
    p.anti_alias = true;
    p
}

/// A pixmap of an image's bytes, premultiplied.
fn decode(data: &[u8]) -> Option<Pixmap> {
    let (w, h, rgba) = rgba_png(data)?;
    let mut pixmap = Pixmap::new(w, h)?;
    for (px, &[r, g, b, a]) in pixmap.pixels_mut().iter_mut().zip(rgba.as_chunks::<4>().0) {
        let m = |c: u8| ((u16::from(c) * u16::from(a) + 127) / 255) as u8;
        *px = tiny_skia::PremultipliedColorU8::from_rgba(m(r), m(g), m(b), a)?;
    }
    Some(pixmap)
}

/// A PNG's size and pixels as 8-bit RGBA (themes' images are PNGs).
fn rgba_png(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(data);
    decoder.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buf).ok()?;
    buf.truncate(frame.buffer_size());
    let rgba = match frame.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::GrayscaleAlpha => buf.as_chunks::<2>().0.iter().flat_map(|&[l, a]| [l, l, l, a]).collect(),
        _ => return None,
    };
    Some((frame.width, frame.height, rgba))
}

/// A layer with each pixel multiplied by `tint`, in integers (it runs over
/// the whole layer whenever a tint changes).
fn tinted(source: &Pixmap, tint: Color) -> Pixmap {
    let mut out = source.clone();
    let m = multipliers(tint);
    for px in out.data_mut().as_chunks_mut::<4>().0 {
        if px[3] == 0 {
            continue;
        }
        for i in 0..4 {
            px[i] = ((u32::from(px[i]) * m[i]) >> 8) as u8;
        }
    }
    out
}

/// A tint as what each premultiplied channel is multiplied by, out of 256.
type Multipliers = [u32; 4];
fn multipliers(tint: Color) -> Multipliers {
    let f = |v: f32| (v.clamp(0.0, 1.0) * 256.0) as u32;
    let a = f(tint.a);
    [(f(tint.r) * a) >> 8, (f(tint.g) * a) >> 8, (f(tint.b) * a) >> 8, a]
}

/// The cache's key for a run of text at `scale` pixels a design unit.
fn run_key(text: &str, style: &TextStyle, outline: f32, scale: f32, frac: (u8, u8)) -> String {
    let (px, spacing) = (style.size * scale, style.spacing * scale);
    format!("{}\u{0}{px:.2}\u{0}{spacing:.2}\u{0}{outline:.2}\u{0}{}.{}\u{0}{text}", style.font, frac.0, frac.1)
}

impl SoftPainter<'_> {
    fn state(&self) -> State {
        *self.stack.last().expect("the stack always has a level")
    }

    fn target(&mut self) -> &mut Pixmap {
        match self.layer.as_mut() {
            Some(l) => l,
            None => &mut self.r.pixmap,
        }
    }

    fn fill(&mut self, path: Option<tiny_skia::Path>, color: Color) {
        let Some(path) = path else { return };
        let started = span("fill");
        let s = self.state();
        self.r.shapes += 1;
        self.target().fill_path(&path, &paint(color, s.alpha), FillRule::Winding, s.transform, None);
        self.r.timed(started);
    }

    fn stroke(&mut self, path: Option<tiny_skia::Path>, color: Color, width: f32, cap: LineCap) {
        let Some(path) = path else { return };
        let started = span("stroke");
        let s = self.state();
        self.r.shapes += 1;
        let stroke = Stroke { width, line_cap: cap, ..Stroke::default() };
        self.target().stroke_path(&path, &paint(color, s.alpha), &stroke, s.transform, None);
        self.r.timed(started);
    }

    fn font(&self, name: &str) -> &typeset::Font {
        self.r.fonts.get(name)
    }

    /// The run of text in pixels, set the first time it is drawn, starting
    /// `frac` sixteenths of a pixel right of and below a whole one.
    fn run(&mut self, text: &str, style: &TextStyle, outline: f32, frac: (u8, u8)) -> Option<&CachedRun> {
        let (px, spacing) = (style.size * self.r.scale, style.spacing * self.r.scale);
        let key = run_key(text, style, outline, self.r.scale, frac);
        self.r.tick += 1;
        if !self.r.runs.contains_key(&key) {
            let font = self.r.fonts.get(&style.font);
            let f = (f32::from(frac.0) / 16.0, f32::from(frac.1) / 16.0);
            let mut run = typeset::rasterize_at(&mut self.r.glyphs, font, self.r.fonts.cjk(text), text, px, spacing, f);
            if outline > 0.0 {
                run = typeset::dilate(&run, outline);
            }
            // White coverage, tinted when drawn.
            trace!(
                "paint: text {text:?} set, {}x{} pixels at {px:.1} px{}",
                run.width,
                run.height,
                if outline > 0.0 { ", outlined" } else { "" }
            );
            let pixmap = Pixmap::new(run.width as u32, run.height as u32).map(|mut p| {
                for (px, &a) in p.pixels_mut().iter_mut().zip(&run.alpha) {
                    *px = tiny_skia::PremultipliedColorU8::from_rgba(a, a, a, a).unwrap_or(*px);
                }
                p
            });
            if self.r.runs.len() >= RUNS {
                if let Some(oldest) = self.r.runs.iter().min_by_key(|(_, r)| r.used).map(|(k, _)| k.clone()) {
                    self.r.runs.remove(&oldest);
                }
            }
            self.r.runs.insert(key.clone(), CachedRun { pixmap, left: run.left, top: run.top, used: 0 });
        }
        let tick = self.r.tick;
        let run = self.r.runs.get_mut(&key)?;
        run.used = tick;
        Some(run)
    }

    fn advance(&mut self, text: &str, style: &TextStyle) -> f32 {
        let (px, spacing) = (style.size * self.r.scale, style.spacing * self.r.scale);
        let key = format!("{}\u{0}{px:.2}\u{0}{spacing:.2}\u{0}{text}", style.font);
        if let Some(&a) = self.r.advances.get(&key) {
            return a;
        }
        if self.r.advances.len() > 512 {
            self.r.advances.clear();
        }
        let a = typeset::advance(
            &mut self.r.glyphs,
            self.r.fonts.get(&style.font),
            self.r.fonts.cjk(text),
            text,
            px,
            spacing,
        );
        self.r.advances.insert(key, a);
        a
    }

    /// Draws a run with its pen at (x, baseline) in design units, on whole
    /// pixels, its coverage in `color`.
    fn draw_run(&mut self, text: &str, style: &TextStyle, outline: f32, x: f32, baseline: f32, color: Color) {
        let scale = self.r.scale;
        let s = self.state();
        let t = s.transform;
        // An unsnapped text FreeType sets upright starts where it falls: its
        // run is set at the start's fraction of a pixel, from the whole one.
        let upright =
            t.kx.abs() < 1e-6 && t.ky.abs() < 1e-6 && (t.sx - scale).abs() < 1e-4 && (t.sy - scale).abs() < 1e-4;
        let (frac, x, baseline) = if !style.snap && upright && self.font(&style.font).subpixel() {
            let (dx, dy) = (t.sx * x + t.tx, t.sy * baseline + t.ty);
            let (fx, fy) = (((dx - dx.floor()) * 16.0).round(), ((dy - dy.floor()) * 16.0).round());
            let (wx, wy) = (dx.floor() + (fx / 16.0).floor(), dy.floor() + (fy / 16.0).floor());
            (((fx as u8) % 16, (fy as u8) % 16), (wx - t.tx) / t.sx, (wy - t.ty) / t.sy)
        } else {
            ((0, 0), x, baseline)
        };
        let Some(run) = self.run(text, style, outline, frac) else {
            return;
        };
        let (left, top) = (run.left, run.top);
        let key = run_key(text, style, outline, scale, frac);
        let Some(coverage) = run.pixmap.as_ref() else { return };
        let snap = |v: f32| (v * scale).round() / scale;
        let at = t
            .pre_translate(snap(x) + left as f32 / scale, snap(baseline) - top as f32 / scale)
            .pre_scale(1.0 / scale, 1.0 / scale);
        if let Some((x, y)) = whole_pixels(&at) {
            let started = span("text coverage");
            self.r.shapes += 1;
            let r = &mut *self.r;
            let Some(coverage) = r.runs.get(&key).and_then(|c| c.pixmap.as_ref()) else {
                return;
            };
            let target = match self.layer.as_mut() {
                Some(l) => l,
                None => &mut r.pixmap,
            };
            blit_coverage(target, coverage, x, y, color, s.alpha);
            r.timed(started);
            return;
        }
        // The white coverage, coloured.
        let mut colored = coverage.clone();
        let k = |c: u8, t: f32| (f32::from(c) * t).round() as u8;
        for px in colored.pixels_mut() {
            let a = px.alpha();
            *px = tiny_skia::PremultipliedColorU8::from_rgba(k(a, color.r), k(a, color.g), k(a, color.b), a)
                .unwrap_or(*px);
        }
        let started = span("text smooth");
        self.r.shapes += 1;
        let opacity = color.a * s.alpha;
        blit_smooth(self.target(), &colored, at, opacity);
        self.r.timed(started);
    }

    fn set_text(&mut self, text: &str, x: f32, y: f32, style: &TextStyle) -> f32 {
        let (scale, px) = (self.r.scale, style.size * self.r.scale);
        let width = self.advance(text, style) / scale;
        let font = self.font(&style.font);
        let (ascent, descent) = typeset::line(font, px);
        let (ascent, descent) = (ascent / scale, descent / scale);
        let left = match style.align {
            Align::Left => x,
            Align::Center => x - width / 2.0,
            Align::Right => x - width,
        };
        let baseline = match style.baseline {
            Baseline::Alphabetic => y,
            Baseline::Top => y + ascent,
            Baseline::Middle => y + (ascent - descent) / 2.0,
            Baseline::Bottom => y - descent,
        };
        if let Some((color, line)) = style.outline {
            self.draw_run(text, style, line * scale / 2.0, left, baseline, color);
        }
        self.draw_run(text, style, 0.0, left, baseline, style.color);
        width
    }

    fn load_image(&mut self, path: &str) -> Result<(), String> {
        self.r.used.insert((false, path.to_string()), self.r.frame);
        if !self.r.images.contains_key(path) {
            let loaded = std::fs::read(self.dir.join(path)).ok().and_then(|data| decode(&data));
            match &loaded {
                Some(p) => debug!("paint: image {path} loaded, {}x{}", p.width(), p.height()),
                None => debug!("paint: image {path} could not be loaded"),
            }
            self.r.images.insert(path.to_string(), loaded);
        }
        self.r.images[path].as_ref().map(|_| ()).ok_or_else(|| format!("image {path} could not be loaded"))
    }

    /// Fills `path` with an image or a layer, whose pixels `pattern`
    /// places in design units.
    fn fill_with(
        &mut self,
        path: Option<tiny_skia::Path>,
        image: &str,
        pattern: Transform,
        opacity: f32,
        quality: FilterQuality,
    ) {
        let Some(path) = path else { return };
        let started = span("image");
        let s = self.state();
        self.r.shapes += 1;
        let r = &mut *self.r;
        let pixmap = r.images.get(image).and_then(Option::as_ref);
        let Some(pixmap) = pixmap else { return };
        let p = Paint {
            shader: Pattern::new(pixmap.as_ref(), SpreadMode::Pad, quality, opacity * s.alpha, pattern),
            anti_alias: true,
            ..Paint::default()
        };
        let target = match self.layer.as_mut() {
            Some(l) => l,
            None => &mut r.pixmap,
        };
        target.fill_path(&path, &p, FillRule::Winding, s.transform, None);
        r.timed(started);
    }
}

/// `source` drawn over `target` with its top-left corner at (x, y), both
/// premultiplied, at `opacity`: tiny-skia's pipeline costs a frame on the
/// gen 2 for what is a copy.
fn blit(target: &mut Pixmap, source: &Pixmap, x: i32, y: i32, opacity: f32) {
    let k = (opacity.clamp(0.0, 1.0) * 256.0) as u32;
    for row in y.max(0)..(y + source.height() as i32).min(target.height() as i32) {
        blit_row(target, source, (x, y), row, (x, x + source.width() as i32), k, None);
    }
}

/// `blit` of a layer, only its `runs`, tinted by `tint` when given.
fn blit_runs(
    target: &mut Pixmap,
    source: &Pixmap,
    runs: &Runs,
    (x, y): (i32, i32),
    opacity: f32,
    tint: Option<Multipliers>,
) {
    let k = (opacity.clamp(0.0, 1.0) * 256.0) as u32;
    for row in y.max(0)..(y + source.height() as i32).min(target.height() as i32) {
        for &(from, to) in &runs[(row - y) as usize] {
            blit_row(target, source, (x, y), row, (x + i32::from(from), x + i32::from(to)), k, tint);
        }
    }
}

/// Where `at` places a pixmap's top-left corner, when it does so one to one
/// on whole pixels.
fn whole_pixels(at: &Transform) -> Option<(i32, i32)> {
    let one = |v: f32| (v - 1.0).abs() < 1e-3;
    (one(at.sx) && one(at.sy) && at.kx.abs() < 1e-6 && at.ky.abs() < 1e-6)
        .then(|| (at.tx.round() as i32, at.ty.round() as i32))
}

/// `source` over `target` placed by `at` (its pixels to the target's),
/// sampled at the nearest pixel, in 16.16 fixed point across each row.
fn blit_transformed(target: &mut Pixmap, source: &Pixmap, at: Transform, opacity: f32) {
    let Some(inv) = at.invert() else { return };
    let (sw, sh) = (source.width() as f32, source.height() as f32);
    let mut corners = [(0.0, 0.0), (sw, 0.0), (0.0, sh), (sw, sh)].map(|(x, y)| tiny_skia::Point::from_xy(x, y));
    at.map_points(&mut corners);
    let (tw, th) = (target.width() as i32, target.height() as i32);
    let (left, right) = corners.iter().fold((f32::MAX, f32::MIN), |(l, r), p| (l.min(p.x), r.max(p.x)));
    let (top, bottom) = corners.iter().fold((f32::MAX, f32::MIN), |(t, b), p| (t.min(p.y), b.max(p.y)));
    let (x0, x1) = ((left.floor() as i32).max(0), (right.ceil() as i32).min(tw));
    let (y0, y1) = ((top.floor() as i32).max(0), (bottom.ceil() as i32).min(th));
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let k = (opacity.clamp(0.0, 1.0) * 256.0) as u32;
    let fixed = |v: f32| (v * 65536.0) as i64;
    let (du, dv) = (fixed(inv.sx), fixed(inv.ky));
    let (iw, ih) = (source.width() as i64, source.height() as i64);
    let stride = source.width() as usize;
    let src = source.data();
    let dst = target.data_mut();
    for row in y0..y1 {
        let cy = row as f32 + 0.5;
        // The row's source coordinates at x0's centre, then the span of x
        // whose samples land inside the source.
        let (u0, v0) =
            (inv.sx * (x0 as f32 + 0.5) + inv.kx * cy + inv.tx, inv.ky * (x0 as f32 + 0.5) + inv.sy * cy + inv.ty);
        let (mut lo, mut hi) = (0.0f32, (x1 - x0) as f32);
        for (start, step, limit) in [(u0, inv.sx, sw), (v0, inv.ky, sh)] {
            if step.abs() < 1e-9 {
                if start < 0.0 || start >= limit {
                    hi = lo;
                }
            } else {
                let (a, b) = ((0.0 - start) / step, (limit - start) / step);
                lo = lo.max(a.min(b));
                hi = hi.min(a.max(b));
            }
        }
        let (from, to) = (lo.ceil().max(0.0) as i32, (hi.floor() as i32 + 1).min(x1 - x0));
        if from >= to {
            continue;
        }
        let (mut u, mut v) = (fixed(u0) + du * i64::from(from), fixed(v0) + dv * i64::from(from));
        let d_row = &mut dst[((row * tw + x0 + from) * 4) as usize..][..((to - from) * 4) as usize];
        for d in d_row.as_chunks_mut::<4>().0 {
            let (sx, sy) = (u >> 16, v >> 16);
            u += du;
            v += dv;
            if sx < 0 || sy < 0 || sx >= iw || sy >= ih {
                continue;
            }
            let i = (sy as usize * stride + sx as usize) * 4;
            let s = &src[i..i + 4];
            let a = (u32::from(s[3]) * k) >> 8;
            if a == 0 {
                continue;
            }
            let keep = 255 - a;
            for c in 0..4 {
                let sc = (u32::from(s[c]) * k) >> 8;
                d[c] = (sc + (u32::from(d[c]) * keep + 127) / 255).min(255) as u8;
            }
        }
    }
}

/// `source` over `target` through `at`, each pixel the source's four
/// nearest blended by distance: text turned or scaled stays smooth.
fn blit_smooth(target: &mut Pixmap, source: &Pixmap, at: Transform, opacity: f32) {
    let Some(inv) = at.invert() else { return };
    let (sw, sh) = (source.width() as f32, source.height() as f32);
    let mut corners = [(0.0, 0.0), (sw, 0.0), (0.0, sh), (sw, sh)].map(|(x, y)| tiny_skia::Point::from_xy(x, y));
    at.map_points(&mut corners);
    let (tw, th) = (target.width() as i32, target.height() as i32);
    let (left, right) = corners.iter().fold((f32::MAX, f32::MIN), |(l, r), p| (l.min(p.x), r.max(p.x)));
    let (top, bottom) = corners.iter().fold((f32::MAX, f32::MIN), |(t, b), p| (t.min(p.y), b.max(p.y)));
    let (x0, x1) = (((left - 1.0).floor() as i32).max(0), ((right + 1.0).ceil() as i32).min(tw));
    let (y0, y1) = (((top - 1.0).floor() as i32).max(0), ((bottom + 1.0).ceil() as i32).min(th));
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let k = (opacity.clamp(0.0, 1.0) * 256.0) as u32;
    let (iw, ih) = (source.width() as i32, source.height() as i32);
    let src = source.data();
    let dst = target.data_mut();
    let texel = |x: i32, y: i32| -> [u32; 4] {
        if x < 0 || y < 0 || x >= iw || y >= ih {
            return [0; 4];
        }
        let i = (y * iw + x) as usize * 4;
        [u32::from(src[i]), u32::from(src[i + 1]), u32::from(src[i + 2]), u32::from(src[i + 3])]
    };
    for row in y0..y1 {
        for col in x0..x1 {
            let (cx, cy) = (col as f32 + 0.5, row as f32 + 0.5);
            // The source point under the pixel's centre, from texel centres.
            let u = inv.sx * cx + inv.kx * cy + inv.tx - 0.5;
            let v = inv.ky * cx + inv.sy * cy + inv.ty - 0.5;
            let (fx, fy) = (u.floor(), v.floor());
            let (ix, iy) = (fx as i32, fy as i32);
            if ix < -1 || iy < -1 || ix >= iw || iy >= ih {
                continue;
            }
            let (wx, wy) = (((u - fx) * 256.0) as u32, ((v - fy) * 256.0) as u32);
            let (a, b, c, d) = (texel(ix, iy), texel(ix + 1, iy), texel(ix, iy + 1), texel(ix + 1, iy + 1));
            let mut s = [0u32; 4];
            for n in 0..4 {
                let top = a[n] * (256 - wx) + b[n] * wx;
                let bottom = c[n] * (256 - wx) + d[n] * wx;
                s[n] = (((top * (256 - wy) + bottom * wy) >> 16) * k) >> 8;
            }
            if s[3] == 0 {
                continue;
            }
            let keep = 255 - s[3].min(255);
            let i = ((row * tw + col) * 4) as usize;
            for n in 0..4 {
                dst[i + n] = (s[n] + (u32::from(dst[i + n]) * keep + 127) / 255).min(255) as u8;
            }
        }
    }
}

/// `source` over `target` onto the quadrilateral `corners` (target pixels;
/// the source's top-left, top-right, bottom-right and bottom-left), in
/// perspective, sampled at the nearest pixel.
fn blit_warped(target: &mut Pixmap, source: &Pixmap, corners: &[(f32, f32); 4], opacity: f32) {
    let [a, b, c, d, e, f, g, h, i] = square_to_quad(corners);
    // Its inverse (the adjugate: the scale cancels in the division).
    let m = [
        e * i - f * h,
        c * h - b * i,
        b * f - c * e,
        f * g - d * i,
        a * i - c * g,
        c * d - a * f,
        d * h - e * g,
        b * g - a * h,
        a * e - b * d,
    ];
    let (tw, th) = (target.width() as i32, target.height() as i32);
    let (left, right) = corners.iter().fold((f32::MAX, f32::MIN), |(l, r), p| (l.min(p.0), r.max(p.0)));
    let (top, bottom) = corners.iter().fold((f32::MAX, f32::MIN), |(t, b), p| (t.min(p.1), b.max(p.1)));
    let (x0, x1) = ((left.floor() as i32).max(0), (right.ceil() as i32).min(tw));
    let (y0, y1) = ((top.floor() as i32).max(0), (bottom.ceil() as i32).min(th));
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let k = (opacity.clamp(0.0, 1.0) * 256.0) as u32;
    let (sw, sh) = (source.width() as usize, source.height() as usize);
    let src = source.data();
    let dst = target.data_mut();
    for row in y0..y1 {
        let cy = row as f32 + 0.5;
        let cx = x0 as f32 + 0.5;
        let (mut nu, mut nv, mut nw) =
            (m[0] * cx + m[1] * cy + m[2], m[3] * cx + m[4] * cy + m[5], m[6] * cx + m[7] * cy + m[8]);
        let d_row = &mut dst[((row * tw + x0) * 4) as usize..][..((x1 - x0) * 4) as usize];
        for px in d_row.as_chunks_mut::<4>().0 {
            let (u, v) = (nu / nw, nv / nw);
            nu += m[0];
            nv += m[3];
            nw += m[6];
            if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
                continue;
            }
            let at = (((v * sh as f32) as usize).min(sh - 1) * sw + ((u * sw as f32) as usize).min(sw - 1)) * 4;
            let s = &src[at..at + 4];
            let alpha = (u32::from(s[3]) * k) >> 8;
            if alpha == 0 {
                continue;
            }
            let keep = 255 - alpha;
            for c in 0..4 {
                let sc = (u32::from(s[c]) * k) >> 8;
                px[c] = (sc + (u32::from(px[c]) * keep + 127) / 255).min(255) as u8;
            }
        }
    }
}

/// One row of `source` over `target`, pixels `span` of the target's row,
/// the source's top-left corner at (x, y); `k` is the opacity out of 256,
/// and `tint`, when given, multiplies the source's pixels as `tinted` does.
fn blit_row(
    target: &mut Pixmap,
    source: &Pixmap,
    (x, y): (i32, i32),
    row: i32,
    (x0, x1): (i32, i32),
    k: u32,
    tint: Option<Multipliers>,
) {
    let (tw, sw) = (target.width() as i32, source.width() as i32);
    let (x0, x1) = (x0.max(x).max(0), x1.min(x + sw).min(tw));
    if x0 >= x1 || row < y.max(0) || row >= (y + source.height() as i32).min(target.height() as i32) {
        return;
    }
    let s_row = &source.data()[(((row - y) * sw + (x0 - x)) * 4) as usize..][..((x1 - x0) * 4) as usize];
    let d_row = &mut target.data_mut()[((row * tw + x0) * 4) as usize..][..((x1 - x0) * 4) as usize];
    if let Some(m) = tint {
        for (d, s) in d_row.as_chunks_mut::<4>().0.iter_mut().zip(s_row.as_chunks::<4>().0) {
            if s[3] == 0 {
                continue;
            }
            let s = [0, 1, 2, 3].map(|i| ((u32::from(s[i]) * m[i]) >> 8) as u8);
            blend(d, &s, k);
        }
        return;
    }
    if k < 256 {
        for (d, s) in d_row.as_chunks_mut::<4>().0.iter_mut().zip(s_row.as_chunks::<4>().0) {
            blend(d, s, k);
        }
        return;
    }
    // At full opacity, stretches of opaque pixels are copied at once.
    let (s_px, d_px) = (s_row.as_chunks::<4>().0, d_row.as_chunks_mut::<4>().0);
    let mut i = 0;
    while i < s_px.len() {
        let start = i;
        while i < s_px.len() && s_px[i][3] == 255 {
            i += 1;
        }
        if i > start {
            d_px[start..i].copy_from_slice(&s_px[start..i]);
            continue;
        }
        blend(&mut d_px[i], &s_px[i], k);
        i += 1;
    }
}

/// One premultiplied pixel over another at opacity `k` out of 256.
#[inline(always)]
fn blend(d: &mut [u8; 4], s: &[u8; 4], k: u32) {
    {
        let a = (u32::from(s[3]) * k) >> 8;
        if a == 0 {
            return;
        }
        if a == 255 {
            *d = *s;
            return;
        }
        let keep = 255 - a;
        for i in 0..4 {
            let sc = (u32::from(s[i]) * k) >> 8;
            d[i] = (sc + (u32::from(d[i]) * keep + 127) / 255).min(255) as u8;
        }
    }
}

/// `source` over `target` as `blit` does, only the pixels whose centres lie
/// inside `polygon` (in the target's pixels, even-odd), with no soft edge.
fn blit_polygon(
    target: &mut Pixmap,
    source: &Pixmap,
    runs: Option<&Runs>,
    (x, y): (i32, i32),
    polygon: &[(f32, f32)],
    opacity: f32,
    tint: Option<Multipliers>,
) {
    if polygon.len() < 3 {
        return;
    }
    let k = (opacity.clamp(0.0, 1.0) * 256.0) as u32;
    let (top, bottom) = polygon.iter().fold((f32::MAX, f32::MIN), |(t, b), p| (t.min(p.1), b.max(p.1)));
    let mut crossings = Vec::with_capacity(8);
    for row in (top - 0.5).ceil().max(0.0) as i32..=((bottom - 0.5).floor() as i32).min(target.height() as i32 - 1) {
        let cy = row as f32 + 0.5;
        crossings.clear();
        for (i, &(ax, ay)) in polygon.iter().enumerate() {
            let (bx, by) = polygon[(i + 1) % polygon.len()];
            if (ay <= cy) != (by <= cy) {
                crossings.push(ax + (cy - ay) / (by - ay) * (bx - ax));
            }
        }
        crossings.sort_by(f32::total_cmp);
        let source_row = usize::try_from(row - y).ok();
        for pair in crossings.as_chunks::<2>().0 {
            let (from, to) = ((pair[0] - 0.5).ceil() as i32, (pair[1] - 0.5).ceil() as i32);
            match (runs, source_row) {
                (Some(runs), Some(r)) if r < runs.len() => {
                    for &(a, b) in &runs[r] {
                        let (a, b) = (from.max(x + i32::from(a)), to.min(x + i32::from(b)));
                        if a < b {
                            blit_row(target, source, (x, y), row, (a, b), k, tint);
                        }
                    }
                }
                _ => blit_row(target, source, (x, y), row, (from, to), k, tint),
            }
        }
    }
}

/// A run's white coverage over `target` in `color` at `opacity`, its
/// top-left corner at (x, y).
fn blit_coverage(target: &mut Pixmap, coverage: &Pixmap, x: i32, y: i32, color: Color, opacity: f32) {
    let (tw, th) = (target.width() as i32, target.height() as i32);
    let (sw, sh) = (coverage.width() as i32, coverage.height() as i32);
    let (x0, y0, x1, y1) = (x.max(0), y.max(0), (x + sw).min(tw), (y + sh).min(th));
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let f = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    let k = (color.a * opacity).clamp(0.0, 1.0);
    let k = (k * 256.0) as u32;
    let rgb = [f(color.r), f(color.g), f(color.b)];
    let src = coverage.data();
    let dst = target.data_mut();
    for row in y0..y1 {
        let s_row = &src[(((row - y) * sw + (x0 - x)) * 4) as usize..][..((x1 - x0) * 4) as usize];
        let d_row = &mut dst[((row * tw + x0) * 4) as usize..][..((x1 - x0) * 4) as usize];
        for (d, s) in d_row.as_chunks_mut::<4>().0.iter_mut().zip(s_row.as_chunks::<4>().0) {
            let a = (u32::from(s[3]) * k) >> 8;
            if a == 0 {
                continue;
            }
            let keep = 255 - a;
            for i in 0..3 {
                d[i] = ((rgb[i] * a + 127) / 255 + (u32::from(d[i]) * keep + 127) / 255).min(255) as u8;
            }
            d[3] = (a + (u32::from(d[3]) * keep + 127) / 255).min(255) as u8;
        }
    }
}

fn rect_path(x: f32, y: f32, w: f32, h: f32) -> Option<tiny_skia::Path> {
    Rect::from_xywh(x, y, w.max(0.0), h.max(0.0)).map(PathBuilder::from_rect)
}

fn polygon_path(points: &[(f32, f32)]) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    for (i, &(x, y)) in points.iter().enumerate() {
        if i == 0 {
            pb.move_to(x, y)
        } else {
            pb.line_to(x, y)
        }
    }
    pb.close();
    pb.finish()
}

/// An arc from `from` to `to` radians, measured clockwise from 12 o'clock.
fn arc_path(x: f32, y: f32, r: f32, from: f32, to: f32) -> Option<tiny_skia::Path> {
    let (a0, a1) = (from - FRAC_PI_2, to - FRAC_PI_2);
    let steps = (((a1 - a0).abs() * r.max(1.0) / 3.0).ceil() as usize).clamp(2, 512);
    let mut pb = PathBuilder::new();
    for i in 0..=steps {
        let a = a0 + (a1 - a0) * i as f32 / steps as f32;
        let (px, py) = (x + r * a.cos(), y + r * a.sin());
        if i == 0 {
            pb.move_to(px, py)
        } else {
            pb.line_to(px, py)
        }
    }
    pb.finish()
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, radius: f32) -> Option<tiny_skia::Path> {
    let r = radius.min(w / 2.0).min(h / 2.0);
    // A cubic's handle for a quarter circle.
    let k = r * 0.552_284_8;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

impl Painter for SoftPainter<'_> {
    /// The arc's colours at once: each pixel of its band coloured by its
    /// angle, rather than an arc stroked for every 3 degrees.
    fn gradient_arc(&mut self, x: f32, y: f32, r: f32, from: f32, to: f32, start: Color, end: Color, width: f32) {
        let State { transform: t, alpha } = self.state();
        if t.kx != 0.0 || t.ky != 0.0 || (t.sx - t.sy).abs() > 1e-3 || t.sx <= 0.0 || (to - from).abs() < 1e-6 {
            return gradient_arcs(self, x, y, r, from, to, start, end, width);
        }
        let started = span("gradient_arc");
        let (cx, cy) = (t.sx * x + t.tx, t.sy * y + t.ty);
        let (radius, half) = (r * t.sx, width * t.sx / 2.0);
        let (lo, hi) = (from.min(to), from.max(to));
        let middle = (lo + hi) / 2.0;
        let (inner, outer) = ((radius - half - 1.0).max(0.0), radius + half + 1.0);
        // The arc's box: its ends at both radii, and the outer radius at each
        // quarter turn it passes.
        let point = |angle: f32, r: f32| (cx + r * angle.sin(), cy - r * angle.cos());
        let mut corners = vec![point(lo, inner), point(lo, outer), point(hi, inner), point(hi, outer)];
        let first_quarter = (lo / FRAC_PI_2).ceil() as i32;
        let last_quarter = (hi / FRAC_PI_2).floor() as i32;
        for q in first_quarter..=last_quarter.min(first_quarter + 4) {
            corners.push(point(q as f32 * FRAC_PI_2, outer));
        }
        let (left, right) = corners.iter().fold((f32::MAX, f32::MIN), |(l, r), p| (l.min(p.0), r.max(p.0)));
        let (top, bottom) = corners.iter().fold((f32::MAX, f32::MIN), |(t, b), p| (t.min(p.1), b.max(p.1)));
        let target = self.target();
        let (w, h) = (target.width() as i32, target.height() as i32);
        let (box_left, box_right) = (((left - 1.0).floor() as i32).max(0), ((right + 1.0).ceil() as i32).min(w));
        let data = target.data_mut();
        let rows = ((top - 1.0).floor() as i32).max(0)..((bottom + 1.0).ceil() as i32).min(h);
        for row in rows {
            let dy = row as f32 + 0.5 - cy;
            let span = |r: f32| (r * r - dy * dy).max(0.0).sqrt();
            let (far, near) = (span(outer), if dy.abs() < inner { span(inner) } else { 0.0 });
            // The band's two runs in this row: left and right of the hole.
            // The second starts past the first's last column, which they share
            // when the row has no hole and the centre is not on a pixel edge.
            let left_end = ((cx - near).ceil() as i32).min(box_right);
            for (first, last) in [
                ((cx - far).floor() as i32, left_end),
                (((cx + near).floor() as i32).max(left_end), (cx + far).ceil() as i32),
            ] {
                for col in first.max(box_left)..last.min(box_right) {
                    let dx = col as f32 + 0.5 - cx;
                    let d = (dx * dx + dy * dy).sqrt();
                    let radial = (half + 0.5 - (d - radius).abs()).clamp(0.0, 1.0);
                    if radial <= 0.0 {
                        continue;
                    }
                    // Clockwise from 12 o'clock, taken nearest the arc.
                    let mut angle = atan2(dx, -dy);
                    angle += TAU * ((middle - angle) / TAU).round();
                    let ends = (((angle - lo).min(hi - angle)) * d + 0.5).clamp(0.0, 1.0);
                    let cover = radial * ends;
                    if cover <= 0.0 {
                        continue;
                    }
                    let k = ((angle - from) / (to - from)).clamp(0.0, 1.0);
                    let mix = |p: f32, q: f32| p + (q - p) * k;
                    let a = mix(start.a, end.a) * alpha * cover;
                    if a <= 0.0 {
                        continue;
                    }
                    let px = &mut data[((row * w + col) * 4) as usize..][..4];
                    let keep = 1.0 - a;
                    for (c, v) in
                        px.iter_mut().zip([mix(start.r, end.r), mix(start.g, end.g), mix(start.b, end.b), 1.0])
                    {
                        *c = (255.0 * v * a + f32::from(*c) * keep + 0.5).min(255.0) as u8;
                    }
                }
            }
        }
        self.r.timed(started);
    }

    /// The glow's discs at once: each pixel's alpha from its distance to the
    /// centre, through the discs' summed coverage, in one pass rather than
    /// one per disc (which on a slow CPU takes seconds).
    fn radial_glow(&mut self, x: f32, y: f32, radius: f32, steps: u32) {
        let State { transform: t, alpha } = self.state();
        if t.kx != 0.0 || t.ky != 0.0 || (t.sx - t.sy).abs() > 1e-3 || t.sx <= 0.0 {
            for i in 0..steps {
                let f = i as f32 / steps as f32;
                self.fill_circle(x, y, radius * (1.0 - f).powf(1.4), Color { a: 5.0 / 255.0, ..Color::WHITE });
            }
            return;
        }
        let started = span("radial_glow");
        let (cx, cy) = (t.sx * x + t.tx, t.sy * y + t.ty);
        let reach = radius * t.sx;
        let radii: Vec<f32> = (0..steps).map(|i| reach * (1.0 - i as f32 / steps as f32).powf(1.4)).collect();
        // The glow's alpha by distance, a sample a quarter pixel apart: a
        // disc covers a pixel as far as its edge passes the pixel's centre
        // (the anti-aliased edge a pixel wide).
        let disc = 5.0 / 255.0 * alpha;
        let profile: Vec<f32> = (0..=(reach * 4.0).ceil() as usize + 4)
            .map(|q| {
                let d = q as f32 / 4.0;
                1.0 - radii.iter().fold(1.0, |keep, r| keep * (1.0 - disc * (r - d + 0.5).clamp(0.0, 1.0)))
            })
            .collect();
        let target = self.target();
        let (w, h) = (target.width() as i32, target.height() as i32);
        let (x0, x1) = (((cx - reach - 1.0).floor() as i32).max(0), ((cx + reach + 1.0).ceil() as i32).min(w));
        let (y0, y1) = (((cy - reach - 1.0).floor() as i32).max(0), ((cy + reach + 1.0).ceil() as i32).min(h));
        let data = target.data_mut();
        for row in y0..y1 {
            let dy = row as f32 + 0.5 - cy;
            for col in x0..x1 {
                let dx = col as f32 + 0.5 - cx;
                let q = ((dx * dx + dy * dy).sqrt() * 4.0) as usize;
                let Some(&a) = profile.get(q) else { continue };
                if a <= 0.0 {
                    continue;
                }
                let px = &mut data[((row * w + col) * 4) as usize..][..4];
                let keep = 1.0 - a;
                for c in px.iter_mut() {
                    *c = (255.0 * a + f32::from(*c) * keep + 0.5).min(255.0) as u8;
                }
            }
        }
        self.r.timed(started);
    }

    fn fill_circle(&mut self, x: f32, y: f32, r: f32, color: Color) {
        self.fill(PathBuilder::from_circle(x, y, r.max(0.01)), color);
    }

    fn stroke_circle(&mut self, x: f32, y: f32, r: f32, color: Color, width: f32) {
        self.stroke(PathBuilder::from_circle(x, y, r.max(0.01)), color, width, LineCap::Butt);
    }

    fn arc(&mut self, x: f32, y: f32, r: f32, from: f32, to: f32, color: Color, width: f32) {
        self.stroke(arc_path(x, y, r, from, to), color, width, LineCap::Butt);
    }

    fn line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, color: Color, width: f32, round: bool) {
        let mut pb = PathBuilder::new();
        pb.move_to(x1, y1);
        pb.line_to(x2, y2);
        self.stroke(pb.finish(), color, width, if round { LineCap::Round } else { LineCap::Butt });
    }

    fn lines(&mut self, segments: &[[f32; 4]], color: Color, width: f32) {
        let mut pb = PathBuilder::new();
        for [x1, y1, x2, y2] in segments {
            pb.move_to(*x1, *y1);
            pb.line_to(*x2, *y2);
        }
        self.stroke(pb.finish(), color, width, LineCap::Butt);
    }

    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, color: Color) {
        let path = if radius > 0.0 { rounded_rect(x, y, w, h, radius) } else { rect_path(x, y, w, h) };
        self.fill(path, color);
    }

    fn polygon(&mut self, points: &[(f32, f32)], color: Color) {
        self.fill(polygon_path(points), color);
    }

    fn text(&mut self, text: &str, x: f32, y: f32, style: &TextStyle) -> f32 {
        let started = span("text");
        let width = self.set_text(text, x, y, style);
        self.r.timed(started);
        width
    }

    fn measure(&mut self, text: &str, style: &TextStyle) -> (f32, f32) {
        let (scale, px) = (self.r.scale, style.size * self.r.scale);
        let width = self.advance(text, style) / scale;
        let (ascent, descent) = typeset::line(self.font(&style.font), px);
        (width, (ascent + descent) / scale)
    }

    fn has_glyphs(&mut self, text: &str, style: &TextStyle) -> bool {
        let font = self.font(&style.font);
        text.chars().all(|c| font.lookup_glyph_index(c) != 0)
    }

    fn image(&mut self, path: &str, x: f32, y: f32, w: f32, h: f32, alpha: f32) -> Result<(), String> {
        self.load_image(path)?;
        let (iw, ih) = self.r.images[path].as_ref().map(|p| (p.width(), p.height())).expect("loaded above");
        // Upright, the image scaled to its pixels once and copied after.
        let State { transform: t, alpha: faded } = self.state();
        if t.kx == 0.0 && t.ky == 0.0 && t.sx > 0.0 && t.sy > 0.0 {
            let started = span("text");
            let (dw, dh) = ((w * t.sx).round() as u32, (h * t.sy).round() as u32);
            let r = &mut *self.r;
            let fresh = r.scaled.get(path).is_some_and(|p| (p.width(), p.height()) == (dw, dh));
            if !fresh {
                let Some(scaled) = Pixmap::new(dw.max(1), dh.max(1)) else {
                    return Ok(());
                };
                let mut scaled = scaled;
                let source = r.images[path].as_ref().expect("loaded above");
                let paint = PixmapPaint { quality: FilterQuality::Bilinear, ..PixmapPaint::default() };
                scaled.draw_pixmap(
                    0,
                    0,
                    source.as_ref(),
                    &paint,
                    Transform::from_scale(dw as f32 / iw as f32, dh as f32 / ih as f32),
                    None,
                );
                r.scaled.insert(path.to_string(), scaled);
            }
            r.shapes += 1;
            let target = match self.layer.as_mut() {
                Some(l) => l,
                None => &mut r.pixmap,
            };
            blit(
                target,
                &r.scaled[path],
                (x * t.sx + t.tx).round() as i32,
                (y * t.sy + t.ty).round() as i32,
                alpha * faded,
            );
            r.timed(started);
            return Ok(());
        }
        let pattern = Transform::from_translate(x, y).pre_scale(w / iw as f32, h / ih as f32);
        self.fill_with(rect_path(x, y, w, h), path, pattern, alpha, FilterQuality::Bilinear);
        Ok(())
    }

    fn sprite(&mut self, path: &str, frame: u32, columns: u32, w: f32, h: f32, x: f32, y: f32) -> Result<u32, String> {
        self.load_image(path)?;
        let ih = self.r.images[path].as_ref().map(|p| p.height()).expect("loaded above");
        let columns = columns.max(1);
        let frames = columns * (ih as f32 / h).floor().max(1.0) as u32;
        let k = frame % frames;
        let (col, row) = ((k % columns) as f32, (k / columns) as f32);
        let pattern = Transform::from_translate(x - col * w, y - row * h);
        self.fill_with(rect_path(x, y, w, h), path, pattern, 1.0, FilterQuality::Bilinear);
        Ok(frames)
    }

    fn save(&mut self) {
        let s = self.state();
        self.stack.push(s);
    }

    fn restore(&mut self) {
        if self.stack.len() > 1 {
            self.stack.pop();
        }
    }

    fn translate(&mut self, x: f32, y: f32) {
        if let Some(s) = self.stack.last_mut() {
            s.transform = s.transform.pre_translate(x, y);
        }
    }

    fn rotate(&mut self, angle: f32) {
        if let Some(s) = self.stack.last_mut() {
            s.transform = s.transform.pre_concat(Transform::from_rotate(angle.to_degrees()));
        }
    }

    fn scale(&mut self, x: f32, y: f32) {
        if let Some(s) = self.stack.last_mut() {
            s.transform = s.transform.pre_scale(x, y);
        }
    }

    fn alpha(&mut self, alpha: f32) {
        if let Some(s) = self.stack.last_mut() {
            s.alpha *= alpha.clamp(0.0, 1.0);
        }
    }

    fn clear(&mut self, color: Color) {
        let c = tiny_skia::Color::from_rgba(color.r, color.g, color.b, color.a).unwrap_or(tiny_skia::Color::BLACK);
        self.target().fill(c);
    }

    fn layer(
        &mut self,
        name: &str,
        key: &str,
        size: Option<(f32, f32)>,
        draw: &mut dyn FnMut(&mut dyn Painter) -> Result<(), String>,
    ) -> Result<(), String> {
        let size = size.unwrap_or((self.r.design, self.r.design));
        self.r.used.insert((true, name.to_string()), self.r.frame);
        let px = |v: f32| (v * self.r.scale).ceil().max(1.0) as u32;
        let pixels = (px(size.0), px(size.1));
        if self.r.layers.get(name).is_some_and(|l| l.key == key && (l.pixmap.width(), l.pixmap.height()) == pixels) {
            return Ok(());
        }
        let redraw = span("layer redraw");
        let pixmap = Pixmap::new(pixels.0, pixels.1).ok_or_else(|| format!("layer {name}: {pixels:?}"))?;
        if key == name {
            trace!("paint: layer {name} drawn, {}x{} pixels", pixels.0, pixels.1);
        } else {
            trace!("paint: layer {name} drawn for {key:?}, {}x{} pixels", pixels.0, pixels.1);
        }
        // Drawn in its own pixels, from its top-left corner, at full opacity.
        let outer_layer = self.layer.replace(pixmap);
        let scale = self.r.scale;
        let outer_stack = std::mem::replace(
            &mut self.stack,
            vec![State { transform: Transform::from_scale(scale, scale), alpha: 1.0 }],
        );
        let drawn = draw(self);
        let pixmap = std::mem::replace(&mut self.layer, outer_layer).expect("the layer was set above");
        self.stack = outer_stack;
        let runs = runs(&pixmap);
        self.r.layers.insert(name.to_string(), Layer { key: key.to_string(), pixmap, runs, size, tinted: Vec::new() });
        // The redraw's own operations count twice: in their buckets and in
        // this one, which takes the whole of it.
        if let Some((operation, started)) = redraw {
            let total = self.r.profile.entry(operation).or_default();
            total.0 += 1;
            total.1 += started.elapsed();
        }
        drawn
    }

    fn paint_layer(&mut self, name: &str, tint: Color, clip: Option<&[(f32, f32)]>) -> Result<(), String> {
        self.r.used.insert((true, name.to_string()), self.r.frame);
        let s = self.state();
        let scale = self.r.scale;
        let Some(layer) = self.r.layers.get_mut(name) else {
            return Err(format!("no layer {name}"));
        };
        let (w, h) = layer.size;
        // Drawn at the panel's pixels: placed one to one, on whole pixels
        // where the theme is upright. A layer drawn at the frame's scale
        // takes whole pixels (its size rounded up), so it is one to one
        // whenever the transform is that scale, however its size rounds.
        let t = s.transform;
        let mut at = t.pre_scale(w / layer.pixmap.width() as f32, h / layer.pixmap.height() as f32);
        let upright = t.kx == 0.0 && t.ky == 0.0;
        if upright && (t.sx - scale).abs() < 1e-4 && (t.sy - scale).abs() < 1e-4 {
            at = Transform::from_translate(t.tx.round(), t.ty.round());
        } else if (at.sx - 1.0).abs() < 1e-3 && (at.sy - 1.0).abs() < 1e-3 && upright {
            at = Transform::from_translate(at.tx.round(), at.ty.round());
        }
        // On whole pixels the tint multiplies the layer as it is copied;
        // elsewhere a tinted copy is painted, kept for the few tints in use.
        let whole = whole_pixels(&at);
        let inline = (tint != Color::WHITE && whole.is_some()).then(|| multipliers(tint));
        let copied = tint != Color::WHITE && whole.is_none();
        if copied {
            match layer.tinted.iter().position(|(t, _)| *t == tint) {
                Some(i) => layer.tinted[..=i].rotate_right(1),
                None => {
                    let started = span("tinted copy");
                    layer.tinted.insert(0, (tint, tinted(&layer.pixmap, tint)));
                    layer.tinted.truncate(TINTS);
                    self.r.timed(started);
                }
            }
        }
        let started = span("paint_layer");
        self.r.shapes += 1;
        let r = &mut *self.r;
        let layer = &r.layers[name];
        let source = if copied { &layer.tinted[0].1 } else { &layer.pixmap };
        let target = match self.layer.as_mut() {
            Some(l) => l,
            None => &mut r.pixmap,
        };
        if let (Some(points), Some(at)) = (clip, whole) {
            let device: Vec<(f32, f32)> = points
                .iter()
                .map(|&(px, py)| {
                    let mut p = [tiny_skia::Point::from_xy(px, py)];
                    s.transform.map_points(&mut p);
                    (p[0].x, p[0].y)
                })
                .collect();
            blit_polygon(target, source, Some(&layer.runs), at, &device, s.alpha, inline);
            r.timed_as(started, Some("paint_layer clipped"));
            return Ok(());
        }
        let path = match (clip.and_then(polygon_path), whole) {
            // Only the clip's pixels: the layer carries its own soft edges,
            // and clips fall between its marks.
            (Some(path), _) => {
                let paint = Paint {
                    shader: Pattern::new(
                        source.as_ref(),
                        SpreadMode::Pad,
                        FilterQuality::Nearest,
                        s.alpha,
                        at.post_concat(s.transform.invert().unwrap_or_default()),
                    ),
                    anti_alias: false,
                    ..Paint::default()
                };
                target.fill_path(&path, &paint, FillRule::Winding, s.transform, None);
                "paint_layer pattern"
            }
            // On whole pixels, copied over directly; else through tiny-skia.
            (None, Some(at)) => {
                blit_runs(target, source, &layer.runs, at, s.alpha, inline);
                "paint_layer runs"
            }
            (None, None) => {
                blit_transformed(target, source, at, s.alpha);
                "paint_layer transformed"
            }
        };
        r.timed_as(started, Some(path));
        Ok(())
    }

    fn warp_layer(&mut self, name: &str, corners: &[(f32, f32); 4]) -> Result<(), String> {
        self.r.used.insert((true, name.to_string()), self.r.frame);
        if !self.r.layers.contains_key(name) {
            return Err(format!("no layer {name}"));
        }
        let started = span("warp_layer");
        let s = self.state();
        self.r.shapes += 1;
        let device = corners.map(|(x, y)| {
            let mut p = [tiny_skia::Point::from_xy(x, y)];
            s.transform.map_points(&mut p);
            (p[0].x, p[0].y)
        });
        let r = &mut *self.r;
        let target = match self.layer.as_mut() {
            Some(l) => l,
            None => &mut r.pixmap,
        };
        blit_warped(target, &r.layers[name].pixmap, &device, s.alpha);
        r.timed(started);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one-pass gradient arc is the 3-degree arcs' own drawing, but for
    /// their seams.
    #[test]
    fn a_gradient_arc_is_its_arcs_at_once() {
        let (start, end) = (Color { r: 1.0, g: 0.5, b: 0.2, a: 0.0 }, Color { r: 0.2, g: 0.6, b: 1.0, a: 1.0 });
        for (from, to) in [(-2.4_f32, -1.2_f32), (1.0, 2.6), (2.6, 1.0), (-0.5, 0.7)] {
            let mut ours = Renderer::new(160, 160).unwrap();
            let mut theirs = Renderer::new(160, 160).unwrap();
            for r in [&mut ours, &mut theirs] {
                r.begin(160.0, 480.0);
            }
            ours.painter(Path::new("")).gradient_arc(240.0, 240.0, 200.0, from, to, start, end, 14.0);
            gradient_arcs(&mut theirs.painter(Path::new("")), 240.0, 240.0, 200.0, from, to, start, end, 14.0);
            let diffs: Vec<u8> =
                ours.pixmap.data().iter().zip(theirs.pixmap.data()).map(|(a, b)| a.abs_diff(*b)).collect();
            let mean = diffs.iter().map(|&d| f64::from(d)).sum::<f64>() / diffs.len() as f64;
            let drawn = |r: &Renderer| r.pixmap.data().iter().filter(|&&v| v > 8).count();
            assert!(mean < 1.0, "from {from} to {to}: {mean:.3} apart on average");
            let (a, b) = (drawn(&ours), drawn(&theirs));
            assert!(a.abs_diff(b) * 10 < b, "from {from} to {to}: {a} against {b} drawn");
        }
    }

    /// A translucent arc over 12 o'clock is as even across its centre column
    /// as beside it, wherever its centre falls.
    #[test]
    fn a_gradient_arc_blends_each_pixel_once() {
        let color = Color { r: 1.0, g: 1.0, b: 1.0, a: 0.5 };
        for centre in [240.0_f32, 241.0, 241.5] {
            let mut r = Renderer::new(160, 160).unwrap();
            r.begin(160.0, 480.0);
            r.painter(Path::new("")).gradient_arc(centre, 240.0, 200.0, -0.5, 0.5, color, color, 14.0);
            let cx = (centre / 3.0) as u32;
            let row = 80 - 200 / 3;
            let level = |x: u32| r.pixmap.pixel(x, row).unwrap().red();
            assert!(level(cx) > 100, "centre {centre}: nothing drawn");
            assert!(
                level(cx).abs_diff(level(cx - 1)) <= 2 && level(cx).abs_diff(level(cx + 1)) <= 2,
                "centre {centre}"
            );
        }
    }

    /// The one-pass glow is the discs' own drawing, but for rounding.
    #[test]
    fn a_glow_is_its_discs_at_once() {
        let mut ours = Renderer::new(160, 160).unwrap();
        let mut theirs = Renderer::new(160, 160).unwrap();
        for r in [&mut ours, &mut theirs] {
            r.begin(160.0, 480.0);
        }
        ours.painter(Path::new("")).radial_glow(240.0, 250.0, 210.0, 120);
        let mut discs = theirs.painter(Path::new(""));
        for i in 0..120 {
            let f = i as f32 / 120.0;
            discs.fill_circle(240.0, 250.0, 210.0 * (1.0 - f).powf(1.4), Color { a: 5.0 / 255.0, ..Color::WHITE });
        }
        // The discs round to whole levels as each is blended, so they lose a
        // little toward the bright middle; the glow at once does not.
        let diffs: Vec<u8> = ours.pixmap.data().iter().zip(theirs.pixmap.data()).map(|(a, b)| a.abs_diff(*b)).collect();
        let mean = diffs.iter().map(|&d| f64::from(d)).sum::<f64>() / diffs.len() as f64;
        let worst = diffs.iter().max().copied().unwrap_or(0);
        assert!(mean < 2.0 && worst <= 16, "the glow differs from its discs by {mean:.2} on average, {worst} at most");
        let centre = |r: &Renderer| r.pixmap.pixel(80, 83).unwrap().red();
        assert!(centre(&ours) > 200 && centre(&ours).abs_diff(centre(&theirs)) <= 16);
    }

    #[test]
    fn rotated_and_clipped_pixmaps_land_where_tiny_skia_puts_them() {
        let mut source = Pixmap::new(200, 120).unwrap();
        for (i, px) in source.pixels_mut().iter_mut().enumerate() {
            let (x, y) = (i % 200, i / 200);
            let a = if (x / 10 + y / 10) % 3 == 0 { 0 } else { 255 };
            *px =
                tiny_skia::PremultipliedColorU8::from_rgba((x % 256) as u8 & a, (y * 2) as u8 & a, 90 & a, a).unwrap();
        }
        let at = Transform::from_translate(160.0, 160.0).pre_rotate(37.0).pre_translate(-100.0, -60.0);
        let mut ours = Pixmap::new(320, 320).unwrap();
        blit_transformed(&mut ours, &source, at, 1.0);
        let mut theirs = Pixmap::new(320, 320).unwrap();
        let paint = PixmapPaint { quality: FilterQuality::Nearest, ..PixmapPaint::default() };
        theirs.draw_pixmap(0, 0, source.as_ref(), &paint, at, None);
        let differ = ours.pixels().iter().zip(theirs.pixels()).filter(|(a, b)| a != b).count();
        // Only pixels whose centres fall on a source pixel's edge may round apart.
        assert!(differ < 320, "{differ} pixels differ");

        let clip = [(40.0, 30.0), (200.0, 50.0), (180.0, 125.0), (60.0, 120.0)];
        let mut ours = Pixmap::new(320, 320).unwrap();
        blit_polygon(&mut ours, &source, None, (20, 10), &clip, 1.0, None);
        let mut theirs = Pixmap::new(320, 320).unwrap();
        let paint = Paint {
            shader: Pattern::new(
                source.as_ref(),
                SpreadMode::Pad,
                FilterQuality::Nearest,
                1.0,
                Transform::from_translate(20.0, 10.0),
            ),
            anti_alias: false,
            ..Paint::default()
        };
        theirs.fill_path(&polygon_path(&clip).unwrap(), &paint, FillRule::Winding, Transform::identity(), None);
        let differ = ours.pixels().iter().zip(theirs.pixels()).filter(|(a, b)| a != b).count();
        assert!(differ < 40, "{differ} pixels differ");
    }

    #[test]
    fn shapes_and_text_land_where_the_theme_maps_them() {
        let mut r = Renderer::new(320, 320).unwrap();
        r.begin(320.0, 480.0);
        {
            let mut p = r.painter(Path::new("."));
            p.rect(0.0, 0.0, 240.0, 240.0, 0.0, Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 });
            let style = TextStyle {
                font: "fallback".into(),
                size: 40.0,
                color: Color { r: 0.0, g: 1.0, b: 0.0, a: 1.0 },
                align: Align::Center,
                baseline: Baseline::Alphabetic,
                spacing: 0.0,
                outline: None,
                snap: true,
            };
            p.text("HELLO", 360.0, 400.0, &style);
        }
        let at = |x: u32, y: u32| r.pixmap.pixel(x, y).unwrap();
        // Design 480 maps onto 320: the red square fills the top-left 160.
        assert_eq!((at(10, 10).red(), at(150, 150).red(), at(170, 170).red()), (255, 255, 0));
        // The text is drawn around (240, 267) in pixels.
        let green =
            (200..280).flat_map(|x| (240..275).map(move |y| (x, y))).filter(|&(x, y)| at(x, y).green() > 128).count();
        assert!(green > 50, "text drawn: {green} pixels");
    }

    #[test]
    fn layers_are_drawn_once_and_painted_tinted() {
        let mut r = Renderer::new(320, 320).unwrap();
        r.begin(320.0, 480.0);
        let mut p = r.painter(Path::new("."));
        let white = Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };
        let mut drawn = 0;
        for _ in 0..2 {
            p.layer("ring", "k", None, &mut |q| {
                drawn += 1;
                q.fill_circle(240.0, 240.0, 100.0, white);
                Ok(())
            })
            .unwrap();
        }
        p.paint_layer("ring", Color { r: 0.0, g: 0.0, b: 1.0, a: 1.0 }, None).unwrap();
        drop(p);
        assert_eq!(drawn, 1, "the same key draws once");
        let c = r.pixmap.pixel(160, 160).unwrap();
        assert_eq!((c.red(), c.blue()), (0, 255));
        // Clipped to the left half, in red: only there.
        r.begin(320.0, 480.0);
        let mut p = r.painter(Path::new("."));
        p.paint_layer(
            "ring",
            Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 },
            Some(&[(0.0, 0.0), (240.0, 0.0), (240.0, 480.0), (0.0, 480.0)]),
        )
        .unwrap();
        drop(p);
        let (left, right) = (r.pixmap.pixel(120, 160).unwrap(), r.pixmap.pixel(200, 160).unwrap());
        assert_eq!(
            (left.red(), right.red(), right.alpha()),
            (255, 0, 255),
            "right of the clip stays the cleared black"
        );
    }
}
