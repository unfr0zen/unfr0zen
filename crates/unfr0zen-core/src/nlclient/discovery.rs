//! Resolve native addresses from this ELF's imports, registrations and code.
//! Reviewed instruction shapes name the build, never select addresses.
use super::{
    arm::{Arm, Ins, Op, Value},
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
/// The screen hand-over's imports: those both kinds use, then each kind's.
const HANDOVER_IMPORTS: [(&str, &str); 6] = [
    ("native_open", "open"),
    ("native_read", "read"),
    ("native_close", "close"),
    ("native_kill", "kill"),
    ("native_usleep", "usleep"),
    ("native_getpid", "getpid"),
];
const EGL_IMPORTS: [(&str, &str); 2] = [
    ("native_egl_swap", "eglSwapBuffers"),
    ("native_pthread_create", "pthread_create"),
];
const FRAMEBUFFER_IMPORTS: [(&str, &str); 1] = [("native_mmap", "mmap")];
/// The screen hand-over this build gets (layout_HANDOVER): 1 where nlclient
/// presents through eglSwapBuffers (the gen 3), 2 where it pans its own
/// framebuffer mapping (the gen 2's nlARMFramebuffer), 0 without either.
pub fn handover(discovery: &Discovery) -> u32 {
    let has = |list: &[(&str, &str)]| list.iter().all(|(key, _)| discovery.native_api.contains_key(*key));
    if !has(&HANDOVER_IMPORTS) {
        0
    } else if has(&EGL_IMPORTS) && discovery.anchors.contains_key("swap_call_slot") {
        1
    } else if has(&FRAMEBUFFER_IMPORTS) && discovery.framebuffer.is_some() {
        2
    } else {
        0
    }
}

/// What the screen's hand-over links against, besides the build's natives:
/// its kind, and the natives and fields of a kind the build hasn't as 0,
/// never used.
pub fn handover_externals(discovery: &Discovery) -> BTreeMap<String, u32> {
    let kind = handover(discovery);
    let mut externals: BTreeMap<String, u32> = HANDOVER_IMPORTS
        .iter()
        .chain(&EGL_IMPORTS)
        .chain(&FRAMEBUFFER_IMPORTS)
        .filter(|(key, _)| !discovery.native_api.contains_key(*key))
        .map(|(key, _)| ((*key).to_owned(), 0))
        .collect();
    let fb = discovery.framebuffer.as_ref().filter(|_| kind == 2);
    externals.insert("native_fb_flip".into(), fb.map_or(0, |f| f.flip));
    externals.insert("layout_FB_FD_OFFSET".into(), fb.map_or(0, |f| f.fd));
    externals.insert("layout_FB_MAP_OFFSET".into(), fb.map_or(0, |f| f.map));
    externals.insert("layout_FB_SIZE_OFFSET".into(), fb.map_or(0, |f| f.size));
    externals.insert("layout_HANDOVER".into(), kind);
    externals
}

/// The gen 2's display, nlARMFramebuffer: nlclient draws into the two pages
/// of its own mapping of fb0 and pans between them in the vtable's third
/// method (flip). The patch routes that slot through custom_flip, which
/// needs the object's fields its init stores: fb0's descriptor, and the
/// mapping's address and length.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Framebuffer {
    pub slot: u32,
    pub flip: u32,
    pub fd: u32,
    pub map: u32,
    pub size: u32,
}

/// The one call that presents nlclient's frames: a BL to eglSwapBuffers's
/// PLT stub. More than one, or none, and the hand-over can't be made whole.
fn swap_call(elf: &Elf, stub: u32) -> Result<u32> {
    let text = elf.section(".text")?;
    let mut sites = Vec::new();
    for off in (text.offset..text.offset + text.size).step_by(4) {
        let w = word(elf.data, off)?;
        if w >> 24 != 0xeb {
            continue;
        }
        let pc = text.address + (off - text.offset) as u32;
        let delta = (((w & 0xff_ffff) << 8) as i32) >> 6;
        if pc.wrapping_add(8).wrapping_add(delta as u32) == stub {
            sites.push(pc);
        }
    }
    unique(sites, "eglSwapBuffers calls")
}
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Discovery {
    pub factory: u32,
    pub native_api: BTreeMap<String, u32>,
    pub layout: BTreeMap<String, u32>,
    pub anchors: BTreeMap<String, u32>,
    pub fingerprints: BTreeMap<String, String>,
    /// The imported symbols each native calls, its evidence across builds.
    pub evidence: BTreeMap<String, Vec<String>>,
    /// Per reviewed variant, how many natives' evidence it matches.
    pub matches: BTreeMap<String, usize>,
    /// The reviewed variant whose instruction shapes this build has, if any.
    /// Every build must pass the structural discovery: addresses, layouts,
    /// call shapes, each native's own shape and the library calls it makes.
    /// A build no variant's shapes match is patched on that alone.
    pub variant: Option<String>,
    pub stock_sha256: String,
    pub validation: String,
    /// The gen 2's framebuffer, for its screen hand-over; absent elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framebuffer: Option<Framebuffer>,
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
    match survey(data)? {
        (found, None) => Ok(found),
        (_, Some(objection)) => device(format!("Native discovery: {objection}")),
    }
}
/// What discovery finds, and why the build is refused when it is: every
/// native must call what a reviewed build's does, and fingerprints mask the
/// targets of calls, so this holds a reviewed build too. A review of a new
/// build starts from the findings whatever the objection.
pub fn survey(data: &[u8]) -> Result<(Discovery, Option<String>)> {
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
    // The screen hand-over: nlclient presents each frame through one call
    // of eglSwapBuffers, which the payload wraps (custom_swap), and a thread
    // of its own keeps the display lit. The gen 2's nlclient draws straight
    // to the framebuffer, with no EGL and no pthread_create: it is patched
    // without the hand-over (handover()).
    for (key, name) in HANDOVER_IMPORTS.iter().chain(&EGL_IMPORTS).chain(&FRAMEBUFFER_IMPORTS) {
        if let Some(&address) = imports.get(*name) {
            api.insert((*key).to_owned(), address);
        }
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
    let mut draw = elf.read(lv + 0x28)?;
    let ec = arm.calls(enter)?;
    let mut dc = arm.calls(draw)?;
    // A build may keep the draw's visibility guard in a wrapper around the
    // drawing itself: the wrapper calls the guard, then the body, whose calls
    // follow the guard's in a build that draws in one function.
    if let [guard, body] = dc[..] {
        let mut calls = vec![guard];
        calls.extend(arm.calls(body)?);
        (draw, dc) = (body, calls);
    }
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
    let known: serde_json::Value = serde_json::from_str(include_str!("abi_shapes.json"))
        .map_err(|e| crate::Error::Device(e.to_string()))?;
    // Screen port additions: real activity notification and periodic refresh.
    activity(&elf, &arm, &imports, &known, &mut api, &mut layout)?;
    for(name,symbol)in [("native_send_timeout","_ZN12nlEventUtils11SendTimeoutERP21nlEventTimeoutRequestjjP12nlEventQueueP15nlIEventHandlerP7nlTimer"),("native_cancel_timeout","_ZN21nlEventTimeoutRequest6CancelEv"),("native_release","_ZNK10nlRefCount7ReleaseEv"),("native_timeout_id","_ZNK15nlEventTimedOut5GetIDEv")]{api.insert(name.into(),*imports.get(symbol).ok_or_else(||crate::Error::Device(format!("missing status-refresh import {symbol}")))?);}
    let screen_draw = elf.read(sv + 0xc)?;
    api.insert("native_screen_draw".into(), screen_draw);
    let menu_hidden = unique(arm.calls(screen_draw)?, "menu visibility accessor")?;
    let mut menu_scales = Vec::new();
    let mut title_draws = Vec::new();
    for target in arm.tails(screen_draw)? {
        let calls = arm.calls(target)?;
        if calls.len() == 4 && calls[3] == api["native_style"] {
            menu_scales.push(calls[2]);
            title_draws.push(target);
        }
    }
    let menu_scale = unique(menu_scales, "menu animation scale accessor")?;
    anchors.insert("menu_title_draw".into(), unique(title_draws, "menu title drawing")?);
    api.insert("native_menu_hidden".into(), menu_hidden);
    api.insert("native_menu_scale".into(), menu_scale);
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
    downlink(&elf, &arm, &imports, &mut api, &mut layout, &mut anchors)?;
    let framebuffer = if EGL_IMPORTS.iter().all(|(key, _)| api.contains_key(*key)) {
        anchors.insert("swap_call_slot".into(), swap_call(&elf, api["native_egl_swap"])?);
        None
    } else {
        framebuffer(&elf, &arm, &imports).ok()
    };
    require(
        layout["LAYER_VTABLE_SIZE"].max(layout["SCREEN_VTABLE_SIZE"]) <= 0x400,
        "vtable larger than the payload holds",
    )?;
    roles(&arm, &api, &anchors)?;
    // Every native of the build's own code: its shape names the build, and
    // the library calls it makes are the evidence an unreviewed build is
    // patched on. The view's event handler differs by build, so its calls
    // are no evidence; the dispatch it owns is checked by its shape.
    let by_address: BTreeMap<u32, String> = imports.iter().map(|(k, v)| (*v, k.clone())).collect();
    let mut fingerprints = BTreeMap::new();
    let mut evidence = BTreeMap::new();
    for (name, &address) in api.iter().chain(&anchors) {
        if by_address.contains_key(&address) || name == "native_screen_vtable" || name.ends_with("_slot") {
            continue;
        }
        fingerprints.insert(name.clone(), arm.fingerprint(address)?);
        if !matches!(name.as_str(), "view_handler" | "view_dispatch") {
            evidence.insert(name.clone(), arm.imports_called(address, &by_address)?.into_iter().collect::<Vec<_>>());
        }
    }
    let layout_value = serde_json::to_value(&layout).unwrap();
    let fingerprint_value = serde_json::to_value(&fingerprints).unwrap();
    let variants = known["variants"]
        .as_array()
        .ok_or_else(|| crate::Error::Device("invalid embedded ABI shapes".into()))?;
    let variant = variants
        .iter()
        .find(|v| v["layout"] == layout_value && v["functions"] == fingerprint_value)
        .and_then(|v| v["label"].as_str())
        .map(str::to_owned);
    let mut matches = BTreeMap::new();
    for v in variants {
        let label = v["label"].as_str().unwrap_or_default().to_owned();
        let count = evidence.iter().filter(|(name, calls)| v["imports"][name] == serde_json::json!(calls)).count();
        matches.insert(label, count);
    }
    let unmatched: Vec<_> = evidence
        .iter()
        .filter(|(name, calls)| !variants.iter().any(|v| v["imports"][name.as_str()] == serde_json::json!(calls)))
        .map(|(name, calls)| format!("{name} calls {}", if calls.is_empty() { "nothing".to_owned() } else { calls.join(", ") }))
        .collect();
    let objection = (!unmatched.is_empty())
        .then(|| format!("no reviewed build's native makes these library calls: {}", unmatched.join("; ")));
    let validation = match &variant {
        Some(label) => format!("instruction shapes of the reviewed {label} build; guest acceptance required"),
        None => {
            let (label, count) = matches.iter().max_by_key(|(_, n)| **n).map(|(l, n)| (l.as_str(), *n)).unwrap_or(("none", 0));
            format!("unreviewed build: structurally consistent, {count} of {} natives' library calls as the reviewed {label} build; \
                     no reviewed instruction shapes", evidence.len())
        }
    };
    let found = Discovery {
        factory: account,
        native_api: api,
        layout,
        anchors,
        fingerprints,
        evidence,
        matches,
        variant,
        stock_sha256: sha256_hex(data),
        validation,
        framebuffer,
    };
    Ok((found, objection))
}
/// What the screen asks of natives found by position, checked on the
/// natives' own instructions: the loads, stores and calls the screen's use
/// of each one assumes, which a recompile keeps and a wrong slot does not.
fn roles(arm: &Arm, api: &BTreeMap<String, u32>, anchors: &BTreeMap<String, u32>) -> Result<()> {
    let body = |name: &str| arm.body(api.get(name).or_else(|| anchors.get(name)).copied().unwrap());
    let loads = |code: &[Ins], disp: i32| code.iter().filter(|i| i.name == "ldr" && i.ops.get(1).and_then(Op::mem)
        .is_some_and(|(b, d, _)| b != "sp" && b != "pc" && d == disp)).count();
    let stores = |code: &[Ins], disp: i32| code.iter().any(|i| i.name == "str" && i.ops.last().and_then(Op::mem)
        .is_some_and(|(b, d, _)| b != "sp" && d == disp));
    let calls = |code: &[Ins]| code.iter().filter(|i| i.name == "bl").count();
    let indirect = |code: &[Ins]| code.iter().any(|i| matches!(i.name.as_str(), "bx" | "blx") && i.ops[0].reg().is_some_and(|r| r != "lr"));
    // A getter: one load through a register, no call, nothing else of note.
    let getter = |code: &[Ins]| -> Option<i32> {
        let fields: Vec<_> = code.iter().filter(|i| i.name == "ldr").filter_map(|i| i.ops.get(1).and_then(Op::mem))
            .filter(|(b, _, _)| *b != "sp" && *b != "pc").map(|(_, d, _)| d).collect();
        (fields.len() == 1 && calls(code) == 0 && !indirect(code)).then(|| fields[0])
    };
    for (name, check) in [
        ("native_image_width", getter(&body("native_image_width")?) == Some(4)),
        ("native_image_height", getter(&body("native_image_height")?) == Some(8)),
        ("native_style", getter(&body("native_style")?).is_some()),
        ("native_activity_client", getter(&body("native_activity_client")?).is_some()),
        ("native_menu_scale", {
            let code = body("native_menu_scale")?;
            loads(&code, i32::MAX) == 0 && calls(&code) == 0 && code.last().is_some_and(|i| i.name == "b")
                && code.iter().filter(|i| i.name == "ldr").count() == 1
        }),
        ("native_menu_hidden", { let code = body("native_menu_hidden")?; loads(&code, 0x28) == 1 && indirect(&code) }),
        ("native_screen_event", { let code = body("native_screen_event")?; loads(&code, 0x20) == 1 && loads(&code, 0) >= 2 && indirect(&code) }),
        ("native_screen_enter", { let code = body("native_screen_enter")?; loads(&code, 0x20) == 1 && loads(&code, 0xc) == 1 && indirect(&code) }),
        ("native_layer_init", { let code = body("native_layer_init")?; stores(&code, 0x8c) && calls(&code) == 1 && indirect(&code) }),
        ("native_config_invalidate", {
            let code = body("native_config_invalidate")?;
            stores(&code, 0x18) && calls(&code) == 2 && code.iter().any(|i| i.name == "cmn" && i.ops[1].imm() == Some(1))
        }),
        ("layout_wrapper", { let code = body("layout_wrapper")?; loads(&code, 0xc) == 1 && code.last().is_some_and(|i| i.name == "b") }),
        ("native_layer_wheel", {
            let code = body("native_layer_wheel")?;
            calls(&code) == 2 && code.iter().any(|i| i.name == "vcvt.f32.s32") && code.iter().any(|i| i.name == "vdiv.f32")
        }),
        ("native_screen_init", { let code = body("native_screen_init")?; stores(&code, 0xe4) && loads(&code, 0xe8) == 1 && calls(&code) == 5 }),
        ("native_config_draw", {
            // The renderer's body is its own function the native calls, or
            // inlined: its measure and draw calls, and the stack check.
            let code = body("native_config_draw")?;
            [1, 4].contains(&calls(&code)) && [0x28, 0x2c, 0x34, 0x38, 0x3c].iter().all(|&d| loads(&code, d) == 1)
        }),
        ("view_dispatch", { let code = body("view_dispatch")?; loads(&code, 0) >= 2 && indirect(&code) }),
    ] {
        require(check, format!("{name} does not have the native's shape"))?;
    }
    Ok(())
}
/// The first virtual slot of the primary vtable of the class whose RTTI
/// name (as mangled, length first) is `name`.
fn primary_vtable(elf: &Elf, name: &str) -> Result<u32> {
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
    let needle = format!("{name}\0");
    let off = unique(
        elf.data
            .windows(needle.len())
            .enumerate()
            .filter(|(_, b)| *b == needle.as_bytes())
            .map(|(off, _)| off),
        &format!("{name} RTTI name"),
    )?;
    let address = unique(
        elf.sections
            .iter()
            .filter(|s| s.address != 0 && s.offset <= off && off < s.offset + s.size)
            .map(|s| s.address + (off - s.offset) as u32),
        &format!("{name} RTTI address"),
    )?;
    let mut vtables = Vec::new();
    for r in refs(address)? {
        for p in refs(r - 4)? {
            if elf.read(p - 4)? == 0 {
                vtables.push(p + 4);
            }
        }
    }
    unique(vtables, &format!("{name} primary vtable"))
}
/// The gen 2's nlARMFramebuffer (Framebuffer). Its init maps fb0 with
/// `mmap(0, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0)` and stores the
/// address; its flip pans with FBIOPAN_DISPLAY. Only an EXEC build: a PIE's
/// vtable slot would need a relocation the patch doesn't write.
fn framebuffer(elf: &Elf, arm: &Arm, imports: &BTreeMap<String, u32>) -> Result<Framebuffer> {
    require(half(elf.data, 16)? == 2, "framebuffer hand-over needs an EXEC build")?;
    let (mmap, ioctl) = match (imports.get("mmap"), imports.get("ioctl")) {
        (Some(&m), Some(&i)) => (m, i),
        _ => return device("framebuffer hand-over needs mmap and ioctl"),
    };
    let vtable = primary_vtable(elf, "16nlARMFramebuffer")?;
    let (init, slot) = (elf.read(vtable)?, vtable + 8);
    let flip = elf.read(slot)?;
    let target = |i: &Ins| i.ops.first().and_then(Op::imm);
    let code = arm.body(flip)?;
    let pan = code.iter().position(|i| i.name == "movw" && i.text.ends_with("#0x4606"));
    require(
        pan.is_some_and(|p| code[p..].iter().any(|i| matches!(i.name.as_str(), "bl" | "b") && target(i) == Some(ioctl))),
        "framebuffer flip does not pan",
    )?;
    let code = arm.code(init, None)?;
    let call = unique(
        code.iter().enumerate().filter(|(_, i)| i.name == "bl" && target(i) == Some(mmap)).map(|(n, _)| n),
        "framebuffer mmap call",
    )?;
    // A store of `reg` into the object (the base register `this` is in).
    let field = |i: &Ins, reg: &str| -> Option<(String, u32)> {
        let (base, disp, indexed) = i.ops.get(1)?.mem()?;
        (i.name == "str" && i.ops.first()?.reg() == Some(reg) && base != "sp" && !indexed && disp > 0)
            .then(|| (base.to_owned(), disp as u32))
    };
    let (this, map) = code[call + 1..code.len().min(call + 5)]
        .iter()
        .find_map(|i| field(i, "r0"))
        .ok_or_else(|| crate::Error::Device("framebuffer mapping not stored".into()))?;
    let before = &code[call.saturating_sub(12)..call];
    let size = before
        .iter()
        .rev()
        .find_map(|i| field(i, "r1").filter(|(b, _)| *b == this))
        .ok_or_else(|| crate::Error::Device("framebuffer mapping length not stored".into()))?
        .1;
    // The fifth argument, fb0's descriptor, goes on the stack from a field.
    let stacked = before
        .iter()
        .rev()
        .find(|i| i.name == "str" && i.ops.get(1).and_then(Op::mem) == Some(("sp", 0, false)))
        .and_then(|i| i.ops.first()?.reg())
        .ok_or_else(|| crate::Error::Device("framebuffer descriptor not passed".into()))?;
    let fd = before
        .iter()
        .rev()
        .find_map(|i| {
            let (base, disp, indexed) = i.ops.get(1)?.mem()?;
            (i.name == "ldr" && i.ops.first()?.reg() == Some(stacked) && base == this && !indexed)
                .then_some(disp as u32)
        })
        .ok_or_else(|| crate::Error::Device("framebuffer descriptor field not found".into()))?;
    Ok(Framebuffer { slot, flip, fd, map, size })
}
fn downlink(
    elf: &Elf,
    arm: &Arm,
    imports: &BTreeMap<String, u32>,
    api: &mut BTreeMap<String, u32>,
    layout: &mut BTreeMap<String, u32>,
    anchors: &mut BTreeMap<String, u32>,
) -> Result<()> {
    for (bucket, name) in [
        ("device", "16nlCZDeviceBucket"),
        ("shared", "16nlCZSharedBucket"),
        ("schedule", "29nlCZTemperatureScheduleBucket"),
        ("view_handler", "12nlClientView"),
    ] {
        // Bucket finish-parsing and view event-dispatch virtual slots.
        let slot = primary_vtable(elf, name)? + if bucket == "view_handler" { 0x10 } else { 0x48 };
        let function = elf.read(slot)?;
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
            anchors.insert("view_handler".into(), function);
            anchors.insert("view_dispatch".into(), dispatch);
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
            layout.insert("BUCKET_GLOBALS_OFFSET".into(), offset);
        }
        api.insert(format!("native_{bucket}_commit"), function);
        anchors.insert(format!("{bucket}_commit_slot"), slot);
    }
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
    let vtable = unique(vtables, "globals primary vtable")?;
    let mark_shape = arm.fingerprint(mark)?;
    let mut getters = Vec::new();
    for slot in [0x10, 0x18] {
        let getter = elf.read(vtable + slot)?;
        let shapes = serde_json::json!({"mark": mark_shape, "client": arm.fingerprint(getter)?});
        if known["activity_variants"].as_array().is_some_and(|v| v.contains(&shapes)) {
            getters.push(getter);
        }
    }
    // The client accessor's slot differs by generation (0x18 on the gen 3,
    // 0x10 on the gen 2) and both slots hold plain getters, so a build no
    // reviewed shapes name gets the gen 3's: the screen checks the client's
    // back-pointer before using it, so a wrong slot costs the display wake,
    // not a crash.
    if getters.is_empty() {
        getters.push(elf.read(vtable + 0x18)?);
    }
    let getter = unique(getters, "validated client globals accessor")?;
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
    Ok(())
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

