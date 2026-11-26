//! The bindings of the theme preload (platform/common/theme), which draws
//! customizable home screen themes in nlclient, in its GPU context or on the
//! CPU: where, in the unit's stock nlclient, the dial, its figures, the background layer
//! and the natives the preload calls are. The stock figures and temperature
//! screen expose the control model through their virtual calls.
//! The setup writes them to BINDINGS as `name value` lines (hexadecimal
//! addresses relative to the executable's load base, or field offsets), and
//! the preload hooks nothing unless the vtable slots hold what they name.

use super::{
    arm::{scalar_field, Arm, Ins, Op},
    discovery,
    elf::Elf,
};
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

/// Whether the stock firmware exposes all the ABI evidence needed by the preload.
pub fn supported(stock: &[u8]) -> bool {
    bindings(stock).is_ok()
}

/// Virtual call sites and their vtable offsets, recovered from the register
/// supplying the indirect call. A caller may pass arguments between the
/// method load and the call.
fn virtual_calls(code: &[Ins]) -> Vec<(usize, u32)> {
    let mut calls = Vec::new();
    for (index, ins) in code.iter().enumerate().filter(|(_, i)| i.name == "blx") {
        let Some(register) = ins.ops[0].reg() else { continue };
        let Some(load) = code[index.saturating_sub(8)..index]
            .iter()
            .rev()
            .take_while(|i| !matches!(i.name.as_str(), "bl" | "blx"))
            .find(|i| i.writes.iter().any(|r| r == register))
        else {
            continue;
        };
        if load.name != "ldr" || ins.address - load.address > 32 {
            continue;
        }
        if let Some((base, slot, false)) = load.ops[1].mem() {
            if !matches!(base, "sp" | "pc" | "fp") && slot > 0 && slot % 4 == 0 {
                calls.push((index, slot as u32));
            }
        }
    }
    calls
}

/// Globals getters called by the temperature controls. The two delegated
/// getters read neighbouring fields of the schedule object. The ring's range
/// constraint compares the high and low setters against its minimum gap.
fn control_slots(elf: &Elf, arm: &Arm, dial: &discovery::Dial) -> Result<(Globals, u32)> {
    let vtable = discovery::primary_vtable(elf, "15nlClientGlobals")?;
    let method = |slot| elf.read(vtable + slot).ok().filter(|&address| elf.text(address));
    let body = |slot| method(slot).and_then(|address| arm.body(address).ok());
    let field = |slot| body(slot).and_then(|code| scalar_field(&code, false));
    let figures = arm.full_code(dial.figures)?;
    let events = arm.full_code(dial.event)?;
    let figure_calls = virtual_calls(&figures);
    let event_calls = virtual_calls(&events);
    let mut delegates = Vec::new();
    for &(_, slot) in &figure_calls {
        let Some(code) = body(slot) else { continue };
        let Some(last) = code.last().filter(|i| i.name == "b") else { continue };
        let Some(destination) = last.ops[0].imm() else { continue };
        let Some(pointer) = code.iter().find_map(|i| {
            if i.name == "ldr" && i.ops[0].reg() == Some("r0") {
                let ("r0", offset, false) = i.ops[1].mem()? else { return None };
                Some(offset)
            } else {
                None
            }
        }) else {
            continue;
        };
        let Ok(callee) = arm.body(destination) else { continue };
        let Some(offset) = scalar_field(&callee, false) else { continue };
        delegates.push((slot, pointer, destination, offset, callee.len() as u32 * 4));
    }
    let mut pairs = Vec::new();
    for &(first, pointer, address, offset, size) in &delegates {
        for &(second, other_pointer, other_address, other_offset, _) in &delegates {
            if pointer == other_pointer
                && address + size == other_address
                && offset + 4 == other_offset
                && event_calls.iter().any(|&(_, slot)| slot == first)
                && event_calls.iter().any(|&(_, slot)| slot == second)
            {
                pairs.push((first, second));
            }
        }
    }
    let (target, mode) = discovery::unique(pairs, "theme schedule target and mode getters")?;
    let fahrenheit = discovery::unique(
        figure_calls
            .windows(2)
            .filter_map(|call| (call[1].1 == mode && field(call[0].1).is_some()).then_some(call[0].1)),
        "theme temperature units",
    )?;
    let mut highs = Vec::new();
    let mut lows = Vec::new();
    for &(index, slot) in &event_calls {
        if field(slot).is_none() {
            continue;
        }
        let after = &events[index + 1..(index + 6).min(events.len())];
        let Some(compare) = after.iter().position(|i| i.name == "cmp" && i.ops[1].imm() == Some(0x18000)) else {
            continue;
        };
        let Some(subtract) = after[..compare]
            .iter()
            .rev()
            .find(|i| matches!(i.name.as_str(), "sub" | "rsb") && i.ops[0].reg() == after[compare].ops[0].reg())
        else {
            continue;
        };
        let left = subtract.ops[1].reg() == Some("r0");
        let right = subtract.ops[2].reg() == Some("r0");
        if left == right {
            continue;
        }
        if left == (subtract.name == "sub") {
            highs.push(slot);
        } else {
            lows.push(slot);
        }
    }
    let high = discovery::unique(highs, "theme upper range setpoint")?;
    let low = discovery::unique(lows, "theme lower range setpoint")?;
    discovery::require(field(high).zip(field(low)).is_some_and(|(h, l)| h + 4 == l), "theme range fields disagree")?;

    // The figures read the eco flag immediately before the system-off flag;
    // the latter is the word after the range's cached state.
    let off = discovery::unique(
        figure_calls
            .iter()
            .map(|&(_, slot)| slot)
            .filter(|&slot| field(slot).zip(field(low)).is_some_and(|(o, l)| o == l + 8)),
        "theme system-off getter",
    )?;
    let eco = discovery::unique(
        figure_calls.windows(2).filter_map(|calls| {
            let [(before, eco), (after, slot)] = calls else { return None };
            (*slot == off
                && field(*eco).is_some()
                && !figures[before + 1..*after]
                    .iter()
                    .any(|i| i.name.starts_with('b') && i.word & 0x0e000000 == 0x0a000000)
                && figures[*after].address - figures[*before].address <= 40)
                .then_some(*eco)
        }),
        "theme eco getter",
    )?;

    // The leaf is drawn by a helper called by the figures method. It tests
    // the getter and has a conditional setter for precisely the same field.
    let mut leaves = Vec::new();
    for callee in figures
        .iter()
        .filter(|i| i.name == "bl")
        .filter_map(|i| i.ops[0].imm())
        .collect::<std::collections::BTreeSet<_>>()
    {
        let Ok(code) = arm.full_code(callee) else { continue };
        for (index, slot) in virtual_calls(&code) {
            let Some(value) = field(slot) else { continue };
            if !body(slot + 4).and_then(|code| scalar_field(&code, true)).is_some_and(|v| v == value) {
                continue;
            }
            let after = &code[index + 1..(index + 4).min(code.len())];
            if after.iter().any(|i| {
                matches!(i.name.as_str(), "cmp" | "subs")
                    && i.ops.last().and_then(Op::imm) == Some(0)
                    && i.ops.iter().any(|o| o.reg() == Some("r0"))
            }) {
                leaves.push(slot);
            }
        }
    }
    let leaf = discovery::unique(leaves, "theme leaf getter")?;
    let slots = [target, fahrenheit, low, high, mode, off, eco, leaf];
    discovery::require(
        slots.into_iter().collect::<std::collections::BTreeSet<_>>().len() == slots.len(),
        "theme control getters overlap",
    )?;
    for slot in slots {
        discovery::relocated(elf, vtable + slot, "theme globals getter")?;
    }
    Ok((Globals { target, fahrenheit, low, high, mode, off, eco }, leaf))
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
    let (globals, leaf) = control_slots(&elf, &arm, dial)?;
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
        ("leaf_slot", leaf),
        ("target_slot", globals.target),
        ("fahrenheit_slot", globals.fahrenheit),
        ("low_slot", globals.low),
        ("high_slot", globals.high),
        ("mode_slot", globals.mode),
        ("off_slot", globals.off),
        ("eco_slot", globals.eco),
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
    fn invalid_elf_is_not_bound() {
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
    fn reference_builds_bind_every_name() {
        let firmware = std::env::var_os("NLCLIENT_FIRMWARE_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("UNFR0ZEN_ROOT")).join(".work/firmware"));
        let mut checked = 0;
        let paths = match std::fs::read_dir(&firmware) {
            Ok(paths) => paths,
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    && std::env::var_os("NLCLIENT_REQUIRE_FIRMWARE").is_none() =>
            {
                return
            }
            Err(e) => panic!("{}: {e}", firmware.display()),
        };
        for path in paths.map(|p| p.unwrap().path()).filter(|p| p.is_file()) {
            let sample = path.display();
            let stock = std::fs::read(&path).unwrap();
            if super::super::legacy_abi(&stock) {
                assert!(bindings(&stock).is_err(), "{sample}: compact theme ABI");
                continue;
            }
            let text = String::from_utf8(bindings(&stock).unwrap_or_else(|e| panic!("{sample}: {e}"))).unwrap();
            checked += 1;
            let values: std::collections::BTreeMap<_, _> = text
                .lines()
                .map(|line| {
                    let (name, hex) = line.split_once(" 0x").unwrap();
                    (name, u32::from_str_radix(hex, 16).unwrap())
                })
                .collect();
            let elf = Elf::parse(&stock).unwrap();
            for (slot, target) in [
                ("dial_slot", "dial_draw"),
                ("dial_event_slot", "dial_event"),
                ("figures_slot", "figures_draw"),
                ("background_slot", "background_draw"),
            ] {
                assert_eq!(elf.read(values[slot]).unwrap(), values[target], "{sample}: {slot}");
                assert!(elf.text(values[target]));
            }
            let globals = discovery::primary_vtable(&elf, "15nlClientGlobals").unwrap();
            for name in [
                "leaf_slot",
                "target_slot",
                "fahrenheit_slot",
                "low_slot",
                "high_slot",
                "mode_slot",
                "off_slot",
                "eco_slot",
            ] {
                assert!(elf.text(elf.read(globals + values[name]).unwrap()), "{sample}: {name}");
            }
            assert_eq!(text.lines().count(), 24 + NATIVES.len(), "{sample}");
        }
        if std::env::var_os("NLCLIENT_REQUIRE_FIRMWARE").is_some() {
            assert!(checked > 0, "missing modern theme firmware");
        }
    }
}
