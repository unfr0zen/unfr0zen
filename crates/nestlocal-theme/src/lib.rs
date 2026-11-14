//! The gen 3's customizable home screen themes, drawn: nestlocal's Lua themes
//! (crates/nestlocal/themes, which the patch installs under
//! /media/scratch/.unfr0zen/themes) in nlclient's own GLES2 context. The theme preload
//! (platform/common/theme) loads this library into nlclient and calls
//! nestlocal_theme_draw for each frame of the dial; the theme, with the device
//! and shared buckets, comes from nestlocal (theme.native.json), the live
//! setpoints, mode and heat-cool pick from the native control model through
//! the preload (nestlocal_native_theme_v1).
#![allow(dead_code)]
mod engine {
    pub mod display;
    pub mod ft;
    pub mod paint;
    pub mod theme;
    #[cfg(test)]
    pub mod soft;
    pub mod typeset;
    pub mod gpu;
}
use std::cell::RefCell;
use std::ffi::{c_char, c_void, CString};
use std::path::Path;
use std::time::Instant;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use engine::{display::Geometry, gpu::{quads::Quads, render::Renderer}, theme::{Theme, Ui, Clock, ThermostatView}};
use serde_json::Value;

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
        if get == 0 || bound as u32 == SCREEN.load(Ordering::Relaxed) { STENCIL_USED.store(true, Ordering::Relaxed); }
    }
    let real = std::mem::transmute::<usize, unsafe extern "C" fn(u32)>(GL_ENABLE.load(Ordering::Relaxed));
    real(cap);
}

#[derive(Default)]
struct Motion { screen: f64, target: f64, turned: Option<f64>, input: u64, home: u64 }
impl Motion {
    fn update(&mut self, now: f64, input: u64, home: u64, theme: bool, target: bool) -> bool {
        let notified = input != self.input || home != self.home || theme;
        if home != self.home || theme { self.screen = now; }
        if target { self.target = now; }
        if input != self.input { self.turned = Some(now); }
        self.input = input;
        self.home = home;
        notified
    }
}

#[repr(C)]
#[derive(Default)]
struct NativeState { target: f32, current: f32, low: f32, high: f32, humidity: f32, unit: u32, mode: u32, hvac: u32, leaf: u32, range: u32 }
impl NativeState {
    fn apply(&self, ui: &mut Ui) -> f32 {
        let shown = |v: f32| if self.unit != 0 { v * 1.8 + 32.0 } else { v };
        let t = &mut ui.thermostat;
        t.unit = if self.unit != 0 { "F" } else { "C" };
        t.current = shown(self.current);
        t.low = shown(self.low);
        t.high = shown(self.high);
        t.mode = match self.mode { 0 => "heat", 1 => "cool", 2 => "heat-cool", 4 => "eco", _ => "off" }.into();
        t.hvac = match self.hvac { 1 => "heating", 2 => "cooling", _ => "idle" };
        t.humidity = Some(self.humidity);
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
struct Engine { native: Option<NativeReader>, renderer: Renderer, theme: Option<Theme>, name: String, started: Instant, motion: Motion, target: f32, frames: u32, timing: [f64; 4], data: Value, revision: Option<std::time::SystemTime>, previous: Option<Ui>, animating: bool, wake: Option<f64>, retained: bool, stencil: bool }
thread_local! { static ENGINE: RefCell<Option<Engine>> = const { RefCell::new(None) }; }
fn view(value: &Value, now: f64, changed: f64, design: f32) -> Ui {
    let dev = &value["dev"]; let sh = &value["shared"];
    let fahrenheit = dev["temperature_scale"] == "F";
    let shown = |v: f64| if fahrenheit { (v * 1.8 + 32.0) as f32 } else { v as f32 };
    let temperature = |key: &str| sh[key].as_f64().map(shown).unwrap_or(0.0);
    let mode = sh["target_temperature_type"].as_str().unwrap_or("off");
    let heat = ["hvac_heater_state", "hvac_heat_x2_state", "hvac_heat_x3_state", "hvac_aux_heater_state", "hvac_alt_heat_state", "hvac_alt_heat_x2_state", "hvac_emer_heat_state"].iter().any(|k| sh[k] == true);
    let cool = ["hvac_ac_state", "hvac_cool_x2_state", "hvac_cool_x3_state"].iter().any(|k| sh[k] == true);
    let local = chrono::Local::now();
    use chrono::{Datelike, Timelike};
    Ui { now, screen: "home", design, dial_px: 480.0, changed_target: changed, changed_screen: 0.0,
        thermostat: ThermostatView {
            target: temperature("target_temperature"), current: temperature("current_temperature"),
            min: shown(9.0), max: shown(32.0), unit: if fahrenheit { "F" } else { "C" },
            mode: if mode == "range" { "heat-cool" } else { mode }.into(),
            hvac: if mode == "off" { "idle" } else if heat { "heating" } else if cool { "cooling" } else { "idle" },
            stage: 1, low: temperature("target_temperature_low"), high: temperature("target_temperature_high"),
            humidity: dev["current_humidity"].as_f64().map(|n| n as f32),
            can_heat: sh["can_heat"].as_bool().unwrap_or(true), can_cool: sh["can_cool"].as_bool().unwrap_or(false),
            leaf_on: dev["leaf"].as_bool().unwrap_or(false), online: true,
            eco: matches!(dev["eco"]["mode"].as_str(), Some("manual-eco" | "auto-eco")), farsight: "target_temp",
            ..Default::default()
        }, fan_on: sh["hvac_fan_state"] == true,
        clock: Clock { weekday: local.weekday().num_days_from_sunday() as u8, hour: local.hour() as u8,
            minute: local.minute() as u8, second: local.second() as u8, ..Default::default() },
        ..Default::default()
    }
}
impl Engine {
    unsafe fn new(loader: Loader, _framebuffer: u32) -> Result<Self, String> {
        let address = |name: &str| { let name = CString::new(name).unwrap(); loader(name.as_ptr()) };
        let femtovg_address = |name: &str| {
            let real = address(name);
            if name != "glEnable" || real.is_null() { return real; }
            GL_GET_INTEGERV.store(address("glGetIntegerv") as usize, Ordering::Relaxed);
            GL_ENABLE.store(real as usize, Ordering::Relaxed);
            enable as *const c_void
        };
        let gl = femtovg::renderer::OpenGl::new_from_function(femtovg_address).map_err(|e| format!("GLES: {e}"))?;
        let canvas = femtovg::Canvas::new(gl).map_err(|e| e.to_string())?;
        let quads = Quads::new(glow::Context::from_loader_function(address))?;
        let renderer = Renderer::new(canvas, Some(quads), (0.0, 0.0, 480, 480))?;
        let native = address("nestlocal_native_theme_v1");
        let native = if native.is_null() { None } else { Some(std::mem::transmute::<*const c_void, NativeReader>(native)) };
        Ok(Self { native, renderer, theme: None, name: String::new(), started: Instant::now(), motion: Motion::default(), target: f32::NAN, frames: 0, timing: [0.0; 4], data: Value::Null, revision: None, previous: None, animating: true, wake: None, retained: false, stencil: false })
    }
    fn draw(&mut self, image: bool, target: f32) -> Result<u32, String> {
        let start = Instant::now();
        let path = "/var/run/unfr0zen/theme.native.json";
        let revision = std::fs::metadata(path).and_then(|m| m.modified()).map_err(|e| e.to_string())?;
        if self.revision != Some(revision) {
            self.data = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            self.revision = Some(revision);
        }
        let value = &self.data;
        let name = value["theme"].as_str().ok_or("no theme chosen")?;
        if !theme_id(name) { return Err("unknown theme".into()); }
        let theme_changed = name != self.name;
        if theme_changed {
            let theme = Theme::load(&Path::new("/media/scratch/.unfr0zen/themes").join(name))?;
            self.renderer.use_fonts(&theme.fonts)?;
            self.theme = Some(theme); self.name = name.into(); self.previous = None; self.stencil = false;
            eprintln!("THEME Lua theme={name}");
        }
        let theme = self.theme.as_ref().unwrap();
        let now = self.started.elapsed().as_secs_f64();
        let mut ui = view(value, now, self.motion.target, theme.design);
        let target = if let Some(read) = self.native {
            let mut state = NativeState::default();
            unsafe { read(&mut state); }
            state.apply(&mut ui)
        } else { target };
        if target.is_finite() {
            let step = if ui.thermostat.unit == "F" { 1.0 } else { 0.5 };
            ui.thermostat.target = (target / step).round() * step;
            ui.thermostat.nudge = target - ui.thermostat.target;
        }
        if self.motion.update(now, INPUT_REVISION.load(Ordering::Acquire), HOME_REVISION.load(Ordering::Acquire), theme_changed, ui.thermostat.target != self.target) {
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
        let decoded = self.renderer.take_decoded();
        if self.retained && !self.animating && !decoded && self.previous.as_ref() == Some(&state)
            && self.wake.is_none_or(|wake| now < wake) {
            if image { return self.renderer.retained_texture().map(|t| t.0.get()).ok_or("no retained texture".into()); }
            self.renderer.present_retained();
            return Ok(1);
        }
        self.previous = Some(state);
        let mut geometry = Geometry::for_framebuffer(480, 480);
        geometry.quarter_turns = 1;
        let prepared = Instant::now();
        let mut direct = self.animating && !image && !self.stencil;
        self.renderer.use_external_target(direct);
        self.renderer.begin(&geometry, theme.design);
        let begun = Instant::now();
        STENCIL_USED.store(false, Ordering::Relaxed);
        self.animating = theme.draw(&mut self.renderer.painter(&theme.dir), &ui)?;
        self.wake = theme.wake().map(|delay| now + delay.as_secs_f64());
        let drawn = Instant::now();
        self.renderer.finish_texture();
        if direct && STENCIL_USED.load(Ordering::Relaxed) {
            // The theme needs the stencil the framebuffer lacks: this frame
            // again, and the theme's later ones, through the frame texture,
            // whose opaque pass covers what was drawn.
            eprintln!("THEME Lua theme={} uses the stencil; frames drawn off-screen", self.name);
            self.stencil = true;
            direct = false;
            self.renderer.use_external_target(false);
            self.renderer.begin(&geometry, theme.design);
            self.animating = theme.draw(&mut self.renderer.painter(&theme.dir), &ui)?;
            self.wake = theme.wake().map(|delay| now + delay.as_secs_f64());
            self.renderer.finish_texture();
        }
        self.retained = !direct;
        if !direct && !image { self.renderer.present_retained(); }
        self.frames += 1;
        for (total, duration) in self.timing.iter_mut().zip([prepared-start, begun-prepared, drawn-begun, drawn.elapsed()]) { *total += duration.as_secs_f64(); }
        if self.frames == 120 {
            eprintln!("THEME painter state_ms={:.3} begin_ms={:.3} lua_ms={:.3} finish_ms={:.3} shapes={} flushes={} layers={} painting_ms={:.3}", self.timing[0]*1000.0/120.0, self.timing[1]*1000.0/120.0, self.timing[2]*1000.0/120.0, self.timing[3]*1000.0/120.0, self.renderer.shapes/120, self.renderer.flushes/120, self.renderer.layers_drawn, self.renderer.painting.as_secs_f64()*1000.0/120.0);
            self.frames=0; self.timing=[0.0; 4]; self.renderer.shapes=0; self.renderer.flushes=0; self.renderer.layers_drawn=0; self.renderer.painting=std::time::Duration::ZERO;
        }
        if image { self.renderer.retained_texture().map(|t| t.0.get()).ok_or("no retained texture".into()) } else { Ok(1) }
    }

    /// Seconds until the theme needs another frame: 0 while it moves, the
    /// time it woke for, or infinite while it stands still.
    fn next(&self) -> f32 {
        if self.animating { return 0.0; }
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
    let result = std::panic::catch_unwind(|| ENGINE.with(|slot| {
        let mut engine = slot.borrow_mut();
        if engine.is_none() { *engine = Some(Engine::new(loader, framebuffer)?); }
        engine.as_mut().unwrap().draw(image != 0, target)
    }));
    match result {
        Ok(Ok(texture)) => texture,
        Ok(Err(error)) => { eprintln!("THEME Lua: {error}"); 0 },
        Err(_) => { eprintln!("THEME Lua renderer panicked"); 0 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_state_overrides_stale_telemetry() {
        let data = serde_json::json!({"dev":{"temperature_scale":"F"},"shared":{"target_temperature_type":"off","current_temperature":0}});
        let mut ui = view(&data, 1.0, 0.0, 480.0);
        let mut state = NativeState { target: 21.5, current: 19.25, low: 18.0, high: 24.0, humidity: 47.0, unit: 0, mode: 2, hvac: 1, leaf: 1, range: 5 };
        assert_eq!(state.apply(&mut ui), 21.5);
        assert_eq!(ui.thermostat.unit, "C");
        assert_eq!(ui.thermostat.current, 19.25);
        assert_eq!(ui.thermostat.mode, "heat-cool");
        assert_eq!(ui.thermostat.hvac, "heating");
        assert_eq!(ui.thermostat.humidity, Some(47.0));
        assert!(ui.thermostat.leaf_on);
        assert_eq!((ui.thermostat.range, ui.thermostat.range_editing), (None, false));
        for (range, expected) in [(1, (Some("low"), true)), (2, (Some("high"), true)), (3, (Some("low"), false)), (4, (Some("high"), false))] {
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
        assert!(theme_id("nest-gen4"));
        assert!(!theme_id("../hyperdrive"));
        assert!(!theme_id(""));
    }
    #[test]
    fn existing_lua_themes_load() {
        for name in ["hyperdrive", "nest-gen4"] {
            let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../nestlocal/themes").join(name);
            let theme = Theme::load(&path).unwrap();
            assert_eq!(theme.design, 480.0);
            assert!(!theme.fonts.is_empty());
            let mut renderer = engine::soft::Renderer::new(240, 240).unwrap();
            renderer.use_fonts(&theme.fonts).unwrap();
            for mode in ["heat", "cool", "range", "off"] {
                let state = serde_json::json!({"dev":{"temperature_scale":"C","current_humidity":40},"shared":{"target_temperature_type":mode,"target_temperature":21,"target_temperature_low":19,"target_temperature_high":24,"current_temperature":19,"hvac_heater_state":true,"can_heat":true,"can_cool":true}});
                for now in [1.0, 1.1] {
                    renderer.begin(240.0, theme.design);
                    theme.draw(&mut renderer.painter(&theme.dir), &view(&state, now, 0.0, theme.design)).unwrap_or_else(|e| panic!("{name} in {mode}: {e}"));
                    assert!(renderer.pixmap.pixels().iter().filter(|p| p.red().max(p.green()).max(p.blue()) > 200).count() > 100, "{name} in {mode} must draw its readout");
                }
            }
        }
    }
    #[test]
    fn view_preserves_units_and_hvac_state() {
        let data = serde_json::json!({"dev":{"temperature_scale":"F","current_humidity":40},"shared":{"target_temperature_type":"heat","target_temperature":20,"current_temperature":19,"hvac_heater_state":true}});
        let ui = view(&data, 1.0, 0.0, 480.0);
        assert_eq!(ui.thermostat.target, 68.0);
        assert_eq!(ui.thermostat.unit, "F");
        assert_eq!(ui.thermostat.hvac, "heating");
        assert_eq!(ui.thermostat.humidity, Some(40.0));
    }
}
