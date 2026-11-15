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
//!
//! Themes are added by hand, so nothing of one is trusted: every file it
//! names is a regular file of a bounded size inside the themes' folder, its
//! manifest is checked field by field, its Lua is bounded in time and in
//! memory, and what it asks drawn is bounded in size and in number.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::Read;
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

/// How much memory a theme's Lua may hold: past it an allocation fails, and
/// the theme with it, rather than nlclient.
const MEMORY_LIMIT: usize = 16 << 20;

/// The most bytes a Lua file, a font and an image may have, and the most
/// pixels an image's side may have.
const SOURCE_LIMIT: u64 = 256 << 10;
const FONT_LIMIT: u64 = 8 << 20;
const IMAGE_LIMIT: u64 = 4 << 20;
const IMAGE_SIDE: u32 = 1024;

/// The longest a theme's folder name, its manifest's name and a font's or a
/// layer's name may be, in bytes.
const NAME_LIMIT: usize = 64;

/// The design units a theme may be across.
const DESIGN: std::ops::RangeInclusive<f64> = 120.0..=1920.0;

/// The most fonts a manifest names, and files tried for one.
const FONTS: usize = 16;
const FONT_FILES: usize = 4;

/// The most layers a theme keeps, and their area in all, in dials: each is
/// a texture for as long as the theme is shown.
const LAYERS: usize = 64;
const LAYER_AREA: f32 = 12.0;

/// The furthest a coordinate lies from the origin, in design units; the most
/// numbers a list of coordinates holds; the most bytes of a text drawn or
/// of a layer's key, and of a text a pattern is matched on; the deepest
/// `gfx:save` nests.
const REACH: f64 = 1e6;
const COORDS: usize = 16384;
const TEXT_LIMIT: usize = 1024;
const MATCHED_LIMIT: usize = 64 << 10;
const SAVES: u32 = 64;

/// The most steps matching a pattern may take at worst (`pattern_cost`):
/// the matcher is C, which the time limit cannot stop.
const PATTERN_STEPS: f64 = 1e7;

/// When the running Lua must stop, if it is running.
type Deadline = Rc<Cell<Option<Instant>>>;

/// A Lua state with only what themes may use.
fn sandbox(dir: &Path) -> mlua::Result<(Lua, Deadline)> {
    let lua = Lua::new_with(StdLib::TABLE | StdLib::STRING | StdLib::MATH, LuaOptions::default())?;
    lua.set_memory_limit(MEMORY_LIMIT)?;
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
    let string: Table = globals.get("string")?;
    string.set("dump", LuaValue::Nil)?;
    // Patterns backtrack: a match is refused where its worst case is long.
    for (name, plain) in [("find", Some(3)), ("match", None), ("gmatch", None), ("gsub", None)] {
        let real: Function = string.get(name)?;
        string.set(name, lua.create_function(move |_, args: mlua::MultiValue| {
            // A number is matched as the text Lua makes of it: short.
            let length = match args.front() {
                Some(LuaValue::String(s)) => s.as_bytes().len(),
                _ => 32,
            };
            let pattern = match args.get(1) {
                Some(LuaValue::String(s)) => s.as_bytes().to_vec(),
                _ => Vec::new(),
            };
            let literal = plain.is_some_and(|i| !matches!(args.get(i), None | Some(LuaValue::Nil | LuaValue::Boolean(false))));
            if length > MATCHED_LIMIT || (!literal && pattern_cost(length, &pattern) > PATTERN_STEPS) {
                return Err(mlua::Error::runtime(format!(
                    "string.{name}: {length} bytes are too many for pattern {:?}; match less text or simplify it",
                    String::from_utf8_lossy(&pattern))));
            }
            real.call::<mlua::MultiValue>(args)
        })?)?;
    }
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
        string.set(name, f)?;
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
    let themes: Rc<Path> = themes_of(dir).into();
    let own = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let require = requirer(&lua, themes, lua.create_table()?, Default::default(), own.clone(), own)?;
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

/// Whether `name` can name a theme (its folder's name) or a font: letters,
/// digits, `_` and `-`.
fn theme_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= NAME_LIMIT && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The steps matching `pattern` on `length` bytes may take at worst: each
/// repetition (`*`, `+`, `-`, `?`) outside a set may try every length at
/// every place the match may start.
fn pattern_cost(length: usize, pattern: &[u8]) -> f64 {
    let (mut i, mut repetitions) = (0, 0);
    while i < pattern.len() {
        match pattern[i] {
            b'%' => i += 1,
            b'[' => {
                i += 1;
                if pattern.get(i) == Some(&b'^') { i += 1; }
                if pattern.get(i) == Some(&b']') { i += 1; }
                while i < pattern.len() && pattern[i] != b']' {
                    if pattern[i] == b'%' { i += 1; }
                    i += 1;
                }
            }
            b'*' | b'+' | b'-' | b'?' => repetitions += 1,
            _ => {}
        }
        i += 1;
    }
    (length.max(1) as f64).powi(repetitions + 1)
}

/// The themes' folder: where `dir`, a theme's, sits.
fn themes_of(dir: &Path) -> &Path {
    dir.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."))
}

/// Checks a file a theme names before it is read: a regular file of at
/// most `limit` bytes that, its links followed, is still under `themes`.
fn checked(themes: &Path, path: &Path, limit: u64) -> Result<(), String> {
    let real = path.canonicalize().map_err(|e| e.to_string())?;
    let root = themes.canonicalize().map_err(|e| format!("{}: {e}", themes.display()))?;
    if !real.starts_with(&root) {
        return Err("leads outside the themes".into());
    }
    let file = std::fs::metadata(&real).map_err(|e| e.to_string())?;
    if !file.is_file() {
        return Err("is not a file".into());
    }
    if file.len() > limit {
        return Err(format!("is {} bytes; at most {limit} are read", file.len()));
    }
    Ok(())
}

/// A Lua file of a theme, as text: source only, bytecode is not checked.
fn source(themes: &Path, path: &Path) -> Result<String, String> {
    checked(themes, path, SOURCE_LIMIT)?;
    let mut bytes = Vec::new();
    std::fs::File::open(path).and_then(|f| f.take(SOURCE_LIMIT + 1).read_to_end(&mut bytes)).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > SOURCE_LIMIT {
        return Err(format!("is longer than {SOURCE_LIMIT} bytes"));
    }
    String::from_utf8(bytes).map_err(|_| "is not UTF-8 text".into())
}

/// `require` for the modules of theme `theme`: `require("name")` loads its
/// `name.lua` (dots for subfolders), `require("other:name")` a module of the
/// theme `other` beside it. Each module is loaded once (`loading` holds
/// those not yet loaded through, which cannot be required again); another
/// theme's modules see a `require` of their own theme's.
fn requirer(lua: &Lua, themes: Rc<Path>, loaded: Table, loading: Rc<RefCell<Vec<String>>>, main: String, theme: String) -> mlua::Result<Function> {
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
        if loading.borrow().contains(&key) {
            return Err(mlua::Error::runtime(format!("require {name}: the module requires itself")));
        }
        let path = themes.join(from).join(format!("{}.lua", module.replace('.', "/")));
        let source = source(&themes, &path).map_err(|e| mlua::Error::runtime(format!("require {name}: {e}")))?;
        let mut chunk = lua.load(source).set_mode(ChunkMode::Text);
        if from == main {
            chunk = chunk.set_name(format!("@{module}.lua"));
        } else {
            let env = lua.create_table()?;
            let meta = lua.create_table()?;
            meta.set("__index", lua.globals())?;
            env.set_metatable(Some(meta))?;
            env.set("require", requirer(lua, themes.clone(), loaded.clone(), loading.clone(), main.clone(), from.to_string())?)?;
            chunk = chunk.set_name(format!("@{key}.lua")).set_environment(env);
        }
        loading.borrow_mut().push(key.clone());
        let module = chunk.eval::<LuaValue>();
        loading.borrow_mut().pop();
        let module = module?;
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
    /// The layers the theme has drawn, by name, and their areas in dials.
    layers: RefCell<HashMap<String, f32>>,
    /// The theme's own images it has asked for, and whether each can be
    /// drawn: each is checked once.
    images: RefCell<HashMap<String, bool>>,
}

/// A number of the manifest: Lua's, not a string that reads as one.
fn number(value: &LuaValue) -> Option<f64> {
    match value {
        LuaValue::Integer(n) => Some(*n as f64),
        LuaValue::Number(n) => Some(*n),
        _ => None,
    }
}

impl std::fmt::Debug for Theme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Theme").field("name", &self.name).field("dir", &self.dir).finish_non_exhaustive()
    }
}

impl Theme {
    pub fn load(dir: &Path) -> Result<Theme, String> {
        if !dir.file_name().and_then(|n| n.to_str()).is_some_and(theme_name) {
            return Err(format!("{}: a theme's folder is named in letters, digits, - and _, {NAME_LIMIT} at most", dir.display()));
        }
        let (lua, deadline) = sandbox(dir).map_err(|e| e.to_string())?;
        let source = source(themes_of(dir), &dir.join("theme.lua")).map_err(|e| format!("theme.lua: {e}"))?;
        let manifest: LuaValue = limited(&deadline, || lua.load(source).set_mode(ChunkMode::Text).set_name("@theme.lua").eval()).map_err(|e| e.to_string())?;
        let LuaValue::Table(table) = manifest else {
            return Err(format!("theme.lua returns a {}, not the theme's table", manifest.type_name()));
        };
        let field = |name: &str| table.raw_get::<LuaValue>(name).map_err(|e| format!("theme.lua {name}: {e}"));
        match number(&field("api")?) {
            Some(api) if api == API as f64 => {}
            Some(api) => return Err(format!("theme targets API {api}; this engine provides {API}")),
            None => return Err(format!("theme.lua api: the theme API's number, {API}")),
        }
        let name = match field("name")? {
            LuaValue::String(s) => s.to_str().map_err(|_| "theme.lua name: not UTF-8 text")?.trim().to_string(),
            _ => return Err("theme.lua name: a text".into()),
        };
        if name.is_empty() || name.len() > NAME_LIMIT || name.chars().any(char::is_control) {
            return Err(format!("theme.lua name: one line of 1 to {NAME_LIMIT} bytes"));
        }
        let design = match field("design")? {
            LuaValue::Nil => 480.0,
            value => match number(&value) {
                Some(design) if DESIGN.contains(&design) => design as f32,
                _ => return Err(format!("theme.lua design: a number from {} to {}", DESIGN.start(), DESIGN.end())),
            },
        };
        let LuaValue::Function(draw) = field("draw")? else {
            return Err("theme.lua draw: a function".into());
        };
        let mut fonts = Vec::new();
        match field("fonts")? {
            LuaValue::Nil => {}
            LuaValue::Table(list) => {
                for pair in list.pairs::<LuaValue, LuaValue>() {
                    let (font, files) = pair.map_err(|e| format!("theme.lua fonts: {e}"))?;
                    let font = match font {
                        LuaValue::String(s) => s.to_string_lossy(),
                        _ => return Err("theme.lua fonts: each font has a name".into()),
                    };
                    if !theme_name(&font) {
                        return Err(format!("theme.lua fonts: {font:?} is not a name of letters, digits, - and _, {NAME_LIMIT} at most"));
                    }
                    if fonts.len() == FONTS {
                        return Err(format!("theme.lua fonts: more than {FONTS}"));
                    }
                    let files: Vec<String> = match files {
                        LuaValue::String(s) => vec![s.to_str().map_err(|e| e.to_string())?.to_string()],
                        LuaValue::Table(t) if t.raw_len() <= FONT_FILES => t.sequence_values::<LuaValue>().map(|file| match file {
                            Ok(LuaValue::String(s)) => s.to_str().map(|s| s.to_string()).map_err(|e| e.to_string()),
                            _ => Err("a list of files".to_string()),
                        }).collect::<Result<_, _>>().map_err(|e| format!("theme.lua fonts.{font}: {e}"))?,
                        _ => return Err(format!("theme.lua fonts.{font}: a file or a list of {FONT_FILES} files at most")),
                    };
                    let file = font_file(dir, &files).map_err(|e| format!("theme.lua fonts.{font}: {e}"))?;
                    fonts.push((font, file));
                }
            }
            _ => return Err("theme.lua fonts: a table of names and files".into()),
        }
        fonts.sort();
        debug!("theme: {name} loaded from {}: design {design}, {} fonts", dir.display(), fonts.len());
        Ok(Theme { lua, deadline, name, dir: dir.to_path_buf(), design, fonts, draw, converting: Default::default(), wake: Cell::new(None), last: Default::default(), nest: Default::default(), layers: Default::default(), images: Default::default() })
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
        let saves = Cell::new(0);
        limited(&self.deadline, || self.lua.scope(|scope| {
            let gfx = scope.create_userdata(Gfx { painter, dir: &self.dir, wake: &self.wake, design: self.design, saves: &saves, layers: &self.layers, images: &self.images })?;
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
/// ship. A theme's own file is checked (`checked`); when none exists the
/// last is reported missing by name.
fn font_file(dir: &Path, files: &[String]) -> Result<PathBuf, String> {
    let themes = themes_of(dir);
    let resolve = |file: &str| match file.split_once(':') {
        Some(("unit", name)) if !name.contains('/') && !name.starts_with('.') => Ok((Path::new(UNIT_SHARE).join("fonts").join(name), false)),
        Some(("unit", _)) => Err(format!("{file:?} is not a font name")),
        Some((theme, path)) if theme_name(theme) => inside(&themes.join(theme), path).map(|p| (p, true)),
        Some(_) => Err(format!("{file:?} names no theme")),
        None => inside(dir, file).map(|p| (p, true)),
    };
    let paths = files.iter().map(|f| resolve(f)).collect::<Result<Vec<_>, _>>()?;
    let Some((path, own)) = paths.iter().find(|(p, _)| p.exists()).or(paths.last()) else { return Err("no files".into()) };
    if !path.exists() {
        return Err(format!("{} is missing", path.display()));
    }
    if *own {
        checked(themes, path, FONT_LIMIT).map_err(|e| format!("{} {e}", path.display()))?;
    }
    Ok(path.clone())
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

/// Checks a theme's own image `file` before it is decoded: a PNG of a
/// bounded size (IMAGE_LIMIT, IMAGE_SIDE).
fn image_checked(dir: &Path, file: &str) -> Result<(), String> {
    let path = dir.join(file);
    checked(themes_of(dir), &path, IMAGE_LIMIT).map_err(|e| format!("image {file:?} {e}"))?;
    // A PNG's header: its signature, then the IHDR chunk's width and height.
    let mut header = [0u8; 24];
    std::fs::File::open(&path).and_then(|mut f| f.read_exact(&mut header)).map_err(|e| format!("image {file:?}: {e}"))?;
    let side = |at: usize| u32::from_be_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]]);
    if header[..8] != *b"\x89PNG\r\n\x1a\n" || header[12..16] != *b"IHDR" {
        return Err(format!("image {file:?} is not a PNG"));
    }
    if !(1..=IMAGE_SIDE).contains(&side(16)) || !(1..=IMAGE_SIDE).contains(&side(20)) {
        return Err(format!("image {file:?} is {} by {} pixels; at most {IMAGE_SIDE} a side are drawn", side(16), side(20)));
    }
    Ok(())
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
    /// The theme's design units across the dial, which bound its sizes.
    design: f32,
    /// How deep the frame's `save`s nest.
    saves: &'a Cell<u32>,
    /// The theme's layers and images (Theme's).
    layers: &'a RefCell<HashMap<String, f32>>,
    images: &'a RefCell<HashMap<String, bool>>,
}

impl Gfx<'_> {
    /// The file of an image the theme names, or none for one of its own
    /// that cannot be drawn (`image_checked`, once for each, the reason
    /// logged): its place stays empty, as a unit image's another unit lacks.
    fn image(&self, name: &str) -> mlua::Result<Option<String>> {
        if name.len() > TEXT_LIMIT {
            return Err(mlua::Error::runtime("an image's name is too long"));
        }
        let file = image_file(self.dir, name).map_err(mlua::Error::runtime)?;
        if name.starts_with("unit:") || name.starts_with("run:") {
            return Ok(Some(file));
        }
        let mut images = self.images.borrow_mut();
        let drawable = match images.get(name) {
            Some(&drawable) => drawable,
            None => {
                let checked = image_checked(self.dir, name);
                if let Err(error) = &checked {
                    eprintln!("THEME Lua: {error}; not drawn");
                }
                *images.entry(name.to_string()).or_insert(checked.is_ok())
            }
        };
        Ok(drawable.then_some(file))
    }
}

/// A text a theme draws or measures, of a bounded length.
fn short(text: &str) -> mlua::Result<()> {
    if text.len() > TEXT_LIMIT {
        return Err(mlua::Error::runtime(format!("a text of {} bytes; at most {TEXT_LIMIT} are drawn", text.len())));
    }
    Ok(())
}

/// A list of coordinates a theme gives: bounded in number and each in reach.
fn in_reach(list: &[f32]) -> mlua::Result<()> {
    if list.len() > COORDS {
        return Err(mlua::Error::runtime(format!("{} coordinates; at most {COORDS} are drawn at once", list.len())));
    }
    if list.iter().any(|v| !v.is_finite() || f64::from(v.abs()) > REACH) {
        return Err(mlua::Error::runtime("a coordinate is not a number in reach"));
    }
    Ok(())
}

fn color(value: Option<String>) -> mlua::Result<Color> {
    match value {
        Some(text) => Color::parse(&text).map_err(mlua::Error::runtime),
        None => Ok(Color::WHITE),
    }
}

fn style(table: Option<Table>, design: f32) -> mlua::Result<TextStyle> {
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
    // Glyphs are set at the size asked: a size beyond the dial's would
    // take memory by its square.
    if !(0.0..=design).contains(&style.size) {
        return Err(mlua::Error::runtime(format!("a text's size is from 0 to {design}, the dial's")));
    }
    if !(-design..=design).contains(&style.spacing) {
        return Err(mlua::Error::runtime(format!("a text's spacing is within {design}, the dial's")));
    }
    if style.outline.is_some_and(|(_, width)| !(0.0..=design / 8.0).contains(&width)) {
        return Err(mlua::Error::runtime(format!("a text's outline is from 0 to {} wide", design / 8.0)));
    }
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
    // Every number but a layer's key (any value) and a wake's delay (checked
    // there) is a place, a size, an angle or a count.
    if name != "layer" && name != "wake" {
        if let Some(at) = args.iter().position(|v| number(v).is_some_and(|n| !n.is_finite() || n.abs() > REACH)) {
            return Err(mlua::Error::runtime(format!("gfx:{name}: argument {} is not a number within {REACH}", at + 1)));
        }
    }
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
        if steps > 255 {
            return Err(mlua::Error::runtime("radial_glow builds of 255 steps at most"));
        }
        g.painter.radial_glow(x, y, radius, steps);
        Ok(())
    });
    method!("line", |_, g, (x1, y1, x2, y2, width, c, round): (f32, f32, f32, f32, f32, Option<String>, Option<bool>)| {
        g.painter.line(x1, y1, x2, y2, color(c)?, width, round.unwrap_or(false));
        Ok(())
    });
    method!("lines", |_, g, (coords, width, c): (Vec<f32>, f32, Option<String>)| {
        in_reach(&coords)?;
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
        in_reach(&coords)?;
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
        short(&text)?;
        Ok(g.painter.text(&text, x, y, &style(s, g.design)?))
    });
    method!("measure", |_, g, (text, s): (String, Option<Table>)| {
        short(&text)?;
        Ok(g.painter.measure(&text, &style(s, g.design)?))
    });
    // Whether the font has the text's every character, as a unit
    // font's symbols, not in a stand-in.
    method!("has_glyphs", |_, g, (text, s): (String, Option<Table>)| {
        short(&text)?;
        Ok(g.painter.has_glyphs(&text, &style(s, g.design)?))
    });
    // Whether the image was drawn: one that cannot be loaded (a unit
    // image another unit lacks) leaves its place empty.
    method!("image", |_, g, (file, x, y, w, h, alpha): (String, f32, f32, f32, f32, Option<f32>)| {
        let Some(file) = g.image(&file)? else { return Ok(false) };
        Ok(g.painter.image(&file, x, y, w, h, alpha.unwrap_or(1.0).clamp(0.0, 1.0)).is_ok())
    });
    // A frame of a sprite sheet; returns the sheet's frame count, or
    // nothing when the image cannot be loaded (a unit image off the unit).
    method!("sprite", |_, g, (file, frame, columns, w, h, x, y): (String, u32, u32, f32, f32, f32, f32)| {
        if columns == 0 || w < 1.0 || h < 1.0 {
            return Err(mlua::Error::runtime("a sprite sheet has a column at least, of frames a unit wide and high at least"));
        }
        let Some(file) = g.image(&file)? else { return Ok(None) };
        Ok(g.painter.sprite(&file, frame, columns, w, h, x, y).ok())
    });
    // Another frame in `seconds`, the soonest asked for in a frame.
    method!("wake", |_, g, seconds: f64| {
        if !seconds.is_finite() || seconds < 0.0 {
            return Err(mlua::Error::runtime("wake takes the seconds from now, 0 or more"));
        }
        g.wake.set(Some(g.wake.get().map_or(seconds, |w| w.min(seconds))));
        Ok(())
    });
    method!("save", |_, g, ()| {
        if g.saves.get() == SAVES {
            return Err(mlua::Error::runtime(format!("save nests {SAVES} deep at most; restore what a frame saves")));
        }
        g.saves.set(g.saves.get() + 1);
        g.painter.save();
        Ok(())
    });
    method!("restore", |_, g, ()| {
        g.saves.set(g.saves.get().saturating_sub(1));
        g.painter.restore();
        Ok(())
    });
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
        if name.is_empty() || name.len() > NAME_LIMIT || key.len() > TEXT_LIMIT {
            return Err(mlua::Error::runtime(format!("a layer's name is 1 to {NAME_LIMIT} bytes, its key {TEXT_LIMIT} at most")));
        }
        let (dir, wake, design, saves, layers, images) = (g.dir, g.wake, g.design, g.saves, g.layers, g.images);
        let size = match (w, h) {
            (Some(w), Some(h)) if w > 0.0 && h > 0.0 && w <= 2.0 * design && h <= 2.0 * design => Some((w, h)),
            (None, None) => None,
            _ => return Err(mlua::Error::runtime(format!("a layer's size is a width and height over 0, {} at most", 2.0 * design))),
        };
        {
            let mut layers = layers.borrow_mut();
            layers.insert(name.clone(), size.map_or(1.0, |(w, h)| w * h / (design * design)));
            if layers.len() > LAYERS || layers.values().sum::<f32>() > LAYER_AREA {
                return Err(mlua::Error::runtime(format!(
                    "layer {name}: a theme keeps {LAYERS} layers at most, {LAYER_AREA} dials in all; draw into a fixed set of names")));
            }
        }
        g.painter.layer(&name, &key, size, &mut |painter| {
            lua.scope(|scope| draw.call::<()>(scope.create_userdata(Gfx { painter, dir, wake, design, saves, layers, images })?)).map_err(|e| e.to_string())
        }).map_err(mlua::Error::runtime)
    });
    method!("paint_layer", |_, g, (name, c, clip): (String, Option<String>, Option<Vec<f32>>)| {
        if let Some(clip) = &clip { in_reach(clip)?; }
        let points: Option<Vec<(f32, f32)>> = clip.map(|c| c.chunks(2).filter(|p| p.len() == 2).map(|p| (p[0], p[1])).collect());
        if points.as_ref().is_some_and(|p| p.len() < 3) {
            return Err(mlua::Error::runtime("paint_layer's clip needs at least three x, y pairs"));
        }
        g.painter.paint_layer(&name, color(c)?, points.as_deref()).map_err(mlua::Error::runtime)
    });
    method!("warp_layer", |_, g, (name, c): (String, Vec<f32>)| {
        in_reach(&c)?;
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
        let (saves, layers, images) = (Cell::new(0), RefCell::default(), RefCell::default());
        let mut painter = renderer.painter(Path::new("."));
        lua.scope(|scope| {
            let gfx = scope.create_userdata(Gfx { painter: &mut painter, dir: Path::new("."), wake: &wake, design: 4.0, saves: &saves, layers: &layers, images: &images })?;
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

#[cfg(test)]
mod validation_tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT: AtomicU32 = AtomicU32::new(0);

    /// A themes folder holding the theme `t` made of `files`, and `t`'s.
    fn themes(files: &[(&str, &[u8])]) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("nestlocal-theme-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let dir = root.join("t");
        for (name, data) in files {
            let path = dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, data).unwrap();
        }
        (root, dir)
    }

    /// Why the theme whose theme.lua is `manifest` does not load.
    fn refused(manifest: &str) -> String {
        let (root, dir) = themes(&[("theme.lua", manifest.as_bytes())]);
        let error = Theme::load(&dir).expect_err(manifest);
        std::fs::remove_dir_all(root).unwrap();
        error
    }

    /// What a theme drawing with `body` (of `function(gfx, ui)`) returns,
    /// its second frame's: the first has drawn its layers.
    fn drawn(body: &str, files: &[(&str, &[u8])]) -> Result<bool, String> {
        let manifest = format!("return {{ name = 't', api = 1, draw = function(gfx, ui) {body} end }}");
        let mut all = vec![("theme.lua", manifest.as_bytes())];
        all.extend_from_slice(files);
        let (root, dir) = themes(&all);
        let theme = Theme::load(&dir).unwrap();
        let mut renderer = crate::engine::soft::Renderer::new(48, 48).unwrap();
        let ui = Ui { design: theme.design, dial_px: 48.0, ..Default::default() };
        let mut result = Ok(false);
        for _ in 0..2 {
            renderer.begin(48.0, theme.design);
            result = theme.draw(&mut renderer.painter(&theme.dir), &ui);
        }
        std::fs::remove_dir_all(root).unwrap();
        result
    }

    /// A PNG's first bytes, for an image `w` by `h`.
    fn png_header(w: u32, h: u32) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&w.to_be_bytes());
        png.extend_from_slice(&h.to_be_bytes());
        png
    }

    const DRAW: &str = "draw = function() end";

    #[test]
    fn a_manifest_is_checked_field_by_field() {
        assert!(refused("return 1").contains("not the theme's table"));
        assert!(refused(&format!("return {{ name = 't', {DRAW} }}")).contains("api"));
        assert!(refused(&format!("return {{ name = 't', api = '1', {DRAW} }}")).contains("api"));
        assert!(refused(&format!("return {{ name = 't', api = 2, {DRAW} }}")).contains("API 2"));
        assert!(refused(&format!("return {{ api = 1, {DRAW} }}")).contains("name"));
        assert!(refused(&format!("return {{ name = 7, api = 1, {DRAW} }}")).contains("name"));
        assert!(refused(&format!("return {{ name = ' ', api = 1, {DRAW} }}")).contains("name"));
        assert!(refused(&format!("return {{ name = 'a\\nb', api = 1, {DRAW} }}")).contains("name"));
        assert!(refused(&format!("return {{ name = ('n'):rep(65), api = 1, {DRAW} }}")).contains("name"));
        assert!(refused("return { name = 't', api = 1, draw = 'home' }").contains("draw"));
        for design in ["0", "-480", "1e9", "0/0", "'480'"] {
            assert!(refused(&format!("return {{ name = 't', api = 1, design = {design}, {DRAW} }}")).contains("design"), "{design}");
        }
        for (fonts, why) in [("7", "a table"), ("{ 'a.ttf' }", "has a name"), ("{ ['a b'] = 'a.ttf' }", "not a name"),
                             ("{ a = 7 }", "a file or a list"), ("{ a = {} }", "no files"), ("{ a = { 'a.ttf', 7 } }", "a list of files"),
                             ("{ a = { '1', '2', '3', '4', '5' } }", "4 files at most"), ("{ a = 'a.ttf' }", "is missing"),
                             ("{ a = '../a.ttf' }", "outside the theme"), ("{ a = '/etc/hosts' }", "outside the theme"),
                             ("{ a = 'no theme:a.ttf' }", "names no theme"), ("{ a = 'unit:../a.ttf' }", "not a font name")] {
            let error = refused(&format!("return {{ name = 't', api = 1, fonts = {fonts}, {DRAW} }}"));
            assert!(error.contains(why), "{fonts}: {error}");
        }
        let many: String = (0..17).map(|i| format!("f{i} = 'a.ttf', ")).collect();
        let (root, dir) = themes(&[("theme.lua", format!("return {{ name = 't', api = 1, fonts = {{ {many} }}, {DRAW} }}").as_bytes()), ("a.ttf", b"font")]);
        assert!(Theme::load(&dir).unwrap_err().contains("more than 16"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_theme_loads_from_its_folder_alone() {
        let (root, dir) = themes(&[("theme.lua", format!("return {{ name = ' Mine ', api = 1, design = 240, fonts = {{ a = {{ 'none.ttf', 'a.ttf' }} }}, {DRAW} }}").as_bytes()),
                                   ("a.ttf", b"font")]);
        let theme = Theme::load(&dir).unwrap();
        assert_eq!((theme.name.as_str(), theme.design), ("Mine", 240.0));
        assert_eq!(theme.fonts, [("a".to_string(), dir.join("a.ttf"))]);
        let named = root.join("not a theme");
        std::fs::rename(&dir, &named).unwrap();
        assert!(Theme::load(&named).unwrap_err().contains("folder is named"));
        assert!(Theme::load(&root.join("missing")).unwrap_err().contains("theme.lua"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn files_are_bounded_regular_and_inside_the_themes() {
        let long = format!("return {{ name = 't', api = 1, {DRAW} }} --{}", "x".repeat(SOURCE_LIMIT as usize));
        assert!(refused(&long).contains("at most 262144"));
        assert!(!refused("\x1bLua").is_empty(), "bytecode is not loaded");
        let (root, dir) = themes(&[("theme.lua", b"\xff\xfe")]);
        assert!(Theme::load(&dir).unwrap_err().contains("not UTF-8"));
        std::fs::create_dir_all(dir.join("big")).unwrap();
        std::fs::write(dir.join("theme.lua"), format!("return {{ name = 't', api = 1, fonts = {{ a = 'big' }}, {DRAW} }}")).unwrap();
        assert!(Theme::load(&dir).unwrap_err().contains("is not a file"));
        std::fs::remove_dir_all(dir.join("big")).unwrap();
        let font = std::fs::File::create(dir.join("big")).unwrap();
        font.set_len(FONT_LIMIT + 1).unwrap();
        assert!(Theme::load(&dir).unwrap_err().contains("at most 8388608"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn links_out_of_the_themes_are_not_followed() {
        let (root, dir) = themes(&[("theme.lua", format!("return {{ name = 't', api = 1, fonts = {{ a = 'a.ttf' }}, {DRAW} }}").as_bytes())]);
        let outside = root.with_extension("outside");
        std::fs::write(&outside, "return {}").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("a.ttf")).unwrap();
        assert!(Theme::load(&dir).unwrap_err().contains("leads outside the themes"));
        std::fs::write(dir.join("theme.lua"), "return require('linked')").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("linked.lua")).unwrap();
        assert!(Theme::load(&dir).unwrap_err().contains("leads outside the themes"));
        // A link to a theme beside it stays inside.
        std::fs::create_dir_all(root.join("other")).unwrap();
        std::fs::write(root.join("other/shared.lua"), format!("return {{ name = 't', api = 1, {DRAW} }}")).unwrap();
        std::fs::remove_file(dir.join("linked.lua")).unwrap();
        std::os::unix::fs::symlink(root.join("other/shared.lua"), dir.join("linked.lua")).unwrap();
        Theme::load(&dir).unwrap();
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_file(outside).unwrap();
    }

    #[test]
    fn lua_is_bounded_in_time_and_memory() {
        let started = Instant::now();
        assert!(refused("while true do end").contains("ran for longer"));
        assert!(refused("local t = {} for i = 1, 1e9 do t[i] = ('x'):rep(1000) .. i end").contains("memory"));
        assert!(refused("return ('x'):rep(2 ^ 30)").contains("memory"));
        assert!(drawn("while true do end", &[]).unwrap_err().contains("ran for longer"));
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        for gone in ["io", "os", "package", "debug", "ffi", "jit", "dofile", "loadfile", "load", "loadstring", "string.dump"] {
            assert!(refused(&format!("assert({gone} == nil, 'present') return 1")).contains("not the theme's table"), "{gone}");
        }
    }

    #[test]
    fn modules_load_once_and_never_through_themselves() {
        let (root, dir) = themes(&[("theme.lua", b"return require('a')"), ("a.lua", b"return require('b')"), ("b.lua", b"return require('a')")]);
        assert!(Theme::load(&dir).unwrap_err().contains("requires itself"));
        std::fs::write(dir.join("b.lua"), format!("return {{ name = 't', api = 1, {DRAW} }}")).unwrap();
        Theme::load(&dir).unwrap();
        for bad in ["../t/a", "a/b", "", "/etc/hosts", "no theme:a"] {
            std::fs::write(dir.join("theme.lua"), format!("return require('{bad}')")).unwrap();
            assert!(Theme::load(&dir).unwrap_err().contains("bad module name"), "{bad}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn patterns_match_where_their_worst_case_is_short() {
        assert_eq!(pattern_cost(10, b"plain"), 10.0);
        assert_eq!(pattern_cost(10, b"(%d+)%.(%d*)"), 1000.0);
        assert_eq!(pattern_cost(10, b"[%w-]+ %- %* [^]+*]"), 100.0, "only repetitions outside sets and escapes count");
        assert_eq!(drawn(r#"
            local whole, part = ("21.5"):match("^(%d+)%.(%d+)$")
            local words = 0
            for _ in ("partly cloudy night"):gmatch("%a+") do words = words + 1 end
            local long = ("a"):rep(5000)
            return whole == "21" and part == "5" and words == 3 and (long:gsub("a", "b")) == ("b"):rep(5000)
              and long:find("a-a-a-a-b", 1, true) == nil and ("a+b"):find("+", 1, true) == 2
        "#, &[]), Ok(true));
        let error = drawn(r#"return (("a"):rep(200)):find("a-a-a-a-a-a-b") ~= nil"#, &[]).unwrap_err();
        assert!(error.contains("string.find") && error.contains("too many"), "{error}");
        assert!(drawn(r#"return (("a"):rep(70000)):match("b") ~= nil"#, &[]).unwrap_err().contains("string.match"));
    }

    #[test]
    fn drawing_is_bounded() {
        for (body, why) in [
            ("gfx:circle(0/0, 0, 4, '#fff')", "argument 1"),
            ("gfx:rect(0, 0, 1e300, 4, '#fff')", "argument 3"),
            ("gfx:line(0, 0, 4, math.huge, 1, '#fff')", "argument 4"),
            ("gfx:lines({ 0, 0, 0/0, 4 }, 1, '#fff')", "not a number in reach"),
            ("gfx:polygon({ 0, 0, 4, 4, 1e30, 0 }, '#fff')", "not a number in reach"),
            ("local t = {} for i = 1, 16388 do t[i] = 0 end gfx:lines(t, 1, '#fff')", "at most 16384"),
            ("gfx:radial_glow(10, 10, 10, 1e6)", "255 steps"),
            ("gfx:text(('x'):rep(1025), 0, 0)", "at most 1024"),
            ("gfx:text('x', 0, 0, { size = 481 })", "size"),
            ("gfx:measure('x', { size = 0/0 })", "size"),
            ("gfx:text('x', 0, 0, { spacing = 1e9 })", "spacing"),
            ("gfx:text('x', 0, 0, { outline = '#000', outline_width = 500 })", "outline"),
            ("gfx:wake(-1)", "wake"),
            ("gfx:wake(0/0)", "wake"),
            ("gfx:wake(math.huge)", "wake"),
            ("for i = 1, 65 do gfx:save() end", "64 deep"),
            ("gfx:layer('a', '1', function() end, 961, 10)", "960 at most"),
            ("gfx:layer('a', '1', function() end, 0, 10)", "over 0"),
            ("gfx:layer('', '1', function() end)", "name"),
            ("gfx:layer(('n'):rep(65), '1', function() end)", "name"),
            ("gfx:layer('a', ('k'):rep(1025), function() end)", "key"),
            ("for i = 1, 65 do gfx:layer('l' .. i, '1', function() end, 1, 1) end", "64 layers at most"),
            ("for i = 1, 13 do gfx:layer('l' .. i, '1', function() end) end", "12 dials"),
            ("gfx:sprite('a.png', 0, 0, 8, 8, 0, 0)", "a column at least"),
            ("gfx:sprite('a.png', 0, 4, 8, 0, 0, 0)", "a column at least"),
            ("gfx:image('../a.png', 0, 0, 8, 8)", "outside the theme"),
            ("gfx:image('unit:../a.png', 0, 0, 8, 8)", "not an image name"),
        ] {
            let error = drawn(body, &[]).expect_err(body);
            assert!(error.contains(why), "{body}: {error}");
        }
        // Within the bounds, and frame after frame into the same layers.
        assert_eq!(drawn(r##"
            for i = 1, 64 do gfx:save() gfx:restore() end
            for i = 1, 63 do gfx:layer("l" .. i, ui.now, function(g) g:rect(0, 0, 1, 1, "#fff") end, 40, 40) end
            gfx:layer("dial", 1e12, function(g) g:clear("#000") end)
            gfx:text("21.5", 240, 240, { size = 480, spacing = -2, outline = "#000", outline_width = 4 })
            gfx:radial_glow(240, 240, 100, 255)
            gfx:wake(1e9)
            return gfx:image("unit:Weather_Default.png", 0, 0, 8, 8) == false
        "##, &[]), Ok(true));
        // An image that is no PNG of a size to draw leaves its place empty.
        let dir = Path::new("/nonexistent/themes/t");
        assert!(image_checked(dir, "missing.png").is_err());
        let (root, dir) = themes(&[("text.png", &[b'x'; 64]), ("wide.png", &png_header(1025, 8)), ("empty.png", &png_header(0, 8)), ("fine.png", &png_header(1024, 1))]);
        assert!(image_checked(&dir, "text.png").unwrap_err().contains("is not a PNG"));
        assert!(image_checked(&dir, "wide.png").unwrap_err().contains("1025 by 8 pixels"));
        assert!(image_checked(&dir, "empty.png").unwrap_err().contains("0 by 8 pixels"));
        assert_eq!(image_checked(&dir, "fine.png"), Ok(()));
        std::fs::File::create(dir.join("big.png")).unwrap().set_len(IMAGE_LIMIT + 1).unwrap();
        assert!(image_checked(&dir, "big.png").unwrap_err().contains("at most 4194304"));
        std::fs::remove_dir_all(root).unwrap();
        assert_eq!(drawn(r#"
            return gfx:image("text.png", 0, 0, 8, 8) == false and gfx:image("missing.png", 0, 0, 8, 8) == false
              and gfx:sprite("text.png", 0, 4, 8, 8, 0, 0) == nil
        "#, &[("text.png", &[b'x'; 64])]), Ok(true));
    }
}

