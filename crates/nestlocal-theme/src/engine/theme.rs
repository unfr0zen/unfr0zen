//! Themes: Lua that draws the home screen from the UI's view of the thermostat.
//!
//! A theme is a folder whose `theme.lua` returns
//!
//! ```lua
//! return {
//!   name = "Nest 4th Gen", api = 1, design = 480,
//!   fonts = { regular = { "unit:AkkuratNest-Regular.ttf", "fonts/Inter-Regular.ttf" }, ... },
//!   draw = function(gfx, ui) ... return animating end,
//! }
//! ```
//!
//! A theme not animating can ask for its next frame later with
//! `gfx:wake(seconds)`; one showing `ui.clock.second` asks for it so, since
//! the second changing alone draws no frame.
//!
//! Lua runs sandboxed: the table, string, math and utf8 libraries, `log`,
//! and a `require` that only loads `.lua` files of the theme, or of the themes
//! beside it as `require("other:module")`. A theme reads
//! `ui` and draws through `gfx`; it cannot change settings or touch files.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use log::debug;
use mlua::{chunk::ChunkMode, Function, HookTriggers, Lua, LuaOptions, StdLib, Table, UserData, UserDataFields, UserDataMethods, Value as LuaValue, VmState};

use super::paint::{Align, Baseline, Color, Painter, TextStyle};

/// The theme API this engine provides.
pub const API: i64 = 1;

/// How long a theme may run at once: loading, or drawing one frame. Past
/// it the theme fails and the engine draws without it, so a looping theme
/// cannot stop the UI (and with it the heartbeat the watchdog needs).
const TIME_LIMIT: Duration = Duration::from_millis(250);

/// When the running Lua must stop, if it is running.
type Deadline = Rc<Cell<Option<Instant>>>;

/// A Lua state with only what themes may use.
fn sandbox(dir: &Path) -> mlua::Result<(Lua, Deadline)> {
    let lua = Lua::new_with(StdLib::TABLE | StdLib::STRING | StdLib::MATH, LuaOptions::default())?;
    let deadline: Deadline = Rc::new(Cell::new(None));
    let watched = deadline.clone();
    lua.set_hook(HookTriggers::new().every_nth_instruction(1_000_000), move |_, _| {
        match watched.get() {
            Some(end) if Instant::now() > end => Err(mlua::Error::runtime(format!("ran for longer than {TIME_LIMIT:?}"))),
            _ => Ok(VmState::Continue),
        }
    })?;
    let globals = lua.globals();
    // The base library is always loaded: two of it read files, and loading
    // or dumping chunks would let bytecode, which is not checked, in.
    for name in ["dofile", "loadfile", "load", "loadstring"] {
        globals.set(name, LuaValue::Nil)?;
    }
    globals.get::<Table>("string")?.set("dump", LuaValue::Nil)?;
    // Case for every language, not only ASCII ("KÜHLEN", not "KüHLEN"); a
    // text that is not UTF-8 changes as the C library would.
    for (name, unicode) in [("upper", true), ("lower", false)] {
        let f = lua.create_function(move |lua, text: mlua::LuaString| {
            let bytes = text.as_bytes();
            match std::str::from_utf8(&bytes) {
                Ok(t) => lua.create_string(if unicode { t.to_uppercase() } else { t.to_lowercase() }),
                Err(_) => lua.create_string(if unicode { bytes.to_ascii_uppercase() } else { bytes.to_ascii_lowercase() }),
            }
        })?;
        globals.get::<Table>("string")?.set(name, f)?;
    }
    let log = lua.create_function(|_, args: mlua::Variadic<LuaValue>| {
        let parts: Vec<String> = args.iter().map(|v| v.to_string().unwrap_or_else(|_| format!("{v:?}"))).collect();
        debug!("theme: {}", parts.join(" "));
        Ok(())
    })?;
    globals.set("log", log.clone())?;
    // A text in the language in use (the client's i18n), for a theme's own words.
    globals.set("tr", lua.create_function(|_, text: String| Ok(text))?)?;
    // Seconds on a monotonic clock, for themes to time their own drawing.
    let epoch = Instant::now();
    globals.set("clock", lua.create_function(move |_, ()| Ok(epoch.elapsed().as_secs_f64()))?)?;
    globals.set("print", log)?;
    // Themes sit side by side in one folder; a module of another theme runs
    // with that theme's own modules.
    let themes: Rc<Path> = dir.parent().unwrap_or(Path::new(".")).into();
    let own = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let require = requirer(&lua, themes, lua.create_table()?, own.clone(), own)?;
    globals.set("require", require)?;
    // The method closures live for the theme; each frame lends one scoped dispatcher.
    let methods: Table = lua.load(r#"
        local methods = {}
        for _, name in ipairs(...) do
            methods[name] = function(self, ...) return self:_dispatch(name, ...) end
        end
        return methods
    "#).call(lua.create_sequence_from(["circle", "ring", "arc", "gradient_arc", "radial_glow", "line", "lines", "rect", "polygon", "text", "measure", "has_glyphs", "image", "sprite", "wake", "save", "restore", "translate", "rotate", "scale", "alpha", "clear", "layer", "paint_layer", "warp_layer"])?)?;
    let metatable = lua.create_table()?;
    metatable.set("__index", methods)?;
    lua.set_named_registry_value("theme.gfx_metatable", metatable)?;
    Ok((lua, deadline))
}

/// Whether `name` can name a theme: its folder's name.
fn theme_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `require` for the modules of theme `theme`: `require("name")` loads its
/// `name.lua` (dots for subfolders), `require("other:name")` a module of the
/// theme `other` beside it. Each module is loaded once; another theme's
/// modules see a `require` of their own theme's.
fn requirer(lua: &Lua, themes: Rc<Path>, loaded: Table, main: String, theme: String) -> mlua::Result<Function> {
    lua.create_function(move |lua, name: String| {
        let (from, module) = name.split_once(':').unwrap_or((theme.as_str(), name.as_str()));
        let key = format!("{from}:{module}");
        let found: LuaValue = loaded.get(key.as_str())?;
        if !found.is_nil() {
            return Ok(found);
        }
        if !theme_name(from) || !module.split('.').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')) {
            return Err(mlua::Error::runtime(format!("require: bad module name {name:?}")));
        }
        let path = themes.join(from).join(format!("{}.lua", module.replace('.', "/")));
        let source = std::fs::read_to_string(&path)
            .map_err(|e| mlua::Error::runtime(format!("require {name}: {e}")))?;
        // Source only: bytecode is not checked.
        let mut chunk = lua.load(source).set_mode(ChunkMode::Text);
        if from == main {
            chunk = chunk.set_name(format!("@{module}.lua"));
        } else {
            let env = lua.create_table()?;
            let meta = lua.create_table()?;
            meta.set("__index", lua.globals())?;
            env.set_metatable(Some(meta))?;
            env.set("require", requirer(lua, themes.clone(), loaded.clone(), main.clone(), from.to_string())?)?;
            chunk = chunk.set_name(format!("@{key}.lua")).set_environment(env);
        }
        let module: LuaValue = chunk.eval()?;
        let module = if module.is_nil() { LuaValue::Boolean(true) } else { module };
        loaded.set(key.as_str(), module.clone())?;
        Ok(module)
    })
}

/// Runs `f` with the Lua time limit armed.
fn limited<T>(deadline: &Deadline, f: impl FnOnce() -> T) -> T {
    deadline.set(Some(Instant::now() + TIME_LIMIT));
    let result = f();
    deadline.set(None);
    result
}

/// A loaded theme.
pub struct Theme {
    lua: Lua,
    deadline: Deadline,
    pub name: String,
    pub dir: PathBuf,
    /// The size of the theme in the theme's own units.
    pub design: f32,
    /// Font names and their files.
    pub fonts: Vec<(String, PathBuf)>,
    draw: Function,
    /// Time spent turning the engine's view into Lua tables, until taken.
    pub converting: std::cell::Cell<Duration>,
    /// The soonest the last frame asked to be drawn again (`gfx:wake`), in
    /// seconds from it.
    wake: Cell<Option<f64>>,
    /// The last view given to Lua and its table: while only the time moves,
    /// the table is kept and its `now` updated.
    last: std::cell::RefCell<Option<(Ui, Table)>>,
    /// The last `ui.nest` given to Lua and its table, kept until nestlocal
    /// writes new data.
    nest: std::cell::RefCell<Option<(Nest, Table)>>,
}

impl std::fmt::Debug for Theme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Theme").field("name", &self.name).field("dir", &self.dir).finish_non_exhaustive()
    }
}

impl Theme {
    pub fn load(dir: &Path) -> Result<Theme, String> {
        let (lua, deadline) = sandbox(dir).map_err(|e| e.to_string())?;
        let entry = dir.join("theme.lua");
        let source = std::fs::read_to_string(&entry).map_err(|e| format!("{}: {e}", entry.display()))?;
        let table: Table = limited(&deadline, || lua.load(source).set_mode(ChunkMode::Text).set_name("@theme.lua").eval()).map_err(|e| e.to_string())?;
        let api: i64 = table.get("api").map_err(|e| format!("theme.lua api: {e}"))?;
        if api != API {
            return Err(format!("theme targets API {api}; this engine provides {API}"));
        }
        let name: String = table.get("name").map_err(|e| format!("theme.lua name: {e}"))?;
        let design: f32 = table.get::<Option<f32>>("design").map_err(|e| e.to_string())?.unwrap_or(480.0);
        let draw: Function = table.get("draw").map_err(|e| format!("theme.lua draw: {e}"))?;
        let mut fonts = Vec::new();
        if let Some(list) = table.get::<Option<Table>>("fonts").map_err(|e| e.to_string())? {
            for pair in list.pairs::<String, LuaValue>() {
                let (font, files) = pair.map_err(|e| format!("theme.lua fonts: {e}"))?;
                let files: Vec<String> = match files {
                    LuaValue::String(s) => vec![s.to_str().map_err(|e| e.to_string())?.to_string()],
                    LuaValue::Table(t) => t.sequence_values::<String>().collect::<Result<_, _>>()
                        .map_err(|e| format!("theme.lua fonts.{font}: {e}"))?,
                    _ => return Err(format!("theme.lua fonts.{font}: a file or a list of files")),
                };
                fonts.push((font.clone(), font_file(dir, &files).map_err(|e| format!("theme.lua fonts.{font}: {e}"))?));
            }
        }
        fonts.sort();
        debug!("theme: {name} loaded from {}: design {design}, {} fonts", dir.display(), fonts.len());
        Ok(Theme { lua, deadline, name, dir: dir.to_path_buf(), design, fonts, draw, converting: Default::default(), wake: Cell::new(None), last: Default::default(), nest: Default::default() })
    }

    /// Draws one frame; true while the theme is animating and wants another.
    pub fn draw(&self, painter: &mut dyn Painter, ui: &Ui) -> Result<bool, String> {
        let started = Instant::now();
        let mut last = self.last.borrow_mut();
        let state = match last.as_ref() {
            Some((prev, table)) if prev.same_but_now(ui) => {
                table.set("now", ui.now).map_err(|e| e.to_string())?;
                table.clone()
            }
            _ => {
                let mut nest = self.nest.borrow_mut();
                let nest = match nest.as_ref() {
                    Some((data, table)) if *data == ui.nest => table.clone(),
                    _ => {
                        let table = ui.nest.to_lua(&self.lua).map_err(|e| e.to_string())?;
                        *nest = Some((ui.nest.clone(), table.clone()));
                        table
                    }
                };
                let table = ui.to_lua(&self.lua, nest).map_err(|e| e.to_string())?;
                *last = Some((ui.clone(), table.clone()));
                table
            }
        };
        drop(last);
        self.converting.set(self.converting.get() + started.elapsed());
        self.wake.set(None);
        limited(&self.deadline, || self.lua.scope(|scope| {
            let gfx = scope.create_userdata(Gfx { painter, dir: &self.dir, wake: &self.wake })?;
            let result: LuaValue = self.draw.call((gfx, state))?;
            Ok(matches!(result, LuaValue::Boolean(true)))
        })).map_err(|e| e.to_string())
    }

    /// How long after the last frame the theme asked to be drawn again,
    /// when it did.
    pub fn wake(&self) -> Option<Duration> {
        self.wake.get().map(|s| Duration::from_secs_f64(s.max(0.0)))
    }
}

/// The first of a font's candidate files that exists: a file in the theme,
/// `other:FILE` in the theme `other` beside it, or `unit:NAME` in the unit's
/// fonts (UNIT_SHARE), which stay on the unit: they are not ours to
/// ship. The last is taken as it stands, so a missing font is reported by
/// name.
fn font_file(dir: &Path, files: &[String]) -> Result<PathBuf, String> {
    let resolve = |file: &str| match file.split_once(':') {
        Some(("unit", name)) if !name.contains('/') && !name.starts_with('.') => Ok(Path::new(UNIT_SHARE).join("fonts").join(name)),
        Some(("unit", _)) => Err(format!("{file:?} is not a font name")),
        Some((theme, path)) if theme_name(theme) => inside(&dir.parent().unwrap_or(Path::new(".")).join(theme), path),
        Some(_) => Err(format!("{file:?} names no theme")),
        None => inside(dir, file),
    };
    let paths = files.iter().map(|f| resolve(f)).collect::<Result<Vec<_>, _>>()?;
    paths.iter().find(|p| p.is_file()).or(paths.last()).cloned().ok_or_else(|| "no files".into())
}

/// Where the unit keeps its own fonts and pictures (`unit:` names).
const UNIT_SHARE: &str = "/nestlabs/share";

/// Where `run:` image names point: pictures unfr0zen's services make as
/// they run (Matter's pairing code).
const RUN_IMAGES: &str = "/var/run/unfr0zen";

/// An image a theme names: a file inside the theme, `unit:NAME` among the
/// unit's images or `run:NAME` among unfr0zen's.
fn image_file(dir: &Path, file: &str) -> Result<String, String> {
    let named = |name: &str, root: &str| if !name.contains('/') && !name.starts_with('.') {
        Ok(format!("{root}/{name}"))
    } else {
        Err(format!("{file:?} is not an image name"))
    };
    if let Some(name) = file.strip_prefix("unit:") {
        named(name, &Path::new(UNIT_SHARE).join("images").display().to_string())
    } else if let Some(name) = file.strip_prefix("run:") {
        named(name, RUN_IMAGES)
    } else {
        inside(dir, file).map(|_| file.to_string())
    }
}

/// A file a theme names, which must stay inside the theme's folder.
fn inside(dir: &Path, file: &str) -> Result<PathBuf, String> {
    let path = Path::new(file);
    if path.is_absolute() || path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(format!("{file:?} is outside the theme"));
    }
    Ok(dir.join(path))
}

/// The drawing API a theme's `draw` receives as `gfx`. Angles are degrees,
/// clockwise from 12 o'clock.
struct Gfx<'a> {
    painter: &'a mut dyn Painter,
    dir: &'a Path,
    wake: &'a Cell<Option<f64>>,
}

fn color(value: Option<String>) -> mlua::Result<Color> {
    match value {
        Some(text) => Color::parse(&text).map_err(mlua::Error::runtime),
        None => Ok(Color::WHITE),
    }
}

fn style(table: Option<Table>) -> mlua::Result<TextStyle> {
    let mut style = TextStyle {
        font: "regular".into(),
        size: 24.0,
        color: Color::WHITE,
        align: Align::Left,
        baseline: Baseline::Alphabetic,
        spacing: 0.0,
        outline: None,
        snap: true,
    };
    let Some(t) = table else { return Ok(style) };
    if let Some(c) = t.get::<Option<String>>("outline")? {
        style.outline = Some((Color::parse(&c).map_err(mlua::Error::runtime)?, t.get::<Option<f32>>("outline_width")?.unwrap_or(2.0)));
    }
    if let Some(font) = t.get::<Option<String>>("font")? { style.font = font; }
    if let Some(size) = t.get::<Option<f32>>("size")? { style.size = size; }
    style.color = color(t.get("color")?)?;
    if let Some(spacing) = t.get::<Option<f32>>("spacing")? { style.spacing = spacing; }
    if let Some(snap) = t.get::<Option<bool>>("snap")? { style.snap = snap; }
    style.align = match t.get::<Option<String>>("align")?.as_deref() {
        None | Some("left") => Align::Left,
        Some("center") => Align::Center,
        Some("right") => Align::Right,
        Some(other) => return Err(mlua::Error::runtime(format!("align {other:?}"))),
    };
    style.baseline = match t.get::<Option<String>>("baseline")?.as_deref() {
        None | Some("alphabetic") => Baseline::Alphabetic,
        Some("top") => Baseline::Top,
        Some("middle") => Baseline::Middle,
        Some("bottom") => Baseline::Bottom,
        Some(other) => return Err(mlua::Error::runtime(format!("baseline {other:?}"))),
    };
    Ok(style)
}

impl UserData for Gfx<'_> {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field_with(mlua::MetaMethod::Index, |lua| {
            let methods = lua.create_table()?;
            methods.set_metatable(Some(lua.named_registry_value::<Table>("theme.gfx_metatable")?))?;
            Ok(methods)
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method_mut("_dispatch", |lua, g, (name, args): (mlua::LuaString, mlua::MultiValue)| {
            dispatch(lua, g, &name.to_str()?, args)
        });
    }
}

fn gfx_method<A: mlua::FromLuaMulti, R: mlua::IntoLuaMulti>(
    lua: &Lua, g: &mut Gfx<'_>, args: mlua::MultiValue,
    call: impl FnOnce(&Lua, &mut Gfx<'_>, A) -> mlua::Result<R>,
) -> mlua::Result<mlua::MultiValue> {
    call(lua, g, A::from_lua_multi(args, lua)?)?.into_lua_multi(lua)
}

fn dispatch(lua: &Lua, g: &mut Gfx<'_>, name: &str, args: mlua::MultiValue) -> mlua::Result<mlua::MultiValue> {
    macro_rules! method {
        ($name:literal, $call:expr) => {
            if name == $name { return gfx_method(lua, g, args, $call); }
        };
    }
    method!("circle", |_, g, (x, y, r, c): (f32, f32, f32, Option<String>)| {
        g.painter.fill_circle(x, y, r, color(c)?);
        Ok(())
    });
    method!("ring", |_, g, (x, y, r, width, c): (f32, f32, f32, f32, Option<String>)| {
        g.painter.stroke_circle(x, y, r, color(c)?, width);
        Ok(())
    });
    method!("arc", |_, g, (x, y, r, from, to, width, c): (f32, f32, f32, f32, f32, f32, Option<String>)| {
        g.painter.arc(x, y, r, from.to_radians(), to.to_radians(), color(c)?, width);
        Ok(())
    });
    method!("gradient_arc", |_, g, (x, y, r, from, to, width, start, end): (f32, f32, f32, f32, f32, f32, String, String)| {
        g.painter.gradient_arc(x, y, r, from.to_radians(), to.to_radians(), color(Some(start))?, color(Some(end))?, width);
        Ok(())
    });
    method!("radial_glow", |_, g, (x, y, radius, steps): (f32, f32, f32, u32)| {
        g.painter.radial_glow(x, y, radius, steps);
        Ok(())
    });
    method!("line", |_, g, (x1, y1, x2, y2, width, c, round): (f32, f32, f32, f32, f32, Option<String>, Option<bool>)| {
        g.painter.line(x1, y1, x2, y2, color(c)?, width, round.unwrap_or(false));
        Ok(())
    });
    method!("lines", |_, g, (coords, width, c): (Vec<f32>, f32, Option<String>)| {
        if coords.len() % 4 != 0 {
            return Err(mlua::Error::runtime("lines takes x1, y1, x2, y2 for each segment"));
        }
        let segments: Vec<[f32; 4]> = coords.chunks(4).map(|s| [s[0], s[1], s[2], s[3]]).collect();
        g.painter.lines(&segments, color(c)?, width);
        Ok(())
    });
    method!("rect", |_, g, (x, y, w, h, c, radius): (f32, f32, f32, f32, Option<String>, Option<f32>)| {
        g.painter.rect(x, y, w, h, radius.unwrap_or(0.0), color(c)?);
        Ok(())
    });
    method!("polygon", |_, g, (coords, c): (Vec<f32>, Option<String>)| {
        if coords.len() < 6 || coords.len() % 2 != 0 {
            return Err(mlua::Error::runtime("polygon needs at least three x, y pairs"));
        }
        let points: Vec<(f32, f32)> = coords.chunks(2).map(|p| (p[0], p[1])).collect();
        g.painter.polygon(&points, color(c)?);
        Ok(())
    });
    // A whole text stock has is shown in the language in use (as `tr`),
    // in capitals when the theme wrote it in capitals.
    method!("text", |_, g, (text, x, y, s): (String, f32, f32, Option<Table>)| {
        Ok(g.painter.text(&text, x, y, &style(s)?))
    });
    method!("measure", |_, g, (text, s): (String, Option<Table>)| {
        Ok(g.painter.measure(&text, &style(s)?))
    });
    // Whether the font has the text's every character, as a unit
    // font's symbols, not in a stand-in.
    method!("has_glyphs", |_, g, (text, s): (String, Option<Table>)| {
        Ok(g.painter.has_glyphs(&text, &style(s)?))
    });
    // Whether the image was drawn: one that cannot be loaded (a unit
    // image another unit lacks) leaves its place empty.
    method!("image", |_, g, (file, x, y, w, h, alpha): (String, f32, f32, f32, f32, Option<f32>)| {
        let file = image_file(g.dir, &file).map_err(mlua::Error::runtime)?;
        Ok(g.painter.image(&file, x, y, w, h, alpha.unwrap_or(1.0)).is_ok())
    });
    // A frame of a sprite sheet; returns the sheet's frame count, or
    // nothing when the image cannot be loaded (a unit image off the unit).
    method!("sprite", |_, g, (file, frame, columns, w, h, x, y): (String, u32, u32, f32, f32, f32, f32)| {
        let file = image_file(g.dir, &file).map_err(mlua::Error::runtime)?;
        Ok(g.painter.sprite(&file, frame, columns, w, h, x, y).ok())
    });
    // Another frame in `seconds`, the soonest asked for in a frame.
    method!("wake", |_, g, seconds: f64| {
        g.wake.set(Some(g.wake.get().map_or(seconds, |w| w.min(seconds))));
        Ok(())
    });
    method!("save", |_, g, ()| { g.painter.save(); Ok(()) });
    method!("restore", |_, g, ()| { g.painter.restore(); Ok(()) });
    method!("translate", |_, g, (x, y): (f32, f32)| { g.painter.translate(x, y); Ok(()) });
    method!("rotate", |_, g, degrees: f32| { g.painter.rotate(degrees.to_radians()); Ok(()) });
    method!("scale", |_, g, (x, y): (f32, Option<f32>)| { g.painter.scale(x, y.unwrap_or(x)); Ok(()) });
    method!("alpha", |_, g, a: f32| { g.painter.alpha(a); Ok(()) });
    method!("clear", |_, g, c: Option<String>| { g.painter.clear(color(c)?); Ok(()) });
    method!("layer", |lua, g, (name, key, draw, w, h): (String, LuaValue, Function, Option<f32>, Option<f32>)| {
        let key = match key {
            LuaValue::String(s) => s.to_str()?.to_string(),
            LuaValue::Integer(n) => n.to_string(),
            LuaValue::Number(n) => n.to_string(),
            LuaValue::Boolean(b) => b.to_string(),
            _ => return Err(mlua::Error::runtime("layer keys are strings, numbers or booleans")),
        };
        let (dir, wake) = (g.dir, g.wake);
        let size = match (w, h) {
            (Some(w), Some(h)) if w > 0.0 && h > 0.0 => Some((w, h)),
            (None, None) => None,
            _ => return Err(mlua::Error::runtime("a layer's size is a positive width and height")),
        };
        g.painter.layer(&name, &key, size, &mut |painter| {
            lua.scope(|scope| draw.call::<()>(scope.create_userdata(Gfx { painter, dir, wake })?)).map_err(|e| e.to_string())
        }).map_err(mlua::Error::runtime)
    });
    method!("paint_layer", |_, g, (name, c, clip): (String, Option<String>, Option<Vec<f32>>)| {
        let points: Option<Vec<(f32, f32)>> = clip.map(|c| c.chunks(2).filter(|p| p.len() == 2).map(|p| (p[0], p[1])).collect());
        if points.as_ref().is_some_and(|p| p.len() < 3) {
            return Err(mlua::Error::runtime("paint_layer's clip needs at least three x, y pairs"));
        }
        g.painter.paint_layer(&name, color(c)?, points.as_deref()).map_err(mlua::Error::runtime)
    });
    method!("warp_layer", |_, g, (name, c): (String, Vec<f32>)| {
        let [x0, y0, x1, y1, x2, y2, x3, y3] = c[..] else {
            return Err(mlua::Error::runtime("warp_layer's corners are four x, y pairs"));
        };
        g.painter.warp_layer(&name, &[(x0, y0), (x1, y1), (x2, y2), (x3, y3)]).map_err(mlua::Error::runtime)
    });
    Err(mlua::Error::runtime(format!("unknown drawing method {name}")))
}

/// Everything a theme's `draw` reads, as the `ui` table: the home screen's
/// part of client-ng's view, which is all the theme shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ui {
    /// Seconds since the theme was first drawn.
    pub now: f64,
    /// Always "home".
    pub screen: &'static str,
    /// The theme in the theme's design units and in pixels.
    pub design: f32,
    pub dial_px: f32,
    pub thermostat: ThermostatView,
    /// The fan runs, for its timer or its schedule.
    pub fan_on: bool,
    pub clock: Clock,
    /// When things last changed, in `now`'s seconds, for animations.
    pub changed_screen: f64,
    pub changed_target: f64,
    /// When the target was last turned.
    pub turned_target: f64,
    pub nest: Nest,
}

/// What nestlocal gives the themes, as it comes (theme.native.json), as
/// `ui.nest`: `device` and `shared`, the unit's buckets; `state`, the
/// fields MQTT's state topic has (what Home Assistant shows); `weather`,
/// Home Assistant's outdoor temperature and humidity (`temp_c`,
/// `humidity`), conditions (`condition`, its own; `icon`, the unit's;
/// `sunrise`, `sunset`) and forecast (`forecast.hourly`, `forecast.daily`). Views of the same data compare
/// equal.
#[derive(Clone, Debug, Default)]
pub struct Nest(pub Rc<serde_json::Value>);

impl PartialEq for Nest {
    fn eq(&self, other: &Nest) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Nest {
    fn to_lua(&self, lua: &Lua) -> mlua::Result<Table> {
        let nest = lua.create_table()?;
        for (name, key) in [("device", "dev"), ("shared", "shared"), ("state", "state"), ("weather", "weather")] {
            nest.set(name, json(lua, &self.0[key])?)?;
        }
        Ok(nest)
    }
}

/// A JSON value as Lua's: null is nil, numbers are numbers, arrays count
/// from 1.
fn json(lua: &Lua, value: &serde_json::Value) -> mlua::Result<LuaValue> {
    use serde_json::Value as Json;
    Ok(match value {
        Json::Null => LuaValue::Nil,
        Json::Bool(b) => LuaValue::Boolean(*b),
        Json::Number(n) => LuaValue::Number(n.as_f64().unwrap_or(0.0)),
        Json::String(s) => LuaValue::String(lua.create_string(s)?),
        Json::Array(items) => {
            let table = lua.create_table_with_capacity(items.len(), 0)?;
            for (i, item) in items.iter().enumerate() {
                table.set(i + 1, json(lua, item)?)?;
            }
            LuaValue::Table(table)
        }
        Json::Object(fields) => {
            let table = lua.create_table_with_capacity(0, fields.len())?;
            for (key, item) in fields {
                table.set(key.as_str(), json(lua, item)?)?;
            }
            LuaValue::Table(table)
        }
    })
}

/// The local time.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Clock {
    /// 0 is Sunday.
    pub weekday: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// Seconds since 1970 (UTC), to the second, and the local time's
    /// offset from UTC in seconds: for times nestlocal gives as epoch
    /// seconds (sunrise, sunset, the forecast's hours).
    pub epoch: i64,
    pub utc_offset: i32,
    pub twelve_hour: bool,
    /// While someone is near Farsight: its digital clock's and its analog
    /// clock's date lines, "Saturday, October 17" and "SAT 17" in English.
    pub weekday_date: Option<String>,
    pub weekday_day: Option<String>,
}

/// The thermostat in the unit it is shown in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThermostatView {
    pub target: f32,
    pub current: f32,
    pub min: f32,
    pub max: f32,
    pub unit: &'static str,
    pub mode: String,
    pub hvac: &'static str,
    /// The stage it runs, 1 to 3, and whether Cool to Dry runs it.
    pub stage: u8,
    pub drying: bool,
    /// Emergency heat is the mode; auxiliary heat runs.
    pub emergency: bool,
    pub aux: bool,
    /// Alternate heat running: its stage, 0 when it is not.
    pub alt: u8,
    /// Airwave cooling with the fan.
    pub airwave: bool,
    /// Early-On running: "heat" or "cool".
    pub preconditioning: Option<&'static str>,
    /// The room near a safety temperature nothing else acts on.
    pub safety: bool,
    /// HvacBatteryDrained: heating with power stealing off has run the
    /// battery down.
    pub system_issue: bool,
    /// The target at its mode's far end on a system that also does the
    /// other: "cool" in heat at or under 10 °C (51 °F), "heat" in cool at
    /// or over 31 °C (89 °F). A press then opens the menus on Mode.
    pub press_to: Option<&'static str>,
    /// The leaf is earned (off and Eco always earn it).
    pub leaf_on: bool,
    /// How far the ring has turned towards its next step, in the shown
    /// unit: stock's dial follows every detent, the target every step.
    pub nudge: f32,
    /// Sunblock is correcting (`sunlight_correction_active`).
    pub sunblock: bool,
    /// The outside temperature in the shown unit, and stock's picture for
    /// the weather, when nestlocal gave them.
    pub outdoor: Option<f32>,
    pub weather_image: Option<&'static str>,
    /// Wi-Fi is connected.
    pub online: bool,
    /// While someone is near: the three hours Farsight's weather forecasts,
    /// their temperature as shown and their time.
    pub forecast: Vec<(f32, String)>,
    /// Minutes until the room reaches the target, while one is known.
    pub time_to_target: Option<u32>,
    /// The heat-cool range's ends.
    pub low: f32,
    pub high: f32,
    /// Relative humidity in percent, when known.
    pub humidity: Option<f32>,
    /// What the wiring can run.
    pub can_heat: bool,
    pub can_cool: bool,
    /// In heat-cool, the end of the range picked on the home screen
    /// ("low" or "high"), and whether it is being changed.
    pub range: Option<&'static str>,
    pub range_editing: bool,
    /// Eco is on: `target` is the Eco temperature.
    pub eco: bool,
    /// Seconds until a held-back system starts.
    pub starts_in: Option<u64>,
    /// Seconds a held-on system runs on before it stops.
    pub delayed_for: Option<u64>,
    /// The lockout `starts_in` or `delayed_for` counts: "compressor" or
    /// "cycle".
    pub lockout: Option<&'static str>,
    /// What Farsight shows: "target_temp", "current_temp", "analog_clock",
    /// "digital_clock" or "weather".
    pub farsight: &'static str,
}

impl Ui {
    /// Whether two views differ in nothing but the time.
    fn same_but_now(&self, o: &Ui) -> bool {
        Ui { now: o.now, ..self.clone() } == *o
    }

    fn to_lua(&self, lua: &Lua, nest: Table) -> mlua::Result<Table> {
        let ui = lua.create_table()?;
        ui.set("nest", nest)?;
        ui.set("now", self.now)?;
        ui.set("screen", self.screen)?;
        ui.set("fan_on", self.fan_on)?;
        let dial = lua.create_table()?;
        dial.set("size", self.design)?;
        dial.set("px", self.dial_px)?;
        dial.set("scale", self.dial_px / self.design)?;
        ui.set("dial", dial)?;
        let t = lua.create_table()?;
        t.set("target", self.thermostat.target)?;
        t.set("current", self.thermostat.current)?;
        t.set("min", self.thermostat.min)?;
        t.set("max", self.thermostat.max)?;
        t.set("unit", self.thermostat.unit)?;
        t.set("mode", self.thermostat.mode.as_str())?;
        t.set("hvac", self.thermostat.hvac)?;
        t.set("stage", self.thermostat.stage)?;
        t.set("drying", self.thermostat.drying)?;
        t.set("emergency", self.thermostat.emergency)?;
        t.set("aux", self.thermostat.aux)?;
        t.set("alt", self.thermostat.alt)?;
        t.set("airwave", self.thermostat.airwave)?;
        t.set("preconditioning", self.thermostat.preconditioning)?;
        t.set("safety", self.thermostat.safety)?;
        t.set("system_issue", self.thermostat.system_issue)?;
        t.set("press_to", self.thermostat.press_to)?;
        t.set("leaf_on", self.thermostat.leaf_on)?;
        t.set("nudge", self.thermostat.nudge)?;
        t.set("sunblock", self.thermostat.sunblock)?;
        t.set("outdoor", self.thermostat.outdoor)?;
        t.set("time_to_target", self.thermostat.time_to_target)?;
        t.set("weather_image", self.thermostat.weather_image)?;
        t.set("online", self.thermostat.online)?;
        let forecast = lua.create_table()?;
        for (i, (temp, time)) in self.thermostat.forecast.iter().enumerate() {
            let hour = lua.create_table()?;
            hour.set("temp", *temp)?;
            hour.set("time", time.as_str())?;
            forecast.set(i + 1, hour)?;
        }
        t.set("forecast", forecast)?;
        t.set("low", self.thermostat.low)?;
        t.set("high", self.thermostat.high)?;
        t.set("humidity", self.thermostat.humidity)?;
        t.set("can_heat", self.thermostat.can_heat)?;
        t.set("can_cool", self.thermostat.can_cool)?;
        t.set("range", self.thermostat.range)?;
        t.set("range_editing", self.thermostat.range_editing)?;
        t.set("eco", self.thermostat.eco)?;
        t.set("starts_in", self.thermostat.starts_in)?;
        t.set("delayed_for", self.thermostat.delayed_for)?;
        t.set("lockout", self.thermostat.lockout)?;
        t.set("farsight", self.thermostat.farsight)?;
        ui.set("thermostat", t)?;
        let clock = lua.create_table()?;
        clock.set("weekday", self.clock.weekday)?;
        clock.set("hour", self.clock.hour)?;
        clock.set("minute", self.clock.minute)?;
        clock.set("second", self.clock.second)?;
        clock.set("epoch", self.clock.epoch)?;
        clock.set("utc_offset", self.clock.utc_offset)?;
        clock.set("twelve_hour", self.clock.twelve_hour)?;
        clock.set("weekday_date", self.clock.weekday_date.as_deref())?;
        clock.set("weekday_day", self.clock.weekday_day.as_deref())?;
        ui.set("clock", clock)?;
        let changed = lua.create_table()?;
        changed.set("screen", self.changed_screen)?;
        changed.set("target", self.changed_target)?;
        changed.set("turned", self.turned_target)?;
        ui.set("changed", changed)?;
        Ok(ui)
    }
}

/// What the screen shows when no theme can draw: the target, plainly.
pub fn fallback(painter: &mut dyn Painter, ui: &Ui, error: &str) {
    let d = ui.design;
    painter.fill_circle(d / 2.0, d / 2.0, d / 2.0, Color::BLACK);
    let style = |size: f32| TextStyle {
        font: "fallback".into(),
        size,
        color: Color::WHITE,
        align: Align::Center,
        baseline: Baseline::Middle,
        spacing: 0.0,
        outline: None,
        snap: true,
    };
    let target = if ui.thermostat.unit == "F" { format!("{:.0}", ui.thermostat.target) } else { format!("{:.1}", ui.thermostat.target) };
    painter.text(&target, d / 2.0, d * 0.45, &style(d * 0.25));
    let first_line: String = error.lines().next().unwrap_or("").chars().take(48).collect();
    painter.text("theme error", d / 2.0, d * 0.68, &style(d * 0.05));
    painter.text(&first_line, d / 2.0, d * 0.75, &style(d * 0.03));
}


#[cfg(test)]
mod dispatch_tests {
    use super::*;

    #[test]
    fn drawing_methods_support_layers_and_expire_with_the_frame() {
        let (lua, _) = sandbox(Path::new(".")).unwrap();
        let mut renderer = crate::engine::soft::Renderer::new(4, 4).unwrap();
        renderer.begin(4.0, 4.0);
        let wake = Cell::new(None);
        let mut painter = renderer.painter(Path::new("."));
        lua.scope(|scope| {
            let gfx = scope.create_userdata(Gfx { painter: &mut painter, dir: Path::new("."), wake: &wake })?;
            lua.globals().set("saved", gfx)?;
            lua.load(r##"
                saved:clear("#123456")
                saved:layer("square", "1", function(g) g:rect(0, 0, 2, 2, "#ffffff") end, 4, 4)
                saved:paint_layer("square", "#ff0000")
                saved:wake(0.5)
                saved:wake(1.0)
            "##).exec()
        }).unwrap();
        drop(painter);
        assert_eq!(wake.get(), Some(0.5));
        let red = renderer.pixmap.pixel(0, 0).unwrap();
        let background = renderer.pixmap.pixel(3, 3).unwrap();
        assert_eq!((red.red(), red.green(), red.blue()), (255, 0, 0));
        assert_eq!((background.red(), background.green(), background.blue()), (18, 52, 86));
        assert!(lua.load(r##"saved:clear("#ffffff")"##).exec().is_err());
    }
}
