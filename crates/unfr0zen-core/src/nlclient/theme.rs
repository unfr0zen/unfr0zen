//! The bindings of the theme preload (platform/common/theme), which draws
//! customizable home screen themes in nlclient, in its GPU context or on the
//! CPU: where, in the unit's stock nlclient, the dial, its figures, the background layer
//! and the natives the preload calls are. The control model's vtable slots
//! and panel size come from the reviewed build.
//! The setup writes them to BINDINGS as `name value` lines (hexadecimal
//! addresses relative to the executable's load base, or field offsets), and
//! the preload hooks nothing unless the vtable slots hold what they name.

use super::{arm::Arm, discovery, elf::Elf};
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
/// 6.x's, the same in every reviewed 6.5-14 and 6.4-8 build and the gen E's 6.3-5.
const V6_GLOBALS: Globals =
    Globals { target: 0xa8, fahrenheit: 0xa0, low: 0xf0, high: 0xe8, mode: 0x138, off: 0, eco: 0 };

/// What discovery does not find, from each reviewed build.
struct Reviewed {
    stock_sha256: &'static str,
    /// The globals' vtable slot for the leaf.
    leaf_slot: u32,
    globals: Globals,
    dial_size: u32,
}

const REVIEWED: &[Reviewed] = &[
    // 6.4-8
    Reviewed {
        stock_sha256: "d28f3e8ec232c4ef122f72ceb1028ddf480a0ad8454677bfc9e818a13cbb7c52",
        leaf_slot: 0x34c,
        globals: V6_GLOBALS,
        dial_size: 480,
    },
    // 6.5-14
    Reviewed {
        stock_sha256: "3f1c68d3ab1a86dc3ea1807c7c0bd0e0e33475efb895d6cb17c5389bcabc94d2",
        leaf_slot: 0x364,
        globals: V6_GLOBALS,
        dial_size: 480,
    },
    // 6.5-14's other build, whose client globals hold four bytes more
    // before the current view.
    Reviewed {
        stock_sha256: "ea07f0d31e27ed859e0e1307a7a4de493deaa346fb5b111277beef30d4664611",
        leaf_slot: 0x364,
        globals: V6_GLOBALS,
        dial_size: 480,
    },
    // 6.0-6, whose globals' vtable is 5.9.4-5's with two more methods first.
    Reviewed {
        stock_sha256: "5d58299c1219e632a534014e8853d4f1b796c9ae1d42a6f0dc68ee0c88e10c9a",
        leaf_slot: 0x30c,
        globals: Globals { target: 0x8c, fahrenheit: 0x84, low: 0xd0, high: 0xc8, mode: 0x114, off: 0xc0, eco: 0x64 },
        dial_size: 480,
    },
    // The gen E's 6.5-14: the gen 3's 6.5-14 drawn on the CPU, its
    // globals laid out the same.
    Reviewed {
        stock_sha256: "be7410378c46ea13c72d2db6ac71be92c44519b7bfe8a72490c691cef584afb4",
        leaf_slot: 0x364,
        globals: V6_GLOBALS,
        dial_size: 320,
    },
    // The gen E's 6.3-5: 6.5-14's globals vtable up to the leaf, which comes
    // two slots earlier, and its globals' fields 0x120 lower.
    Reviewed {
        stock_sha256: "6bdf8634147c4adb4e0ac03dfc7fc8e3313c1c424420eb22f721a0012adaaf25",
        leaf_slot: 0x344,
        globals: V6_GLOBALS,
        dial_size: 320,
    },
    // The gen E's 6.1-9, whose globals' vtable has fewer slots before the
    // setpoints'.
    Reviewed {
        stock_sha256: "5da23c395b1ee0b258bc1d8a7a5a76931e778cf25afa8846e81245b38d088ca3",
        leaf_slot: 0x314,
        globals: Globals { target: 0x8c, fahrenheit: 0x84, low: 0xd0, high: 0xc8, mode: 0x118, off: 0, eco: 0 },
        dial_size: 320,
    },
    // 5.9.4-5, whose globals' vtable is laid out apart from 6.x's.
    Reviewed {
        stock_sha256: "67596bb71d0225a3b54966e96cc19bc469c0e593ee72e37bfe96fc30a3741bb7",
        leaf_slot: 0x2f8,
        globals: Globals { target: 0x84, fahrenheit: 0x7c, low: 0xc8, high: 0xc0, mode: 0x10c, off: 0xb8, eco: 0x5c },
        dial_size: 320,
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

/// The light, temperature and humidity getters all load the same sensor
/// interface from globals. The shared bucket and Away Temperatures layer both
/// read temperature. The RCS bucket reads humidity on newer clients and
/// the device bucket does on clients with direct virtual sensor getters.
fn sensor_getters(elf: &Elf, arm: &Arm, sampler: u32) -> Result<(u32, u32)> {
    let calls = arm.calls(sampler)?;
    let Some(&sensor) = calls.first() else { return device("theme: missing ambient sensor accessor") };
    let load = elf.read(sensor)?;
    discovery::require(load & 0xfffff000 == 0xe5900000, "unknown ambient sensor accessor")?;
    let bucket_calls = |class: &str, slot| -> Result<Vec<u32>> {
        let address = discovery::primary_vtable(elf, class)? + slot;
        discovery::relocated(elf, address, class)?;
        let mut calls = arm.calls(elf.read(address)?)?;
        calls.retain(|&target| elf.read(target).ok() == Some(load));
        calls.sort_unstable();
        calls.dedup();
        Ok(calls)
    };
    let display = bucket_calls("16nlAwayTempsLayer", 0x1c)?;
    let temperature = discovery::unique(
        bucket_calls("16nlCZSharedBucket", 0x24)?.into_iter().filter(|t| display.contains(t)),
        "displayed temperature",
    )?;
    let rcs = bucket_calls("21nlCZRCSSettingsBucket", 0x24)?;
    let humidity = if rcs.is_empty() {
        discovery::unique(bucket_calls("16nlCZDeviceBucket", 0x24)?, "device humidity")?
    } else {
        discovery::unique(rcs, "RCS humidity")?
    };
    discovery::require(temperature != humidity, "temperature and humidity getters coincide")?;
    Ok((temperature, humidity))
}

/// The bindings file for the unit's stock nlclient.
pub fn bindings(stock: &[u8]) -> Result<Vec<u8>> {
    let sha = sha256_hex(stock);
    let Some(reviewed) = REVIEWED.iter().find(|r| r.stock_sha256 == sha) else {
        return device("customizable themes do not support this firmware");
    };
    let found = discovery::discover_hashed(stock, &sha)?;
    let Some(dial) = &found.dial else {
        return device("customizable themes: nlclient's dial was not found");
    };
    let elf = Elf::parse(stock)?;
    let arm = Arm::new(&elf)?;
    let background_slot = discovery::primary_vtable(&elf, "17nlBackgroundLayer")? + 0x10;
    discovery::relocated(&elf, background_slot, "background draw")?;
    let background_draw = elf.read(background_slot)?;
    discovery::require(elf.text(background_draw), "background draw is not executable")?;
    // Both menu accessors start by loading the current view from globals.
    let mut offsets = Vec::new();
    for name in ["native_menu_scale", "native_menu_hidden"] {
        let code = arm.body(found.native_api[name])?;
        let offset = code
            .iter()
            .find(|i| i.word & 0xfffff000 == 0xe5900000)
            .ok_or_else(|| crate::Error::Device(format!("missing view field in {name}")))?;
        offsets.push(offset.word & 0xfff);
    }
    discovery::require(offsets[0] == offsets[1] && offsets[0] != 0, "menu view fields disagree")?;
    let bright =
        discovery::brightness(&elf).ok_or_else(|| crate::Error::Device("theme brightness manager not found".into()))?;
    let sampler = discovery::ambient_light_sampler(&elf, &arm, &bright)?;
    let (temperature, humidity) = sensor_getters(&elf, &arm, sampler)?;
    let mut lines = vec![
        ("dial_slot", dial.slot),
        ("dial_draw", dial.draw),
        ("dial_event_slot", dial.event_slot),
        ("dial_event", dial.event),
        ("figures_slot", dial.figures_slot),
        ("figures_draw", dial.figures),
        ("background_slot", background_slot),
        ("background_draw", background_draw),
        ("brightness_manager", bright.accessor),
        ("read_als", sampler),
        ("settings_view", discovery::primary_vtable(&elf, "14nlSettingsView")?),
        ("thermostat_view", discovery::primary_vtable(&elf, "16nlThermostatView")?),
        ("view_offset", offsets[0]),
        ("current_temperature", temperature),
        ("current_humidity", humidity),
        ("leaf_slot", reviewed.leaf_slot),
        ("target_slot", reviewed.globals.target),
        ("fahrenheit_slot", reviewed.globals.fahrenheit),
        ("low_slot", reviewed.globals.low),
        ("high_slot", reviewed.globals.high),
        ("mode_slot", reviewed.globals.mode),
        ("off_slot", reviewed.globals.off),
        ("eco_slot", reviewed.globals.eco),
        ("dial_size", reviewed.dial_size),
        ("software", u32::from(found.framebuffer.is_some())),
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
        let firmware = std::env::var_os("NLCLIENT_FIRMWARE_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("UNFR0ZEN_ROOT")).join(".work/firmware"));
        for (sample, expected) in [
            ("nlclient-gen3-2025", "742245b5bd73e178e4cb537d615c7db8d662779ae2931637030994803deae705"),
            ("nlclient-gen3-6.5-14", "c8c77f9e274760ec702e26872afa065d7a816374053f86e32c5dc98091edee27"),
            ("nlclient-gen3-6.5-14-2", "26daac6544e251348e6aa7e973ea42fa361c00a80b1305c0347a5fe056a9fdd6"),
            ("nlclient-gen3-6.0-6", "843accc0ff15f6be24bc77a23d2b084bd0d5bd346e3a435234705251cba6b7bb"),
            ("nlclient-genE-6.1-9", "187e2e6a5661282f8698a3b84785cbf5203d51655971b13e0f74840933ae6231"),
            ("nlclient-genE-6.3-5", "c4d1798f5693266be78be1523eb27ef8fa9e2790222a648447c02cb233a5e507"),
            ("nlclient-genE-6.5-14", "aca741a27514537d936c9d6d757e2c3db33a9dc5deab7448b4dcc79af596c2f6"),
            ("nlclient-gen2-2019", "06ad736fd836b2fb9b3c5c73ca60ab7e7088e3055020d9cdd0eb8526e007693f"),
        ] {
            let Ok(stock) = std::fs::read(firmware.join(sample)) else {
                assert!(std::env::var_os("NLCLIENT_REQUIRE_GOLDENS").is_none(), "strict checks need {sample}");
                continue;
            };
            let text = String::from_utf8(bindings(&stock).unwrap_or_else(|e| panic!("{sample}: {e}"))).unwrap();
            assert_eq!(sha256_hex(text.as_bytes()), expected, "{sample} theme bindings");
            assert_eq!(text.lines().count(), 25 + NATIVES.len(), "{sample}");
            assert!(text.lines().all(|l| l.split_once(" 0x").is_some()), "{sample}");
        }
    }
}
