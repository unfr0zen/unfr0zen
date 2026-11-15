//! The bindings of the theme preload (platform/common/theme), which draws
//! customizable home screen themes in nlclient's own GPU context: where, in
//! the unit's stock nlclient, the dial, its figures, the background layer
//! and the natives the preload calls are. Discovery finds the dial and the
//! natives in any build; the rest comes from the builds reviewed by hand.
//! The patch writes them to BINDINGS as `name value` lines (hexadecimal
//! addresses relative to the executable's load base, or field offsets), and
//! the preload hooks nothing unless the vtable slots hold what they name.

use super::discovery;
use crate::shell::sha256_hex;
use crate::{device, Result};

/// Where the patch puts the bindings for the preload.
pub const BINDINGS: &str = "etc/unfr0zen/theme.bindings";

/// What discovery does not find, from each reviewed gen 3 build.
struct Reviewed {
    stock_sha256: &'static str,
    /// nlBackgroundLayer's draw: its vtable slot and the stock method (it
    /// reads its ARGB colour at +0x9c).
    background_slot: u32,
    background_draw: u32,
    /// The brightness manager's accessor and its ambient light sampler,
    /// read once to seed automatic brightness.
    brightness_manager: u32,
    read_als: u32,
    /// The settings and thermostat views' vtables, and where nlClient's
    /// globals keep the current view.
    settings_view: u32,
    thermostat_view: u32,
    view_offset: u32,
    /// The slider's displayed ambient temperature and the controller's
    /// humidity (16.16), and the globals' vtable slot for the leaf.
    current_temperature: u32,
    current_humidity: u32,
    leaf_slot: u32,
}

const REVIEWED: &[Reviewed] = &[
    // 6.4-8
    Reviewed {
        stock_sha256: "d28f3e8ec232c4ef122f72ceb1028ddf480a0ad8454677bfc9e818a13cbb7c52",
        background_slot: 0x6e1a08, background_draw: 0x21dbec,
        brightness_manager: 0xab388, read_als: 0x2282a4,
        settings_view: 0x6d6bc8, thermostat_view: 0x6d9bb8, view_offset: 0x1b8,
        current_temperature: 0xae050, current_humidity: 0xae068, leaf_slot: 0x34c,
    },
    // 6.5-14
    Reviewed {
        stock_sha256: "3f1c68d3ab1a86dc3ea1807c7c0bd0e0e33475efb895d6cb17c5389bcabc94d2",
        background_slot: 0x6bac30, background_draw: 0x236744,
        brightness_manager: 0xaac68, read_als: 0x240f4c,
        settings_view: 0x6b0564, thermostat_view: 0x6b3554, view_offset: 0x2f0,
        current_temperature: 0xad880, current_humidity: 0xad898, leaf_slot: 0x364,
    },
];

/// The natives the preload calls, by discovery's names.
const NATIVES: &[&str] = &[
    "native_activity_client", "native_activity_ctor", "native_activity_init", "native_delete", "native_enqueue",
    "native_event_type", "native_image_load", "native_layer_lookup", "native_menu_hidden", "native_menu_scale",
    "native_new",
];

/// Whether `stock` is a build the preload supports.
pub fn supported(stock: &[u8]) -> bool {
    let sha = sha256_hex(stock);
    REVIEWED.iter().any(|r| r.stock_sha256 == sha)
}

/// The bindings file for the unit's stock nlclient.
pub fn bindings(stock: &[u8]) -> Result<Vec<u8>> {
    let sha = sha256_hex(stock);
    let Some(reviewed) = REVIEWED.iter().find(|r| r.stock_sha256 == sha) else {
        return device("customizable themes do not support this firmware");
    };
    let found = discovery::discover(stock)?;
    let Some(dial) = &found.dial else {
        return device("customizable themes: nlclient's dial was not found");
    };
    let mut lines = vec![
        ("dial_slot", dial.slot), ("dial_draw", dial.draw), ("dial_event_slot", dial.event_slot),
        ("dial_event", dial.event), ("figures_slot", dial.figures_slot), ("figures_draw", dial.figures),
        ("background_slot", reviewed.background_slot), ("background_draw", reviewed.background_draw),
        ("brightness_manager", reviewed.brightness_manager), ("read_als", reviewed.read_als),
        ("settings_view", reviewed.settings_view), ("thermostat_view", reviewed.thermostat_view),
        ("view_offset", reviewed.view_offset), ("current_temperature", reviewed.current_temperature),
        ("current_humidity", reviewed.current_humidity), ("leaf_slot", reviewed.leaf_slot),
    ];
    for name in NATIVES {
        let Some(&address) = found.native_api.get(*name) else {
            return device(format!("customizable themes: nlclient has no {name}"));
        };
        lines.push((name.strip_prefix("native_").unwrap(), address));
    }
    Ok(lines.iter().map(|(name, value)| format!("{name} {value:#x}\n")).collect::<String>().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_reviewed_builds_are_bound() {
        assert!(!supported(b"not nlclient"));
        assert!(bindings(b"not nlclient").is_err());
    }

    /// The preload's names, which theme.c reads: each line is one of them.
    #[test]
    fn names_match_the_preload() {
        let preload = include_str!(concat!(env!("UNFR0ZEN_ROOT"), "/platform/common/theme/theme.c"));
        for name in ["dial_slot", "dial_draw", "dial_event_slot", "dial_event", "figures_slot", "figures_draw",
                     "background_slot", "background_draw", "brightness_manager", "read_als", "settings_view",
                     "thermostat_view", "view_offset", "current_temperature", "current_humidity", "leaf_slot"]
            .iter().copied().chain(NATIVES.iter().map(|n| n.strip_prefix("native_").unwrap())) {
            assert!(preload.contains(&format!("\"{name}\"")), "theme.c does not read {name}");
        }
    }

    #[test]
    fn reviewed_builds_bind_every_name() {
        let firmware = std::path::Path::new(env!("UNFR0ZEN_ROOT")).join(".work/firmware");
        for sample in ["nlclient-gen3-2025", "nlclient-gen3-6.5-14"] {
            let Ok(stock) = std::fs::read(firmware.join(sample)) else {
                assert!(std::env::var_os("NLCLIENT_REQUIRE_GOLDENS").is_none(), "strict checks need {sample}");
                continue;
            };
            let text = String::from_utf8(bindings(&stock).unwrap()).unwrap();
            assert_eq!(text.lines().count(), 16 + NATIVES.len(), "{sample}");
            assert!(text.lines().all(|l| l.split_once(" 0x").is_some()), "{sample}");
        }
    }
}
