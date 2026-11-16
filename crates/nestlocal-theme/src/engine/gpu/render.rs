//! The GPU painter: themes' drawing, through femtovg, on GLES2.

use std::collections::HashMap;
use std::f32::consts::FRAC_PI_2;
use std::path::Path;
use std::time::Instant;

use log::{debug, trace, warn};

use femtovg::imgref::Img;
use femtovg::renderer::OpenGl;
use femtovg::renderer::RenderTarget;
use femtovg::rgb::RGBA8;
use femtovg::{Canvas, ImageFlags, ImageId, LineCap, Paint, Path as Shape, PixelFormat, Solidity};

use super::quads::Quads;
use crate::engine::display::{Geometry, Step};
use crate::engine::paint::{perspective_weights, Align, Baseline, Color, Painter, TextStyle};
use crate::engine::typeset;

/// A canvas and what it has loaded for the current theme.
pub struct Renderer {
    pub canvas: Canvas<OpenGl>,
    fonts: typeset::Fonts,
    images: HashMap<String, Option<ImageId>>,
    /// Images asked for and not yet decoded: a thread decodes them, as a
    /// unit picture takes the render thread a good part of a second.
    decoding: std::collections::HashSet<String>,
    decode: std::sync::mpsc::Sender<(String, std::path::PathBuf)>,
    decoded: std::sync::mpsc::Receiver<(String, Option<Decoded>)>,
    layers: HashMap<String, Layer>,
    /// Runs of text set on the CPU, as images.
    runs: HashMap<String, CachedRun>,
    /// Counts lookups, to drop the run least recently drawn.
    tick: u64,
    glyphs: typeset::Glyphs,
    /// How far runs of text advance, in pixels, by the same keys as `runs`.
    advances: HashMap<String, f32>,
    /// Shapes filled or stroked since the count was last taken: each is a
    /// draw call or more.
    pub shapes: u32,
    /// Time spent in the painter since last taken, the theme's own excluded.
    pub painting: std::time::Duration,
    /// Layers drawn again since last taken, the time they took (within
    /// `painting`), and femtovg flushes: each is a pass of GL state and
    /// draw calls, costly on the unit's driver.
    pub layers_drawn: u32,
    pub layer_time: std::time::Duration,
    pub flushes: u32,
    /// Textured shapes for the screen, batched; none where its program
    /// could not be built, and femtovg draws them.
    quads: Option<Quads>,
    /// Whether femtovg holds drawing not yet flushed, which the next quads
    /// must come after.
    queued: bool,
    /// The frame's design units, and pixels to a design unit.
    design: f32,
    scale: f32,
    /// White discs and rounded rectangles by their sizes in quarter
    /// pixels, with when each was last drawn: filled, they go through the
    /// quads, whose few instructions a pixel cost the unit's GPU far less
    /// than femtovg's fill program.
    cached: HashMap<ShapeKey, (ImageId, u64)>,
    /// One white pixel: rectangles on whole pixels, as quads.
    white: Option<ImageId>,
    /// The colour the panel is cleared to before anything is drawn on it:
    /// a theme's own clear first thing replaces the frame's, one clear in all.
    clear: Option<Color>,
    /// Whether the stencil has been cleared: femtovg leaves it as it found
    /// it, so the panel's needs clearing only once.
    stencil_cleared: bool,
    /// Where on the framebuffer frames are drawn, and their size: the
    /// theme's square, as stock's window, not the whole panel.
    window: (f32, f32, u32, u32),
    /// The texture each frame is drawn into before it goes onto the panel
    /// in one opaque pass, and what stands for the panel while drawing:
    /// it, or the panel itself where the quads are not to be had. The
    /// unit's driver, switching between a layer and the panel mid-frame,
    /// at times writes into the page the panel is showing; with the frame
    /// drawn apart, the panel's page is only written by that last pass.
    frame: Option<ImageId>,
    screen: RenderTarget,
    external: bool,
}

/// An image's pixels, decoded off the render thread.
struct Decoded {
    width: usize,
    height: usize,
    rgba: Vec<RGBA8>,
}

/// Decodes images as they are asked for, in order, until the renderer goes.
fn decode_images(
    asked: std::sync::mpsc::Receiver<(String, std::path::PathBuf)>,
    done: std::sync::mpsc::Sender<(String, Option<Decoded>)>,
) {
    while let Ok((name, file)) = asked.recv() {
        let started = Instant::now();
        let decoded = std::fs::read(&file).ok().and_then(|data| image::load_from_memory(&data).ok()).map(|image| {
            let rgba = image.into_rgba8();
            let (width, height) = (rgba.width() as usize, rgba.height() as usize);
            let rgba = rgba.into_raw().as_chunks::<4>().0.iter().map(|&[r, g, b, a]| RGBA8::new(r, g, b, a)).collect();
            Decoded { width, height, rgba }
        });
        match &decoded {
            Some(d) => {
                debug!("paint: image {name} decoded, {}x{} in {} ms", d.width, d.height, started.elapsed().as_millis())
            }
            None => debug!("paint: image {name} could not be loaded"),
        }
        if done.send((name, decoded)).is_err() {
            break;
        }
    }
}

/// A white shape's size in quarter pixels: a disc's radius, or a rounded
/// rectangle's width, height and corner radius.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum ShapeKey {
    Disc(u32),
    Rounded(u32, u32, u32),
    /// A ring's cross-section, `n` texels across: clear at each end.
    Ring(u32),
}

/// At most this many shapes are kept from one frame to the next.
const SHAPES: usize = 24;

/// A run of text as an image, its placement in pixels from the pen.
struct CachedRun {
    image: Option<ImageId>,
    left: i32,
    top: i32,
    width: usize,
    height: usize,
    used: u64,
}

/// At most this many runs of text are kept.
const RUNS: usize = 96;

/// An offscreen drawing of the theme, and what it was drawn for.
struct Layer {
    key: String,
    image: ImageId,
    /// Its size in pixels, and in design units.
    pixels: (usize, usize),
    size: (f32, f32),
    /// Whether every pixel of it is opaque: drawn first with an opaque
    /// clear, it needs no blending.
    opaque: bool,
}

impl Renderer {
    pub fn new(canvas: Canvas<OpenGl>, quads: Option<Quads>, window: (f32, f32, u32, u32)) -> Result<Renderer, String> {
        let (decode, asked) = std::sync::mpsc::channel();
        let (done, decoded) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("images".into())
            .spawn(move || decode_images(asked, done))
            .map_err(|e| format!("image decoder thread: {e}"))?;
        Ok(Renderer {
            canvas,
            fonts: typeset::Fonts::new()?,
            images: HashMap::new(),
            decoding: std::collections::HashSet::new(),
            decode,
            decoded,
            layers: HashMap::new(),
            runs: HashMap::new(),
            tick: 0,
            glyphs: typeset::Glyphs::default(),
            advances: HashMap::new(),
            shapes: 0,
            painting: std::time::Duration::ZERO,
            layers_drawn: 0,
            layer_time: std::time::Duration::ZERO,
            flushes: 0,
            quads,
            queued: false,
            design: 480.0,
            scale: 1.0,
            cached: HashMap::new(),
            white: None,
            clear: None,
            stencil_cleared: false,
            window,
            frame: None,
            screen: RenderTarget::Screen,
            external: false,
        })
    }

    /// Draws the batched quads, which come after everything femtovg has
    /// already drawn, onto what femtovg last drew into: the panel, or a
    /// layer, `size` pixels.
    fn draw_quads(&mut self, size: (u32, u32)) {
        if let Some(quads) = self.quads.as_mut().filter(|q| !q.is_empty()) {
            // SAFETY: the context is current and the batched images live
            // until the end of the frame at least.
            unsafe { quads.draw(size.0, size.1) };
        }
    }

    /// Draws onto the canvas's external screen target without a full-frame copy.
    #[allow(dead_code)] // Used by the native nlclient theme library.
    pub fn use_external_target(&mut self, external: bool) {
        self.external = external;
    }

    /// Clears the panel, when it is still to be cleared.
    fn cleared(&mut self) {
        let Some(color) = self.clear.take() else { return };
        let premultiplied = [color.r * color.a, color.g * color.a, color.b * color.a, color.a];
        if let (Some(quads), true) = (self.quads.as_mut(), self.stencil_cleared) {
            match self.screen {
                // SAFETY: the context is current, the panel bound: nothing
                // has been drawn into a layer yet this frame.
                RenderTarget::Screen => unsafe { quads.clear(premultiplied) },
                // The frame's texture is bound to be cleared: the panel is
                // what the last frame left bound.
                RenderTarget::Image(frame) => match self.canvas.get_native_texture(frame) {
                    // SAFETY: as above; the frame's texture lives on.
                    Ok(texture) => unsafe { quads.clear_texture(texture, premultiplied) },
                    Err(_) => return,
                },
            }
            return;
        }
        {
            self.stencil_cleared = true;
            let (w, h) = (self.canvas.width(), self.canvas.height());
            self.canvas.save();
            self.canvas.reset_transform();
            self.canvas.clear_rect(0, 0, w, h, rgba(color));
            self.canvas.restore();
            self.queued = true;
        }
    }

    /// Ends a frame in its GPU texture, without presenting or reading pixels.
    pub fn finish_texture(&mut self) -> Option<glow::Texture> {
        self.cleared();
        let size = (self.canvas.width(), self.canvas.height());
        self.draw_quads(size);
        if self.queued {
            self.canvas.flush();
            self.flushes += 1;
            self.queued = false;
        }
        self.frame
            .filter(|&f| self.screen == RenderTarget::Image(f))
            .and_then(|frame| self.canvas.get_native_texture(frame).ok())
    }

    /// Ends a frame: everything drawn, in order, onto the panel.
    pub fn finish(&mut self) {
        let Some(texture) = self.finish_texture() else { return };
        if let Some(quads) = self.quads.as_mut() {
            self.canvas.set_render_target(RenderTarget::Screen);
            let (w, h) = (self.canvas.width() as f32, self.canvas.height() as f32);
            // SAFETY: the current context owns the frame texture.
            unsafe { quads.present(texture, w, h) };
        }
    }

    #[allow(dead_code)] // Used by the native nlclient theme library.
    pub fn retained_texture(&self) -> Option<glow::Texture> {
        self.frame.and_then(|frame| self.canvas.get_native_texture(frame).ok())
    }

    /// Presents the last rendered GPU frame without running the painter again.
    #[allow(dead_code)] // Used by the native nlclient theme library.
    pub fn present_retained(&mut self) -> bool {
        self.canvas.set_render_target(RenderTarget::Screen);
        let texture = self.frame.and_then(|frame| self.canvas.get_native_texture(frame).ok());
        let Some((texture, quads)) = texture.zip(self.quads.as_mut()) else {
            return false;
        };
        // SAFETY: the context owns the frame texture and is current.
        unsafe { quads.present(texture, self.window.2 as f32, self.window.3 as f32) };
        true
    }

    /// Loads a theme's fonts, replacing the previous theme's names.
    pub fn use_fonts(&mut self, fonts: &[(String, std::path::PathBuf)]) -> Result<(), String> {
        self.fonts.use_theme(fonts)?;
        if let Some(quads) = self.quads.as_mut() {
            quads.invalidate_composite();
        }
        for image in self.images.drain().filter_map(|(_, id)| id) {
            self.canvas.delete_image(image);
        }
        // Those still decoding are dropped as they come.
        self.decoding.clear();
        for (_, layer) in self.layers.drain() {
            self.canvas.delete_image(layer.image);
        }
        for image in self.runs.drain().filter_map(|(_, run)| run.image) {
            self.canvas.delete_image(image);
        }
        self.glyphs.clear();
        self.advances.clear();
        debug!("paint: {} fonts in use; images, layers and text dropped", fonts.len());
        Ok(())
    }

    /// Drops what a frame that failed left saved on the canvas (a theme's
    /// `save`s it never restored), so the next starts from its defaults.
    pub fn abandon(&mut self) {
        for _ in 0..=crate::engine::theme::SAVES {
            self.canvas.restore();
        }
    }

    /// Starts a frame: clears the panel and sets the transform from the
    /// theme's upright design units.
    pub fn begin(&mut self, geometry: &Geometry, design: f32) {
        // Discs are dropped between frames, when nothing queued uses them.
        while self.cached.len() > SHAPES {
            let Some((&key, &(image, _))) = self.cached.iter().min_by_key(|(_, (_, used))| *used) else {
                break;
            };
            self.canvas.delete_image(image);
            self.cached.remove(&key);
            trace!("paint: a cached shape dropped, {} kept", self.cached.len());
        }
        let (x, y, w, h) = self.window;
        let resized = (self.canvas.width(), self.canvas.height()) != (w, h);
        if resized || !self.external {
            // set_size queues a switch to the screen that the canvas does not
            // record: on the screen first, the frame's own target is queued
            // after it whichever the last frame ended on.
            self.canvas.set_render_target(RenderTarget::Screen);
            self.canvas.set_size(w, h, 1.0);
        }
        if self.quads.is_some() && !self.external {
            let fits =
                self.frame.and_then(|f| self.canvas.image_size(f).ok()).is_some_and(|s| s == (w as usize, h as usize));
            if !fits {
                if let Some(old) = self.frame.take() {
                    self.canvas.delete_image(old);
                }
                self.frame = self
                    .canvas
                    .create_image_empty(
                        w as usize,
                        h as usize,
                        PixelFormat::Rgba8,
                        ImageFlags::FLIP_Y | ImageFlags::PREMULTIPLIED,
                    )
                    .map_err(|e| warn!("paint: a frame texture: {e:?}; frames drawn onto the panel"))
                    .ok();
                self.stencil_cleared = false;
            }
        }
        let previous_target = self.screen;
        self.screen = if self.external {
            RenderTarget::Screen
        } else {
            self.frame.map_or(RenderTarget::Screen, RenderTarget::Image)
        };
        self.canvas.set_render_target(self.screen);
        self.canvas.reset_transform();
        self.canvas.translate(-x, -y);
        self.clear = Some(Color::BLACK);
        self.queued = resized || !self.external || previous_target != self.screen;
        self.design = design;
        self.scale = geometry.theme / design;
        for step in geometry.transform(design) {
            match step {
                Step::Translate(x, y) => self.canvas.translate(x, y),
                Step::Rotate(a) => self.canvas.rotate(a),
                Step::Scale(s) => self.canvas.scale(s, s),
            }
        }
    }

    /// Takes in the images decoded since last asked, as textures; true
    /// when any came, so the theme can be drawn again with them. Layers are
    /// dropped then: one drawn while an image was decoding lacks it, and
    /// its key, unchanged, would keep it so.
    pub fn take_decoded(&mut self) -> bool {
        let mut arrived = false;
        while let Ok((name, decoded)) = self.decoded.try_recv() {
            if !self.decoding.remove(&name) {
                continue;
            }
            let image = decoded.and_then(|d| {
                self.canvas.create_image(Img::new(d.rgba.as_slice(), d.width, d.height), ImageFlags::empty()).ok()
            });
            self.images.insert(name, image);
            arrived = true;
        }
        if arrived {
            for (_, layer) in self.layers.drain() {
                self.canvas.delete_image(layer.image);
            }
        }
        arrived
    }

    pub fn painter<'a>(&'a mut self, theme_dir: &'a Path) -> GpuPainter<'a> {
        let target = self.screen;
        GpuPainter { r: self, dir: theme_dir, alpha: vec![1.0], fb: None, target, opaque: false }
    }
}

pub struct GpuPainter<'a> {
    r: &'a mut Renderer,
    dir: &'a Path,
    /// The alpha multiplier, one per save level.
    alpha: Vec<f32>,
    /// The panel's size while drawing into a layer, for clear.
    fb: Option<(u32, u32)>,
    /// What is being drawn into: the screen, or a layer being drawn.
    target: RenderTarget,
    /// Whether the layer being drawn was cleared to an opaque colour.
    opaque: bool,
}

fn rgba(c: Color) -> femtovg::Color {
    femtovg::Color::rgbaf(c.r, c.g, c.b, c.a)
}

impl GpuPainter<'_> {
    /// Before femtovg draws: the quads batched so far go first.
    fn femtovg(&mut self) {
        self.r.cleared();
        let size = self.fb.unwrap_or((self.r.canvas.width(), self.r.canvas.height()));
        self.r.draw_quads(size);
        self.r.queued = true;
    }

    fn fill(&mut self, path: &Shape, paint: &Paint) {
        let started = Instant::now();
        self.femtovg();
        self.r.shapes += 1;
        self.r.canvas.fill_path(path, paint);
        self.r.painting += started.elapsed();
    }

    /// Strokes `path` in `color`. An opaque stroke looks the same drawn
    /// twice where it crosses itself, so it skips the stencil femtovg
    /// otherwise guards overlaps with: one pass where it takes three.
    fn stroke(&mut self, path: &Shape, mut paint: Paint, color: Color) {
        let started = Instant::now();
        self.femtovg();
        self.r.shapes += 1;
        if color.a >= 1.0 && self.alpha.last().is_none_or(|&a| a >= 1.0) {
            paint.set_stencil_strokes(false);
        }
        self.r.canvas.stroke_path(path, &paint);
        self.r.painting += started.elapsed();
    }

    /// Batches a convex polygon of `image`, its corners as (x, y, u, v) in
    /// design units and texture coordinates, tinted; false where quads do
    /// not apply (no program) and femtovg must. Quads follow femtovg's
    /// drawing, flushed here, onto the target it leaves bound.
    fn quad(&mut self, image: ImageId, corners: &[(f32, f32, f32, f32)], tint: Color, opaque: bool) -> bool {
        if self.r.quads.is_none() || self.target == RenderTarget::Image(image) {
            return false;
        }
        let Ok(texture) = self.r.canvas.get_native_texture(image) else {
            return false;
        };
        let started = Instant::now();
        self.r.cleared();
        if self.r.queued {
            self.r.canvas.flush();
            self.r.flushes += 1;
            self.r.queued = false;
        }
        let transform = self.r.canvas.transform();
        let mut points = [[0f32; 4]; 16];
        let n = corners.len().min(points.len());
        for (point, &(x, y, u, v)) in points.iter_mut().zip(corners) {
            let (px, py) = transform.transform_point(x, y);
            *point = [px, py, u, v];
        }
        let a = tint.a * self.alpha.last().copied().unwrap_or(1.0);
        if let Some(quads) = self.r.quads.as_mut() {
            quads.push(texture, &points[..n], [tint.r * a, tint.g * a, tint.b * a, a], !(opaque && a >= 1.0));
        }
        self.r.shapes += 1;
        self.r.painting += started.elapsed();
        true
    }

    /// The white shape `key` names, drawn by femtovg the first time it is
    /// asked for 2 pixels in from its image's edges, and the image's size.
    fn shape(&mut self, key: ShapeKey) -> Option<(ImageId, (usize, usize))> {
        let q = |v: u32| v as f32 / 4.0;
        let (w, h) = match key {
            ShapeKey::Disc(r) => (2.0 * q(r), 2.0 * q(r)),
            ShapeKey::Rounded(w, h, _) => (q(w), q(h)),
            ShapeKey::Ring(n) => {
                self.r.tick += 1;
                let tick = self.r.tick;
                if let Some((image, used)) = self.r.cached.get_mut(&key) {
                    *used = tick;
                    return Some((*image, (n as usize, 1)));
                }
                let pixels: Vec<RGBA8> = (0..n)
                    .map(|i| if i == 0 || i == n - 1 { RGBA8::new(0, 0, 0, 0) } else { RGBA8::new(255, 255, 255, 255) })
                    .collect();
                let image = self
                    .r
                    .canvas
                    .create_image(Img::new(pixels.as_slice(), n as usize, 1), ImageFlags::PREMULTIPLIED)
                    .ok()?;
                self.r.cached.insert(key, (image, tick));
                return Some((image, (n as usize, 1)));
            }
        };
        let size = (w.ceil() as usize + 4, h.ceil() as usize + 4);
        self.r.tick += 1;
        let tick = self.r.tick;
        if let Some((image, used)) = self.r.cached.get_mut(&key) {
            *used = tick;
            return Some((*image, size));
        }
        let image =
            self.r.canvas.create_image_empty(size.0, size.1, PixelFormat::Rgba8, ImageFlags::PREMULTIPLIED).ok()?;
        self.femtovg();
        self.r.canvas.set_render_target(RenderTarget::Image(image));
        self.r.canvas.save();
        self.r.canvas.reset_transform();
        self.r.canvas.set_global_alpha(1.0);
        self.r.canvas.clear_rect(0, 0, size.0 as u32, size.1 as u32, femtovg::Color::rgbaf(0.0, 0.0, 0.0, 0.0));
        let mut path = Shape::new();
        match key {
            ShapeKey::Disc(r) => path.circle(size.0 as f32 / 2.0, size.1 as f32 / 2.0, q(r)),
            ShapeKey::Rounded(_, _, r) => path.rounded_rect(2.0, 2.0, w, h, q(r)),
            ShapeKey::Ring(_) => {}
        }
        self.r.canvas.fill_path(&path, &Paint::color(femtovg::Color::white()));
        self.r.canvas.restore();
        self.r.canvas.set_render_target(self.target);
        self.r.cached.insert(key, (image, tick));
        Some((image, size))
    }

    /// Pixels to a design unit for a cached shape's image under `t`: the
    /// transform's, or the frame's own where the transform scales further
    /// (a shape growing with an animation then keeps one image, scaled).
    fn shape_scale(&self, t: &femtovg::Transform2D) -> f32 {
        let full = (t[0] * t[3] - t[1] * t[2]).abs().sqrt();
        if (full / self.r.scale - 1.0).abs() < 0.02 {
            full
        } else {
            self.r.scale
        }
    }

    /// One white pixel, made the first time it is asked for.
    fn white(&mut self) -> Option<ImageId> {
        if self.r.white.is_none() {
            let pixel = [RGBA8::new(255, 255, 255, 255)];
            self.r.white = self.r.canvas.create_image(Img::new(&pixel[..], 1, 1), ImageFlags::PREMULTIPLIED).ok();
        }
        self.r.white
    }

    fn font(&self, name: &str) -> &typeset::Font {
        self.r.fonts.get(name)
    }

    /// The run of text in pixels, set and uploaded the first time it is
    /// drawn; `outline` grows it by that many pixels for an outline.
    fn run(&mut self, text: &str, style: &TextStyle, outline: f32) -> Option<&CachedRun> {
        let (px, spacing) = (style.size * self.r.scale, style.spacing * self.r.scale);
        let key = format!("{}\u{0}{px:.2}\u{0}{spacing:.2}\u{0}{outline:.2}\u{0}{text}", style.font);
        self.r.tick += 1;
        if !self.r.runs.contains_key(&key) {
            let font = self.r.fonts.get(&style.font);
            let mut run = typeset::rasterize_with(&mut self.r.glyphs, font, self.r.fonts.cjk(text), text, px, spacing);
            if outline > 0.0 {
                run = typeset::dilate(&run, outline);
            }
            let image = (run.width > 0 && run.height > 0)
                .then(|| {
                    let pixels: Vec<RGBA8> = run.alpha.iter().map(|&a| RGBA8::new(a, a, a, a)).collect();
                    self.r
                        .canvas
                        .create_image(Img::new(pixels.as_slice(), run.width, run.height), ImageFlags::PREMULTIPLIED)
                        .ok()
                })
                .flatten();
            trace!(
                "paint: text {text:?} set, {}x{} pixels at {px:.1} px{}",
                run.width,
                run.height,
                if outline > 0.0 { ", outlined" } else { "" }
            );
            if self.r.runs.len() >= RUNS {
                if let Some(oldest) = self.r.runs.iter().min_by_key(|(_, r)| r.used).map(|(k, _)| k.clone()) {
                    if let Some(image) = self.r.runs.remove(&oldest).and_then(|r| r.image) {
                        self.r.canvas.delete_image(image);
                    }
                }
            }
            self.r.runs.insert(
                key.clone(),
                CachedRun { image, left: run.left, top: run.top, width: run.width, height: run.height, used: 0 },
            );
        }
        let tick = self.r.tick;
        let run = self.r.runs.get_mut(&key)?;
        run.used = tick;
        Some(run)
    }

    /// How far `text` advances, in pixels, worked out once per text and style.
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

    /// Draws a cached run with its pen at (x, baseline) in design units,
    /// on whole pixels.
    fn draw_run(&mut self, text: &str, style: &TextStyle, outline: f32, x: f32, baseline: f32, color: Color) {
        let scale = self.r.scale;
        let Some(run) = self.run(text, style, outline) else {
            return;
        };
        let Some(image) = run.image else { return };
        // An unsnapped text starts where it falls, the GPU sampling its run
        // there.
        let snap = |v: f32| if style.snap { (v * scale).round() / scale } else { v };
        let (ox, oy) = (snap(x) + run.left as f32 / scale, snap(baseline) - run.top as f32 / scale);
        let (w, h) = (run.width as f32 / scale, run.height as f32 / scale);
        let corners = [(ox, oy, 0.0, 0.0), (ox + w, oy, 1.0, 0.0), (ox + w, oy + h, 1.0, 1.0), (ox, oy + h, 0.0, 1.0)];
        if self.quad(image, &corners, color, false) {
            return;
        }
        let mut shape = Shape::new();
        shape.rect(ox, oy, w, h);
        let mut paint = Paint::image_tint(image, ox, oy, w, h, 0.0, rgba(color));
        paint.set_anti_alias(false);
        self.fill(&shape, &paint);
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

    /// The image, once decoded: None while it still is, Err when it
    /// cannot be.
    fn load_image(&mut self, path: &str) -> Result<Option<ImageId>, String> {
        match self.r.images.get(path) {
            Some(Some(id)) => Ok(Some(*id)),
            Some(None) => Err(format!("image {path} could not be loaded")),
            None => {
                if self.r.decoding.insert(path.to_string()) {
                    let _ = self.r.decode.send((path.to_string(), self.dir.join(path)));
                }
                Ok(None)
            }
        }
    }

    /// Draws the image, or nothing yet while it is being decoded.
    fn draw_image(&mut self, path: &str, x: f32, y: f32, w: f32, h: f32, alpha: f32) -> Result<(), String> {
        let Some(id) = self.load_image(path)? else {
            return Ok(());
        };
        let mut shape = Shape::new();
        shape.rect(x, y, w, h);
        self.fill(&shape, &Paint::image(id, x, y, w, h, 0.0, alpha));
        Ok(())
    }
}

impl Painter for GpuPainter<'_> {
    fn fill_circle(&mut self, x: f32, y: f32, r: f32, color: Color) {
        let t = self.r.canvas.transform();
        let scale = self.shape_scale(&t);
        let quarters = (r * scale * 4.0).round() as u32;
        if quarters >= 64 && self.r.quads.is_some() {
            if let Some((image, (size, _))) = self.shape(ShapeKey::Disc(quarters)) {
                let half = size as f32 / 2.0 / scale;
                let corners = [
                    (x - half, y - half, 0.0, 0.0),
                    (x + half, y - half, 1.0, 0.0),
                    (x + half, y + half, 1.0, 1.0),
                    (x - half, y + half, 0.0, 1.0),
                ];
                if self.quad(image, &corners, color, false) {
                    return;
                }
            }
        }
        let mut path = Shape::new();
        path.circle(x, y, r);
        self.fill(&path, &Paint::color(rgba(color)));
    }

    fn stroke_circle(&mut self, x: f32, y: f32, r: f32, color: Color, width: f32) {
        // As quads: a band of trapezoids round the ring, each across its
        // cross-section, whose clear ends smooth its edges. Only the ring's
        // own pixels are shaded, as femtovg's stroke does, but by the
        // quads' few instructions.
        let t = self.r.canvas.transform();
        let scale = self.shape_scale(&t);
        let texels = (width * scale).round() as u32 + 2;
        if self.r.quads.is_some() && scale > 0.0 && width > 0.0 && r * scale > width * scale + 2.0 {
            if let Some((image, _)) = self.shape(ShapeKey::Ring(texels)) {
                let px = 1.0 / scale;
                let half = (texels - 2) as f32 * px / 2.0;
                let (inner, outer) = ((r - half - px).max(0.0), r + half + px);
                let n = ((std::f32::consts::TAU * r * scale / 8.0).ceil() as usize).clamp(24, 128);
                // Chords inside the outer edge would clip it: the outer
                // corners sit on the circle the chords touch it from outside.
                let reach = outer / (std::f32::consts::PI / n as f32).cos();
                let at = |i: usize, radius: f32| {
                    let a = std::f32::consts::TAU * i as f32 / n as f32;
                    (x + radius * a.sin(), y - radius * a.cos())
                };
                let u_out = (reach - inner) / (outer - inner);
                for i in 0..n {
                    let ((ax, ay), (bx, by)) = (at(i, inner), at(i, reach));
                    let ((cx, cy), (dx, dy)) = (at(i + 1, reach), at(i + 1, inner));
                    let corners = [(ax, ay, 0.0, 0.5), (bx, by, u_out, 0.5), (cx, cy, u_out, 0.5), (dx, dy, 0.0, 0.5)];
                    if !self.quad(image, &corners, color, false) {
                        break;
                    }
                }
                return;
            }
        }
        let mut path = Shape::new();
        path.circle(x, y, r);
        // A circle's stroke never covers a pixel twice: one pass, whatever
        // its colour.
        let mut paint = Paint::color(rgba(color)).with_line_width(width);
        paint.set_stencil_strokes(false);
        self.stroke(&path, paint, color);
    }

    fn arc(&mut self, x: f32, y: f32, r: f32, from: f32, to: f32, color: Color, width: f32) {
        // femtovg measures from 3 o'clock; themes from 12.
        let (a0, a1) = (from - FRAC_PI_2, to - FRAC_PI_2);
        let mut path = Shape::new();
        path.arc(x, y, r, a0, a1, if a1 >= a0 { Solidity::Hole } else { Solidity::Solid });
        let mut paint = Paint::color(rgba(color)).with_line_width(width).with_line_cap(LineCap::Butt);
        // Less than a turn, no wider than its radius: never over a pixel twice.
        if (to - from).abs() < std::f32::consts::TAU && width <= r {
            paint.set_stencil_strokes(false);
        }
        self.stroke(&path, paint, color);
    }

    fn gradient_arc(&mut self, x: f32, y: f32, r: f32, from: f32, to: f32, start: Color, end: Color, width: f32) {
        if (to - from).abs() < 0.01 {
            return self.arc(
                x,
                y,
                r,
                from.min(to) - 0.3_f32.to_radians(),
                from.max(to) + 0.3_f32.to_radians(),
                start,
                width,
            );
        }
        let n = ((to - from).abs() / std::f32::consts::FRAC_PI_3).ceil().max(1.0) as usize;
        let mix = |a: f32, b: f32, t: f32| a + (b - a) * t;
        for i in 0..n {
            let t0 = i as f32 / n as f32;
            let t1 = (i + 1) as f32 / n as f32;
            let a0 = from + (to - from) * t0;
            let a1 = from + (to - from) * t1;
            let point = |a: f32| (x + r * a.sin(), y - r * a.cos());
            let (x0, y0) = point(a0);
            let (x1, y1) = point(a1);
            let color = |t: f32| Color {
                r: mix(start.r, end.r, t),
                g: mix(start.g, end.g, t),
                b: mix(start.b, end.b, t),
                a: mix(start.a, end.a, t),
            };
            let mut path = Shape::new();
            let lo = a0.min(a1) - 0.3_f32.to_radians();
            let hi = a0.max(a1) + 0.3_f32.to_radians();
            path.arc(x, y, r, lo - FRAC_PI_2, hi - FRAC_PI_2, Solidity::Hole);
            let mut paint = Paint::linear_gradient(x0, y0, x1, y1, rgba(color(t0)), rgba(color(t1)))
                .with_line_width(width)
                .with_line_cap(LineCap::Butt);
            // A sixth of a turn at most: one pass, as for arc.
            if width <= r {
                paint.set_stencil_strokes(false);
            }
            self.stroke(&path, paint, Color { a: start.a.min(end.a), ..Color::WHITE });
        }
    }

    fn radial_glow(&mut self, x: f32, y: f32, radius: f32, steps: u32) {
        let stops = (0..=16).map(|i| {
            let f = i as f32 / 16.0;
            let discs = steps as f32 * (1.0 - f.powf(1.0 / 1.4));
            let alpha = 1.0 - (1.0 - 5.0 / 255.0_f32).powf(discs);
            (f, femtovg::Color::rgbaf(1.0, 1.0, 1.0, alpha))
        });
        let mut path = Shape::new();
        path.circle(x, y, radius);
        self.fill(&path, &Paint::radial_gradient_stops(x, y, 0.0, radius, stops));
    }

    fn line(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, color: Color, width: f32, round: bool) {
        let mut path = Shape::new();
        path.move_to(x1, y1);
        path.line_to(x2, y2);
        let cap = if round { LineCap::Round } else { LineCap::Butt };
        // One segment never crosses itself.
        let mut paint = Paint::color(rgba(color)).with_line_width(width).with_line_cap(cap);
        paint.set_stencil_strokes(false);
        self.stroke(&path, paint, color);
    }

    fn lines(&mut self, segments: &[[f32; 4]], color: Color, width: f32) {
        let mut path = Shape::new();
        for [x1, y1, x2, y2] in segments {
            path.move_to(*x1, *y1);
            path.line_to(*x2, *y2);
        }
        self.stroke(&path, Paint::color(rgba(color)).with_line_width(width).with_line_cap(LineCap::Butt), color);
    }

    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, color: Color) {
        if self.r.quads.is_some() && w > 0.0 && h > 0.0 {
            let t = self.r.canvas.transform();
            let scale = self.shape_scale(&t);
            let quarter = |v: f32| (v * scale * 4.0).round() as u32;
            if radius > 0.0 {
                let key = ShapeKey::Rounded(quarter(w), quarter(h), quarter(radius.min(w / 2.0).min(h / 2.0)));
                if let Some((image, size)) = self.shape(key) {
                    let (x0, y0) = (x - 2.0 / scale, y - 2.0 / scale);
                    let (x1, y1) = (x0 + size.0 as f32 / scale, y0 + size.1 as f32 / scale);
                    let corners = [(x0, y0, 0.0, 0.0), (x1, y0, 1.0, 0.0), (x1, y1, 1.0, 1.0), (x0, y1, 0.0, 1.0)];
                    if self.quad(image, &corners, color, false) {
                        return;
                    }
                }
            } else if let Some(image) = self.white() {
                // On whole pixels and square to them, a rectangle's edges
                // need no smoothing: one white pixel, stretched.
                let (ax, ay) = t.transform_point(x, y);
                let (bx, by) = t.transform_point(x + w, y + h);
                let whole = |v: f32| (v - v.round()).abs() < 0.01;
                let square = (t[1].abs() < 1e-6 && t[2].abs() < 1e-6) || (t[0].abs() < 1e-6 && t[3].abs() < 1e-6);
                if square && whole(ax) && whole(ay) && whole(bx) && whole(by) {
                    let corners =
                        [(x, y, 0.5, 0.5), (x + w, y, 0.5, 0.5), (x + w, y + h, 0.5, 0.5), (x, y + h, 0.5, 0.5)];
                    if self.quad(image, &corners, color, color.a >= 1.0) {
                        return;
                    }
                }
            }
        }
        let mut path = Shape::new();
        if radius > 0.0 {
            path.rounded_rect(x, y, w, h, radius)
        } else {
            path.rect(x, y, w, h)
        }
        self.fill(&path, &Paint::color(rgba(color)));
    }

    fn polygon(&mut self, points: &[(f32, f32)], color: Color) {
        let mut path = Shape::new();
        for (i, &(x, y)) in points.iter().enumerate() {
            if i == 0 {
                path.move_to(x, y)
            } else {
                path.line_to(x, y)
            }
        }
        path.close();
        // femtovg fills a contour without its stencil only when it turns
        // left throughout: Solid winds it so. nlclient's framebuffer has no
        // stencil, so a stencilled fill there covers its whole bounding box.
        path.solidity(Solidity::Solid);
        self.fill(&path, &Paint::color(rgba(color)));
    }

    fn text(&mut self, text: &str, x: f32, y: f32, style: &TextStyle) -> f32 {
        let started = Instant::now();
        let width = self.set_text(text, x, y, style);
        self.r.painting += started.elapsed();
        width
    }

    fn measure(&mut self, text: &str, style: &TextStyle) -> (f32, f32) {
        let started = Instant::now();
        let (scale, px) = (self.r.scale, style.size * self.r.scale);
        let width = self.advance(text, style) / scale;
        let (ascent, descent) = typeset::line(self.font(&style.font), px);
        self.r.painting += started.elapsed();
        (width, (ascent + descent) / scale)
    }

    fn has_glyphs(&mut self, text: &str, style: &TextStyle) -> bool {
        let font = self.font(&style.font);
        text.chars().all(|c| font.lookup_glyph_index(c) != 0)
    }

    fn image(&mut self, path: &str, x: f32, y: f32, w: f32, h: f32, alpha: f32) -> Result<(), String> {
        self.draw_image(path, x, y, w, h, alpha)
    }

    fn sprite(&mut self, path: &str, frame: u32, columns: u32, w: f32, h: f32, x: f32, y: f32) -> Result<u32, String> {
        let id = self.load_image(path)?.ok_or_else(|| format!("image {path} is still being decoded"))?;
        let (sw, sh) = self.r.canvas.image_size(id).map_err(|e| format!("image {path}: {e:?}"))?;
        let columns = columns.max(1);
        let frames = columns * (sh as f32 / h).floor().max(1.0) as u32;
        let k = frame % frames;
        let (col, row) = ((k % columns) as f32, (k / columns) as f32);
        let mut shape = Shape::new();
        shape.rect(x, y, w, h);
        self.fill(&shape, &Paint::image(id, x - col * w, y - row * h, sw as f32, sh as f32, 0.0, 1.0));
        Ok(frames)
    }

    fn save(&mut self) {
        self.r.canvas.save();
        let current = *self.alpha.last().unwrap_or(&1.0);
        self.alpha.push(current);
    }

    fn restore(&mut self) {
        if self.alpha.len() > 1 {
            self.alpha.pop();
            self.r.canvas.restore();
            self.r.canvas.set_global_alpha(*self.alpha.last().unwrap_or(&1.0));
        }
    }

    fn translate(&mut self, x: f32, y: f32) {
        self.r.canvas.translate(x, y);
    }
    fn rotate(&mut self, angle: f32) {
        self.r.canvas.rotate(angle);
    }
    fn scale(&mut self, x: f32, y: f32) {
        self.r.canvas.scale(x, y);
    }

    fn alpha(&mut self, alpha: f32) {
        if let Some(current) = self.alpha.last_mut() {
            *current *= alpha.clamp(0.0, 1.0);
            self.r.canvas.set_global_alpha(*current);
        }
    }

    fn clear(&mut self, color: Color) {
        if self.target == self.r.screen && self.r.clear.is_some() {
            self.r.clear = Some(color);
            return;
        }
        self.femtovg();
        if self.target != self.r.screen {
            self.opaque = color.a >= 1.0;
        }
        let (w, h) = self.fb.unwrap_or((self.r.canvas.width(), self.r.canvas.height()));
        self.r.canvas.clear_rect(0, 0, w, h, rgba(color));
    }

    fn layer(
        &mut self,
        name: &str,
        key: &str,
        size: Option<(f32, f32)>,
        draw: &mut dyn FnMut(&mut dyn Painter) -> Result<(), String>,
    ) -> Result<(), String> {
        let size = size.unwrap_or((self.r.design, self.r.design));
        let px = |v: f32| (v * self.r.scale).ceil().max(1.0) as usize;
        let pixels = (px(size.0), px(size.1));
        match self.r.layers.get(name) {
            Some(l) if l.key == key && l.pixels == pixels => return Ok(()),
            Some(l) if l.pixels != pixels => {
                let image = l.image;
                self.r.canvas.delete_image(image);
                self.r.layers.remove(name);
            }
            _ => {}
        }
        let image = match self.r.layers.get(name) {
            Some(l) => l.image,
            None => self
                .r
                .canvas
                .create_image_empty(
                    pixels.0,
                    pixels.1,
                    PixelFormat::Rgba8,
                    ImageFlags::FLIP_Y | ImageFlags::PREMULTIPLIED,
                )
                .map_err(|e| format!("layer {name}: {e:?}"))?,
        };
        let (pw, ph) = (pixels.0 as u32, pixels.1 as u32);
        if key == name {
            trace!("paint: layer {name} drawn, {pw}x{ph} pixels");
        } else {
            trace!("paint: layer {name} drawn for {key:?}, {pw}x{ph} pixels");
        }
        let started = Instant::now();
        self.femtovg();
        let (fb, alpha) = (self.fb.replace((pw, ph)), std::mem::replace(&mut self.alpha, vec![1.0]));
        let outer = std::mem::replace(&mut self.target, RenderTarget::Image(image));
        let outer_opaque = std::mem::replace(&mut self.opaque, false);
        self.r.canvas.set_render_target(RenderTarget::Image(image));
        self.r.canvas.save();
        self.r.canvas.reset_transform();
        self.r.canvas.set_global_alpha(1.0);
        self.r.canvas.clear_rect(0, 0, pw, ph, femtovg::Color::rgbaf(0.0, 0.0, 0.0, 0.0));
        self.r.canvas.scale(self.r.scale, self.r.scale);
        let drawn = draw(self);
        // Its quads into it, while it is bound.
        self.r.draw_quads((pw, ph));
        self.r.canvas.restore();
        // Back to what was being drawn: a layer may be drawn into another.
        self.target = outer;
        self.r.canvas.set_render_target(self.target);
        // The switch back is femtovg's to make before the next quads.
        self.r.queued = true;
        (self.fb, self.alpha) = (fb, alpha);
        self.r.canvas.set_global_alpha(*self.alpha.last().unwrap_or(&1.0));
        let opaque = std::mem::replace(&mut self.opaque, outer_opaque);
        self.r.layers.insert(name.to_string(), Layer { key: key.to_string(), image, pixels, size, opaque });
        if let Some(quads) = self.r.quads.as_mut() {
            quads.invalidate_composite();
        }
        self.r.layers_drawn += 1;
        self.r.layer_time += started.elapsed();
        drawn
    }

    fn paint_layer(&mut self, name: &str, tint: Color, clip: Option<&[(f32, f32)]>) -> Result<(), String> {
        let Some(layer) = self.r.layers.get(name) else {
            return Err(format!("no layer {name}"));
        };
        let ((w, h), image, opaque) = (layer.size, layer.image, layer.opaque);
        // Layers are drawn upside down in their textures (FLIP_Y).
        let corner = |x: f32, y: f32| (x, y, x / w, 1.0 - y / h);
        let corners: Vec<_> = match clip {
            Some(points) => points.iter().map(|&(x, y)| corner(x, y)).collect(),
            None => vec![corner(0.0, 0.0), corner(w, 0.0), corner(w, h), corner(0.0, h)],
        };
        if corners.len() <= 16 && self.quad(image, &corners, tint, opaque) {
            return Ok(());
        }
        let mut paint = Paint::image_tint(image, 0.0, 0.0, w, h, 0.0, rgba(tint));
        // The layer carries its own soft edges; clips fall between its marks.
        paint.set_anti_alias(false);
        let mut shape = Shape::new();
        match clip {
            Some(points) => {
                for (i, &(x, y)) in points.iter().enumerate() {
                    if i == 0 {
                        shape.move_to(x, y)
                    } else {
                        shape.line_to(x, y)
                    }
                }
                shape.close();
            }
            None => shape.rect(0.0, 0.0, w, h),
        }
        self.fill(&shape, &paint);
        Ok(())
    }

    fn warp_layer(&mut self, name: &str, corners: &[(f32, f32); 4]) -> Result<(), String> {
        let Some(layer) = self.r.layers.get(name) else {
            return Err(format!("no layer {name}"));
        };
        let ((w, h), image, opaque) = (layer.size, layer.image, layer.opaque);
        let texture = match self.r.quads {
            Some(_) if self.target != RenderTarget::Image(image) => self.r.canvas.get_native_texture(image).ok(),
            _ => None,
        };
        let Some(texture) = texture else {
            // Without quads, the affine nearest it: the card's top edge
            // and left side placed exactly.
            let [(x0, y0), (x1, y1), _, (x3, y3)] = *corners;
            self.save();
            self.r.canvas.set_transform(&femtovg::Transform2D::new(
                (x1 - x0) / w,
                (y1 - y0) / w,
                (x3 - x0) / h,
                (y3 - y0) / h,
                x0,
                y0,
            ));
            let painted = self.paint_layer(name, Color::WHITE, None);
            self.restore();
            return painted;
        };
        let started = Instant::now();
        self.r.cleared();
        if self.r.queued {
            self.r.canvas.flush();
            self.r.flushes += 1;
            self.r.queued = false;
        }
        let transform = self.r.canvas.transform();
        let device = corners.map(|(x, y)| transform.transform_point(x, y));
        let q = perspective_weights(&device);
        // Layers are drawn upside down in their textures (FLIP_Y).
        let uv = [(0.0, 1.0), (1.0, 1.0), (1.0, 0.0), (0.0, 0.0)];
        let points: [[f32; 5]; 4] =
            std::array::from_fn(|i| [device[i].0, device[i].1, uv[i].0 * q[i], uv[i].1 * q[i], q[i]]);
        let a = self.alpha.last().copied().unwrap_or(1.0);
        if let Some(quads) = self.r.quads.as_mut() {
            quads.push_projective(texture, &points, [a, a, a, a], !(opaque && a >= 1.0));
        }
        self.r.shapes += 1;
        self.r.painting += started.elapsed();
        Ok(())
    }
}
