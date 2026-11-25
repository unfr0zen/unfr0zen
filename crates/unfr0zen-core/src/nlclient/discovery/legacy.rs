//! The reviewed 5.6-7 native ABI: 32-byte strings and the compact UI vtables.
//! Bindings are accepted only for the complete, unchanged stock executable.
use super::*;

pub(super) fn survey(data: &[u8]) -> Result<Discovery> {
    require(crate::nlclient::legacy_abi(data), "unknown legacy ABI executable")?;
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
    for (key, address) in [
        ("native_layer_ctor", 0xc75b4),
        ("native_screen_ctor", 0xff1c0),
        ("native_screen_vtable", 0x374060),
        ("native_layer_lookup", 0x4d01c),
        ("native_attach", 0x28fb4c),
        ("native_screen_init", 0xfe878),
        ("native_layer_init", 0x1609e0),
        ("native_config_invalidate", 0x290650),
        ("native_config_y", 0x2903b8),
        ("native_config_draw", 0x29642c),
        ("native_style", 0x502a4),
        ("native_image_load", 0x29dc78),
        ("native_image_width", 0x29d5a0),
        ("native_image_height", 0x29d5b4),
        ("native_scroll_target", 0x299898),
        ("native_menu_hidden", 0x4d2d0),
        ("native_menu_scale", 0x50274),
        ("native_scroll_draw", 0x161bcc),
        ("native_select", 0x1611e8),
        ("native_layer_wheel", 0x1614fc),
        ("native_screen_event", 0xc7b74),
        ("native_screen_enter", 0xc7c44),
        ("native_rebuild_title", 0xfe770),
        ("native_layer_event", 0xc73ac),
        ("native_config_layout", 0x161e4c),
        ("native_screen_draw", 0xc7eb8),
    ] {
        api.insert(key.into(), address);
    }
    let layer = 0x37383c;
    let screen = 0x374060;
    let common = 0xc4;
    let parts = [0x3738c0, 0x373918, 0x37392c];
    let mut layout = BTreeMap::from([
        ("PRIMARY_SIZE".into(), 0x150),
        ("COMMON_OFFSET".into(), common),
        ("LAYER_VTABLE_SIZE".into(), group_end(&elf, parts[2])? - layer + 12),
        ("SCREEN_VTABLE_SIZE".into(), group_end(&elf, 0x3740e8)? - screen + 8),
        ("LAYER_COMMON_VPTR".into(), parts[0] - layer + 12),
        ("LAYER_SECOND_VPTR".into(), parts[1] - layer + 12),
        ("LAYER_THIRD_VPTR".into(), parts[2] - layer + 12),
        ("SCREEN_SECOND_VPTR".into(), 0x3740d4 - screen + 8),
        ("SCREEN_THIRD_VPTR".into(), 0x3740e8 - screen + 8),
        ("SCREEN_TITLE_SLOT".into(), 0x68),
        ("LEGACY_CLICK".into(), 1),
        ("RENAME_SYSCALL".into(), u32::from(!imports.contains_key("rename"))),
    ]);
    let factories = screen_factories(&elf)?;
    let mut anchors = BTreeMap::from([
        ("account_factory".into(), factories["account"]),
        ("layer_factory".into(), 0xc7664),
        ("screen_factory".into(), 0xc7dac),
        ("screen_composition".into(), 0xc7cc8),
        ("layer_enter".into(), 0xc741c),
        ("layer_draw".into(), 0xc72d8),
        ("layer_event".into(), 0xc73ac),
        ("layout_wrapper".into(), 0x161fb4),
        ("screen_title".into(), 0xc7e4c),
        ("generic_event".into(), 0x161940),
    ]);
    for (key, symbol) in [
        ("native_activity_ctor", "_ZN7nlEventC1Ev"),
        ("native_activity_init", "_ZN7nlEvent4InitEji"),
        ("native_enqueue", "_ZN12nlEventQueue7EnqueueEPK7nlEventP15nlIEventHandler"),
        ("native_add_ref", "_ZNK10nlRefCount6AddRefEv"),
        ("native_milliseconds_to_interval", "PR_MillisecondsToInterval"),
    ] {
        api.insert(
            key.into(),
            *imports.get(symbol).ok_or_else(|| crate::Error::Device(format!("missing legacy import {symbol}")))?,
        );
    }
    api.insert("native_mark_activity".into(), 0x56bf4);
    api.insert("native_activity_client".into(), 0x48d8c);
    layout.insert("ACTIVITY_QUEUE_OFFSET".into(), 0x1a4);
    layout.insert("GLOBALS_TIMER_OFFSET".into(), 0x80);
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
    api.insert("native_delete".into(), imports["_ZdlPv"]);
    api.insert("native_time".into(), imports["time"]);
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
    Ok(Discovery {
        factory: factories["account"],
        native_api: api,
        layout,
        anchors,
        fingerprints,
        evidence,
        matches: BTreeMap::new(),
        variant: Some("gen3-5.6-7".into()),
        stock_sha256: sha256_hex(data),
        validation: "reviewed gen3-5.6-7 executable; guest acceptance required".into(),
        framebuffer: None,
        dial: None,
    })
}
