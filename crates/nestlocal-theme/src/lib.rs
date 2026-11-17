//! Customizable home screen themes, drawn: nestlocal's Lua themes
//! (crates/nestlocal/themes, which the setup installs under
//! /media/scratch/.unfr0zen/themes). The theme preload (platform/common/theme)
//! loads this library into nlclient and asks it for each frame of the dial:
//! on the gen 3 in nlclient's own GLES2 context (nestlocal_theme_draw), on the
//! gen 2, which has no GPU, on the CPU into the image nlclient draws in the
//! dial's place (nestlocal_theme_draw_pixels). The theme, with the device and
//! shared buckets, comes from nestlocal (theme.native.json), the live
//! setpoints, mode and heat-cool pick from the native control model through
//! the preload (nestlocal_native_theme_v1).
//!
//! A theme that cannot be loaded or drawn leaves the dial to a diagnostic
//! (engine::theme::diagnostic): the target, the theme and its error, with
//! nlclient's menus as ever to pick another from. A theme that fails to load
//! is tried again when it is picked again (`picked` changes), one whose frame
//! failed with nestlocal's next state; one that takes nlclient down with it
//! time after time is refused (engine::guard).
#![allow(dead_code)]
mod engine {
    pub mod display;
    pub mod ft;
    pub mod gpu;
    pub mod guard;
    pub mod paint;
    pub mod soft;
    pub mod theme;
    pub mod typeset;
}
use engine::{
    display::Geometry,
    gpu::{quads::Quads, render::Renderer},
    guard,
    paint::Painter,
    soft,
    theme::{diagnostic, Clock, Nest, Theme, ThermostatView, Ui},
};
use serde_json::Value;
use std::cell::RefCell;
use std::ffi::{c_char, c_void, CString};
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

/// Where the setup installs the themes, one folder each.
const THEMES: &str = "/media/scratch/.unfr0zen/themes";

/// The crash-loop guard's count (engine::guard), beside the themes: it
/// outlives a reboot. A theme that has run this long has survived its start.
const GUARD: &str = "/media/scratch/.unfr0zen/theme.guard";
const GUARD_WINDOW: std::time::Duration = std::time::Duration::from_secs(120);

/// Why the chosen theme does not draw, shown in its place.
struct Broken {
    error: String,
    /// The theme is loaded and a frame of it failed: it is tried again with
    /// nestlocal's next state, which may be one it can draw.
    frame: bool,
}

pub fn theme_id(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

static INPUT_REVISION: AtomicU64 = AtomicU64::new(0);
static HOME_REVISION: AtomicU64 = AtomicU64::new(0);

/// The context's glEnable, behind femtovg's, and whether femtovg has
/// enabled the stencil test since last asked: it does for concave fills and
/// overlapping translucent strokes, which nlclient's framebuffer, without a
/// stencil, draws as their bounding boxes.
/// Only enables while the frame's own framebuffer (SCREEN) is bound count:
/// layers and cached shapes draw into textures that have a stencil.
static GL_ENABLE: AtomicUsize = AtomicUsize::new(0);
static GL_GET_INTEGERV: AtomicUsize = AtomicUsize::new(0);
static SCREEN: AtomicU32 = AtomicU32::new(0);
static STENCIL_USED: AtomicBool = AtomicBool::new(false);
const GL_STENCIL_TEST: u32 = 0x0b90;
const GL_FRAMEBUFFER_BINDING: u32 = 0x8ca6;

unsafe extern "C" fn enable(cap: u32) {
    if cap == GL_STENCIL_TEST {
        let get = GL_GET_INTEGERV.load(Ordering::Relaxed);
        let mut bound = 0i32;
        if get != 0 {
            std::mem::transmute::<usize, unsafe extern "C" fn(u32, *mut i32)>(get)(GL_FRAMEBUFFER_BINDING, &mut bound);
        }
        if get == 0 || bound as u32 == SCREEN.load(Ordering::Relaxed) {
            STENCIL_USED.store(true, Ordering::Relaxed);
        }
    }
    let real = std::mem::transmute::<usize, unsafe extern "C" fn(u32)>(GL_ENABLE.load(Ordering::Relaxed));
    real(cap);
}

#[derive(Default)]
struct Motion {
    screen: f64,
    target: f64,
    turned: Option<f64>,
    input: u64,
    home: u64,
}
impl Motion {
    fn update(&mut self, now: f64, input: u64, home: u64, theme: bool, target: bool) -> bool {
        let notified = input != self.input || home != self.home || theme;
        if home != self.home || theme {
            self.screen = now;
        }
        if target {
            self.target = now;
        }
        if input != self.input {
            self.turned = Some(now);
        }
        self.input = input;
        self.home = home;
        notified
    }
}

#[repr(C)]
#[derive(Default)]
pub struct NativeState {
    target: f32,
    current: f32,
    low: f32,
    high: f32,
    humidity: f32,
    unit: u32,
    mode: u32,
    hvac: u32,
    leaf: u32,
    range: u32,
}
impl NativeState {
    fn apply(&self, ui: &mut Ui) -> f32 {
        let shown = |v: f32| if self.unit != 0 { v * 1.8 + 32.0 } else { v };
        let t = &mut ui.thermostat;
        t.unit = if self.unit != 0 { "F" } else { "C" };
        t.current = shown(self.current);
        t.low = shown(self.low);
        t.high = shown(self.high);
        t.mode = match self.mode {
            0 => "heat",
            1 => "cool",
            2 => "heat-cool",
            4 => "eco",
            _ => "off",
        }
        .into();
        t.hvac = match self.hvac {
            1 => "heating",
            2 => "cooling",
            _ => "idle",
        };
        // nestlocal's reading, as the stock screens show it; the native
        // one reads 0 on builds whose bound getter does not give it.
        if t.humidity.is_none() && self.humidity > 0.0 {
            t.humidity = Some(self.humidity);
        }
        t.leaf_on = self.leaf != 0;
        t.eco = self.mode == 4;
        // The heat-cool end being edited (1, 2) or picked (3, 4) on the dial.
        (t.range, t.range_editing) = match (self.mode, self.range) {
            (2, 1) => (Some("low"), true),
            (2, 2) => (Some("high"), true),
            (2, 3) => (Some("low"), false),
            (2, 4) => (Some("high"), false),
            _ => (None, false),
        };
        t.min = shown(9.0);
        t.max = shown(32.0);
        shown(self.target)
    }
}
type NativeReader = unsafe extern "C" fn(*mut NativeState);
type Loader = unsafe extern "C" fn(*const c_char) -> *const c_void;
/// What paints the theme: femtovg in nlclient's GLES2 context (the gen 3), or
/// tiny-skia on the CPU into a pixmap the dial's size (the gen 2).
enum Painting {
    Gpu(Box<Renderer>),
    Soft(Box<soft::Renderer>),
}
impl Painting {
    fn use_fonts(&mut self, fonts: &[(String, std::path::PathBuf)]) -> Result<(), String> {
        match self {
            Painting::Gpu(renderer) => renderer.use_fonts(fonts),
            Painting::Soft(renderer) => renderer.use_fonts(fonts),
        }
    }
}
/// What the frame prepared from nestlocal's state and the native model shows.
struct Prepared {
    ui: Ui,
    /// The frame's state without the time, to tell an unchanged frame.
    state: Ui,
    now: f64,
    design: f32,
    start: Instant,
}
struct Engine {
    native: Option<NativeReader>,
    painting: Painting,
    theme: Option<Theme>,
    name: String,
    picked: u64,
    broken: Option<Broken>,
    started: Instant,
    motion: Motion,
    target: f32,
    frames: u32,
    timing: [f64; 4],
    data: Rc<Value>,
    revision: Option<std::time::SystemTime>,
    previous: Option<Ui>,
    animating: bool,
    wake: Option<f64>,
    retained: bool,
    stencil: bool,
}
thread_local! { static ENGINE: RefCell<Option<Engine>> = const { RefCell::new(None) }; }
/// The stock picture for the weather (`unit:Weather_*.png`), from
/// nestlocal's condition names (weather.rs's nest_icon), by day between
/// sunrise and sunset, or for a clear or sunny sky without them.
fn weather_image(weather: &Value, now: i64) -> Option<&'static str> {
    let icon = weather["icon"].as_str()?;
    let day = match (weather["sunrise"].as_i64(), weather["sunset"].as_i64()) {
        (Some(rise), Some(set)) => rise < now && now < set,
        _ => icon != "clear",
    };
    Some(match (icon, day) {
        ("clear" | "sunny", true) => "unit:Weather_ClearDay.png",
        ("clear" | "sunny", false) => "unit:Weather_ClearNight.png",
        ("partlycloudy", true) => "unit:Weather_PartlyCloudyDay.png",
        ("partlycloudy", false) => "unit:Weather_PartlyCloudyNight.png",
        ("cloudy" | "fog", _) => "unit:Weather_Cloudy.png",
        ("rain" | "tstorms", _) => "unit:Weather_Rain.png",
        ("snow" | "sleet", _) => "unit:Weather_Snow.png",
        _ => "unit:Weather_Default.png",
    })
}

fn view(value: &Rc<Value>, now: f64, changed: f64, design: f32) -> Ui {
    let dev = &value["dev"];
    let sh = &value["shared"];
    let weather = &value["weather"];
    let epoch = chrono::Utc::now().timestamp();
    let fahrenheit = dev["temperature_scale"] == "F";
    let shown = |v: f64| if fahrenheit { (v * 1.8 + 32.0) as f32 } else { v as f32 };
    let temperature = |key: &str| sh[key].as_f64().map(shown).unwrap_or(0.0);
    let mode = sh["target_temperature_type"].as_str().unwrap_or("off");
    let heat = [
        "hvac_heater_state",
        "hvac_heat_x2_state",
        "hvac_heat_x3_state",
        "hvac_aux_heater_state",
        "hvac_alt_heat_state",
        "hvac_alt_heat_x2_state",
        "hvac_emer_heat_state",
    ]
    .iter()
    .any(|k| sh[k] == true);
    let cool = ["hvac_ac_state", "hvac_cool_x2_state", "hvac_cool_x3_state"].iter().any(|k| sh[k] == true);
    let on = |key: &str| sh[key] == true;
    let stage = if on("hvac_heat_x3_state") || on("hvac_cool_x3_state") {
        3
    } else if on("hvac_heat_x2_state") || on("hvac_cool_x2_state") {
        2
    } else {
        1
    };
    let alt = if on("hvac_alt_heat_x2_state") {
        2
    } else if on("hvac_alt_heat_state") {
        1
    } else {
        0
    };
    let preconditioning = (dev["preconditioning_active"] == true).then_some(if cool || (!heat && mode == "cool") {
        "cool"
    } else {
        "heat"
    });
    let time_to_target =
        dev["time_to_target"].as_f64().filter(|&t| t > epoch as f64).map(|t| ((t - epoch as f64) / 60.0).ceil() as u32);
    let local = chrono::Local::now();
    use chrono::{Datelike, Timelike};
    Ui {
        now,
        screen: "home",
        design,
        dial_px: 480.0,
        changed_target: changed,
        changed_screen: 0.0,
        thermostat: ThermostatView {
            target: temperature("target_temperature"),
            current: temperature("current_temperature"),
            min: shown(9.0),
            max: shown(32.0),
            unit: if fahrenheit { "F" } else { "C" },
            mode: if mode == "range" { "heat-cool" } else { mode }.into(),
            hvac: if mode == "off" {
                "idle"
            } else if heat {
                "heating"
            } else if cool {
                "cooling"
            } else {
                "idle"
            },
            stage,
            aux: on("hvac_aux_heater_state"),
            emergency: on("hvac_emer_heat_state"),
            alt,
            preconditioning,
            safety: dev["hvac_safety_shutoff_active"] == true,
            sunblock: dev["sunlight_correction_active"] == true,
            time_to_target,
            outdoor: weather["temp_c"].as_f64().map(shown),
            weather_image: weather_image(weather, epoch),
            low: temperature("target_temperature_low"),
            high: temperature("target_temperature_high"),
            humidity: dev["current_humidity"].as_f64().map(|n| n as f32),
            can_heat: sh["can_heat"].as_bool().unwrap_or(true),
            can_cool: sh["can_cool"].as_bool().unwrap_or(false),
            leaf_on: dev["leaf"].as_bool().unwrap_or(false),
            online: true,
            eco: matches!(dev["eco"]["mode"].as_str(), Some("manual-eco" | "auto-eco")),
            farsight: "target_temp",
            ..Default::default()
        },
        fan_on: sh["hvac_fan_state"] == true,
        clock: Clock {
            weekday: local.weekday().num_days_from_sunday() as u8,
            hour: local.hour() as u8,
            minute: local.minute() as u8,
            second: local.second() as u8,
            epoch: local.timestamp(),
            utc_offset: local.offset().local_minus_utc(),
            ..Default::default()
        },
        nest: Nest(value.clone()),
        ..Default::default()
    }
}
impl Engine {
    unsafe fn new(loader: Loader, _framebuffer: u32) -> Result<Self, String> {
        let address = |name: &str| {
            let name = CString::new(name).unwrap();
            loader(name.as_ptr())
        };
        let femtovg_address = |name: &str| {
            let real = address(name);
            if name != "glEnable" || real.is_null() {
                return real;
            }
            GL_GET_INTEGERV.store(address("glGetIntegerv") as usize, Ordering::Relaxed);
            GL_ENABLE.store(real as usize, Ordering::Relaxed);
            enable as *const c_void
        };
        let gl = femtovg::renderer::OpenGl::new_from_function(femtovg_address).map_err(|e| format!("GLES: {e}"))?;
        let canvas = femtovg::Canvas::new(gl).map_err(|e| e.to_string())?;
        let quads = Quads::new(glow::Context::from_loader_function(address))?;
        let renderer = Renderer::new(canvas, Some(quads), (0.0, 0.0, 480, 480))?;
        let native = address("nestlocal_native_theme_v1");
        let native =
            if native.is_null() { None } else { Some(std::mem::transmute::<*const c_void, NativeReader>(native)) };
        Ok(Self::with(Painting::Gpu(Box::new(renderer)), native))
    }
    /// The engine painting on the CPU, `size` pixels square.
    fn new_soft(native: Option<NativeReader>, size: u32) -> Result<Self, String> {
        Ok(Self::with(Painting::Soft(Box::new(soft::Renderer::new(size, size)?)), native))
    }
    fn with(painting: Painting, native: Option<NativeReader>) -> Self {
        Self {
            native,
            painting,
            theme: None,
            name: String::new(),
            picked: 0,
            broken: None,
            started: Instant::now(),
            motion: Motion::default(),
            target: f32::NAN,
            frames: 0,
            timing: [0.0; 4],
            data: Rc::new(Value::Null),
            revision: None,
            previous: None,
            animating: true,
            wake: None,
            retained: false,
            stencil: false,
        }
    }
    /// The GPU's painter, which nestlocal_theme_draw's engine has.
    fn gpu(&mut self) -> Result<&mut Renderer, String> {
        match &mut self.painting {
            Painting::Gpu(renderer) => Ok(renderer),
            Painting::Soft(_) => Err("the engine paints on the CPU".into()),
        }
    }
    /// The CPU's painter, which nestlocal_theme_draw_pixels's engine has.
    fn soft(&mut self) -> Result<&mut soft::Renderer, String> {
        match &mut self.painting {
            Painting::Soft(renderer) => Ok(renderer),
            Painting::Gpu(_) => Err("the engine paints on the GPU".into()),
        }
    }
    /// Loads the theme `name` and its fonts, counted by the crash-loop guard
    /// until it has run for GUARD_WINDOW (guard::watch) or failed here.
    fn load(&mut self, name: &str) -> Result<Theme, String> {
        guard::enter(Path::new(GUARD), name)?;
        let loaded = Theme::load(&Path::new(THEMES).join(name)).and_then(|theme| {
            self.painting.use_fonts(&theme.fonts)?;
            Ok(theme)
        });
        match &loaded {
            Ok(_) => guard::watch(Path::new(GUARD), GUARD_WINDOW),
            Err(_) => guard::leave(Path::new(GUARD)),
        }
        loaded
    }

    /// Paints the frame begun: the theme's, or the diagnostic's while the
    /// theme is broken.
    fn paint(&mut self, ui: &Ui, now: f64) -> Result<(), String> {
        let dir = match (&self.broken, &self.theme) {
            (Some(_), _) => Path::new(""),
            (None, Some(theme)) => theme.dir.as_path(),
            (None, None) => return Err("no theme loaded".into()),
        };
        let (mut gpu, mut cpu);
        let painter: &mut dyn Painter = match &mut self.painting {
            Painting::Gpu(renderer) => {
                gpu = renderer.painter(dir);
                &mut gpu
            }
            Painting::Soft(renderer) => {
                cpu = renderer.painter(dir);
                &mut cpu
            }
        };
        if let Some(broken) = &self.broken {
            diagnostic(painter, ui, &self.name, &broken.error);
            (self.animating, self.wake) = (false, None);
            return Ok(());
        }
        let theme = self.theme.as_ref().ok_or("no theme loaded")?;
        self.animating = theme.draw(painter, ui)?;
        self.wake = theme.wake().map(|delay| now + delay.as_secs_f64());
        Ok(())
    }

    /// The frame's view: nestlocal's state read anew when it changed, the
    /// theme (re)loaded when another is picked, the native model applied.
    fn prepare(&mut self, target: f32) -> Result<Prepared, String> {
        let start = Instant::now();
        let path = "/var/run/unfr0zen/theme.native.json";
        let revision = std::fs::metadata(path).and_then(|m| m.modified()).map_err(|e| e.to_string())?;
        if self.revision != Some(revision) {
            self.data = Rc::new(
                serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?,
            );
            self.revision = Some(revision);
            if self.broken.as_ref().is_some_and(|broken| broken.frame) {
                self.broken = None;
                self.previous = None;
            }
        }
        let value = self.data.clone();
        let value = &value;
        let name = value["theme"].as_str().ok_or("no theme chosen")?;
        if !theme_id(name) {
            return Err("unknown theme".into());
        }
        let picked = value["picked"].as_u64().unwrap_or(0);
        let theme_changed = name != self.name || picked != self.picked;
        if theme_changed {
            // Picked while this runs, not found chosen as it starts: the
            // guard's count starts again.
            if !self.name.is_empty() {
                guard::leave(Path::new(GUARD));
            }
            guard::unwatch();
            (self.theme, self.broken) = (None, None);
            self.name = name.into();
            self.picked = picked;
            self.previous = None;
            self.stencil = false;
            match self.load(name) {
                Ok(theme) => {
                    self.theme = Some(theme);
                    eprintln!("THEME Lua theme={name}");
                }
                Err(error) => {
                    eprintln!("THEME Lua theme={name}: {error}");
                    // The theme shown before leaves nothing behind.
                    self.painting.use_fonts(&[])?;
                    self.broken = Some(Broken { error, frame: false });
                }
            }
        }
        let design = self.theme.as_ref().map_or(480.0, |theme| theme.design);
        let now = self.started.elapsed().as_secs_f64();
        let mut ui = view(value, now, self.motion.target, design);
        let target = if let Some(read) = self.native {
            let mut state = NativeState::default();
            unsafe {
                read(&mut state);
            }
            state.apply(&mut ui)
        } else {
            target
        };
        if target.is_finite() {
            let step = if ui.thermostat.unit == "F" { 1.0 } else { 0.5 };
            ui.thermostat.target = (target / step).round() * step;
            ui.thermostat.nudge = target - ui.thermostat.target;
        }
        if self.motion.update(
            now,
            INPUT_REVISION.load(Ordering::Acquire),
            HOME_REVISION.load(Ordering::Acquire),
            theme_changed,
            ui.thermostat.target != self.target,
        ) {
            self.previous = None;
            self.animating = true;
        }
        self.target = ui.thermostat.target;
        ui.changed_target = self.motion.target;
        ui.changed_screen = self.motion.screen;
        ui.turned_target = self.motion.turned.unwrap_or(-100.0);
        // The second alone changes no frame: a theme showing it wakes for it.
        let mut state = Ui { now: 0.0, ..ui.clone() };
        state.clock.second = 0;
        state.clock.epoch -= state.clock.epoch.rem_euclid(60);
        Ok(Prepared { ui, state, now, design, start })
    }

    /// The GPU's frame: into the framebuffer bound, or with `image` into the
    /// frame texture, whose name it gives.
    fn draw(&mut self, image: bool, target: f32) -> Result<u32, String> {
        let Prepared { ui, state, now, design, start } = self.prepare(target)?;
        let decoded = self.gpu()?.take_decoded();
        if self.retained
            && !self.animating
            && !decoded
            && self.previous.as_ref() == Some(&state)
            && self.wake.is_none_or(|wake| now < wake)
        {
            let renderer = self.gpu()?;
            if image {
                return renderer.retained_texture().map(|t| t.0.get()).ok_or("no retained texture".into());
            }
            renderer.present_retained();
            return Ok(1);
        }
        self.previous = Some(state);
        let mut geometry = Geometry::for_framebuffer(480, 480);
        geometry.quarter_turns = 1;
        let prepared = Instant::now();
        let mut direct = self.broken.is_none() && self.animating && !image && !self.stencil;
        self.gpu()?.use_external_target(direct);
        self.gpu()?.begin(&geometry, design);
        let begun = Instant::now();
        STENCIL_USED.store(false, Ordering::Relaxed);
        if let Err(error) = self.paint(&ui, now) {
            // The theme's frame failed: what it drew is finished where it
            // went, and the diagnostic drawn over all of it.
            eprintln!("THEME Lua theme={}: {error}", self.name);
            let renderer = self.gpu()?;
            renderer.finish_texture();
            renderer.abandon();
            renderer.use_external_target(false);
            renderer.begin(&geometry, design);
            self.broken = Some(Broken { error, frame: true });
            direct = false;
            self.paint(&ui, now)?;
        }
        let drawn = Instant::now();
        self.gpu()?.finish_texture();
        if direct && STENCIL_USED.load(Ordering::Relaxed) {
            // The theme needs the stencil the framebuffer lacks: this frame
            // again, and the theme's later ones, through the frame texture,
            // whose opaque pass covers what was drawn.
            eprintln!("THEME Lua theme={} uses the stencil; frames drawn off-screen", self.name);
            self.stencil = true;
            direct = false;
            self.gpu()?.use_external_target(false);
            self.gpu()?.begin(&geometry, design);
            self.paint(&ui, now)?;
            self.gpu()?.finish_texture();
        }
        self.retained = !direct;
        if !direct && !image {
            self.gpu()?.present_retained();
        }
        self.frames += 1;
        for (total, duration) in
            self.timing.iter_mut().zip([prepared - start, begun - prepared, drawn - begun, drawn.elapsed()])
        {
            *total += duration.as_secs_f64();
        }
        if self.frames == 120 {
            let timing = std::mem::take(&mut self.timing);
            self.frames = 0;
            let r = self.gpu()?;
            eprintln!("THEME painter state_ms={:.3} begin_ms={:.3} lua_ms={:.3} finish_ms={:.3} shapes={} flushes={} layers={} painting_ms={:.3}", timing[0]*1000.0/120.0, timing[1]*1000.0/120.0, timing[2]*1000.0/120.0, timing[3]*1000.0/120.0, r.shapes/120, r.flushes/120, r.layers_drawn, r.painting.as_secs_f64()*1000.0/120.0);
            r.shapes = 0;
            r.flushes = 0;
            r.layers_drawn = 0;
            r.painting = std::time::Duration::ZERO;
        }
        if image {
            self.gpu()?.retained_texture().map(|t| t.0.get()).ok_or("no retained texture".into())
        } else {
            Ok(1)
        }
    }

    /// The CPU's frame, into `pixels`: the dial's size squared, as ARGB
    /// words. An unchanged frame is not drawn again: `pixels` keeps it.
    fn draw_pixels(&mut self, pixels: &mut [u32], size: u32, target: f32) -> Result<(), String> {
        let Prepared { mut ui, state, now, design, start } = self.prepare(target)?;
        ui.dial_px = size as f32;
        if !self.animating && self.previous.as_ref() == Some(&state) && self.wake.is_none_or(|wake| now < wake) {
            return Ok(());
        }
        self.previous = Some(state);
        self.soft()?.begin(size as f32, design);
        let begun = Instant::now();
        if let Err(error) = self.paint(&ui, now) {
            eprintln!("THEME Lua theme={}: {error}", self.name);
            self.broken = Some(Broken { error, frame: true });
            self.soft()?.begin(size as f32, design);
            self.paint(&ui, now)?;
        }
        let drawn = Instant::now();
        // The pixmap is opaque (begin fills it black): premultiplied is plain.
        for (out, px) in pixels.iter_mut().zip(self.soft()?.pixmap.pixels()) {
            *out = 0xff00_0000 | u32::from(px.red()) << 16 | u32::from(px.green()) << 8 | u32::from(px.blue());
        }
        self.frames += 1;
        for (total, duration) in self.timing.iter_mut().zip([begun - start, drawn - begun, drawn.elapsed()]) {
            *total += duration.as_secs_f64();
        }
        if self.frames == 120 {
            let timing = std::mem::take(&mut self.timing);
            self.frames = 0;
            let r = self.soft()?;
            eprintln!(
                "THEME soft state_ms={:.3} lua_ms={:.3} copy_ms={:.3} shapes={} painting_ms={:.3}",
                timing[0] * 1000.0 / 120.0,
                timing[1] * 1000.0 / 120.0,
                timing[2] * 1000.0 / 120.0,
                r.shapes / 120,
                r.painting.as_secs_f64() * 1000.0 / 120.0
            );
            r.shapes = 0;
            r.painting = std::time::Duration::ZERO;
        }
        Ok(())
    }

    /// Seconds until the theme needs another frame: 0 while it moves, the
    /// time it woke for, or infinite while it stands still.
    fn next(&self) -> f32 {
        if self.animating {
            return 0.0;
        }
        self.wake.map_or(f32::INFINITY, |wake| (wake - self.started.elapsed().as_secs_f64()).max(0.0) as f32)
    }
}
/// Native wheel notification, consumed by the GL thread on its next frame.
#[no_mangle]
pub extern "C" fn nestlocal_theme_input() {
    INPUT_REVISION.fetch_add(1, Ordering::Release);
}

/// Native home-entry notification, consumed on the next GL frame.
#[no_mangle]
pub extern "C" fn nestlocal_theme_home() {
    HOME_REVISION.fetch_add(1, Ordering::Release);
}

/// When the theme drawn last needs its next frame (Engine::next); 0 before
/// the first. Called only on nlclient's GL thread, after a draw.
#[no_mangle]
pub extern "C" fn nestlocal_theme_next() -> f32 {
    ENGINE.with(|slot| slot.try_borrow().ok().and_then(|engine| engine.as_ref().map(Engine::next))).unwrap_or(0.0)
}

/// Called only on nlclient's GL thread, with its current theme framebuffer bound.
///
/// # Safety
/// `loader` must return valid GLES2 entry points for the current context.
#[no_mangle]
pub unsafe extern "C" fn nestlocal_theme_draw(loader: Loader, framebuffer: u32, image: u32, target: f32) -> u32 {
    SCREEN.store(framebuffer, Ordering::Relaxed);
    guard::drawing(true);
    let result = std::panic::catch_unwind(|| {
        ENGINE.with(|slot| {
            let mut engine = slot.borrow_mut();
            if engine.is_none() {
                *engine = Some(Engine::new(loader, framebuffer)?);
            }
            engine.as_mut().unwrap().draw(image != 0, target)
        })
    });
    guard::drawing(false);
    match result {
        Ok(Ok(texture)) => texture,
        Ok(Err(error)) => {
            eprintln!("THEME Lua: {error}");
            0
        }
        Err(_) => {
            eprintln!("THEME Lua renderer panicked");
            0
        }
    }
}

/// Draws the dial's frame on the CPU into `pixels`, `size` pixels square, as
/// ARGB words (the gen 2's image in the dial's place, which nlclient draws);
/// `native` reads the native control model. Called only on nlclient's UI
/// thread; 1 when drawn (or unchanged and copied out again), 0 when not.
///
/// # Safety
/// `pixels` must hold `size` squared words, and `native` must be callable.
#[no_mangle]
pub unsafe extern "C" fn nestlocal_theme_draw_pixels(
    native: Option<NativeReader>,
    pixels: *mut u32,
    size: u32,
    target: f32,
) -> u32 {
    if pixels.is_null() || size == 0 {
        return 0;
    }
    let pixels = std::slice::from_raw_parts_mut(pixels, (size * size) as usize);
    guard::drawing(true);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ENGINE.with(|slot| {
            let mut engine = slot.borrow_mut();
            if engine.is_none() {
                *engine = Some(Engine::new_soft(native, size)?);
            }
            engine.as_mut().unwrap().draw_pixels(pixels, size, target)
        })
    }));
    guard::drawing(false);
    match result {
        Ok(Ok(())) => 1,
        Ok(Err(error)) => {
            eprintln!("THEME Lua: {error}");
            0
        }
        Err(_) => {
            eprintln!("THEME Lua renderer panicked");
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_state_overrides_stale_telemetry() {
        let data = serde_json::json!({"dev":{"temperature_scale":"F"},"shared":{"target_temperature_type":"off","current_temperature":0}});
        let mut ui = view(&Rc::new(data), 1.0, 0.0, 480.0);
        let mut state = NativeState {
            target: 21.5,
            current: 19.25,
            low: 18.0,
            high: 24.0,
            humidity: 47.0,
            unit: 0,
            mode: 2,
            hvac: 1,
            leaf: 1,
            range: 5,
        };
        assert_eq!(state.apply(&mut ui), 21.5);
        assert_eq!(ui.thermostat.unit, "C");
        assert_eq!(ui.thermostat.current, 19.25);
        assert_eq!(ui.thermostat.mode, "heat-cool");
        assert_eq!(ui.thermostat.hvac, "heating");
        assert_eq!(ui.thermostat.humidity, Some(47.0));
        let mut reported =
            view(&Rc::new(serde_json::json!({"dev":{"current_humidity":45},"shared":{}})), 1.0, 0.0, 480.0);
        NativeState { humidity: 0.0, ..Default::default() }.apply(&mut reported);
        assert_eq!(reported.thermostat.humidity, Some(45.0), "nestlocal's humidity, not a native 0");
        let mut unknown = view(&Rc::new(serde_json::json!({"dev":{},"shared":{}})), 1.0, 0.0, 480.0);
        NativeState::default().apply(&mut unknown);
        assert_eq!(unknown.thermostat.humidity, None);
        assert!(ui.thermostat.leaf_on);
        assert_eq!((ui.thermostat.range, ui.thermostat.range_editing), (None, false));
        for (range, expected) in
            [(1, (Some("low"), true)), (2, (Some("high"), true)), (3, (Some("low"), false)), (4, (Some("high"), false))]
        {
            state.range = range;
            state.apply(&mut ui);
            assert_eq!((ui.thermostat.range, ui.thermostat.range_editing), expected);
        }
        state.unit = 1;
        state.mode = 1;
        state.hvac = 2;
        assert!((state.apply(&mut ui) - 70.7).abs() < 0.001);
        assert!((ui.thermostat.current - 66.65).abs() < 0.001);
        assert_eq!(ui.thermostat.mode, "cool");
        assert_eq!(ui.thermostat.hvac, "cooling");
        assert_eq!(ui.thermostat.range, None, "only heat-cool has ends to pick");
        state.mode = 3;
        state.apply(&mut ui);
        assert_eq!(ui.thermostat.mode, "off");
    }
    #[test]
    fn animation_events_share_the_frame_clock() {
        let mut motion = Motion::default();
        assert!(motion.update(2.0, 1, 1, true, true));
        assert_eq!(motion.turned, Some(motion.target));
        assert!(!motion.update(2.1, 1, 1, false, false));
        assert_eq!(motion.screen, 2.0);
        assert!(!motion.update(3.0, 1, 1, false, true));
        assert!(motion.turned.unwrap() < motion.target);
        assert!(motion.update(4.0, 1, 2, false, false));
        assert_eq!(motion.screen, 4.0);
        assert_eq!(motion.turned, Some(2.0));
    }
    #[test]
    fn theme_names_are_folder_names() {
        assert!(theme_id("my-theme_2"));
        assert!(!theme_id("../my-theme"));
        assert!(!theme_id(""));
    }
    #[test]
    fn existing_lua_themes_load() {
        let themes = Path::new(env!("UNFR0ZEN_ROOT")).join("crates/nestlocal/themes");
        let mut names: Vec<String> = std::fs::read_dir(&themes)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.join("theme.lua").is_file())
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        names.sort();
        assert!(names.len() >= 5, "{names:?}");
        for name in names {
            let path = themes.join(&name);
            let theme = Theme::load(&path).unwrap();
            assert_eq!(theme.design, 480.0);
            assert!(!theme.fonts.is_empty());
            let mut renderer = engine::soft::Renderer::new(240, 240).unwrap();
            renderer.use_fonts(&theme.fonts).unwrap();
            for (mode, scale) in ["heat", "cool", "range", "off"].into_iter().flat_map(|m| [(m, "C"), (m, "F")]) {
                let state = serde_json::json!({"dev":{"temperature_scale":scale,"current_humidity":40},"shared":{"target_temperature_type":mode,"target_temperature":21,"target_temperature_low":19,"target_temperature_high":24,"current_temperature":19,"hvac_heater_state":true,"can_heat":true,"can_cool":true},
                    "weather":{"temp_c":-3.5,"condition":"rainy","forecast":{"hourly":[{"hour":1,"time":4_000_000_000_i64,"temp_c":-2}]}}});
                for now in [1.0, 1.1] {
                    renderer.begin(240.0, theme.design);
                    theme
                        .draw(&mut renderer.painter(&theme.dir), &view(&Rc::new(state.clone()), now, 0.0, theme.design))
                        .unwrap_or_else(|e| panic!("{name} in {mode} ({scale}): {e}"));
                    assert!(
                        renderer.pixmap.pixels().iter().filter(|p| p.red().max(p.green()).max(p.blue()) > 200).count()
                            > 50,
                        "{name} in {mode} ({scale}) must draw its readout"
                    );
                }
            }
        }
    }
    #[test]
    fn view_reads_the_equipment_and_the_weather() {
        let now = chrono::Utc::now().timestamp();
        let data = serde_json::json!({
            "dev": {"temperature_scale": "F", "time_to_target": now + 600, "sunlight_correction_active": true,
                    "preconditioning_active": true, "hvac_safety_shutoff_active": false},
            "shared": {"target_temperature_type": "heat", "hvac_heater_state": true, "hvac_heat_x2_state": true,
                       "hvac_aux_heater_state": true},
            "weather": {"temp_c": 10, "icon": "partlycloudy", "sunrise": now - 60, "sunset": now + 60},
        });
        let ui = view(&Rc::new(data), 1.0, 0.0, 480.0);
        let t = &ui.thermostat;
        assert_eq!((t.stage, t.aux, t.emergency, t.alt), (2, true, false, 0));
        assert_eq!(t.outdoor, Some(50.0));
        assert_eq!(t.weather_image, Some("unit:Weather_PartlyCloudyDay.png"));
        assert_eq!(t.time_to_target, Some(10));
        assert_eq!(t.preconditioning, Some("heat"));
        assert!(t.sunblock && !t.safety);
        let bare = view(&Rc::new(serde_json::json!({"dev": {}, "shared": {}})), 1.0, 0.0, 480.0);
        assert_eq!(
            (bare.thermostat.outdoor, bare.thermostat.weather_image, bare.thermostat.time_to_target),
            (None, None, None)
        );
        assert_eq!(weather_image(&serde_json::json!({"icon": "clear"}), now), Some("unit:Weather_ClearNight.png"));
        assert_eq!(weather_image(&serde_json::json!({"icon": "windy"}), now), Some("unit:Weather_Default.png"));
    }
    #[test]
    fn themes_read_what_nestlocal_gives_as_ui_nest() {
        let dir = std::env::temp_dir().join(format!("nestlocal-theme-nest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("theme.lua"),
            r#"return { name = "t", api = 1, draw = function(gfx, ui)
            local n = ui.nest
            return n.weather.temp_c == 4.5 and n.weather.forecast.hourly[1].temp_c == 5 and n.state.mode == "heat"
              and n.device.temperature_scale == "C" and n.shared.can_cool == false and n.state.missing == nil
          end }"#,
        )
        .unwrap();
        let theme = Theme::load(&dir).unwrap();
        let data = serde_json::json!({"dev": {"temperature_scale": "C"}, "shared": {"can_cool": false},
            "state": {"mode": "heat", "missing": null}, "weather": {"temp_c": 4.5, "forecast": {"hourly": [{"temp_c": 5}]}}});
        let mut renderer = engine::soft::Renderer::new(4, 4).unwrap();
        renderer.begin(4.0, theme.design);
        assert!(theme.draw(&mut renderer.painter(&theme.dir), &view(&Rc::new(data), 1.0, 0.0, theme.design)).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }
    /// Renders themes in a range of states into PNGs, to look at:
    /// `THEME_PREVIEWS=DIR [THEME_PREVIEW_THEMES=a,b] cargo test -p
    /// nestlocal-theme previews -- --ignored`: every theme without the list.
    #[test]
    #[ignore]
    fn previews() {
        let out = std::path::PathBuf::from(std::env::var("THEME_PREVIEWS").expect("THEME_PREVIEWS=DIR"));
        std::fs::create_dir_all(&out).unwrap();
        let themes = Path::new(env!("UNFR0ZEN_ROOT")).join("crates/nestlocal/themes");
        let names = std::env::var("THEME_PREVIEW_THEMES").unwrap_or_else(|_| {
            let mut all: Vec<String> = std::fs::read_dir(&themes)
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.join("theme.lua").is_file())
                .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
                .collect();
            all.sort();
            all.join(",")
        });
        let day = 1_794_660_000_i64; // 2026-11-14 12:40 UTC
        let weather = |condition: &str, temp: f64| {
            serde_json::json!({"temp_c": temp, "humidity": 70, "condition": condition,
            "sunrise": day - 5 * 3600, "sunset": day + 4 * 3600,
            "forecast": {"hourly": (1..=6).map(|h| serde_json::json!({"hour": h, "time": day + h * 3600 - 2400,
                "temp_c": temp + h as f64 * 0.7})).collect::<Vec<_>>()}})
        };
        let scenes = [
            ("heat-idle", "heat", false, false, 12.0, weather("sunny", 14.0)),
            ("heating", "heat", true, false, 12.0, weather("partlycloudy", 9.5)),
            ("cooling", "cool", false, true, 15.0, weather("sunny", 31.0)),
            ("range", "range", false, false, 12.0, weather("cloudy", 12.0)),
            ("off", "off", false, false, 12.0, weather("fog", 6.0)),
            ("rain", "heat", true, false, 10.0, weather("rainy", 7.0)),
            ("storm", "heat", false, false, 17.0, weather("lightning-rainy", 18.0)),
            ("snow", "heat", true, false, 9.0, weather("snowy", -2.0)),
            ("dusk", "heat", false, false, 16.6, weather("partlycloudy", 8.0)),
            ("night", "heat", false, false, 23.0, weather("clear-night", 3.0)),
            ("night-rain", "heat", true, false, 2.0, weather("pouring", 5.0)),
            ("dawn", "heat", false, false, 7.4, weather("sunny", 2.0)),
            ("no-weather", "heat", false, false, 12.0, serde_json::json!({})),
        ];
        for name in names.split(',') {
            let theme = Theme::load(&themes.join(name)).unwrap();
            let mut renderer = engine::soft::Renderer::new(480, 480).unwrap();
            renderer.use_fonts(&theme.fonts).unwrap();
            // One unit's scenes, then the other's: what glides goes on from
            // the scene before.
            for (scale, &(label, mode, heating, cooling, hour, ref weather)) in
                ["C", "F"].into_iter().flat_map(|u| scenes.iter().map(move |s| (u, s)))
            {
                let data = serde_json::json!({"dev": {"temperature_scale": scale, "current_humidity": 41},
                    "shared": {"target_temperature_type": mode, "target_temperature": 21.5, "target_temperature_low": 19.5,
                        "target_temperature_high": 24.0, "current_temperature": 20.0, "hvac_heater_state": heating,
                        "hvac_ac_state": cooling, "can_heat": true, "can_cool": true},
                    "weather": weather});
                let data = Rc::new(data);
                let epoch = day + ((hour - 12.0_f64) * 3600.0) as i64 - 2400;
                // Frames a second apart: the last is kept, a moment after the scene settled.
                for now in [1.0, 2.0, 3.0, 4.0, 4.5] {
                    let mut ui = view(&data, now, 0.0, theme.design);
                    ui.clock.hour = hour.floor() as u8;
                    ui.clock.minute = ((hour.fract()) * 60.0) as u8;
                    ui.clock.weekday = 6;
                    ui.clock.epoch = epoch;
                    ui.clock.utc_offset = 0;
                    renderer.begin(480.0, theme.design);
                    theme
                        .draw(&mut renderer.painter(&theme.dir), &ui)
                        .unwrap_or_else(|e| panic!("{name} {label}: {e}"));
                }
                let mut png = Vec::new();
                let mut encoder = png::Encoder::new(&mut png, 480, 480);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                encoder.write_header().unwrap().write_image_data(renderer.pixmap.data()).unwrap();
                let suffix = if scale == "F" { "-f" } else { "" };
                std::fs::write(out.join(format!("{name}-{label}{suffix}.png")), png).unwrap();
            }
        }
    }
    #[test]
    fn view_preserves_units_and_hvac_state() {
        let data = serde_json::json!({"dev":{"temperature_scale":"F","current_humidity":40},"shared":{"target_temperature_type":"heat","target_temperature":20,"current_temperature":19,"hvac_heater_state":true}});
        let ui = view(&Rc::new(data), 1.0, 0.0, 480.0);
        assert_eq!(ui.thermostat.target, 68.0);
        assert_eq!(ui.thermostat.unit, "F");
        assert_eq!(ui.thermostat.hvac, "heating");
        assert_eq!(ui.thermostat.humidity, Some(40.0));
    }
}
