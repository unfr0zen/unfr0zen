//! Resolve modern native addresses from this ELF's imports, registrations and code.
//! Compact UI builds use their own vtable and string layout checks.
use super::{
    arm::{scalar_field, Arm, Ins, Op, Value},
    elf::{bytes, half, string, word, Elf},
};
use crate::shell::sha256_hex;
use crate::{device, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
mod legacy;
const IMPORTS: [(&str, &str); 11] = [
    ("native_new", "_Znwj"),
    ("native_string_ctor", "_ZN8nlStringC1Ev"),
    ("native_string_copy", "_ZN8nlString4CopyEPKc"),
    ("native_string_destroy", "_ZN8nlStringD1Ev"),
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
const EGL_IMPORTS: [(&str, &str); 2] =
    [("native_egl_swap", "eglSwapBuffers"), ("native_pthread_create", "pthread_create")];
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
/// the hand-over's kind, and the natives and fields of a kind the build
/// hasn't as 0, never used.
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Discovery {
    pub factory: u32,
    pub native_api: BTreeMap<String, u32>,
    pub layout: BTreeMap<String, u32>,
    pub anchors: BTreeMap<String, u32>,
    pub fingerprints: BTreeMap<String, String>,
    /// The imported symbols each native calls, its evidence across builds.
    pub evidence: BTreeMap<String, Vec<String>>,
    pub stock_sha256: String,
    /// The gen 2's framebuffer, for its screen hand-over; absent elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framebuffer: Option<Framebuffer>,
    /// The thermostat's dial, which the theme preload draws themes in place of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dial: Option<Dial>,
}
/// The thermostat's dial, nlTempSliderLayer: the layer of the home screen
/// that shows the temperature, whose draw (the vtable's fifth method, as for
/// every nlLayer) the theme preload replaces (platform/common/theme). Its
/// screen's, nlTempScreen's, event handler (the first method) turns the ring
/// into setpoints; `value` is nlEvent::GetValue, a turn's size.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Dial {
    pub slot: u32,
    pub draw: u32,
    pub event_slot: u32,
    pub event: u32,
    pub value: u32,
    /// The thermostat's figures (nlTempLayer, over the dial), which the
    /// theme hides, having its own.
    pub figures_slot: u32,
    pub figures: u32,
}
pub(super) fn require(ok: bool, message: impl Into<String>) -> Result<()> {
    if ok {
        Ok(())
    } else {
        device(format!("Native discovery: {}", message.into()))
    }
}
pub(super) fn unique<T: Ord>(values: impl IntoIterator<Item = T>, label: &str) -> Result<T> {
    let mut values: BTreeSet<T> = values.into_iter().collect();
    require(values.len() == 1, format!("{label}: expected one result, found {}", values.len()))?;
    Ok(values.pop_first().unwrap())
}
fn immediate(w: u32) -> u32 {
    (w & 255).rotate_right(((w >> 8) & 15) * 2)
}
pub(super) fn imported(elf: &Elf) -> Result<BTreeMap<String, u32>> {
    let (rel, sym, strings, plt) =
        (elf.section(".rel.plt")?, elf.section(".dynsym")?, elf.section(".dynstr")?, elf.section(".plt")?);
    require(rel.size % 8 == 0 && sym.size % 16 == 0, "invalid PLT metadata")?;
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
        let (a, b, c) = (word(elf.data, off)?, word(elf.data, off + 4)?, word(elf.data, off + 8)?);
        if (a & 0xfffff000, b & 0xfffff000, c & 0xfffff000) != (0xe28fc000, 0xe28cc000, 0xe5bcf000) {
            continue;
        }
        let address = plt.address + (off - plt.offset) as u32;
        let target =
            address.wrapping_add(8).wrapping_add(immediate(a)).wrapping_add(immediate(b)).wrapping_add(c & 0xfff);
        if let Some(name) = got.get(&target) {
            require(result.insert(name.clone(), address).is_none(), "duplicate PLT symbol")?;
        }
    }
    require(result.len() == got.len(), "not every PLT relocation has a decoded stub")?;
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
                    best = Some(Registry { address: va, names, code_first });
                }
                va = if p > va { p + 8 } else { va + 4 };
            }
        }
    }
    best.ok_or_else(|| crate::Error::Device("screen registration table not found".into()))
}
/// The address of the one string `text` (with its NUL) in the read-only
/// data, if it is there once.
pub(super) fn string_address(elf: &Elf, text: &[u8]) -> Option<u32> {
    let mut found = Vec::new();
    for section in elf.sections.iter().filter(|s| s.kind == 1 && s.flags & 2 != 0 && s.flags & 4 == 0) {
        let data = elf.contents(section);
        let mut at = 0;
        while let Some(i) = data[at..].windows(text.len()).position(|w| w == text) {
            found.push(section.address + (at + i) as u32);
            at += i + 1;
        }
    }
    match found[..] {
        [one] => Some(one),
        _ => None,
    }
}

/// The ARM code's words, indexed across the code sections in order and
/// read in place: a table of them would take twice the code's size, which
/// the 64 MB OMAP units cannot spare beside a setup.
struct Words<'a> {
    /// Each code section's address, contents, and the index of its first word.
    sections: Vec<(u32, &'a [u8], usize)>,
    len: usize,
}

impl<'a> Words<'a> {
    fn new(elf: &'a Elf) -> Self {
        let (mut sections, mut len) = (Vec::new(), 0);
        for section in elf.sections.iter().filter(|s| s.kind == 1 && s.flags & 4 != 0) {
            let data = elf.contents(section);
            if data.len() >= 4 {
                sections.push((section.address, data, len));
                len += data.len() / 4;
            }
        }
        Words { sections, len }
    }

    fn len(&self) -> usize {
        self.len
    }

    /// Word `i`'s address and value.
    fn get(&self, i: usize) -> Option<(u32, u32)> {
        if i >= self.len {
            return None;
        }
        let &(address, data, first) = self.sections.iter().rev().find(|(_, _, first)| *first <= i)?;
        let at = (i - first) * 4;
        Some((address + at as u32, u32::from_le_bytes(data[at..at + 4].try_into().unwrap())))
    }

    fn at(&self, i: usize) -> (u32, u32) {
        self.get(i).expect("a word index below len")
    }

    fn iter(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        (0..self.len).map(|i| self.at(i))
    }

    /// The index of the word at `address`.
    fn index(&self, address: u32) -> Option<usize> {
        self.sections.iter().find_map(|&(start, data, first)| {
            let offset = address.checked_sub(start)? as usize;
            (offset.is_multiple_of(4) && offset < data.len() / 4 * 4).then_some(first + offset / 4)
        })
    }
}

/// The indexes in `words` of the instructions that complete `target`'s
/// address in a register: a literal load (plus pc on a position-independent
/// build), or a movw/movt pair.
fn references(elf: &Elf, words: &Words, target: u32) -> Vec<usize> {
    let mut found = Vec::new();
    for (i, (address, w)) in words.iter().enumerate() {
        let window = |n: usize| i + 1..(i + n).min(words.len());
        if w & 0xff7f_0000 == 0xe51f_0000 {
            let pc = address + 8;
            let literal = if w & (1 << 23) != 0 { pc.wrapping_add(w & 0xfff) } else { pc.wrapping_sub(w & 0xfff) };
            let Ok(value) = elf.read(literal) else { continue };
            let register = (w >> 12) & 0xf;
            if value == target {
                found.push(i);
            } else if let Some(j) = window(40).find(|&j| {
                let (at, x) = words.at(j);
                x & 0xffff_0ff0 == 0xe08f_0000 && x & 0xf == register && value.wrapping_add(at + 8) == target
            }) {
                found.push(j);
            }
        } else if w & 0xfff0_0000 == 0xe300_0000 {
            let register = (w >> 12) & 0xf;
            let low = ((w >> 4) & 0xf000) | (w & 0xfff);
            if let Some(j) = window(6).find(|&j| {
                let x = words.at(j).1;
                x & 0xfff0_f000 == 0xe340_0000 | register << 12
                    && ((((x >> 4) & 0xf000) | (x & 0xfff)) << 16 | low) == target
            }) {
                found.push(j);
            }
        }
    }
    found
}

pub(super) fn referencing_functions(elf: &Elf, arm: &Arm, address: u32) -> BTreeSet<u32> {
    let words = Words::new(elf);
    references(elf, &words, address).into_iter().filter_map(|i| arm.enclosing(words.at(i).0)).collect()
}

/// The target of the BL (or B, with `link` false) instruction `w` at `at`.
fn branch_target(at: u32, w: u32, link: bool) -> Option<u32> {
    if w >> 24 != if link { 0xeb } else { 0xea } {
        return None;
    }
    let offset = ((w & 0xff_ffff) << 8) as i32 >> 6;
    Some(at.wrapping_add(8).wrapping_add(offset as u32))
}

/// Startup samples the ambient light, then loads the same manager again
/// and compares its user brightness setting with the automatic mode (3).
pub(super) fn ambient_light_sampler(elf: &Elf, arm: &Arm, bright: &Brightness) -> Result<u32> {
    let words = Words::new(elf);
    let mut found = Vec::new();
    for i in 1..words.len().saturating_sub(5) {
        let call = |j| {
            let (at, w) = words.at(j);
            branch_target(at, w, true)
        };
        if call(i) != Some(bright.accessor)
            || call(i + 3) != Some(bright.accessor)
            || words.at(i - 1).1 != words.at(i + 2).1
            || words.at(i + 5).1 != 0xe3500003
        {
            continue;
        }
        let (Some(sampler), Some(mode)) = (call(i + 1), call(i + 4)) else { continue };
        if arm.body(mode)?.iter().any(|i| i.word == 0xe5900000 | bright.mode) {
            found.push(sampler);
        }
    }
    unique(found, "ambient light sampler")
}

/// The client's screen timeouts: its default, client.config's sleeptimeout,
/// and the one in force (mSleepTimeout), which it resets to the default.
/// Found where it logs that reset: the code loads the log's format, calls
/// the logger, then copies the default over the one in force (`ldr Rx,
/// [Rn, #default]`, then `str Rx, [Rn, #active]`). None on a build without
/// that shape.
fn sleep_timeout(elf: &Elf) -> Option<(u32, u32)> {
    let string = string_address(elf, b"client: Setting mSleepTimeout - Old: %d New: %d\0")?;
    let words = Words::new(elf);
    let mut pairs = Vec::new();
    for at in references(elf, &words, string) {
        // The logger's call, then the copy.
        let Some(call) = (at + 1..(at + 16).min(words.len())).find(|&j| words.at(j).1 >> 24 == 0xeb) else {
            continue;
        };
        for j in call + 1..(call + 6).min(words.len()) {
            let load = words.at(j).1;
            if load & 0xfff0_0000 != 0xe590_0000 {
                continue;
            }
            let (value, base) = ((load >> 12) & 0xf, (load >> 16) & 0xf);
            if let Some(store) = (j + 1..(j + 5).min(words.len()))
                .map(|k| words.at(k).1)
                .find(|x| x & 0xfff0_0000 == 0xe580_0000 && (x >> 12) & 0xf == value && (x >> 16) & 0xf == base)
            {
                pairs.push((load & 0xfff, store & 0xfff));
            }
            break;
        }
    }
    pairs.sort();
    pairs.dedup();
    match pairs[..] {
        [(default, active)] if default != active => Some((default, active)),
        _ => None,
    }
}

/// The display's brightness manager, as the Brightness screen drives it:
/// its setter (SetUserBrightnessSetting: 0 low, 1 medium, 2 high, 3 auto),
/// what applies the setting to the backlight, the globals' field holding
/// the manager, the setting's field and the high level's (a float, in %).
pub(super) struct Brightness {
    pub accessor: u32,
    pub set: u32,
    pub apply: u32,
    pub globals: u32,
    pub mode: u32,
    pub high: u32,
}

/// Found from the setter's log of an invalid value: the setter is the
/// nearest called function before it, storing its argument at the mode's
/// field. Its callers (the Brightness screen) get the manager from an
/// accessor of the globals (`ldr r0, [r0, #globals]`) and then call apply,
/// which loads the mode and, for high (2), the level at `high`.
pub(super) fn brightness(elf: &Elf) -> Option<Brightness> {
    let string = string_address(elf, b"brightness manager: SetUserBrightnessSetting() invalid value:%d\0")?;
    let words = Words::new(elf);
    let calls_to = |target: u32| {
        words.iter().enumerate().filter(move |&(_, (at, w))| branch_target(at, w, true) == Some(target)).map(|(i, _)| i)
    };
    let mut found = Vec::new();
    // Inlining the setter can leave more than one reference to its log.
    // The callable setter must still have the screen's accessor and apply.
    for reference in references(elf, &words, string) {
        let referenced = words.at(reference).0;
        let Some(set) = words
            .iter()
            .filter_map(|(at, w)| branch_target(at, w, true))
            .filter(|target| (referenced.saturating_sub(0x100)..=referenced).contains(target))
            .max()
        else {
            continue;
        };
        let Some(start) = words.index(set) else { continue };
        // str r1, [rN, #mode]
        let Some((_, store)) =
            (start..(start + 16).min(words.len())).map(|j| words.at(j)).find(|(_, w)| w & 0xfff0_f000 == 0xe580_1000)
        else {
            continue;
        };
        let mode = store & 0xfff;
        for call in calls_to(set) {
            // bl accessor, then (within a few) bl set.
            let Some(accessor) = (call.saturating_sub(3)..call).rev().find_map(|j| {
                let (at, w) = words.at(j);
                branch_target(at, w, true)
            }) else {
                continue;
            };
            let Some(first) = words.index(accessor) else { continue };
            let Some(globals) = (first..(first + 4).min(words.len()))
                .map(|j| words.at(j).1)
                .find(|w| w & 0xffff_f000 == 0xe590_0000)
                .map(|w| w & 0xfff)
            else {
                continue;
            };
            for j in call + 1..(call + 12).min(words.len()) {
                let (at, w) = words.at(j);
                let Some(apply) = branch_target(at, w, true).filter(|&t| t != accessor) else {
                    continue;
                };
                if let Some(high) = words.index(apply).and_then(|at| high_level(&words, at, mode)) {
                    found.push((accessor, set, apply, globals, mode, high));
                }
            }
        }
    }
    found.sort();
    found.dedup();
    match found[..] {
        [(accessor, set, apply, globals, mode, high)] => Some(Brightness { accessor, set, apply, globals, mode, high }),
        _ => None,
    }
}

/// The offset of the level `apply` (at `words[start]`) loads for mode 2,
/// high, after loading the mode: through a jump table (`ldrls pc, [pc, rX,
/// lsl #2]`) or a compare with 2 and a branch.
fn high_level(words: &Words, start: usize, mode: u32) -> Option<u32> {
    let end = (start + 24).min(words.len());
    let index = |address: u32| words.index(address);
    let load = (start..end).find(|&i| words.at(i).1 & 0xfff0_0fff == 0xe590_0000 | mode)?;
    let register = (words.at(load).1 >> 12) & 0xf;
    // vldr s0, [rN, #imm*4] at or after `from`, following one branch.
    let level = |from: usize| -> Option<u32> {
        let mut i = from;
        for _ in 0..6 {
            let (at, x) = words.get(i)?;
            if x & 0x0f70_ff00 == 0x0d10_0a00 && x & (1 << 23) != 0 && x >> 28 == 0xe {
                return Some((x & 0xff) * 4);
            }
            if let Some(target) = branch_target(at, x, false) {
                i = index(target)?;
                continue;
            }
            i += 1;
        }
        None
    };
    for i in load + 1..end {
        let x = words.at(i).1;
        // ldrls pc, [pc, rX, lsl #2]: the table follows the next word.
        if x == 0x979f_f100 | register {
            return level(index(words.get(i + 2 + 2)?.1)?);
        }
        // cmp rX, #2, then beq target.
        if x == 0xe350_0002 | register << 16 {
            let (b_at, b) = (i + 1..i + 3)
                .map(|j| words.get(j))
                .collect::<Option<Vec<_>>>()?
                .into_iter()
                .find(|(_, b)| b >> 24 == 0x0a)?;
            let offset = ((b & 0xff_ffff) << 8) as i32 >> 6;
            return level(index(b_at.wrapping_add(8).wrapping_add(offset as u32))?);
        }
    }
    None
}

fn legal_layer_factory(elf: &Elf) -> Result<u32> {
    let mut layers = Vec::new();
    for s in &elf.sections {
        if s.kind == 1 && s.flags & 7 == 3 {
            for r in (0..s.size.saturating_sub(7)).step_by(4) {
                let va = s.address + r as u32;
                if elf.ident(elf.read(va + 4)?).as_deref() == Some("legalinfolayer") && elf.text(elf.read(va)?) {
                    layers.push(elf.read(va)?);
                }
            }
        }
    }
    unique(layers, "Legal Info layer factory")
}

/// Each registered screen's factory, by its name.
pub(super) fn screen_factories(elf: &Elf) -> Result<BTreeMap<String, u32>> {
    let table = registry(elf)?;
    let mut factories = BTreeMap::new();
    for (i, name) in table.names.iter().enumerate() {
        factories.insert(name.clone(), elf.read(table.address + i as u32 * 8 + if table.code_first { 0 } else { 4 })?);
    }
    Ok(factories)
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
            let name = if relative + 4 <= s.size { elf.ident(word(elf.data, s.offset + relative)?) } else { None };
            if let Some(name) = name {
                if run.is_empty() {
                    start = address;
                }
                run.push(name);
                continue;
            }
            if run.len() >= 15 {
                let first = run.iter().position(|n| screens.contains(n)).unwrap_or(run.len());
                let last = run.iter().rposition(|n| screens.contains(n));
                if let Some(last) = last {
                    let names = &run[first..=last];
                    if names.len() >= 15
                        && names.first().is_some_and(|n| n == "temperature")
                        && names.last().is_some_and(|n| n == "temperature")
                        && names.iter().filter(|n| n.as_str() == "account").count() == 1
                        && names.iter().filter(|n| screens.contains(*n)).count() * 4 >= names.len() * 3
                    {
                        candidates.push((start + 4 * first as u32, names.to_vec()));
                    }
                }
            }
            run.clear();
        }
    }
    require(candidates.len() == 1, format!("settings menu expected one result, found {}", candidates.len()))?;
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
fn image_height(arm: &Arm, width: u32) -> Result<u32> {
    let code = arm.code(width, Some(64))?;
    let mut heights = Vec::new();
    for (i, ins) in code.iter().enumerate() {
        if ins.name == "ldr" && ins.text == "r0, [r0, #8]" {
            heights.push(if i > 0 && code[i - 1].text == "ip, sp" { code[i - 1].address } else { ins.address });
        }
    }
    unique(heights, "image height getter")
}

/// The path from nlConfig::GetValue goes to the image loader. Its result is
/// saved in the layer, so a logging or cleanup call cannot supply the binding.
fn image_loader(arm: &Arm, function: u32, get_value: u32, imports: &BTreeSet<u32>) -> Result<u32> {
    let code = arm.code(function, None)?;
    let mut found = Vec::new();
    for (i, ins) in code.iter().enumerate() {
        if ins.name != "bl" || ins.ops[0].imm() != Some(get_value) {
            continue;
        }
        let tail = &code[i + 1..];
        let Some(call) = tail.iter().position(|i| matches!(i.name.as_str(), "bl" | "blx")) else { continue };
        let Some(target) = tail[call].ops[0].imm().filter(|a| !imports.contains(a)) else { continue };
        if tail[call + 1..]
            .iter()
            .take_while(|i| !matches!(i.name.as_str(), "bl" | "blx") && !i.writes.iter().any(|r| r == "r0"))
            .any(|i| {
                i.name == "str"
                    && i.ops[0].reg() == Some("r0")
                    && i.ops[1].mem().is_some_and(|(base, _, indexed)| !indexed && !matches!(base, "pc" | "sp" | "fp"))
            })
        {
            found.push(target);
        }
    }
    unique(found, "configured image loader")
}
pub fn discover(data: &[u8]) -> Result<Discovery> {
    let found = match survey(data)? {
        (found, None) => found,
        (_, Some(objection)) => return device(format!("Native discovery: {objection}")),
    };
    *LAST.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(found.clone());
    Ok(found)
}

/// The last executable discover accepted: a setup's screen build and its
/// theme bindings discover the same nlclient, which takes the unit longest.
static LAST: std::sync::Mutex<Option<Discovery>> = std::sync::Mutex::new(None);

/// discover, for an executable whose SHA-256 the caller already has: the last
/// discovery again when it was of this executable.
pub fn discover_hashed(data: &[u8], sha256: &str) -> Result<Discovery> {
    let last = LAST.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    match last {
        Some(found) if found.stock_sha256 == sha256 => Ok(found),
        _ => discover(data),
    }
}
/// Return the resolved bindings and any missing library calls required by
/// their roles. The report includes the findings even when a role fails.
pub fn survey(data: &[u8]) -> Result<(Discovery, Option<String>)> {
    if super::legacy_abi(data) {
        return legacy::survey(data).map(|found| (found, None));
    }
    survey_with(data, brightness)
}

/// survey, with the brightness manager found by `find_brightness`.
fn survey_with(data: &[u8], find_brightness: fn(&Elf) -> Option<Brightness>) -> Result<(Discovery, Option<String>)> {
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
            *imports.get(name).ok_or_else(|| crate::Error::Device(format!("missing import {name}")))?,
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
        screens.insert(name.as_str(), elf.read(table.address + i as u32 * 8 + if table.code_first { 0 } else { 4 })?);
    }
    let account = *screens.get("account").ok_or_else(|| crate::Error::Device("missing account registration".into()))?;
    let screen =
        *screens.get("legalinfo").ok_or_else(|| crate::Error::Device("missing legalinfo registration".into()))?;
    let layer = legal_layer_factory(&elf)?;
    let arm = Arm::new(&elf)?;
    let lc = arm.calls(layer)?;
    let sc = arm.calls(screen)?;
    require(lc.len() >= 2 && sc.len() >= 3, "factory call graph incomplete")?;
    require(lc[0] == sc[0] && sc[0] == api["native_new"], "factory allocator mismatch")?;
    api.insert("native_layer_ctor".into(), lc[1]);
    let (ls, _) = arm.stores(lc[1], None)?;
    let (mut ss, ssizes) = arm.stores(screen, Some(api["native_new"]))?;
    // The factory sets Legal Info's vtables after the base constructor, or
    // calls Legal Info's own constructor, which calls the base one first.
    if ss.contains_key(&0) {
        api.insert("native_screen_ctor".into(), sc[1]);
    } else {
        let base =
            *arm.calls(sc[1])?.first().ok_or_else(|| crate::Error::Device("missing base screen constructor".into()))?;
        api.insert("native_screen_ctor".into(), base);
        ss = arm.stores(sc[1], None)?.0;
    }
    let (_, lsizes) = arm.stores(layer, Some(api["native_new"]))?;
    // The Legal Info layer and where its nlLayer part starts: 5.9.1-6's has
    // one more string (its page text index at +0xa0, the string at +0xa4).
    let (primary, common_offset) = match lsizes.as_slice() {
        [Some(Value::Number(0x140))] => (0x140, 0xa4),
        [Some(Value::Number(0x15c))] => (0x15c, 0xc0),
        _ => (0, 0),
    };
    require(ssizes == [Some(Value::Number(0xf0))] && primary != 0, "unrecognised allocation sizes")?;
    let (lv, sv) = (number(&ls, 0)?, number(&ss, 0)?);
    for vptr in [lv, sv] {
        require(
            elf.at(vptr).is_some_and(|s| s.flags & 6 == 2 && vptr - s.address >= 12)
                && vptr.checked_add(0x400).is_some(),
            "invalid constructor vtable address",
        )?;
    }
    let common = elf.read(lv - 12)?;
    require(common == common_offset, "unrecognised common base adjustment")?;
    let parts = [number(&ls, common as i32)?, number(&ls, common as i32 + 4)?, number(&ls, common as i32 + 8)?];
    require(number(&ss, 0xec)? == 0, "unrecognised screen constructor stores")?;
    let second = number(&ss, 4)?;
    let third = number(&ss, 8)?;
    require(
        parts.iter().all(|p| p.wrapping_sub(lv) < 0x400 && *p >= lv + 8)
            && [second, third].iter().all(|p| p.wrapping_sub(sv) < 0x400 && *p >= sv + 8),
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
        ("PRIMARY_SIZE".into(), primary),
        ("COMMON_OFFSET".into(), common),
        ("LAYER_VTABLE_SIZE".into(), group_end(&elf, parts[2])? - lv + 12),
        ("SCREEN_VTABLE_SIZE".into(), group_end(&elf, third)? - sv + 8),
        ("LAYER_COMMON_VPTR".into(), parts[0] - lv + 12),
        ("LAYER_SECOND_VPTR".into(), parts[1] - lv + 12),
        ("LAYER_THIRD_VPTR".into(), parts[2] - lv + 12),
        ("SCREEN_SECOND_VPTR".into(), second - sv + 8),
        ("SCREEN_THIRD_VPTR".into(), third - sv + 8),
        ("SCREEN_TITLE_SLOT".into(), title),
    ]);
    api.insert("native_screen_vtable".into(), sv);
    // The screen's composition is the factory's first call into its own code
    // after the constructor; a build may assign an nlString in between.
    let imported_at: BTreeSet<u32> = imports.values().copied().collect();
    let init = *sc[2..]
        .iter()
        .find(|a| !imported_at.contains(a))
        .ok_or_else(|| crate::Error::Device("missing screen composition call".into()))?;
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
    let internal = |calls: Vec<u32>| calls.into_iter().filter(|a| !imported_at.contains(a)).collect::<Vec<_>>();
    let ec = internal(arm.calls(enter)?);
    let mut dc = internal(arm.calls(draw)?);
    let get_value = *imports
        .get("_ZNK8nlConfig8GetValueEPKc")
        .ok_or_else(|| crate::Error::Device("missing image configuration lookup".into()))?;
    let (wrapper, image_helper) = if let [guard, style, other_style, text_draw, scroll] = dc[..] {
        require(style == other_style, "Legal Info text styles disagree")?;
        let [invalidate, wrapper] = ec[..] else { return device("unknown Legal Info text layout calls") };
        // The equipment layer loads a configured image on entry. Its draw
        // uses the same text renderer and calls a helper to position the icon.
        let equipment = primary_vtable(&elf, "19nlWWNEquipmentLayer")?;
        let load = image_loader(&arm, elf.read(equipment + 0x2c)?, get_value, &imported_at)?;
        let paint = internal(arm.calls(elf.read(equipment + 0x28)?)?);
        require([guard, style, text_draw, scroll].iter().all(|a| paint.contains(a)), "equipment renderer mismatch")?;
        let mut icons = Vec::new();
        for callee in paint.into_iter().collect::<BTreeSet<_>>() {
            let calls = internal(arm.calls(callee)?);
            if let [icon_style, width, y, height] = calls[..] {
                if icon_style == style {
                    icons.push((width, y, height));
                }
            }
        }
        let (width, y, height) = unique(icons, "equipment image drawing")?;
        for (name, value) in [
            ("native_config_invalidate", invalidate),
            ("native_config_layout", unique(arm.tails(wrapper)?, "layout implementation")?),
            ("native_image_load", load),
            ("native_style", style),
            ("native_config_y", y),
            ("native_config_draw", text_draw),
            ("native_image_width", width),
            ("native_image_height", height),
            ("native_scroll_draw", scroll),
        ] {
            api.insert(name.into(), value);
        }
        (wrapper, None)
    } else {
        // A build may keep the draw's visibility guard in a wrapper around the
        // drawing itself: the wrapper calls the guard, then the body, whose calls
        // follow the guard's in a build that draws in one function.
        if let [guard, body] = dc[..] {
            let mut calls = vec![guard];
            calls.extend(internal(arm.calls(body)?));
            (draw, dc) = (body, calls);
        }
        let [helper, invalidate] = ec[..] else { return device("unknown Legal Info image layout calls") };
        require(dc.len() == 10, "Legal Info image draw call shape changed")?;
        api.insert("native_config_invalidate".into(), invalidate);
        let wrapper = unique(arm.tails(enter)?, "layout wrapper")?;
        api.insert("native_config_layout".into(), unique(arm.tails(wrapper)?, "layout implementation")?);
        let load = image_loader(&arm, helper, get_value, &imported_at)?;
        require(dc[1] == dc[5] && dc[1] == dc[7], "renderer style calls disagree")?;
        for (name, value) in [
            ("native_image_load", load),
            ("native_style", dc[1]),
            ("native_config_y", dc[4]),
            ("native_config_draw", dc[6]),
            ("native_image_width", dc[8]),
            ("native_scroll_draw", dc[9]),
        ] {
            api.insert(name.into(), value);
        }
        api.insert("native_image_height".into(), image_height(&arm, dc[8])?);
        (wrapper, Some(helper))
    };
    let event = elf.read(lv + 0x14)?;
    let legacy = arm.code(event, None)?[0].name != "b";
    let handler = if legacy { event } else { unique(arm.tails(event)?, "layer event handler")? };
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
        ("RENAME_SYSCALL".into(), (!imports.contains_key("rename")) as u32),
    ]);
    let mut anchors = BTreeMap::from([
        ("account_factory".into(), account),
        ("layer_factory".into(), layer),
        ("screen_factory".into(), screen),
        ("screen_composition".into(), init),
        ("layer_enter".into(), enter),
        ("layer_draw".into(), draw),
        ("layer_event".into(), event),
        ("layout_wrapper".into(), wrapper),
        ("screen_title".into(), elf.read(sv + title)?),
        ("generic_event".into(), generic),
    ]);
    if let Some(helper) = image_helper {
        anchors.insert("image_helper".into(), helper);
    }
    activity(&elf, &arm, &imports, false, &mut api, &mut layout)?;
    for (name, symbol) in [
        (
            "native_send_timeout",
            "_ZN12nlEventUtils11SendTimeoutERP21nlEventTimeoutRequestjjP12nlEventQueueP15nlIEventHandlerP7nlTimer",
        ),
        ("native_cancel_timeout", "_ZN21nlEventTimeoutRequest6CancelEv"),
        ("native_release", "_ZNK10nlRefCount7ReleaseEv"),
        ("native_timeout_id", "_ZNK15nlEventTimedOut5GetIDEv"),
    ] {
        api.insert(
            name.into(),
            *imports
                .get(symbol)
                .ok_or_else(|| crate::Error::Device(format!("missing status-refresh import {symbol}")))?,
        );
    }
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
        target.iter().any(|i| i.name == "vstr" && i.text == "s0, [r0, #0x10]")
            && target.iter().any(|i| i.name == "vldr" && i.text.contains("[r0, #0x24]"))
            && target.iter().any(|i| i.name == "vcvt.s32.f32"),
        "unknown native scroll target ABI",
    )?;
    api.insert("native_scroll_target".into(), select[1]);
    layout.insert("GLOBALS_TIMER_OFFSET".into(), timer(&elf, &arm, &imports)?);
    let (sleep_default, sleep_active) = sleep_timeout(&elf).unwrap_or((0, 0));
    layout.insert("SLEEP_DEFAULT_OFFSET".into(), sleep_default);
    layout.insert("SLEEP_ACTIVE_OFFSET".into(), sleep_active);
    // Without the brightness manager's shape the screen code never calls
    // its natives (BRIGHTNESS_MODE_OFFSET 0); they are bound to the screen
    // initializer so the payload still links, and are no natives of the
    // build's own: no fingerprint, no evidence.
    let bright = find_brightness(&elf);
    let aliases: &[&str] = if bright.is_none() { &["native_set_brightness", "native_apply_brightness"] } else { &[] };
    for (key, value) in [
        ("BRIGHTNESS_GLOBALS_OFFSET", bright.as_ref().map_or(0, |b| b.globals)),
        ("BRIGHTNESS_MODE_OFFSET", bright.as_ref().map_or(0, |b| b.mode)),
        ("BRIGHTNESS_HIGH_OFFSET", bright.as_ref().map_or(0, |b| b.high)),
    ] {
        layout.insert(key.into(), value);
    }
    let unused = api["native_screen_init"];
    api.insert("native_set_brightness".into(), bright.as_ref().map_or(unused, |b| b.set));
    api.insert("native_apply_brightness".into(), bright.as_ref().map_or(unused, |b| b.apply));
    downlink(&elf, &arm, &imports, &mut api, &mut layout, &mut anchors)?;
    let framebuffer = if EGL_IMPORTS.iter().all(|(key, _)| api.contains_key(*key)) {
        anchors.insert("swap_call_slot".into(), swap_call(&elf, api["native_egl_swap"])?);
        None
    } else {
        framebuffer(&elf, &arm, &imports).ok()
    };
    let dial = dial(&elf, &imports).ok();
    require(
        layout["LAYER_VTABLE_SIZE"].max(layout["SCREEN_VTABLE_SIZE"]) <= 0x400,
        "vtable larger than the payload holds",
    )?;
    roles(&arm, &api, &anchors)?;
    // Report fingerprints for inspection. Import evidence also validates native roles.
    let by_address: BTreeMap<u32, String> = imports.iter().map(|(k, v)| (*v, k.clone())).collect();
    let mut fingerprints = BTreeMap::new();
    let mut evidence = BTreeMap::new();
    for (name, &address) in api.iter().chain(&anchors) {
        if by_address.contains_key(&address)
            || name == "native_screen_vtable"
            || name.ends_with("_slot")
            || aliases.contains(&name.as_str())
        {
            continue;
        }
        fingerprints.insert(name.clone(), arm.fingerprint(address)?);
        if !matches!(name.as_str(), "view_handler" | "view_dispatch") {
            evidence.insert(name.clone(), arm.imports_called(address, &by_address)?.into_iter().collect::<Vec<_>>());
        }
    }
    let objection = missing_role_imports(&evidence);
    let found = Discovery {
        factory: account,
        native_api: api,
        layout,
        anchors,
        fingerprints,
        evidence,
        stock_sha256: sha256_hex(data),
        framebuffer,
        dial,
    };
    Ok((found, objection))
}
/// Independent anchors for the roles that call into shared libraries. Other
/// imports may vary with inlining, logging, exception cleanup and refcounts.
fn missing_role_imports(evidence: &BTreeMap<String, Vec<String>>) -> Option<String> {
    let mut missing = Vec::new();
    for (name, required) in [
        ("account_factory", &["_Znwj"][..]),
        ("layer_factory", &["_Znwj"]),
        ("screen_factory", &["_Znwj"]),
        ("screen_composition", &["__dynamic_cast"]),
        ("native_screen_ctor", &["_ZN7nlArrayC2Ev", "_ZN8nlStringC1Ev"]),
        ("native_layer_lookup", &["PL_HashTableLookupConst"]),
        ("native_attach", &["_Znwj"]),
        ("native_image_load", &["_ZN8nlStringC1EPKc", "_ZN8nlStringD1Ev"]),
        ("generic_event", &["_ZNK7nlEvent7GetTypeEv"]),
        ("native_layer_event", &["_ZNK7nlEvent7GetTypeEv", "_ZNK7nlEvent8GetValueEv"]),
        ("screen_title", &["_ZN8nlString18GetLocalizedStringEPKcS1_", "_ZN8nlStringaSERKS_"]),
        ("native_rebuild_title", &["_ZN13nlStringArray5ClearEv", "_ZN8nlString6AppendEPKc"]),
        ("native_device_commit", &["_ZN10nlSettings5WriteEv"]),
        ("native_shared_commit", &["_ZN8nlStringC1ERKS_", "_ZN8nlString5ClearEv"]),
        ("native_schedule_commit", &["_ZNK8nlString6EqualsEPKc6nlBOOL", "_ZNK8nlString7IsEmptyEv"]),
    ] {
        for &symbol in required {
            if !evidence.get(name).is_some_and(|calls| calls.iter().any(|s| s == symbol)) {
                missing.push(format!("{name} requires {symbol}"));
            }
        }
    }
    // These accessors only read fields or dispatch through the object's vtable.
    for name in [
        "native_activity_client",
        "native_image_width",
        "native_image_height",
        "native_menu_hidden",
        "native_menu_scale",
        "native_style",
    ] {
        if evidence.get(name).is_some_and(|calls| !calls.is_empty()) {
            missing.push(format!("{name} calls an unexpected library function"));
        }
    }
    (!missing.is_empty()).then(|| missing.join(", "))
}

/// What the screen asks of natives found by position, checked on the
/// natives' own instructions: the loads, stores and calls the screen's use
/// of each one assumes, which a recompile keeps and a wrong slot does not.
fn roles(arm: &Arm, api: &BTreeMap<String, u32>, anchors: &BTreeMap<String, u32>) -> Result<()> {
    let body = |name: &str| arm.body(api.get(name).or_else(|| anchors.get(name)).copied().unwrap());
    let loads = |code: &[Ins], disp: i32| {
        code.iter()
            .filter(|i| {
                i.name == "ldr"
                    && i.ops.get(1).and_then(Op::mem).is_some_and(|(b, d, _)| b != "sp" && b != "pc" && d == disp)
            })
            .count()
    };
    let stores = |code: &[Ins], disp: i32| {
        code.iter()
            .any(|i| i.name == "str" && i.ops.last().and_then(Op::mem).is_some_and(|(b, d, _)| b != "sp" && d == disp))
    };
    let calls = |code: &[Ins]| code.iter().filter(|i| i.name == "bl").count();
    let indirect = |code: &[Ins]| {
        code.iter().any(|i| matches!(i.name.as_str(), "bx" | "blx") && i.ops[0].reg().is_some_and(|r| r != "lr"))
    };
    // A getter: one load through a register, no call, nothing else of note.
    let getter = |code: &[Ins]| -> Option<i32> {
        let fields: Vec<_> = code
            .iter()
            .filter(|i| i.name == "ldr")
            .filter_map(|i| i.ops.get(1).and_then(Op::mem))
            .filter(|(b, _, _)| *b != "sp" && *b != "pc")
            .map(|(_, d, _)| d)
            .collect();
        (fields.len() == 1 && calls(code) == 0 && !indirect(code)).then(|| fields[0])
    };
    for (name, check) in [
        ("native_image_width", getter(&body("native_image_width")?) == Some(4)),
        ("native_image_height", getter(&body("native_image_height")?) == Some(8)),
        ("native_style", getter(&body("native_style")?).is_some()),
        ("native_activity_client", getter(&body("native_activity_client")?).is_some()),
        ("native_menu_scale", {
            let code = body("native_menu_scale")?;
            loads(&code, i32::MAX) == 0
                && calls(&code) == 0
                && code.last().is_some_and(|i| i.name == "b")
                && code.iter().filter(|i| i.name == "ldr").count() == 1
        }),
        ("native_menu_hidden", {
            let code = body("native_menu_hidden")?;
            loads(&code, 0x28) == 1 && indirect(&code)
        }),
        ("native_screen_event", {
            let code = body("native_screen_event")?;
            loads(&code, 0x20) == 1 && loads(&code, 0) >= 2 && indirect(&code)
        }),
        ("native_screen_enter", {
            let code = body("native_screen_enter")?;
            loads(&code, 0x20) == 1 && loads(&code, 0xc) == 1 && indirect(&code)
        }),
        ("native_layer_init", {
            let code = body("native_layer_init")?;
            stores(&code, 0x8c) && calls(&code) == 1 && indirect(&code)
        }),
        ("native_config_invalidate", {
            let code = body("native_config_invalidate")?;
            stores(&code, 0x18)
                && calls(&code) == 2
                && code.iter().any(|i| i.name == "cmn" && i.ops[1].imm() == Some(1))
        }),
        ("layout_wrapper", {
            let code = body("layout_wrapper")?;
            loads(&code, 0xc) == 1 && code.last().is_some_and(|i| i.name == "b")
        }),
        ("native_layer_wheel", {
            let code = body("native_layer_wheel")?;
            // The ring's 16.16 delta, negated; a 480 px screen scales it too.
            calls(&code) == 2
                && code.iter().any(|i| i.name == "vcvt.f32.s32")
                && code.iter().any(|i| i.name == "vneg.f32")
        }),
        ("native_screen_init", {
            let code = body("native_screen_init")?;
            stores(&code, 0xe4) && loads(&code, 0xe8) == 1 && calls(&code) == 5
        }),
        ("native_config_draw", {
            // The renderer's body is its own function the native calls, or
            // inlined: its measure and draw calls, and the stack check.
            let code = body("native_config_draw")?;
            [1, 4].contains(&calls(&code)) && [0x28, 0x2c, 0x34, 0x38, 0x3c].iter().all(|&d| loads(&code, d) == 1)
        }),
        ("view_dispatch", {
            let code = body("view_dispatch")?;
            loads(&code, 0) >= 2 && indirect(&code)
        }),
    ] {
        require(check, format!("{name} does not have the native's shape"))?;
    }
    Ok(())
}
/// The first virtual slot of the primary vtable of the class whose RTTI
/// name (as mangled, length first) is `name`.
pub(super) fn primary_vtable(elf: &Elf, name: &str) -> Result<u32> {
    let refs = |value: u32| -> Result<Vec<u32>> {
        let mut result = Vec::new();
        for s in elf.sections.iter().filter(|s| s.address != 0 && s.kind != 8 && s.flags & 4 == 0) {
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
        elf.data.windows(needle.len()).enumerate().filter(|(_, b)| *b == needle.as_bytes()).map(|(off, _)| off),
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
/// A PIE's vtable slot holds an address the loader biases: one the patch
/// rewrites must have its R_ARM_RELATIVE relocation.
pub(super) fn relocated(elf: &Elf, slot: u32, what: &str) -> Result<()> {
    if half(elf.data, 16)? != 3 {
        return Ok(());
    }
    let relocations =
        elf.sections.iter().filter(|s| s.kind == 9).flat_map(|s| (s.offset..s.offset + s.size).step_by(8));
    let mut relative = false;
    for off in relocations {
        if word(elf.data, off)? == slot {
            require(word(elf.data, off + 4)? == 0x17, format!("{what} requires R_ARM_RELATIVE"))?;
            relative = true;
        }
    }
    require(relative, format!("{what} has no load-bias relocation"))
}
/// The thermostat's dial (Dial).
fn dial(elf: &Elf, imports: &BTreeMap<String, u32>) -> Result<Dial> {
    let slot = primary_vtable(elf, "17nlTempSliderLayer")? + 0x10;
    let event_slot = primary_vtable(elf, "12nlTempScreen")?;
    let (draw, event) = (elf.read(slot)?, elf.read(event_slot)?);
    require(elf.text(draw) && elf.text(event), "dial methods are not code")?;
    relocated(elf, slot, "dial vtable")?;
    relocated(elf, event_slot, "dial vtable")?;
    let value = *imports
        .get("_ZNK7nlEvent8GetValueEv")
        .ok_or_else(|| crate::Error::Device("missing event value import".into()))?;
    let figures_slot = primary_vtable(elf, "11nlTempLayer")? + 0x10;
    let figures = elf.read(figures_slot)?;
    require(elf.text(figures), "figures draw is not code")?;
    relocated(elf, figures_slot, "figures vtable")?;
    Ok(Dial { slot, draw, event_slot, event, value, figures_slot, figures })
}
/// The gen 2's nlARMFramebuffer (Framebuffer). Its init maps fb0 with
/// `mmap(0, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0)` and stores the
/// address; its flip pans with FBIOPAN_DISPLAY. In a PIE the vtable slot
/// must carry the load bias's relocation, as every slot the patch writes.
fn framebuffer(elf: &Elf, arm: &Arm, imports: &BTreeMap<String, u32>) -> Result<Framebuffer> {
    let (mmap, ioctl) = match (imports.get("mmap"), imports.get("ioctl")) {
        (Some(&m), Some(&i)) => (m, i),
        _ => return device("framebuffer hand-over needs mmap and ioctl"),
    };
    let vtable = primary_vtable(elf, "16nlARMFramebuffer")?;
    let (init, slot) = (elf.read(vtable)?, vtable + 8);
    relocated(elf, slot, "framebuffer vtable")?;
    let flip = elf.read(slot)?;
    let target = |i: &Ins| i.ops.first().and_then(Op::imm);
    let code = arm.body(flip)?;
    let pan = code.iter().position(|i| i.name == "movw" && i.text.ends_with("#0x4606"));
    require(
        pan.is_some_and(|p| {
            code[p..].iter().any(|i| matches!(i.name.as_str(), "bl" | "b") && target(i) == Some(ioctl))
        }),
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
    // The fifth argument, fb0's descriptor, goes on the stack from a field:
    // stored alone, or first of a store multiple with the sixth.
    let stacked = before
        .iter()
        .rev()
        .find_map(|i| match i.name.as_str() {
            "str" if i.ops.get(1).and_then(Op::mem) == Some(("sp", 0, false)) => i.ops.first()?.reg(),
            "stm" if i.ops.first()?.reg() == Some("sp") => i.ops.get(1)?.reg(),
            _ => None,
        })
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
                    .ok_or_else(|| crate::Error::Device("missing view dispatcher".into()))? as u32
            } else {
                function
            };
            anchors.insert("view_handler".into(), function);
            anchors.insert("view_dispatch".into(), dispatch);
            continue;
        }
        relocated(elf, slot, "bucket vtable")?;
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
        *imports.get("_ZdlPv").ok_or_else(|| crate::Error::Device("missing operator delete import".into()))?,
    );
    api.insert(
        "native_time".into(),
        *imports.get("time").ok_or_else(|| crate::Error::Device("missing time import".into()))?,
    );
    Ok(())
}

fn activity(
    elf: &Elf,
    arm: &Arm,
    imports: &BTreeMap<String, u32>,
    compact: bool,
    api: &mut BTreeMap<String, u32>,
    layout: &mut BTreeMap<String, u32>,
) -> Result<()> {
    for (name, symbol) in [
        ("native_activity_ctor", "_ZN7nlEventC1Ev"),
        ("native_activity_init", "_ZN7nlEvent4InitEji"),
        ("native_enqueue", "_ZN12nlEventQueue7EnqueueEPK7nlEventP15nlIEventHandler"),
    ] {
        api.insert(
            name.into(),
            *imports.get(symbol).ok_or_else(|| crate::Error::Device(format!("missing activity import {symbol}")))?,
        );
    }
    let globals_field = if compact { ", #0x68]" } else { ", #0x70]" };
    let mut candidates = Vec::new();
    for s in elf.sections.iter().filter(|s| s.kind == 1 && s.flags & 4 != 0) {
        for off in (s.offset + 64..(s.offset + s.size).saturating_sub(32)).step_by(4) {
            let w = word(elf.data, off)?;
            if w & 0xfffff000 != 0xe3a01000 || immediate(w) != 0x40000005 {
                continue;
            }
            let address = s.address + (off - s.offset) as u32;
            let c = arm.decode(address - 64, bytes(elf.data, off - 64, 96)?)?;
            if !c.iter().any(|i| i.name == "bl" && i.ops[0].imm() == Some(api["native_activity_init"])) {
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
            // The client: its globals at +0x70 are loaded for the marking call,
            // from whichever register the build keeps the client in.
            let Some(client) = c[j - 3..j - 1].iter().find_map(|i| {
                (i.name == "ldr")
                    .then(|| i.text.strip_prefix("r0, [")?.strip_suffix(globals_field))
                    .flatten()
                    .filter(|r| !matches!(*r, "sp" | "pc"))
            }) else {
                continue;
            };
            let base = format!("r0, [{client}, #");
            let dest: Vec<_> = c
                .iter()
                .filter(|i| i.address > address && i.name == "ldr" && i.text.starts_with(&base))
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
    let getter = activity_client(elf, arm, if compact { 0x68 } else { 0x70 }, queue)?;
    api.insert("native_activity_client".into(), getter);
    Ok(())
}
/// The client is passed to the globals initializer as its third stack
/// argument. Follow that argument from the initializer's prologue to its field.
fn client_argument_field(arm: &Arm, initializer: u32) -> Result<u32> {
    let mut stack = BTreeMap::from([("sp", 0i32)]);
    let mut objects = BTreeSet::from(["r0"]);
    let mut clients = BTreeSet::new();
    let mut fields = Vec::new();
    let code = arm.code(initializer, Some(128))?;
    for ins in &code {
        let ops = &ins.ops;
        if matches!(ins.name.as_str(), "bl" | "blx" | "b") {
            break;
        }
        if ins.name == "str" {
            if let (Some(source), Some((base, offset, false))) = (ops[0].reg(), ops[1].mem()) {
                if objects.contains(base) && clients.contains(source) && offset > 0 {
                    fields.push(offset as u32);
                }
            }
        }
        let mut next_stack = None;
        let mut next_object = false;
        let mut next_client = false;
        let dest = ops.first().and_then(Op::reg);
        match ins.name.as_str() {
            "push" | "vpush" => {
                let width = if ins.name == "vpush" { 8 } else { 4 };
                if let Some(&offset) = stack.get("sp") {
                    stack.insert("sp", offset - width * ops.len() as i32);
                }
                continue;
            }
            "mov" => {
                if let Some(source) = ops.get(1).and_then(Op::reg) {
                    next_stack = stack.get(source).copied();
                    next_object = objects.contains(source);
                    next_client = clients.contains(source);
                }
            }
            "add" | "sub" => {
                if let (Some(source), Some(delta)) = (ops[1].reg(), ops[2].imm()) {
                    next_stack =
                        stack.get(source).map(|n| if ins.name == "add" { n + delta as i32 } else { n - delta as i32 });
                }
            }
            "ldr" => {
                if let Some((base, offset, false)) = ops[1].mem() {
                    next_client = stack.get(base).is_some_and(|n| n + offset == 8);
                }
            }
            "ldm" => {
                let base = dest.and_then(|r| stack.get(r)).copied();
                for (index, reg) in ops[1..].iter().filter_map(Op::reg).enumerate() {
                    stack.remove(reg);
                    objects.remove(reg);
                    clients.remove(reg);
                    if base.is_some_and(|offset| offset + index as i32 * 4 == 8) {
                        clients.insert(reg);
                    }
                }
                continue;
            }
            _ => {}
        }
        for register in &ins.writes {
            stack.remove(register.as_str());
            objects.remove(register.as_str());
            clients.remove(register.as_str());
        }
        if let Some(dest) = dest {
            if let Some(offset) = next_stack {
                stack.insert(dest, offset);
            }
            if next_object {
                objects.insert(dest);
            }
            if next_client {
                clients.insert(dest);
            }
        }
    }
    unique(fields, "globals initializer client argument")
}

/// Find the globals constructor by its RTTI vtable, then its caller which
/// stores the new globals on the client and passes the client and queue to
/// the initializer. The initializer identifies the client pointer getter.
fn activity_client(elf: &Elf, arm: &Arm, globals_field: i32, queue: u32) -> Result<u32> {
    let vtable = primary_vtable(elf, "15nlClientGlobals")?;
    let words = Words::new(elf);
    let constructors: BTreeSet<_> = [vtable, vtable - 8]
        .into_iter()
        .flat_map(|address| references(elf, &words, address))
        .filter_map(|index| arm.enclosing(words.at(index).0))
        .collect();
    let mut fields = Vec::new();
    for (site, word) in words.iter() {
        if !branch_target(site, word, true).is_some_and(|target| constructors.contains(&target)) {
            continue;
        }
        let code = arm.code(site + 4, Some(80))?;
        let Some(call) = code.iter().position(|i| i.name == "bl") else { continue };
        let args = &code[..call];
        let Some((global, client)) = args.iter().find_map(|i| {
            if i.name != "str" {
                return None;
            }
            let (base, offset, false) = i.ops[1].mem()? else { return None };
            (offset == globals_field).then_some((i.ops[0].reg()?, base))
        }) else {
            continue;
        };
        if !args.iter().any(|i| i.name == "mov" && i.ops[0].reg() == Some("r0") && i.ops[1].reg() == Some(global))
            || !args.iter().any(|i| i.name == "ldr" && i.ops[1].mem() == Some((client, queue as i32, false)))
            || !args.iter().any(|i| {
                i.name == "str"
                    && i.ops[1].mem() == Some(("sp", 8, false))
                    && (globals_field == 0x68 || i.ops[0].reg() == Some(client))
            })
        {
            continue;
        }
        if let Ok(field) = client_argument_field(arm, code[call].ops[0].imm().unwrap()) {
            fields.push(field);
        }
    }
    let field = unique(fields, "globals client pointer field")?;
    let mut getters = Vec::new();
    for slot in (vtable..).step_by(4) {
        let Ok(address) = elf.read(slot) else { break };
        if !elf.text(address) {
            break;
        }
        let Ok(code) = arm.body(address) else { continue };
        if scalar_field(&code, false) == Some(field) {
            getters.push(address);
        }
    }
    unique(getters, "client globals accessor")
}
fn timer(elf: &Elf, arm: &Arm, imports: &BTreeMap<String, u32>) -> Result<u32> {
    let ctor =
        *imports.get("_ZN7nlTimerC1Ev").ok_or_else(|| crate::Error::Device("missing timer constructor".into()))?;
    let init = *imports
        .get("_ZN7nlTimer4InitEP12nlEventQueue")
        .ok_or_else(|| crate::Error::Device("missing timer init".into()))?;
    let mut offsets = Vec::new();
    for s in elf.sections.iter().filter(|s| s.kind == 1 && s.flags & 4 != 0) {
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
            let Some(index) = code.iter().position(|i| i.name == "bl" && i.ops[0].imm() == Some(init)) else {
                continue;
            };
            let code = &code[..index];
            if code
                .iter()
                .any(|i| !matches!(i.name.as_str(), "ldr" | "str" | "mov" | "cmp" | "beq") && i.word != 0x12811004)
            {
                continue;
            }
            let saves: Vec<_> = code.iter().filter(|i| i.name == "str" && i.ops[1].mem().is_some()).collect();
            let loads: Vec<_> = code.iter().filter(|i| i.name == "ldr" && i.ops[0].reg() == Some("r1")).collect();
            if saves.len() != 1 || loads.len() != 1 {
                continue;
            }
            let (save, load) = (saves[0], loads[0]);
            let reg = save.ops[0].reg().unwrap();
            if !code.iter().any(|i| i.name == "mov" && i.text == format!("r0, {reg}")) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A private nlclient sample, when this checkout has one (see
    /// tests/nlclient_firmware.rs).
    fn sample(name: &str) -> Option<Vec<u8>> {
        let dir = std::env::var_os("NLCLIENT_FIRMWARE_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("UNFR0ZEN_ROOT")).join(".work/firmware"));
        std::fs::read(dir.join(name)).ok()
    }

    #[test]
    fn a_build_without_the_brightness_managers_shape_is_still_set_up() {
        let Some(data) = sample("nlclient-gen3-2019") else {
            eprintln!("No private nlclient sample");
            return;
        };
        let (found, objection) = survey_with(&data, |_| None).unwrap();
        assert_eq!(objection, None);
        assert_eq!(found.layout["BRIGHTNESS_MODE_OFFSET"], 0);
        assert_eq!(found.native_api["native_set_brightness"], found.native_api["native_screen_init"]);
        for name in ["native_set_brightness", "native_apply_brightness"] {
            assert!(!found.evidence.contains_key(name) && !found.fingerprints.contains_key(name), "{name}");
        }
    }

    #[test]
    fn onyx_discovers_brightness_despite_multiple_log_references() {
        let Some(data) = sample("nlclient-genE-6.5-14") else {
            eprintln!("No private Onyx nlclient sample");
            return;
        };
        let elf = Elf::parse(&data).unwrap();
        let found = brightness(&elf).expect("Onyx's brightness manager");
        assert_eq!((found.globals, found.mode, found.high), (0x4b4, 8, 28));
        assert!(elf.text(found.set) && elf.text(found.apply));
        assert_ne!(found.set, found.apply);
        let accessor = Arm::new(&elf).unwrap().body(found.accessor).unwrap();
        assert!(accessor.iter().any(|i| i.word == 0xe5900000 | found.globals));
    }
}
