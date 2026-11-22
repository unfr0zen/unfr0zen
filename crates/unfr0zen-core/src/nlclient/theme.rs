//! The bindings of the theme preload (platform/common/theme), which draws
//! customizable home screen themes in nlclient, in its GPU context or on the
//! CPU: where, in the unit's stock nlclient, the dial, its figures, the background layer
//! and the natives the preload calls are. Discovery finds the dial and the
//! natives in any build; the rest comes from the builds reviewed by hand.
//! The setup writes them to BINDINGS as `name value` lines (hexadecimal
//! addresses relative to the executable's load base, or field offsets), and
//! the preload hooks nothing unless the vtable slots hold what they name.

use super::discovery;
use crate::shell::sha256_hex;
use crate::{device, Result};

/// Where the setup puts the bindings for the preload.
pub const BINDINGS: &str = "etc/unfr0zen/theme.bindings";

/// The globals' vtable slots of the native control model's getters:
/// setpoints (Celsius 16.16), the unit and the mode; where the mode is the
/// schedule's, the system off and eco apart from it (0 for none).
struct Globals {
    target: u32,
    fahrenheit: u32,
    low: u32,
    high: u32,
    mode: u32,
    off: u32,
    eco: u32,
}
/// 6.x's, the same in every reviewed 6.x build.
const V6_GLOBALS: Globals =
    Globals { target: 0xa8, fahrenheit: 0xa0, low: 0xf0, high: 0xe8, mode: 0x138, off: 0, eco: 0 };

/// What discovery does not find, from each reviewed build.
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
    globals: Globals,
    /// The dial's size in pixels, and whether nlclient draws on the CPU
    /// (into the framebuffer) rather than in GLES2.
    dial_size: u32,
    software: bool,
}

const REVIEWED: &[Reviewed] = &[
    // 6.4-8
    Reviewed {
        stock_sha256: "d28f3e8ec232c4ef122f72ceb1028ddf480a0ad8454677bfc9e818a13cbb7c52",
        background_slot: 0x6e1a08,
        background_draw: 0x21dbec,
        brightness_manager: 0xab388,
        read_als: 0x2282a4,
        settings_view: 0x6d6bc8,
        thermostat_view: 0x6d9bb8,
        view_offset: 0x1b8,
        current_temperature: 0xae050,
        current_humidity: 0xae068,
        leaf_slot: 0x34c,
        globals: V6_GLOBALS,
        dial_size: 480,
        software: false,
    },
    // 6.5-14
    Reviewed {
        stock_sha256: "3f1c68d3ab1a86dc3ea1807c7c0bd0e0e33475efb895d6cb17c5389bcabc94d2",
        background_slot: 0x6bac30,
        background_draw: 0x236744,
        brightness_manager: 0xaac68,
        read_als: 0x240f4c,
        settings_view: 0x6b0564,
        thermostat_view: 0x6b3554,
        view_offset: 0x2f0,
        current_temperature: 0xad880,
        current_humidity: 0xad898,
        leaf_slot: 0x364,
        globals: V6_GLOBALS,
        dial_size: 480,
        software: false,
    },
    // 6.5-14's other build, whose client globals hold four bytes more
    // before the current view.
    Reviewed {
        stock_sha256: "ea07f0d31e27ed859e0e1307a7a4de493deaa346fb5b111277beef30d4664611",
        background_slot: 0x6df6f0,
        background_draw: 0x2373b0,
        brightness_manager: 0xaba44,
        read_als: 0x241f34,
        settings_view: 0x6d5024,
        thermostat_view: 0x6d8014,
        view_offset: 0x2f4,
        current_temperature: 0xae804,
        current_humidity: 0xae81c,
        leaf_slot: 0x364,
        globals: V6_GLOBALS,
        dial_size: 480,
        software: false,
    },
    // 5.9.4-5, whose globals' vtable is laid out apart from 6.x's.
    Reviewed {
        stock_sha256: "67596bb71d0225a3b54966e96cc19bc469c0e593ee72e37bfe96fc30a3741bb7",
        background_slot: 0x3f5b58,
        background_draw: 0x17d6d4,
        brightness_manager: 0x51940,
        read_als: 0x1831cc,
        settings_view: 0x3d6908,
        thermostat_view: 0x3e3270,
        view_offset: 0x100,
        current_temperature: 0x55174,
        current_humidity: 0x5522c,
        leaf_slot: 0x2f8,
        globals: Globals { target: 0x84, fahrenheit: 0x7c, low: 0xc8, high: 0xc0, mode: 0x10c, off: 0xb8, eco: 0x5c },
        dial_size: 320,
        software: true,
    },
];

/// The natives the preload calls, by discovery's names.
const NATIVES: &[&str] = &[
    "native_activity_client",
    "native_activity_ctor",
    "native_activity_init",
    "native_delete",
    "native_enqueue",
    "native_event_type",
    "native_image_load",
    "native_layer_lookup",
    "native_menu_hidden",
    "native_menu_scale",
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
        ("dial_slot", dial.slot),
        ("dial_draw", dial.draw),
        ("dial_event_slot", dial.event_slot),
        ("dial_event", dial.event),
        ("figures_slot", dial.figures_slot),
        ("figures_draw", dial.figures),
        ("background_slot", reviewed.background_slot),
        ("background_draw", reviewed.background_draw),
        ("brightness_manager", reviewed.brightness_manager),
        ("read_als", reviewed.read_als),
        ("settings_view", reviewed.settings_view),
        ("thermostat_view", reviewed.thermostat_view),
        ("view_offset", reviewed.view_offset),
        ("current_temperature", reviewed.current_temperature),
        ("current_humidity", reviewed.current_humidity),
        ("leaf_slot", reviewed.leaf_slot),
        ("target_slot", reviewed.globals.target),
        ("fahrenheit_slot", reviewed.globals.fahrenheit),
        ("low_slot", reviewed.globals.low),
        ("high_slot", reviewed.globals.high),
        ("mode_slot", reviewed.globals.mode),
        ("off_slot", reviewed.globals.off),
        ("eco_slot", reviewed.globals.eco),
        ("dial_size", reviewed.dial_size),
        ("software", u32::from(reviewed.software)),
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
        for name in [
            "dial_slot",
            "dial_draw",
            "dial_event_slot",
            "dial_event",
            "figures_slot",
            "figures_draw",
            "background_slot",
            "background_draw",
            "brightness_manager",
            "read_als",
            "settings_view",
            "thermostat_view",
            "view_offset",
            "current_temperature",
            "current_humidity",
            "leaf_slot",
            "target_slot",
            "fahrenheit_slot",
            "low_slot",
            "high_slot",
            "mode_slot",
            "off_slot",
            "eco_slot",
            "dial_size",
            "software",
        ]
        .iter()
        .copied()
        .chain(NATIVES.iter().map(|n| n.strip_prefix("native_").unwrap()))
        {
            assert!(preload.contains(&format!("\"{name}\"")), "theme.c does not read {name}");
        }
    }

    #[test]
    fn reviewed_builds_bind_every_name() {
        let firmware = std::path::Path::new(env!("UNFR0ZEN_ROOT")).join(".work/firmware");
        for sample in ["nlclient-gen3-2025", "nlclient-gen3-6.5-14", "nlclient-gen3-6.5-14-2", "nlclient-gen2-2019"] {
            let Ok(stock) = std::fs::read(firmware.join(sample)) else {
                assert!(std::env::var_os("NLCLIENT_REQUIRE_GOLDENS").is_none(), "strict checks need {sample}");
                continue;
            };
            let text = String::from_utf8(bindings(&stock).unwrap()).unwrap();
            assert_eq!(text.lines().count(), 25 + NATIVES.len(), "{sample}");
            assert!(text.lines().all(|l| l.split_once(" 0x").is_some()), "{sample}");
        }
    }
}
