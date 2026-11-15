//! Customizable home screen themes (-themes, gen 3): the themes (themes/,
//! which the patch installs in THEMES on the scratch volume) draw the home
//! screen's dial inside nlclient, on its GPU, through the theme preload
//! (platform/common/theme) and the theme library (theme_plugin.rs). This side
//! offers them on the UNFR0ZEN screen's Themes page, keeps the choice, and
//! hands the library the chosen theme with the device and shared buckets.
//!
//! The files, under settings_ui::DIR: theme.catalog (`id<TAB>label` lines)
//! and theme.status (the chosen theme's label) for the Themes page, which
//! answers in theme.request; theme.native.enabled while a theme is chosen
//! (absent for stock), theme.native.json for the library (`theme`, the
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

/// The themes the UNFR0ZEN screen offers, by the names it asks for them
/// with, and as it shows them: nlclient's own dial, then the themes.
const THEMES: &[(&str, &str)] = &[("stock", "Stock"), ("hyperdrive", "Hyperdrive"), ("nest-gen4", "Nest 4th Gen"),
                                  ("analog", "Analog"), ("lcd", "LCD"), ("weather", "Weather")];

/// The theme `name` as the UNFR0ZEN screen shows it, if it is one.
fn label(name: &str) -> Option<&'static str> {
    THEMES.iter().find(|(n, _)| *n == name).map(|(_, l)| *l)
}

/// Whether the theme `name` can be shown: stock always, another once
/// installed under `root`.
fn installed(root: &Path, name: &str) -> bool {
    name == "stock" || (label(name).is_some() && root.join(name).join("theme.lua").is_file())
}

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
fn state(theme: &str, dev: Value, shared: Value, weather: Fields) -> Vec<u8> {
    let shown = match (&dev, &shared) {
        (Value::Object(dev), Value::Object(shared)) => Value::Object(bridge::state(dev, shared)),
        _ => Value::Null,
    };
    json!({"theme": theme, "dev": dev, "shared": shared, "state": shown, "weather": weather}).to_string().into_bytes()
}

/// Makes `name` the theme shown: its label for the Themes page, and the
/// library on or off.
fn select(name: &str) -> std::io::Result<()> {
    settings_ui::publish("theme.status", label(name).unwrap_or("Stock").as_bytes())?;
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
        let missing: Vec<&str> = THEMES.iter().map(|(id, _)| *id).filter(|id| !installed(root, id)).collect();
        if !missing.is_empty() {
            logf!("theme: not installed in {INSTALLED}: {}; patch again to restore them", missing.join(", "));
        }
        let catalog: String = THEMES.iter().filter(|(id, _)| installed(root, id))
            .map(|(id, label)| format!("{id}\t{label}\n")).collect();
        let mut theme = std::fs::read_to_string(CHOSEN).map(|n| n.trim().to_string()).unwrap_or_default();
        if !installed(root, &theme) {
            theme = "stock".into();
        }
        let buckets = |key: &str| store.get(key).map_or(Value::Null, |o| Value::Object(o.value));
        let publish = |theme: &str, weather: &Fields| settings_ui::publish("theme.native.json",
            &state(theme, buckets(&device), buckets(&shared), weather.clone()));
        let mut weather = outdoor.known();
        let ready = marker().map_err(std::io::Error::other)
            .and_then(|png| settings_ui::publish("theme-marker.png", &png))
            .and_then(|()| settings_ui::publish("theme.catalog", catalog.as_bytes()))
            .and_then(|()| publish(&theme, &weather))
            .and_then(|()| select(&theme))
            .and_then(|()| settings_ui::publish("theme.native.ready", b"1"));
        if let Err(e) = ready {
            return logf!("theme: {e}");
        }
        logf!("theme: {}", label(&theme).unwrap_or("Stock"));
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
            let request = Path::new(settings_ui::DIR).join("theme.request");
            if let Ok(name) = std::fs::read_to_string(&request) {
                let _ = std::fs::remove_file(&request);
                let name = name.trim();
                if installed(root, name) && name != theme {
                    theme = name.to_string();
                    // The library is on only while its state names a theme:
                    // the state first for a theme, and for stock select
                    // alone, the state following below.
                    let state = if theme == "stock" { Ok(()) } else { publish(&theme, &weather) };
                    if let Err(e) = std::fs::write(CHOSEN, &theme).and(state).and_then(|()| select(&theme)) {
                        logf!("theme: {e}");
                    }
                    logf!("theme: {}", label(&theme).unwrap_or("Stock"));
                    changed = true;
                }
            }
            if changed {
                if let Err(e) = publish(&theme, &weather) {
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
    fn themes_are_stock_and_every_theme_in_themes() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("themes");
        let themes: std::collections::BTreeSet<String> = std::fs::read_dir(&source).unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.join("theme.lua").is_file())
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_string()).collect();
        let offered: std::collections::BTreeSet<String> = THEMES.iter().skip(1).map(|(id, _)| id.to_string()).collect();
        assert_eq!(THEMES[0].0, "stock");
        assert_eq!(themes, offered);
        assert_eq!(label("nest-gen4"), Some("Nest 4th Gen"));
        assert_eq!(label("unknown"), None);
    }

    #[test]
    fn only_installed_themes_are_offered() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("themes");
        assert!(installed(&source, "nest-gen4") && installed(&source, "stock"));
        let wiped = Path::new("/nonexistent/unfr0zen-themes");
        assert!(installed(wiped, "stock"));
        assert!(!installed(wiped, "nest-gen4"));
        assert!(!installed(&source, "common"), "a theme only if the screen names it");
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
        let first: Value = serde_json::from_slice(&state("hyperdrive", json!({"temperature_scale": "F"}), Value::Null,
                                                         weather)).unwrap();
        assert_eq!(first["theme"], "hyperdrive");
        assert_eq!(first["dev"]["temperature_scale"], "F");
        assert!(first["shared"].is_null() && first["state"].is_null(), "no state before the unit reports");
        assert_eq!(first["weather"]["icon"], "snow");
        let second: Value = serde_json::from_slice(&state("nest-gen4", json!({"temperature_scale": "C", "current_humidity": 41}),
            json!({"target_temperature_type": "heat", "target_temperature": 20.6, "hvac_heater_state": true}), Fields::new())).unwrap();
        assert_eq!(second["state"]["mode"], "heat");
        assert_eq!(second["state"]["target"], 20.5);
        assert_eq!(second["state"]["action"], "heating");
        assert_eq!(second["state"]["current_humidity"], 41);
    }
}
