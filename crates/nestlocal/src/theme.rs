//! Customizable home screen themes (-themes, gen 3): the themes (themes/,
//! embedded and unpacked under /var/run/unfr0zen/themes) draw the home
//! screen's dial inside nlclient, on its GPU, through the theme preload
//! (platform/common/theme) and the theme library (theme_plugin.rs). This side
//! offers them on the UNFR0ZEN screen's Themes page, keeps the choice, and
//! hands the library the chosen theme with the device and shared buckets.
//!
//! The files, under settings_ui::DIR: theme.catalog (`id<TAB>label` lines)
//! and theme.status (the chosen theme's label) for the Themes page, which
//! answers in theme.request; theme.native.enabled while a theme is chosen
//! (absent for stock), theme.native.json (`{"theme", "dev", "shared"}`) for
//! the library, theme-marker.png (480 pixels square, every one RGBA
//! 12 34 56 ff) for the preload to mark the theme's place in nlclient's
//! drawing with, and theme.native.ready once all of these are written.
//!
//! theme.rs, paint.rs, soft.rs, typeset.rs, display.rs and gpu/ are
//! client-ng's theme host and painters, built into the theme library only.

use std::path::Path;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use serde_json::{json, Value};

use crate::store::Store;
use crate::{logf, settings_ui};

const THEME_FILES: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/themes.rs"));

/// The theme chosen on the UNFR0ZEN screen, kept across restarts.
const CHOSEN: &str = "/media/data/nestlocal.theme";

/// The themes the UNFR0ZEN screen offers, by the names it asks for them
/// with, and as it shows them: nlclient's own dial, then the themes.
const THEMES: &[(&str, &str)] = &[("stock", "Stock"), ("hyperdrive", "Hyperdrive"), ("nest-gen4", "Nest 4th Gen")];

/// The theme `name` as the UNFR0ZEN screen shows it, if it is one.
fn label(name: &str) -> Option<&'static str> {
    THEMES.iter().find(|(n, _)| *n == name).map(|(_, l)| *l)
}

/// The themes' files, unpacked into `root` where the themes' `require` and
/// fonts read them.
fn unpack(root: &Path) -> std::io::Result<()> {
    for (name, data) in THEME_FILES {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap_or(root))?;
        std::fs::write(&path, data)?;
    }
    Ok(())
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

/// theme.native.json: the theme and the buckets the library draws from.
fn state(theme: &str, dev: Value, shared: Value) -> Vec<u8> {
    json!({"theme": theme, "dev": dev, "shared": shared}).to_string().into_bytes()
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
/// unit `serial`.
pub fn start(store: Arc<Store>, serial: String) {
    let (tx, rx) = mpsc::channel::<()>();
    let (device, shared) = (format!("device.{serial}"), format!("shared.{serial}"));
    let watched = (device.clone(), shared.clone());
    store.watch(move |o| if o.key == watched.0 || o.key == watched.1 { let _ = tx.send(()); });
    let spawned = std::thread::Builder::new().name("theme".into()).spawn(move || {
        if let Err(e) = unpack(&Path::new(settings_ui::DIR).join("themes")) {
            return logf!("theme: unpacking the themes: {e}");
        }
        let catalog: String = THEMES.iter().map(|(id, label)| format!("{id}\t{label}\n")).collect();
        let mut theme = std::fs::read_to_string(CHOSEN).map(|n| n.trim().to_string()).unwrap_or_default();
        if label(&theme).is_none() {
            theme = "stock".into();
        }
        let buckets = |key: &str| store.get(key).map_or(Value::Null, |o| Value::Object(o.value));
        let publish = |theme: &str| settings_ui::publish("theme.native.json", &state(theme, buckets(&device), buckets(&shared)));
        let ready = marker().map_err(std::io::Error::other)
            .and_then(|png| settings_ui::publish("theme-marker.png", &png))
            .and_then(|()| settings_ui::publish("theme.catalog", catalog.as_bytes()))
            .and_then(|()| publish(&theme))
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
            let request = Path::new(settings_ui::DIR).join("theme.request");
            if let Ok(name) = std::fs::read_to_string(&request) {
                let _ = std::fs::remove_file(&request);
                let name = name.trim();
                if label(name).is_some() && name != theme {
                    theme = name.to_string();
                    // The library is on only while its state names a theme:
                    // the state first for a theme, and for stock select
                    // alone, the state following below.
                    let state = if theme == "stock" { Ok(()) } else { publish(&theme) };
                    if let Err(e) = std::fs::write(CHOSEN, &theme).and(state).and_then(|()| select(&theme)) {
                        logf!("theme: {e}");
                    }
                    logf!("theme: {}", label(&theme).unwrap_or("Stock"));
                    changed = true;
                }
            }
            if changed {
                if let Err(e) = publish(&theme) {
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
    fn themes_are_stock_and_every_bundled_theme() {
        let themes: std::collections::BTreeSet<&str> = THEME_FILES.iter()
            .filter_map(|(name, _)| name.strip_suffix("/theme.lua")).collect();
        let offered: std::collections::BTreeSet<&str> = THEMES.iter().skip(1).map(|(id, _)| *id).collect();
        assert_eq!(THEMES[0].0, "stock");
        assert_eq!(themes, offered);
        assert_eq!(label("nest-gen4"), Some("Nest 4th Gen"));
        assert_eq!(label("unknown"), None);
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
    fn state_carries_the_theme_and_buckets() {
        let state: Value = serde_json::from_slice(&state("hyperdrive", json!({"temperature_scale": "F"}), Value::Null)).unwrap();
        assert_eq!(state["theme"], "hyperdrive");
        assert_eq!(state["dev"]["temperature_scale"], "F");
        assert!(state["shared"].is_null());
    }
}
