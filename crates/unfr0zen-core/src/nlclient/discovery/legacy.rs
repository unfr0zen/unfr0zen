//! Discover the native ABI with 32-byte strings and compact UI vtables.
use super::*;

/// The image constructor logs its failed path. The C-string overload wraps
/// that constructor between nlString construction and destruction.
fn image_load(elf: &Elf, arm: &Arm, words: &Words, imports: &BTreeMap<String, u32>) -> Result<u32> {
    let message = string_address(elf, b"nlImage: failed to load image %s\0")
        .ok_or_else(|| crate::Error::Device("missing image loader diagnostic".into()))?;
    let constructor = unique(
        references(elf, words, message).into_iter().filter_map(|i| arm.enclosing(words.at(i).0)),
        "nlImage constructor",
    )?;
    let mut wrappers = Vec::new();
    for (site, w) in words.iter() {
        if branch_target(site, w, true) != Some(constructor) {
            continue;
        }
        let Some(start) = arm.enclosing(site) else { continue };
        let calls = arm.calls(start)?;
        if calls.windows(3).any(|calls| {
            imports.get("_ZN8nlStringC1EPKc") == Some(&calls[0])
                && calls[1] == constructor
                && imports.get("_ZN8nlStringD1Ev") == Some(&calls[2])
        }) {
            wrappers.push(start);
        }
    }
    unique(wrappers, "nlImage C-string loader")
}

/// Each label draws two scaled images. FreeType scales their dimensions,
/// then the same width getter and vertical layout function position both.
fn image_layout(arm: &Arm, words: &Words, imports: &BTreeMap<String, u32>, style: u32) -> Result<(u32, u32)> {
    let callers: BTreeSet<_> = words
        .iter()
        .filter(|&(a, w)| branch_target(a, w, true) == Some(style))
        .filter_map(|(a, _)| arm.enclosing(a))
        .collect();
    let mut found = Vec::new();
    for caller in callers {
        let calls = arm.calls(caller)?;
        if let [first, div, mul, width, y, w2, y2] = calls[..] {
            if first == style
                && imports.get("FT_DivFix") == Some(&div)
                && imports.get("FT_MulFix") == Some(&mul)
                && width == w2
                && y == y2
                && arm.body(width)?.iter().any(|i| i.word == 0xe5900004)
            {
                found.push((width, y));
            }
        }
    }
    unique(found, "compact image width and text layout")
}

pub(super) fn survey(data: &[u8]) -> Result<Discovery> {
    let flags = word(data, 36)?;
    require(
        matches!(half(data, 16)?, 2 | 3) && flags >> 24 == 5 && flags & 0x600 == 0x400,
        "compact UI requires ARM hard-float EXEC or PIE",
    )?;
    let elf = Elf::parse(data)?;
    let arm = Arm::new(&elf)?;
    let imports = imported(&elf)?;
    let mut api = BTreeMap::new();
    for (key, symbol) in
        IMPORTS.iter().chain(&HANDOVER_IMPORTS).chain(&EGL_IMPORTS).chain(&FRAMEBUFFER_IMPORTS).chain(&[
            ("native_rename", "rename"),
            ("native_event_value", "_ZNK7nlEvent8GetValueEv"),
            ("native_timeout_ctor", "_ZN21nlEventTimeoutRequestC1Ev"),
            ("native_timeout_init", "_ZN21nlEventTimeoutRequest4InitEijP12nlEventQueueP15nlIEventHandler"),
            ("native_cancel_timeout", "_ZN21nlEventTimeoutRequest6CancelEv"),
            ("native_release", "_ZNK10nlRefCount7ReleaseEv"),
            ("native_timeout_id", "_ZNK15nlEventTimedOut5GetIDEv"),
        ])
    {
        if let Some(&address) = imports.get(*symbol) {
            api.insert((*key).into(), address);
        }
    }
    let factories = screen_factories(&elf)?;
    let layer_factory = legal_layer_factory(&elf)?;
    let registered =
        |name| factories.get(name).copied().ok_or_else(|| crate::Error::Device(format!("missing {name} registration")));
    let screen_factory = registered("legalinfo")?;
    let account = registered("account")?;
    for (name, symbol) in IMPORTS.iter().chain(&[("native_egl_swap", "eglSwapBuffers")]) {
        require(api.contains_key(*name), format!("missing compact UI import {symbol}"))?;
    }
    let lc = arm.calls(layer_factory)?;
    let sc = arm.calls(screen_factory)?;
    require(
        lc.len() >= 2 && sc.len() >= 3 && lc[0] == api["native_new"] && lc[0] == sc[0],
        "compact UI factory allocator mismatch",
    )?;
    let layer = primary_vtable(&elf, "16nlLegalInfoLayer")?;
    let screen = primary_vtable(&elf, "17nlLegalInfoScreen")?;
    let common = elf.read(layer - 12)?;
    let (ls, _) = arm.stores(lc[1], None)?;
    let (ss, ssizes) = arm.stores(screen_factory, Some(lc[0]))?;
    let (_, lsizes) = arm.stores(layer_factory, Some(lc[0]))?;
    require(
        common == 0xc4
            && number(&ls, 0)? == layer
            && number(&ss, 0)? == screen
            && lsizes == [Some(Value::Number(0x150))]
            && ssizes == [Some(Value::Number(0xe8))],
        "compact UI constructor layout mismatch",
    )?;
    let parts = [number(&ls, common as i32)?, number(&ls, common as i32 + 4)?, number(&ls, common as i32 + 8)?];
    let second = number(&ss, 4)?;
    let third = number(&ss, 8)?;
    for (primary, group) in [(layer, parts.as_slice()), (screen, [second, third].as_slice())] {
        require(
            group.windows(2).all(|p| p[0] < p[1])
                && group.iter().all(|p| (8..0x400).contains(&p.wrapping_sub(primary))),
            "invalid compact UI vtable group",
        )?;
        require(group_end(&elf, *group.last().unwrap())? - primary + 12 <= 0x400, "oversize compact UI vtable group")?;
    }
    require(
        elf.read(parts[0] - 8)? == common.wrapping_neg()
            && elf.read(parts[1] - 8)? == (common + 4).wrapping_neg()
            && elf.read(parts[2] - 8)? == (common + 8).wrapping_neg()
            && elf.read(second - 8)? == 4u32.wrapping_neg()
            && elf.read(third - 8)? == 8u32.wrapping_neg()
            && number(&ss, 0xe4)? == 0,
        "compact UI base adjustment mismatch",
    )?;
    require(group_end(&elf, screen)? == screen + 0x6c, "compact screen title slot mismatch")?;
    let title = 0x68;
    // The payload embeds four 32-byte strings at these same offsets.
    let ctor = arm.body(sc[1])?;
    for field in [0x54, 0x74, 0x94, 0xb4] {
        require(
            ctor.iter().any(|i| i.name == "add" && i.ops.get(2).and_then(Op::imm) == Some(field)),
            "compact screen string offsets mismatch",
        )?;
    }
    require(
        arm.calls(sc[1])?.iter().filter(|&&t| t == api["native_string_ctor"]).count() == 4,
        "compact screen string constructor mismatch",
    )?;
    let layer_init = elf.read(parts[0] + 0x40)?;
    require(
        arm.body(layer_init)?.iter().any(|i| {
            i.name == "str"
                && i.ops
                    .last()
                    .and_then(Op::mem)
                    .is_some_and(|(base, offset, indexed)| base != "sp" && offset == 0x7c && !indexed)
        }),
        "compact common layer globals field mismatch",
    )?;
    let enter = elf.read(layer + 0x1c)?;
    let draw = elf.read(layer + 0x18)?;
    let event = elf.read(layer + 0xc)?;
    let wrapper = elf.read(layer + 0x10)?;
    let internal = |addr| -> Result<Vec<u32>> {
        Ok(arm.calls(addr)?.into_iter().filter(|a| !imports.values().any(|v| v == a)).collect())
    };
    let composition = sc[2];
    let ic = internal(composition)?;
    let [lookup, attach, lookup2, attach2, lookup3, attach3, lookup4, attach4, init] = ic[..] else {
        return device("unknown compact screen composition");
    };
    require(
        [lookup2, lookup3, lookup4] == [lookup; 3] && [attach2, attach3, attach4] == [attach; 3],
        "compact screen composition calls disagree",
    )?;
    let dc = internal(draw)?;
    let [hidden, style, style2, text_draw, scroll] = dc[..] else { return device("unknown compact text draw") };
    require(style == style2, "compact screen draw ABI mismatch")?;
    let enter_calls = internal(enter)?;
    let [invalidate, enter_layout] = enter_calls[..] else { return device("unknown compact text layout") };
    let layout_impl = unique(arm.tails(wrapper)?, "compact layout implementation")?;
    require(arm.tails(enter_layout)? == [layout_impl], "compact layer layouts disagree")?;
    let event_calls = arm.calls(event)?;
    let generic = *event_calls.first().ok_or_else(|| crate::Error::Device("missing compact event handler".into()))?;
    let words = Words::new(&elf);
    let (width, y) = image_layout(&arm, &words, &imports, style)?;
    let base_draw = elf.read(primary_vtable(&elf, "20nlSettingsBaseScreen")? + 0xc)?;
    let title_calls = arm.calls(base_draw)?;
    let scale = unique(
        title_calls
            .windows(2)
            .filter(|w| w[1] == style && title_calls.iter().filter(|&&t| t == w[0]).count() == 2)
            .map(|w| w[0]),
        "compact menu scale",
    )?;
    // The layout's scroll calculation calls the target setter. It stores
    // the float target, divides by the step and converts to a row number.
    let mut targets = Vec::new();
    for helper in internal(layout_impl)? {
        for target in internal(helper)? {
            let code = arm.body(target)?;
            if code.iter().any(|i| i.name == "vstr" && i.text == "s0, [r0, #0x10]")
                && code.iter().any(|i| i.name == "vldr" && i.text.contains("[r0, #0x24]"))
                && code.iter().any(|i| i.name == "vcvt.s32.f32")
            {
                targets.push(target);
            }
        }
    }
    for (key, address) in [
        ("native_layer_ctor", lc[1]),
        ("native_screen_ctor", sc[1]),
        ("native_screen_vtable", screen),
        ("native_layer_lookup", lookup),
        ("native_attach", attach),
        ("native_screen_init", init),
        ("native_layer_init", layer_init),
        ("native_config_invalidate", invalidate),
        ("native_config_draw", text_draw),
        ("native_style", style),
        ("native_menu_hidden", hidden),
        ("native_scroll_draw", scroll),
        ("native_select", elf.read(layer + 0x14)?),
        ("native_layer_wheel", unique(arm.tails(event)?, "compact layer wheel")?),
        ("native_screen_event", elf.read(screen)?),
        ("native_screen_enter", elf.read(screen + 0x1c)?),
        ("native_rebuild_title", elf.read(screen + title - 4)?),
        ("native_layer_event", event),
        ("native_config_layout", layout_impl),
        ("native_screen_draw", elf.read(screen + 0xc)?),
        ("native_config_y", y),
        ("native_image_load", image_load(&elf, &arm, &words, &imports)?),
        ("native_image_width", width),
        ("native_image_height", image_height(&arm, width)?),
        ("native_scroll_target", unique(targets, "compact scroll target")?),
        ("native_menu_scale", scale),
    ] {
        api.insert(key.into(), address);
    }
    let mut layout = BTreeMap::from([
        ("PRIMARY_SIZE".into(), 0x150),
        ("COMMON_OFFSET".into(), common),
        ("LAYER_VTABLE_SIZE".into(), group_end(&elf, parts[2])? - layer + 12),
        ("SCREEN_VTABLE_SIZE".into(), group_end(&elf, third)? - screen + 8),
        ("LAYER_COMMON_VPTR".into(), parts[0] - layer + 12),
        ("LAYER_SECOND_VPTR".into(), parts[1] - layer + 12),
        ("LAYER_THIRD_VPTR".into(), parts[2] - layer + 12),
        ("SCREEN_SECOND_VPTR".into(), second - screen + 8),
        ("SCREEN_THIRD_VPTR".into(), third - screen + 8),
        ("SCREEN_TITLE_SLOT".into(), title),
        ("LEGACY_CLICK".into(), 1),
        ("RENAME_SYSCALL".into(), u32::from(!imports.contains_key("rename"))),
    ]);
    let mut anchors = BTreeMap::from([
        ("account_factory".into(), account),
        ("layer_factory".into(), layer_factory),
        ("screen_factory".into(), screen_factory),
        ("screen_composition".into(), composition),
        ("layer_enter".into(), enter),
        ("layer_draw".into(), draw),
        ("layer_event".into(), event),
        ("layout_wrapper".into(), wrapper),
        ("screen_title".into(), elf.read(screen + title)?),
        ("generic_event".into(), generic),
    ]);
    for (key, symbol) in [
        ("native_add_ref", "_ZNK10nlRefCount6AddRefEv"),
        ("native_milliseconds_to_interval", "PR_MillisecondsToInterval"),
    ] {
        api.insert(
            key.into(),
            *imports.get(symbol).ok_or_else(|| crate::Error::Device(format!("missing legacy import {symbol}")))?,
        );
    }
    activity(&elf, &arm, &imports, true, &mut api, &mut layout)?;
    layout.insert("GLOBALS_TIMER_OFFSET".into(), timer(&elf, &arm, &imports)?);
    let (sleep_default, sleep_active) = sleep_timeout(&elf).unwrap_or((0, 0));
    layout.insert("SLEEP_DEFAULT_OFFSET".into(), sleep_default);
    layout.insert("SLEEP_ACTIVE_OFFSET".into(), sleep_active);
    let bright = brightness(&elf);
    layout.insert("BRIGHTNESS_GLOBALS_OFFSET".into(), bright.as_ref().map_or(0, |b| b.globals));
    layout.insert("BRIGHTNESS_MODE_OFFSET".into(), bright.as_ref().map_or(0, |b| b.mode));
    layout.insert("BRIGHTNESS_HIGH_OFFSET".into(), bright.as_ref().map_or(0, |b| b.high));
    api.insert("native_set_brightness".into(), bright.as_ref().map_or(api["native_screen_init"], |b| b.set));
    api.insert("native_apply_brightness".into(), bright.as_ref().map_or(api["native_screen_init"], |b| b.apply));
    for (bucket, name) in [
        ("device", "16nlCZDeviceBucket"),
        ("shared", "16nlCZSharedBucket"),
        ("schedule", "29nlCZTemperatureScheduleBucket"),
    ] {
        let slot = primary_vtable(&elf, name)? + 0x48;
        let function = elf.read(slot)?;
        relocated(&elf, slot, "legacy bucket vtable")?;
        if bucket == "device" {
            let offsets = arm
                .code(function, None)?
                .into_iter()
                .filter(|i| i.name == "ldr" && i.text.starts_with("r0, [r0, #"))
                .filter_map(|i| i.ops.get(1).and_then(Op::mem).map(|(_, d, _)| d as u32));
            layout.insert("BUCKET_GLOBALS_OFFSET".into(), unique(offsets, "legacy bucket globals")?);
        }
        api.insert(format!("native_{bucket}_commit"), function);
        anchors.insert(format!("{bucket}_commit_slot"), slot);
    }
    for (name, symbol) in [("native_delete", "_ZdlPv"), ("native_time", "time")] {
        api.insert(name.into(), *imports.get(symbol).ok_or_else(|| crate::Error::Device(format!("missing {symbol}")))?);
    }
    anchors.insert("swap_call_slot".into(), swap_call(&elf, api["native_egl_swap"])?);
    let by_address = imports.iter().map(|(k, v)| (*v, k.clone())).collect();
    let mut fingerprints = BTreeMap::new();
    let mut evidence = BTreeMap::new();
    for (name, &address) in api.iter().chain(&anchors) {
        if imports.values().any(|&v| v == address) || name == "native_screen_vtable" || name.ends_with("_slot") {
            continue;
        }
        fingerprints.insert(name.clone(), arm.fingerprint(address)?);
        evidence.insert(name.clone(), arm.imports_called(address, &by_address)?.into_iter().collect());
    }
    let sha = sha256_hex(data);
    Ok(Discovery {
        factory: account,
        native_api: api,
        layout,
        anchors,
        fingerprints,
        evidence,
        stock_sha256: sha,
        framebuffer: None,
        dial: None,
    })
}
