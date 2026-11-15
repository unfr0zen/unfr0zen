//! Customizable home screen themes (-themes, gen 3): the themes (themes/,
//! which the patch installs in THEMES on the scratch volume) draw the home
//! screen's dial inside nlclient, on its GPU, through the theme preload
//! (platform/common/theme) and the theme library (theme_plugin.rs). This side
//! offers them on the UNFR0ZEN screen's Themes page, keeps the choice, and
//! hands the library the chosen theme with the device and shared buckets.
//! Themes are added by hand too: the catalog follows the folder as it
//! changes, and a theme picked again, the one shown included, is loaded
//! again by the library (`picked`), which is how one that failed is retried.
//!
//! The files, under settings_ui::DIR: theme.catalog (`id<TAB>label` lines)
//! and theme.status (the chosen theme's label) for the Themes page, which
//! answers in theme.request; theme.native.enabled while a theme is chosen
//! (absent for stock), theme.native.json for the library (`theme`,
//! `picked`: how many times a theme has been picked since nestlocal started, the
//! device and shared buckets as `dev` and `shared`, `state` as MQTT's state
//! topic has it, and `weather`: Home Assistant's outdoor readings, conditions
//! and forecast, weather::Outdoor::known), theme-marker.png (480 pixels square, every one RGBA
//! 12 34 56 ff) for the preload to mark the theme's place in nlclient's
//! drawing with, and theme.native.ready once all of these are written.
//!
//! theme.rs, paint.rs, soft.rs, typeset.rs, display.rs and gpu/ are
//! client-ng's theme host and painters, built into the theme library only.

use std::path::Path;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use serde_json::{json, Value};

use crate::json::Fields;
use crate::store::Store;
use crate::weather::Outdoor;
use crate::{bridge, logf, settings_ui};

/// Where the patch installs the themes (unfr0zen-core's diamond3::scratch::THEMES),
/// one directory each. A scratch volume wiped since leaves only stock.
const INSTALLED: &str = "/media/scratch/.unfr0zen/themes";

/// The theme chosen on the UNFR0ZEN screen, kept across restarts.
const CHOSEN: &str = "/media/data/nestlocal.theme";

/// nlclient's own dial, offered first.
const STOCK: &str = "stock";

/// The theme library's limits (nestlocal-theme's engine/theme.rs): the
/// longest a theme's folder name and its label, and the largest its
/// theme.lua, in bytes. What is past them is not offered.
const NAME_LIMIT: usize = 64;
const SOURCE_LIMIT: u64 = 256 << 10;

/// Whether `name` can name a theme: its folder's name.
fn theme_id(name: &str) -> bool {
    !name.is_empty() && name.len() <= NAME_LIMIT && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Whether the theme `name` can be shown: stock always, another once its
/// folder under `root` has a theme.lua the library would read.
fn installed(root: &Path, name: &str) -> bool {
    name == STOCK || (theme_id(name)
        && std::fs::metadata(root.join(name).join("theme.lua")).is_ok_and(|f| f.is_file() && f.len() <= SOURCE_LIMIT))
}

/// The theme `name` as the UNFR0ZEN screen shows it: the name its
/// theme.lua gives it (a `name = "..."` line), where that is one the
/// library takes and a line of the catalog can hold, else its folder's.
fn label(root: &Path, name: &str) -> String {
    if name == STOCK {
        return "Stock".into();
    }
    std::fs::read_to_string(root.join(name).join("theme.lua")).ok()
        .and_then(|lua| lua.lines().find_map(|line| {
            let rest = line.trim_start().strip_prefix("name = \"")?;
            Some(rest[..rest.find('"')?].trim().to_string())
        }))
        .filter(|l| !l.is_empty() && l.len() <= NAME_LIMIT && !l.chars().any(char::is_control))
        .unwrap_or_else(|| name.to_string())
}

/// The themes the UNFR0ZEN screen offers: stock, then every theme under
/// `root` in the order its folder lists them.
fn themes(root: &Path) -> Vec<String> {
    let mut out = vec![STOCK.to_string()];
    if let Ok(entries) = std::fs::read_dir(root) {
        out.extend(entries.filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|name| name != STOCK && installed(root, name)));
    }
    out
}

/// What the catalog is made of: the themes offered, and when each one's
/// theme.lua, which names it, last changed.
fn listed(root: &Path) -> Vec<(String, Option<std::time::SystemTime>)> {
    themes(root).into_iter().map(|name| {
        let changed = std::fs::metadata(root.join(&name).join("theme.lua")).and_then(|f| f.modified()).ok();
        (name, changed)
    }).collect()
}

/// theme.catalog: an `id<TAB>label` line for each theme offered.
fn catalog(root: &Path, listed: &[(String, Option<std::time::SystemTime>)]) -> String {
    listed.iter().map(|(id, _)| format!("{id}\t{}\n", label(root, id))).collect()
}

/// How often the themes' folder is looked at for the catalog.
const CATALOG_EVERY: Duration = Duration::from_secs(2);

/// The marker image: the theme's size, in a colour the preload knows it by.
fn marker() -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, 480, 480);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&[0x12, 0x34, 0x56, 0xff].repeat(480 * 480)).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

/// theme.native.json: the theme and what the library draws from.
fn state(theme: &str, picked: u64, dev: Value, shared: Value, weather: Fields) -> Vec<u8> {
    let shown = match (&dev, &shared) {
        (Value::Object(dev), Value::Object(shared)) => Value::Object(bridge::state(dev, shared)),
        _ => Value::Null,
    };
    json!({"theme": theme, "picked": picked, "dev": dev, "shared": shared, "state": shown, "weather": weather}).to_string().into_bytes()
}

/// Makes `name` the theme shown: its label for the Themes page, and the
/// library on or off.
fn select(root: &Path, name: &str) -> std::io::Result<()> {
    settings_ui::publish("theme.status", label(root, name).as_bytes())?;
    let enabled = Path::new(settings_ui::DIR).join("theme.native.enabled");
    if name == "stock" {
        match std::fs::remove_file(enabled) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        settings_ui::publish("theme.native.enabled", b"1")
    }
}

/// Offers the themes and serves the chosen one, from the buckets of the
/// unit `serial` and the weather in `outdoor`.
pub fn start(store: Arc<Store>, serial: String, outdoor: Arc<Outdoor>) {
    let (tx, rx) = mpsc::channel::<()>();
    let (device, shared) = (format!("device.{serial}"), format!("shared.{serial}"));
    let watched = (device.clone(), shared.clone());
    store.watch(move |o| if o.key == watched.0 || o.key == watched.1 { let _ = tx.send(()); });
    let spawned = std::thread::Builder::new().name("theme".into()).spawn(move || {
        let root = Path::new(INSTALLED);
        let mut offered = listed(root);
        let mut looked = std::time::Instant::now();
        let mut picked = 0u64;
        let mut theme = std::fs::read_to_string(CHOSEN).map(|n| n.trim().to_string()).unwrap_or_default();
        if !installed(root, &theme) {
            theme = "stock".into();
        }
        let buckets = |key: &str| store.get(key).map_or(Value::Null, |o| Value::Object(o.value));
        let publish = |theme: &str, picked: u64, weather: &Fields| settings_ui::publish("theme.native.json",
            &state(theme, picked, buckets(&device), buckets(&shared), weather.clone()));
        let mut weather = outdoor.known();
        let ready = marker().map_err(std::io::Error::other)
            .and_then(|png| settings_ui::publish("theme-marker.png", &png))
            .and_then(|()| settings_ui::publish("theme.catalog", catalog(root, &offered).as_bytes()))
            .and_then(|()| publish(&theme, picked, &weather))
            .and_then(|()| select(root, &theme))
            .and_then(|()| settings_ui::publish("theme.native.ready", b"1"));
        if let Err(e) = ready {
            return logf!("theme: {e}");
        }
        logf!("theme: {}", label(root, &theme));
        loop {
            let mut changed = match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(()) => true,
                Err(mpsc::RecvTimeoutError::Timeout) => false,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            };
            changed |= rx.try_iter().count() > 0;
            let known = outdoor.known();
            if known != weather {
                weather = known;
                changed = true;
            }
            // The Themes page reads the catalog as it opens: a theme added
            // or removed by hand is there by then.
            if looked.elapsed() >= CATALOG_EVERY {
                looked = std::time::Instant::now();
                let now = listed(root);
                if now != offered {
                    offered = now;
                    if let Err(e) = settings_ui::publish("theme.catalog", catalog(root, &offered).as_bytes()) {
                        logf!("theme: {e}");
                    }
                }
            }
            let request = Path::new(settings_ui::DIR).join("theme.request");
            if let Ok(name) = std::fs::read_to_string(&request) {
                let _ = std::fs::remove_file(&request);
                let name = name.trim();
                // The theme shown picked again is loaded again.
                if installed(root, name) && (name != theme || name != STOCK) {
                    theme = name.to_string();
                    picked += 1;
                    // The library is on only while its state names a theme:
                    // the state first for a theme, and for stock select
                    // alone, the state following below.
                    let state = if theme == "stock" { Ok(()) } else { publish(&theme, picked, &weather) };
                    if let Err(e) = std::fs::write(CHOSEN, &theme).and(state).and_then(|()| select(root, &theme)) {
                        logf!("theme: {e}");
                    }
                    logf!("theme: {}", label(root, &theme));
                    changed = true;
                }
            }
            if changed {
                if let Err(e) = publish(&theme, picked, &weather) {
                    logf!("theme: {e}");
                }
            }
        }
    });
    if let Err(e) = spawned {
        logf!("theme: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_comes_first_then_every_theme_folder_as_listed() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("themes");
        let listed: Vec<String> = std::fs::read_dir(&source).unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|name| source.join(name).join("theme.lua").is_file()).collect();
        let offered = themes(&source);
        assert_eq!(offered[0], "stock");
        assert_eq!(offered[1..], listed[..]);
        assert!(!offered.iter().any(|t| t == "common"));
        assert_eq!(label(&source, "nest-gen4"), "Nest 4th Gen");
        assert_eq!(label(&source, "lcd"), "LCD");
        assert_eq!(label(&source, "stock"), "Stock");
        assert_eq!(label(&source, "missing"), "missing");
        assert_eq!(themes(Path::new("/nonexistent/unfr0zen-themes")), ["stock"]);
    }

    #[test]
    fn only_installed_themes_are_offered() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("themes");
        assert!(installed(&source, "nest-gen4") && installed(&source, "stock"));
        let wiped = Path::new("/nonexistent/unfr0zen-themes");
        assert!(installed(wiped, "stock"));
        assert!(!installed(wiped, "nest-gen4"));
        assert!(!installed(&source, "common"), "a theme has a theme.lua");
        assert!(!installed(&source, "../themes/nest-gen4"), "a theme is a folder's name");
    }

    #[test]
    fn themes_the_library_would_refuse_are_not_offered() {
        let root = std::env::temp_dir().join(format!("nestlocal-themes-{}", std::process::id()));
        let theme = |name: &str, lua: &str| {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(root.join(name).join("theme.lua"), lua).unwrap();
        };
        theme("mine", "return {\n  name = \" My Theme \",\n}");
        theme("tabbed", "return { }\nname = \"a\tb\",");
        theme("long", &format!("name = \"{}\",", "n".repeat(NAME_LIMIT + 1)));
        theme("huge", &" ".repeat(SOURCE_LIMIT as usize + 1));
        theme(&"n".repeat(NAME_LIMIT + 1), "");
        theme("not a theme", "");
        std::fs::create_dir_all(root.join("folder/theme.lua")).unwrap();
        let mut offered = themes(&root);
        offered.sort();
        assert_eq!(offered, ["long", "mine", "stock", "tabbed"]);
        assert_eq!(label(&root, "mine"), "My Theme");
        assert_eq!(label(&root, "tabbed"), "tabbed", "a label is one field of a catalog line");
        assert_eq!(label(&root, "long"), "long");
        // The catalog follows the folder: a theme added, renamed in its
        // theme.lua, or removed.
        let before = listed(&root);
        assert_eq!(before, listed(&root));
        assert!(catalog(&root, &before).contains("mine\tMy Theme\n") && catalog(&root, &before).starts_with("stock\tStock\n"));
        theme("added", "name = \"Added\",");
        let added = listed(&root);
        assert!(added != before && catalog(&root, &added).contains("added\tAdded\n"));
        let old = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        std::fs::File::options().write(true).open(root.join("added/theme.lua")).unwrap().set_modified(old).unwrap();
        assert!(listed(&root) != added, "a theme.lua changed is read again");
        std::fs::remove_dir_all(root.join("added")).unwrap();
        assert_eq!(listed(&root).len(), before.len());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn marker_is_the_colour_the_preload_looks_for() {
        let png = marker().unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(png)).read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!((info.width, info.height), (480, 480));
        assert!(pixels.as_chunks::<4>().0.iter().all(|p| *p == [0x12, 0x34, 0x56, 0xff]));
    }

    #[test]
    fn state_carries_the_theme_buckets_mqtt_state_and_weather() {
        let weather = json!({"temp_c": 4.5, "icon": "snow"}).as_object().unwrap().clone();
        let first: Value = serde_json::from_slice(&state("hyperdrive", 2, json!({"temperature_scale": "F"}), Value::Null,
                                                         weather)).unwrap();
        assert_eq!(first["theme"], "hyperdrive");
        assert_eq!(first["picked"], 2);
        assert_eq!(first["dev"]["temperature_scale"], "F");
        assert!(first["shared"].is_null() && first["state"].is_null(), "no state before the unit reports");
        assert_eq!(first["weather"]["icon"], "snow");
        let second: Value = serde_json::from_slice(&state("nest-gen4", 0, json!({"temperature_scale": "C", "current_humidity": 41}),
            json!({"target_temperature_type": "heat", "target_temperature": 20.6, "hvac_heater_state": true}), Fields::new())).unwrap();
        assert_eq!(second["state"]["mode"], "heat");
        assert_eq!(second["state"]["target"], 20.5);
        assert_eq!(second["state"]["action"], "heating");
        assert_eq!(second["state"]["current_humidity"], 41);
    }
}
