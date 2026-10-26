//! Resolve native addresses from this ELF's imports, registrations and code.
//! Reviewed instruction shapes constrain the ABI, never select addresses.
use super::{
    arm::{Arm, Value},
    elf::{bytes, half, string, word, Elf},
};
use crate::shell::sha256_hex;
use crate::{device, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
const IMPORTS: [(&str, &str); 10] = [
    ("native_new", "_Znwj"),
    ("native_string_ctor", "_ZN8nlStringC1Ev"),
    ("native_string_copy", "_ZN8nlString4CopyEPKc"),
    ("native_event_type", "_ZNK7nlEvent7GetTypeEv"),
    ("native_fopen", "fopen"),
    ("native_fread", "fread"),
    ("native_fwrite", "fwrite"),
    ("native_fclose", "fclose"),
    ("memcpy", "memcpy"),
    ("memset", "memset"),
];
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Discovery {
    pub factory: u32,
    pub native_api: BTreeMap<String, u32>,
    pub layout: BTreeMap<String, u32>,
    pub anchors: BTreeMap<String, u32>,
    pub fingerprints: BTreeMap<String, String>,
    pub stock_sha256: String,
    pub validation: String,
}
pub(super) fn require(ok: bool, message: impl Into<String>) -> Result<()> {
    if ok {
        Ok(())
    } else {
        device(format!("Native discovery: {}", message.into()))
    }
}
fn unique<T: Ord>(values: impl IntoIterator<Item = T>, label: &str) -> Result<T> {
    let mut values: BTreeSet<T> = values.into_iter().collect();
    require(
        values.len() == 1,
        format!("{label}: expected one result, found {}", values.len()),
    )?;
    Ok(values.pop_first().unwrap())
}
fn immediate(w: u32) -> u32 {
    (w & 255).rotate_right(((w >> 8) & 15) * 2)
}
fn imported(elf: &Elf) -> Result<BTreeMap<String, u32>> {
    let (rel, sym, strings, plt) = (
        elf.section(".rel.plt")?,
        elf.section(".dynsym")?,
        elf.section(".dynstr")?,
        elf.section(".plt")?,
    );
    require(
        rel.size % 8 == 0 && sym.size % 16 == 0,
        "invalid PLT metadata",
    )?;
    let mut got = BTreeMap::new();
    for off in (rel.offset..rel.offset + rel.size).step_by(8) {
        let target = word(elf.data, off)?;
        let info = word(elf.data, off + 4)?;
        require(info & 255 == 22, "non-JUMP_SLOT PLT relocation")?;
        let so = (info >> 8) as usize * 16;
        let name = word(bytes(elf.data, sym.offset, sym.size)?, so)?;
        got.insert(target, string(elf.contents(strings), name as usize)?);
    }
    let mut result = BTreeMap::new();
    for off in (plt.offset..plt.offset + plt.size.saturating_sub(11)).step_by(4) {
        let (a, b, c) = (
            word(elf.data, off)?,
            word(elf.data, off + 4)?,
            word(elf.data, off + 8)?,
        );
        if (a & 0xfffff000, b & 0xfffff000, c & 0xfffff000) != (0xe28fc000, 0xe28cc000, 0xe5bcf000)
        {
            continue;
        }
        let address = plt.address + (off - plt.offset) as u32;
        let target = address
            .wrapping_add(8)
            .wrapping_add(immediate(a))
            .wrapping_add(immediate(b))
            .wrapping_add(c & 0xfff);
        if let Some(name) = got.get(&target) {
            require(
                result.insert(name.clone(), address).is_none(),
                "duplicate PLT symbol",
            )?;
        }
    }
    require(
        result.len() == got.len(),
        "not every PLT relocation has a decoded stub",
    )?;
    Ok(result)
}
#[derive(Clone)]
pub(super) struct Registry {
    pub address: u32,
    pub names: Vec<String>,
    pub code_first: bool,
}
fn registry(elf: &Elf) -> Result<Registry> {
    let mut best: Option<Registry> = None;
    for s in &elf.sections {
        if s.kind != 1 || s.flags & 7 != 3 {
            continue;
        }
        for code_first in [true, false] {
            let mut va = s.address;
            let end = s.address + s.size as u32;
            while va + 8 <= end {
                let mut names = Vec::new();
                let mut p = va;
                while p + 8 <= end {
                    let (a, b) = (elf.read(p)?, elf.read(p + 4)?);
                    let (code, name) = if code_first { (a, b) } else { (b, a) };
                    match elf.ident(name) {
                        Some(name) if code != 0 && elf.text(code) => names.push(name),
                        _ => break,
                    }
                    p += 8;
                }
                if names.len() >= 10 && best.as_ref().is_none_or(|b| names.len() > b.names.len()) {
                    best = Some(Registry {
                        address: va,
                        names,
                        code_first,
                    });
                }
                va = if p > va { p + 8 } else { va + 4 };
            }
        }
    }
    best.ok_or_else(|| crate::Error::Device("screen registration table not found".into()))
}
pub(super) fn settings(elf: &Elf) -> Result<(u32, Vec<String>)> {
    let screens: BTreeSet<_> = registry(elf)?.names.into_iter().collect();
    let mut candidates = Vec::new();
    for s in &elf.sections {
        if s.kind != 1 || s.flags & 2 == 0 || s.flags & 4 != 0 {
            continue;
        }
        let (mut run, mut start) = (Vec::new(), 0);
        for relative in (0..=s.size).step_by(4) {
            let address = s.address + relative as u32;
            let name = if relative + 4 <= s.size {
                elf.ident(word(elf.data, s.offset + relative)?)
            } else {
                None
            };
            if let Some(name) = name {
                if run.is_empty() {
                    start = address;
                }
                run.push(name);
                continue;
            }
            if run.len() >= 15 {
                let first = run
                    .iter()
                    .position(|n| screens.contains(n))
                    .unwrap_or(run.len());
                let last = run.iter().rposition(|n| screens.contains(n));
                if let Some(last) = last {
                    let names = &run[first..=last];
                    if names.len() >= 15
                        && names.first().is_some_and(|n| n == "temperature")
                        && names.last().is_some_and(|n| n == "temperature")
                        && names.iter().filter(|n| n.as_str() == "account").count() == 1
                        && names.iter().filter(|n| screens.contains(*n)).count() * 4
                            >= names.len() * 3
                    {
                        candidates.push((start + 4 * first as u32, names.to_vec()));
                    }
                }
            }
            run.clear();
        }
    }
    require(
        candidates.len() == 1,
        format!(
            "settings menu expected one result, found {}",
            candidates.len()
        ),
    )?;
    Ok(candidates.pop().unwrap())
}
fn number(stores: &BTreeMap<i32, Value>, offset: i32) -> Result<u32> {
    match stores.get(&offset) {
        Some(Value::Number(v)) => Ok(*v),
        _ => device(format!("unresolved constructor store at {offset:#x}")),
    }
}
fn group_end(elf: &Elf, vptr: u32) -> Result<u32> {
    let mut end = vptr;
    while elf.read(end).ok().is_some_and(|v| elf.text(v)) {
        end += 4;
        require(end - vptr <= 0x400, "unterminated vtable group")?;
    }
    Ok(end)
}
pub fn discover(data: &[u8]) -> Result<Discovery> {
    let elf = Elf::parse(data)?;
    let flags = word(data, 36)?;
    require(
        matches!(half(data, 16)?, 2 | 3) && flags >> 24 == 5 && flags & 0x600 == 0x400,
        "requires ARM hard-float EXEC or PIE",
    )?;
    let imports = imported(&elf)?;
    let mut api = BTreeMap::new();
    for (key, name) in IMPORTS {
        api.insert(
            key.to_owned(),
            *imports
                .get(name)
                .ok_or_else(|| crate::Error::Device(format!("missing import {name}")))?,
        );
    }
    if let Some(&rename) = imports.get("rename") {
        api.insert("native_rename".into(), rename);
    }
    let table = registry(&elf)?;
    require(
        table.names.iter().collect::<BTreeSet<_>>().len() == table.names.len(),
        "duplicate screen registration names",
    )?;
    let mut screens = BTreeMap::new();
    for (i, name) in table.names.iter().enumerate() {
        screens.insert(
            name.as_str(),
            elf.read(table.address + i as u32 * 8 + if table.code_first { 0 } else { 4 })?,
        );
    }
    let account = *screens
        .get("account")
        .ok_or_else(|| crate::Error::Device("missing account registration".into()))?;
    let screen = *screens
        .get("legalinfo")
        .ok_or_else(|| crate::Error::Device("missing legalinfo registration".into()))?;
    let mut layers = Vec::new();
    for s in &elf.sections {
        if s.kind == 1 && s.flags & 7 == 3 {
            for r in (0..s.size.saturating_sub(7)).step_by(4) {
                let va = s.address + r as u32;
                if elf.ident(elf.read(va + 4)?).as_deref() == Some("legalinfolayer")
                    && elf.text(elf.read(va)?)
                {
                    layers.push(elf.read(va)?);
                }
            }
        }
    }
    let layer = unique(layers, "Legal Info layer factory")?;
    let arm = Arm::new(&elf)?;
    let lc = arm.calls(layer)?;
    let sc = arm.calls(screen)?;
    require(
        lc.len() >= 2 && sc.len() >= 3,
        "factory call graph incomplete",
    )?;
    require(
        lc[0] == sc[0] && sc[0] == api["native_new"],
        "factory allocator mismatch",
    )?;
    api.insert("native_layer_ctor".into(), lc[1]);
    api.insert("native_screen_ctor".into(), sc[1]);
    let (ls, _) = arm.stores(lc[1], None)?;
    let (ss, ssizes) = arm.stores(screen, Some(api["native_new"]))?;
    let (_, lsizes) = arm.stores(layer, Some(api["native_new"]))?;
    require(
        ssizes == [Some(Value::Number(0xf0))] && lsizes == [Some(Value::Number(0x140))],
        "unrecognised allocation sizes",
    )?;
    let (lv, sv) = (number(&ls, 0)?, number(&ss, 0)?);
    for vptr in [lv, sv] {
        require(
            elf.at(vptr)
                .is_some_and(|s| s.flags & 6 == 2 && vptr - s.address >= 12)
                && vptr.checked_add(0x400).is_some(),
            "invalid constructor vtable address",
        )?;
    }
    let common = elf.read(lv - 12)?;
    require(common == 0xa4, "unrecognised common base adjustment")?;
    let parts = [
        number(&ls, common as i32)?,
        number(&ls, common as i32 + 4)?,
        number(&ls, common as i32 + 8)?,
    ];
    require(
        number(&ss, 0xec)? == 0,
        "unrecognised screen constructor stores",
    )?;
    let second = number(&ss, 4)?;
    let third = number(&ss, 8)?;
    require(
        parts
            .iter()
            .all(|p| p.wrapping_sub(lv) < 0x400 && *p >= lv + 8)
            && [second, third]
                .iter()
                .all(|p| p.wrapping_sub(sv) < 0x400 && *p >= sv + 8),
        "invalid secondary vtable group",
    )?;
    let end = group_end(&elf, sv)?;
    require(end > sv, "empty primary screen vtable")?;
    let title = end - sv - 4;
    require(matches!(title, 0x84 | 0x88), "unknown title vtable slot")?;
    require(
        elf.read(second - 8)? == 0xffff_fffc
            && elf.read(third - 8)? == 0xffff_fff8
            && elf.read(parts[0] - 8)? == common.wrapping_neg(),
        "thunk adjustment mismatch",
    )?;
    let mut layout = BTreeMap::from([
        ("PRIMARY_SIZE".into(), 0x140),
        ("COMMON_OFFSET".into(), common),
        (
            "LAYER_VTABLE_SIZE".into(),
            group_end(&elf, parts[2])? - lv + 12,
        ),
        (
            "SCREEN_VTABLE_SIZE".into(),
            group_end(&elf, third)? - sv + 8,
        ),
        ("LAYER_COMMON_VPTR".into(), parts[0] - lv + 12),
        ("LAYER_SECOND_VPTR".into(), parts[1] - lv + 12),
        ("LAYER_THIRD_VPTR".into(), parts[2] - lv + 12),
        ("SCREEN_SECOND_VPTR".into(), second - sv + 8),
        ("SCREEN_THIRD_VPTR".into(), third - sv + 8),
        ("SCREEN_TITLE_SLOT".into(), title),
    ]);
    api.insert("native_screen_vtable".into(), sv);
    let init = sc[2];
    let ic = arm.calls(init)?;
    require(ic.len() >= 5, "screen composition call graph incomplete")?;
    require(
        [2, 3].contains(&ic.iter().filter(|&&v| v == ic[0]).count())
            && [2, 3].contains(&ic.iter().filter(|&&v| v == ic[1]).count()),
        "screen composition call shape changed",
    )?;
    for (name, value) in [
        ("native_layer_lookup", ic[0]),
        ("native_attach", ic[1]),
        ("native_screen_init", *ic.last().unwrap()),
        ("native_layer_init", elf.read(parts[0] + 0x60)?),
        ("native_select", elf.read(lv + 0x24)?),
        ("native_layer_wheel", elf.read(lv + 0x18)?),
        ("native_screen_event", elf.read(sv)?),
        ("native_screen_enter", elf.read(sv + 0x1c)?),
        ("native_rebuild_title", elf.read(sv + title - 4)?),
    ] {
        api.insert(name.into(), value);
    }
    let enter = elf.read(lv + 0x2c)?;
    let draw = elf.read(lv + 0x28)?;
    let ec = arm.calls(enter)?;
    let dc = arm.calls(draw)?;
    require(
        [4, 5].contains(&ec.len()) && [10, 11].contains(&dc.len()),
        "Legal Info lifecycle call shape changed",
    )?;
    api.insert("native_config_invalidate".into(), ec[3]);
    let wrapper = unique(arm.tails(enter)?, "layout wrapper")?;
    api.insert(
        "native_config_layout".into(),
        unique(arm.tails(wrapper)?, "layout implementation")?,
    );
    let image_calls = arm.calls(ec[2])?;
    require(
        [3, 4].contains(&image_calls.len()),
        "image helper call shape changed",
    )?;
    require(
        dc[1] == dc[5] && dc[1] == dc[7],
        "renderer style calls disagree",
    )?;
    for (name, value) in [
        ("native_image_load", image_calls[1]),
        ("native_style", dc[1]),
        ("native_config_y", dc[4]),
        ("native_config_draw", dc[6]),
        ("native_image_width", dc[8]),
        ("native_scroll_draw", dc[9]),
    ] {
        api.insert(name.into(), value);
    }
    let code = arm.code(dc[8], Some(64))?;
    let mut heights = Vec::new();
    for (i, ins) in code.iter().enumerate() {
        if ins.name == "ldr" && ins.text == "r0, [r0, #8]" {
            heights.push(if i > 0 && code[i - 1].text == "ip, sp" {
                code[i - 1].address
            } else {
                ins.address
            });
        }
    }
    api.insert(
        "native_image_height".into(),
        unique(heights, "image height getter")?,
    );
    let event = elf.read(lv + 0x14)?;
    let legacy = arm.code(event, None)?[0].name != "b";
    let handler = if legacy {
        event
    } else {
        unique(arm.tails(event)?, "layer event handler")?
    };
    api.insert("native_layer_event".into(), handler);
    let calls = arm.calls(if legacy { event } else { handler })?;
    require(!calls.is_empty(), "missing generic event call")?;
    let generic = calls[0];
    let timers: BTreeSet<_> = arm
        .code(generic, None)?
        .iter()
        .filter(|i| i.name == "cmp" && i.ops.len() == 2)
        .filter_map(|i| i.ops[1].imm())
        .filter(|t| (0x40000020..=0x40000040).contains(t))
        .collect();
    require(timers.len() == 2, "selection timer event IDs unresolved")?;
    layout.extend([
        ("LEGACY_CLICK".into(), legacy as u32),
        ("TIMER_EVENT_FIRST".into(), *timers.first().unwrap()),
        ("TIMER_EVENT_SECOND".into(), *timers.last().unwrap()),
        (
            "RENAME_SYSCALL".into(),
            (!imports.contains_key("rename")) as u32,
        ),
    ]);
    let mut anchors = BTreeMap::from([
        ("account_factory".into(), account),
        ("layer_factory".into(), layer),
        ("screen_factory".into(), screen),
        ("screen_composition".into(), init),
        ("layer_enter".into(), enter),
        ("layer_draw".into(), draw),
        ("image_helper".into(), ec[2]),
        ("layer_event".into(), event),
        ("layout_wrapper".into(), wrapper),
        ("screen_title".into(), elf.read(sv + title)?),
        ("generic_event".into(), generic),
    ]);
    let mut implementations = api.clone();
    implementations.extend(anchors.clone());
    let mut fingerprints = BTreeMap::new();
    for (name, address) in implementations {
        if !IMPORTS.iter().any(|(key, _)| *key == name)
            && name != "native_rename"
            && name != "native_screen_vtable"
        {
            fingerprints.insert(name, arm.fingerprint(address)?);
        }
    }
    let known: serde_json::Value = serde_json::from_str(include_str!("abi_shapes.json"))
        .map_err(|e| crate::Error::Device(e.to_string()))?;
    let layout_value = serde_json::to_value(&layout).unwrap();
    let fingerprint_value = serde_json::to_value(&fingerprints).unwrap();
    let variants = known["variants"]
        .as_array()
        .ok_or_else(|| crate::Error::Device("invalid embedded ABI shapes".into()))?;
    require(
        variants
            .iter()
            .any(|v| v["layout"] == layout_value && v["functions"] == fingerprint_value),
        "unrecognised instruction shapes or C++ layout",
    )?;
    // Screen port additions: real activity notification and periodic refresh.
    activity(&elf, &arm, &imports, &known, &mut api, &mut layout)?;
    for(name,symbol)in [("native_send_timeout","_ZN12nlEventUtils11SendTimeoutERP21nlEventTimeoutRequestjjP12nlEventQueueP15nlIEventHandlerP7nlTimer"),("native_cancel_timeout","_ZN21nlEventTimeoutRequest6CancelEv"),("native_release","_ZNK10nlRefCount7ReleaseEv"),("native_timeout_id","_ZNK15nlEventTimedOut5GetIDEv")]{api.insert(name.into(),*imports.get(symbol).ok_or_else(||crate::Error::Device(format!("missing status-refresh import {symbol}")))?);}
    let screen_draw = elf.read(sv + 0xc)?;
    let shape = arm.fingerprint(screen_draw)?;
    require(
        known["menu_draw_variants"]
            .as_array()
            .is_some_and(|v| v.contains(&serde_json::Value::String(shape))),
        "unknown settings tile drawing ABI",
    )?;
    api.insert("native_screen_draw".into(), screen_draw);
    let menu_hidden = unique(arm.calls(screen_draw)?, "menu visibility accessor")?;
    let mut menu_scales = Vec::new();
    for target in arm.tails(screen_draw)? {
        let calls = arm.calls(target)?;
        if calls.len() == 4 && calls[3] == api["native_style"] {
            menu_scales.push(calls[2]);
            let shape = arm.fingerprint(target)?;
            require(
                known["menu_title_draw_variants"]
                    .as_array()
                    .is_some_and(|v| v.contains(&serde_json::Value::String(shape))),
                "unknown menu title drawing ABI",
            )?;
        }
    }
    let menu_scale = unique(menu_scales, "menu animation scale accessor")?;
    for (name, target, shapes) in [
        ("native_menu_hidden", menu_hidden, "menu_hidden_variants"),
        ("native_menu_scale", menu_scale, "menu_scale_variants"),
    ] {
        let shape = arm.fingerprint(target)?;
        require(
            known[shapes]
                .as_array()
                .is_some_and(|v| v.contains(&serde_json::Value::String(shape))),
            "unknown menu animation accessor ABI",
        )?;
        api.insert(name.into(), target);
    }
    let select = arm.calls(api["native_select"])?;
    require(select.len() == 3, "unknown native scroll selection calls")?;
    let target = arm.code(select[1], None)?;
    require(
        target
            .iter()
            .any(|i| i.name == "vstr" && i.text == "s0, [r0, #0x10]")
            && target
                .iter()
                .any(|i| i.name == "vldr" && i.text.contains("[r0, #0x24]"))
            && target.iter().any(|i| i.name == "vcvt.s32.f32"),
        "unknown native scroll target ABI",
    )?;
    api.insert("native_scroll_target".into(), select[1]);
    layout.insert("GLOBALS_TIMER_OFFSET".into(), timer(&elf, &arm, &imports)?);
    downlink(&elf, &arm, &imports, &known, &mut api, &mut layout, &mut anchors)?;
    require(
        layout["LAYER_VTABLE_SIZE"].max(layout["SCREEN_VTABLE_SIZE"]) <= 0x400,
        "vtable larger than the payload holds",
    )?;
    Ok(Discovery {
        factory: account,
        native_api: api,
        layout,
        anchors,
        fingerprints,
        stock_sha256: sha256_hex(data),
        validation: "audited instruction shapes; guest acceptance required".into(),
    })
}
fn downlink(
    elf: &Elf,
    arm: &Arm,
    imports: &BTreeMap<String, u32>,
    known: &serde_json::Value,
    api: &mut BTreeMap<String, u32>,
    layout: &mut BTreeMap<String, u32>,
    anchors: &mut BTreeMap<String, u32>,
) -> Result<()> {
    let refs = |value: u32| -> Result<Vec<u32>> {
        let mut result = Vec::new();
        for s in elf
            .sections
            .iter()
            .filter(|s| s.address != 0 && s.kind != 8 && s.flags & 4 == 0)
        {
            for off in (s.offset..s.offset + s.size.saturating_sub(3)).step_by(4) {
                if word(elf.data, off)? == value {
                    result.push(s.address + (off - s.offset) as u32);
                }
            }
        }
        Ok(result)
    };
    let mut shapes = serde_json::Map::new();
    for (bucket, name) in [
        ("device", "16nlCZDeviceBucket"),
        ("shared", "16nlCZSharedBucket"),
        ("schedule", "29nlCZTemperatureScheduleBucket"),
        ("view_handler", "12nlClientView"),
    ] {
        let needle = format!("{name}\0");
        let off = unique(
            elf.data
                .windows(needle.len())
                .enumerate()
                .filter(|(_, b)| *b == needle.as_bytes())
                .map(|(off, _)| off),
            "bucket RTTI name",
        )?;
        let address = unique(
            elf.sections
                .iter()
                .filter(|s| s.address != 0 && s.offset <= off && off < s.offset + s.size)
                .map(|s| s.address + (off - s.offset) as u32),
            "bucket RTTI address",
        )?;
        let mut vtables = Vec::new();
        for r in refs(address)? {
            for p in refs(r - 4)? {
                if elf.read(p - 4)? == 0 {
                    vtables.push(p + 4);
                }
            }
        }
        // Bucket finish-parsing and view event-dispatch virtual slots.
        let slot = unique(vtables, "bucket primary vtable")?
            + if bucket == "view_handler" { 0x10 } else { 0x48 };
        let function = elf.read(slot)?;
        shapes.insert(bucket.into(), arm.fingerprint(function)?.into());
        if bucket == "view_handler" {
            let code = arm.code(function, None)?;
            let dispatch = if code.first().is_some_and(|i| i.name == "push") {
                code.iter()
                    .find(|i| i.name == "bl")
                    .and_then(|i| i.ops[0].imm())
                    .ok_or_else(|| crate::Error::Device("missing view dispatcher".into()))?
                    as u32
            } else {
                function
            };
            shapes.insert("dispatch".into(), arm.fingerprint(dispatch)?.into());
            continue;
        }
        if half(elf.data, 16)? == 3 {
            let relocations = elf
                .sections
                .iter()
                .filter(|s| s.kind == 9)
                .flat_map(|s| (s.offset..s.offset + s.size).step_by(8));
            let mut relative = false;
            for off in relocations {
                if word(elf.data, off)? == slot {
                    require(
                        word(elf.data, off + 4)? == 0x17,
                        "bucket vtable requires R_ARM_RELATIVE",
                    )?;
                    relative = true;
                }
            }
            require(relative, "bucket vtable has no load-bias relocation")?;
        }
        if bucket == "device" {
            let offsets = arm
                .code(function, None)?
                .into_iter()
                .filter(|i| i.name == "ldr" && i.text.starts_with("r0, [r0, #"))
                .filter_map(|i| i.ops.get(1).and_then(|o| o.mem()).map(|(_, d, _)| d as u32));
            let offset = unique(offsets, "bucket client globals offset")?;
            shapes.insert("globals_offset".into(), offset.into());
            layout.insert("BUCKET_GLOBALS_OFFSET".into(), offset);
        }
        api.insert(format!("native_{bucket}_commit"), function);
        anchors.insert(format!("{bucket}_commit_slot"), slot);
    }
    require(
        known["downlink_variants"]
            .as_array()
            .is_some_and(|v| v.contains(&shapes.into())),
        "unrecognised downlink completion ABI",
    )?;
    api.insert(
        "native_delete".into(),
        *imports
            .get("_ZdlPv")
            .ok_or_else(|| crate::Error::Device("missing operator delete import".into()))?,
    );
    api.insert(
        "native_time".into(),
        *imports
            .get("time")
            .ok_or_else(|| crate::Error::Device("missing time import".into()))?,
    );
    Ok(())
}

fn activity(
    elf: &Elf,
    arm: &Arm,
    imports: &BTreeMap<String, u32>,
    known: &serde_json::Value,
    api: &mut BTreeMap<String, u32>,
    layout: &mut BTreeMap<String, u32>,
) -> Result<()> {
    for (name, symbol) in [
        ("native_activity_ctor", "_ZN7nlEventC1Ev"),
        ("native_activity_init", "_ZN7nlEvent4InitEji"),
        (
            "native_enqueue",
            "_ZN12nlEventQueue7EnqueueEPK7nlEventP15nlIEventHandler",
        ),
    ] {
        api.insert(
            name.into(),
            *imports
                .get(symbol)
                .ok_or_else(|| crate::Error::Device(format!("missing activity import {symbol}")))?,
        );
    }
    let mut candidates = Vec::new();
    for s in elf
        .sections
        .iter()
        .filter(|s| s.kind == 1 && s.flags & 4 != 0)
    {
        for off in (s.offset + 64..(s.offset + s.size).saturating_sub(32)).step_by(4) {
            let w = word(elf.data, off)?;
            if w & 0xfffff000 != 0xe3a01000 || immediate(w) != 0x40000005 {
                continue;
            }
            let address = s.address + (off - s.offset) as u32;
            let c = arm.decode(address - 64, bytes(elf.data, off - 64, 96)?)?;
            if !c
                .iter()
                .any(|i| i.name == "bl" && i.ops[0].imm() == Some(api["native_activity_init"]))
            {
                continue;
            }
            let indices: Vec<_> = c
                .iter()
                .enumerate()
                .filter(|(_, i)| i.name == "mov" && i.text == "r0, #0x10")
                .map(|(j, _)| j)
                .collect();
            if indices.len() != 1 {
                continue;
            }
            let j = indices[0];
            if j < 3 || c[j - 1].name != "bl" {
                continue;
            }
            if !c[j - 3..j - 1]
                .iter()
                .any(|i| i.name == "ldr" && i.text == "r0, [r4, #0x70]")
            {
                continue;
            }
            let dest: Vec<_> = c
                .iter()
                .filter(|i| {
                    i.address > address && i.name == "ldr" && i.text.starts_with("r0, [r4, #")
                })
                .filter_map(|i| i.ops[1].mem().map(|(_, d, _)| d as u32))
                .collect();
            if dest.len() == 1 {
                candidates.push((c[j - 1].ops[0].imm().unwrap(), dest[0]));
            }
        }
    }
    let (mark, queue) = unique(candidates, "client user-activity notification")?;
    api.insert("native_mark_activity".into(), mark);
    layout.insert("ACTIVITY_QUEUE_OFFSET".into(), queue);
    fn refs(elf: &Elf, value: u32) -> Result<Vec<u32>> {
        let mut found = Vec::new();
        for s in elf
            .sections
            .iter()
            .filter(|s| s.address != 0 && s.kind != 8 && s.flags & 4 == 0)
        {
            for off in (s.offset..s.offset + s.size.saturating_sub(3)).step_by(4) {
                if word(elf.data, off)? == value {
                    found.push(s.address + (off - s.offset) as u32);
                }
            }
        }
        Ok(found)
    }
    let needle = b"15nlClientGlobals\0";
    let off = elf
        .data
        .windows(needle.len())
        .position(|v| v == needle)
        .ok_or_else(|| crate::Error::Device("missing client globals RTTI".into()))?;
    let name = unique(
        elf.sections
            .iter()
            .filter(|s| s.address != 0 && s.offset <= off && off < s.offset + s.size)
            .map(|s| s.address + (off - s.offset) as u32),
        "globals RTTI name",
    )?;
    let mut vtables = Vec::new();
    for r in refs(elf, name)? {
        for p in refs(elf, r - 4)? {
            if elf.read(p - 4)? == 0 {
                vtables.push(p + 4);
            }
        }
    }
    let getter = elf.read(unique(vtables, "globals primary vtable")? + 0x18)?;
    let code = arm.code(getter, Some(20))?;
    require(
        code.len() >= 2
            && code[..2]
                .iter()
                .filter(|i| i.name == "ldr" && i.text.starts_with("r0, [r0, #"))
                .count()
                == 1
            && (code[1].name == "bx" || code[0].text == "ip, sp"),
        "unknown client accessor ABI",
    )?;
    api.insert("native_activity_client".into(), getter);
    let shapes =
        serde_json::json!({"mark":arm.fingerprint(mark)?,"client":arm.fingerprint(getter)?});
    require(
        known["activity_variants"]
            .as_array()
            .is_some_and(|v| v.contains(&shapes)),
        "unrecognised user-activity ABI",
    )
}
fn timer(elf: &Elf, arm: &Arm, imports: &BTreeMap<String, u32>) -> Result<u32> {
    let ctor = *imports
        .get("_ZN7nlTimerC1Ev")
        .ok_or_else(|| crate::Error::Device("missing timer constructor".into()))?;
    let init = *imports
        .get("_ZN7nlTimer4InitEP12nlEventQueue")
        .ok_or_else(|| crate::Error::Device("missing timer init".into()))?;
    let mut offsets = Vec::new();
    for s in elf
        .sections
        .iter()
        .filter(|s| s.kind == 1 && s.flags & 4 != 0)
    {
        for off in (s.offset..s.offset + s.size.saturating_sub(20)).step_by(4) {
            let w = word(elf.data, off)?;
            if w >> 24 != 0xeb {
                continue;
            }
            let site = s.address + (off - s.offset) as u32;
            let displacement = ((w << 8) as i32) >> 6;
            if site.wrapping_add(8).wrapping_add(displacement as u32) != ctor {
                continue;
            }
            let code = arm.code(site + 4, Some(32))?;
            let Some(index) = code
                .iter()
                .position(|i| i.name == "bl" && i.ops[0].imm() == Some(init))
            else {
                continue;
            };
            let code = &code[..index];
            if code
                .iter()
                .any(|i| !matches!(i.name.as_str(), "ldr" | "str" | "mov" | "cmp" | "beq"))
            {
                continue;
            }
            let saves: Vec<_> = code
                .iter()
                .filter(|i| i.name == "str" && i.ops[1].mem().is_some())
                .collect();
            let loads: Vec<_> = code
                .iter()
                .filter(|i| i.name == "ldr" && i.ops[0].reg() == Some("r1"))
                .collect();
            if saves.len() != 1 || loads.len() != 1 {
                continue;
            }
            let (save, load) = (saves[0], loads[0]);
            let reg = save.ops[0].reg().unwrap();
            if !code
                .iter()
                .any(|i| i.name == "mov" && i.text == format!("r0, {reg}"))
            {
                continue;
            }
            let Some((sb, sd, _)) = save.ops[1].mem() else {
                continue;
            };
            let Some((lb, ld, _)) = load.ops[1].mem() else {
                continue;
            };
            if sb != lb {
                continue;
            }
            let prior = arm.code(site - 4, Some(4))?;
            if prior[0].name != "mov" || prior[0].text != format!("{reg}, r0") {
                continue;
            }
            offsets.push((sd as u32, ld as u32));
        }
    }
    Ok(unique(offsets, "nlGlobals timer field")?.0)
}
