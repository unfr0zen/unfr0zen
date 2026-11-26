//! The gen 1's and gen 2's settings screens in the gen 3's dark palette:
//! black backgrounds, white text, the gen 3's blue selection and grey
//! unselected rows. The colours are the ones the gen 3's nlclient sets in the
//! same functions.
use super::{
    arm::{Arm, Ins, Op},
    discovery,
    elf::{word, Elf},
};
use crate::{device, Result};
use std::collections::{BTreeMap, BTreeSet};

const R0: u32 = 0;
const R1: u32 = 1;
const R2: u32 = 2;
const R3: u32 = 3;
const R12: u32 = 12;
const NOP: u32 = 0xe1a00000; // mov r0, r0
const WHITE_R0: u32 = 0xe3e004ff; // mvn r0, #0xff000000
const WHITE_R1: u32 = 0xe3e01000; // mvn r1, #0
const BLACK_R1: u32 = 0xe3a014ff; // mov r1, #0xff000000
const BLACK_R12: u32 = 0xe3a0c4ff; // mov r12, #0xff000000

fn movw(rd: u32, value: u32) -> u32 {
    0xe3000000 | (value >> 12) << 16 | rd << 12 | value & 0xfff
}
fn movt(rd: u32, value: u32) -> u32 {
    0xe3400000 | (value >> 12) << 16 | rd << 12 | value & 0xfff
}
fn store(rd: u32, slot: u32) -> u32 {
    0xe5840000 | rd << 12 | slot
}

/// A colour as the `movw`/`movt` pair at `low` and `high`.
fn colour(low: u32, high: u32, rd: u32, argb: u32) -> [(u32, u32); 2] {
    [(low, movw(rd, argb & 0xffff)), (high, movt(rd, argb >> 16))]
}

/// The layer palette's slots (layer + offset) by role, as the gen 3 sets them.
/// The base palette setter writes their alpha bytes separately, so only RGB counts.
fn palette() -> Vec<u32> {
    let mut code = vec![
        0xe1a05001, // mov r5, r1 (the fade, read after the alpha stores)
        WHITE_R0,
        store(R0, 0x28),
        store(R0, 0x34),
        store(R0, 0x40), // text, pill text, field text
        movw(R3, 0xc4c4),
        movt(R3, 0xffc4),
        store(R3, 0x2c), // secondary text
        movw(R3, 0x9999),
        movt(R3, 0xff99),
        store(R3, 0x30),
        movw(R3, 0x6666),
        movt(R3, 0xff66),
        store(R3, 0x38),
        store(R3, 0x54), // unselected, field outline
        movw(R3, 0xa3d9),
        movt(R3, 0xff00),
        store(R3, 0x3c),
        store(R3, 0x50),
        store(R3, 0x5c), // selection, accent
        movw(R3, 0x4157),
        movt(R3, 0xff00),
        store(R3, 0x44), // field being edited
        BLACK_R12,
        store(R12, 0x48),
        store(R12, 0x4c),
        store(R12, 0x58), // field fills, dialog
        movw(R3, 0xe0e0),
        movt(R3, 0xffe0),
        store(R3, 0x60),
    ];
    code.resize(31, NOP);
    code
}

/// The same palette from the nlLayer constructor, which also clears its other
/// fields here and returns `this` in r0; r1 is 0 and r2 is 1 on entry.
fn constructor() -> Vec<u32> {
    vec![
        store(R2, 0x68),
        store(R1, 0x14),
        store(R1, 0x10),
        store(R1, 0x18),
        store(R1, 0x64),
        store(R1, 0x1c),
        store(R1, 0x6c),
        store(R1, 0x24),
        BLACK_R12,
        store(R12, 0x48),
        store(R12, 0x58),
        WHITE_R1,
        store(R1, 0x28),
        store(R1, 0x34),
        store(R1, 0x40),
        movw(R3, 0xc4c4),
        movt(R3, 0xffc4),
        store(R3, 0x2c),
        movw(R3, 0x9999),
        movt(R3, 0xff99),
        store(R3, 0x30),
        movw(R3, 0x6666),
        movt(R3, 0xff66),
        store(R3, 0x38),
        store(R3, 0x54),
        movw(R3, 0xa3d9),
        movt(R3, 0xff00),
        store(R3, 0x3c),
        store(R3, 0x50),
        movw(R3, 0x4157),
        movt(R3, 0xff00),
        store(R3, 0x44),
    ]
}

const LIGHT: u32 = 0xffeff1e7;
const BLUE: u32 = 0xff1a6380;
const GREY: u32 = 0xffb8b8b8;
const SECONDARY: u32 = 0xff505050;
const BLACK: u32 = 0xff000000;
const WHITE: u32 = 0xffffffff;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Colour {
    low: u32,
    high: u32,
    reg: u32,
    value: u32,
}

impl Colour {
    fn replace(self, value: u32) -> [(u32, u32); 2] {
        colour(self.low, self.high, self.reg, value)
    }
}

/// Constant colour construction through mov/movw and movt, even when the
/// compiler schedules independent instructions between the halves.
fn colours(code: &[Ins]) -> Vec<Colour> {
    let mut pending = BTreeMap::new();
    let mut found = Vec::new();
    for ins in code {
        let register = ins.ops.first().and_then(Op::reg);
        let immediate = ins.ops.get(1).and_then(Op::imm);
        if ins.name == "movt" {
            if let (Some(reg), Some(high)) = (register, immediate) {
                if let Some((low, value)) = pending.get(reg).copied() {
                    found.push(Colour {
                        low,
                        high: ins.address,
                        reg: (ins.word >> 12) & 15,
                        value: value | high << 16,
                    });
                }
            }
        }
        for reg in &ins.writes {
            pending.remove(reg.as_str());
        }
        if ins.word & 0x0e000000 == 0x0a000000 || ins.name == "blx" {
            pending.clear();
        } else if matches!(ins.name.as_str(), "mov" | "movw") {
            if let (Some(reg), Some(value)) = (register, immediate.filter(|&v| v <= 0xffff)) {
                pending.insert(reg, (ins.address, value));
            }
        }
    }
    found.sort_unstable();
    found
}

fn reg_name(reg: u32) -> &'static str {
    ["r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "sb", "sl", "fp", "ip", "sp", "lr", "pc"][reg as usize]
}

/// The uses of a colour before its register is overwritten or control flow
/// leaves the local block. The first call itself is included.
fn uses(code: &[Ins], after: u32, reg: u32) -> impl Iterator<Item = &Ins> {
    let register = reg_name(reg);
    let mut ended = false;
    code.iter().filter(move |ins| {
        if ins.address <= after || ended {
            return false;
        }
        if ins.name == "bl" || ins.name == "b" {
            ended = true;
            return true;
        }
        ended = ins.word & 0x0e000000 == 0x0a000000
            || ins.writes.iter().any(|r| r == register)
            || matches!(ins.name.as_str(), "blx" | "bx");
        !ended
    })
}

fn passed_to(code: &[Ins], pair: Colour) -> Option<u32> {
    uses(code, pair.high, pair.reg).find(|i| matches!(i.name.as_str(), "bl" | "b"))?.ops[0].imm()
}

fn stored_at(code: &[Ins], after: u32, reg: u32) -> Option<(&str, i32)> {
    let store = uses(code, after, reg).find(|i| i.name == "str" && i.ops[0].reg() == Some(reg_name(reg)))?;
    let (base, offset, false) = store.ops[1].mem()? else { return None };
    Some((base, offset))
}

fn next_call(code: &[Ins], after: u32) -> Option<u32> {
    code.iter().find(|i| i.address > after && i.name == "bl")?.ops[0].imm()
}

fn is_text(elf: &Elf, address: u32) -> bool {
    elf.at(address).is_some_and(|s| s.name == ".text") && address.is_multiple_of(4)
}

fn methods(elf: &Elf, class: &str) -> Result<BTreeSet<u32>> {
    let vtable = discovery::primary_vtable(elf, class)?;
    let mut result = BTreeSet::new();
    for slot in (vtable..).step_by(4) {
        let Ok(address) = elf.read(slot) else { break };
        if !elf.text(address) {
            break;
        }
        if is_text(elf, address) {
            result.insert(address);
        }
    }
    Ok(result)
}

/// Calls and tail calls within a method's actual ARM body.
fn callees(arm: &Arm, address: u32) -> Result<BTreeSet<u32>> {
    let code = arm.full_code(address)?;
    let end = code.last().unwrap().address;
    Ok(code
        .iter()
        .filter_map(|i| {
            let target = i.ops.first()?.imm()?;
            (is_text(arm.elf, target) && (i.name == "bl" || (i.name == "b" && !(address..=end).contains(&target))))
                .then_some(target)
        })
        .collect())
}

/// A bounded class call graph for palette helpers factored out of methods.
fn reachable(arm: &Arm, roots: BTreeSet<u32>, depth: usize) -> BTreeSet<u32> {
    let mut found = roots.clone();
    let mut frontier = roots;
    for _ in 0..depth {
        let mut next = BTreeSet::new();
        for function in frontier {
            if let Ok(calls) = callees(arm, function) {
                next.extend(calls.into_iter().filter(|address| found.insert(*address)));
            }
        }
        frontier = next;
    }
    found
}

fn resolve<T: Ord>(
    arm: &Arm,
    functions: impl IntoIterator<Item = u32>,
    role: &str,
    mut identify: impl FnMut(u32, &[Ins]) -> Vec<T>,
) -> Result<T> {
    let mut matches = Vec::new();
    for function in functions {
        if let Ok(code) = arm.body(function) {
            matches.extend(identify(function, &code));
        }
    }
    discovery::unique(matches, &format!("dark theme {role}"))
}

/// A native colour setter writes its second argument into the object's
/// colour field and returns without other operations.
fn setter(arm: &Arm, target: u32, field: i32) -> bool {
    let Ok(code) = arm.body(target) else { return false };
    code.iter().filter(|i| i.name == "str").count() == 1
        && code
            .iter()
            .any(|i| i.name == "str" && i.ops[0].reg() == Some("r1") && i.ops[1].mem() == Some(("r0", field, false)))
        && code.iter().all(|i| match i.name.as_str() {
            "str" | "push" | "pop" | "ldm" => true,
            "mov" => i.text == "ip, sp",
            "sub" => i.text == "fp, ip, #4",
            "bx" => i.text == "lr",
            _ => false,
        })
}

/// Find all light backgrounds passed to the carousel's colour setter.
fn backgrounds(elf: &Elf, arm: &Arm, setter: u32) -> Result<Vec<(u32, u32)>> {
    let mut edits = Vec::new();
    for function in colour_functions(elf, arm, LIGHT)? {
        let Ok(code) = arm.body(function) else { continue };
        for pair in colours(&code) {
            if pair.value == LIGHT && pair.reg == R1 && passed_to(&code, pair) == Some(setter) {
                edits.extend(pair.replace(BLACK));
            }
        }
    }
    discovery::require(!edits.is_empty(), "dark theme: no light screen backgrounds")?;
    Ok(edits)
}

/// Candidate functions containing a colour's low half. The decoded uses,
/// rather than the word alone, identify the operation.
fn colour_functions(elf: &Elf, arm: &Arm, value: u32) -> Result<BTreeSet<u32>> {
    let text = elf.section(".text")?;
    let key = movw(0, value & 0xffff);
    Ok(elf
        .contents(text)
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
        .filter_map(|(index, &bytes)| {
            (u32::from_le_bytes(bytes) & !0xf000 == key)
                .then(|| arm.enclosing(text.address + index as u32 * 4))
                .flatten()
        })
        .collect())
}

/// The stock layer palette is an uninterrupted block of constant stores.
/// Verify the object fields and preserved fade register before replacing it.
fn palette_block(code: &[Ins], constructor: bool) -> Option<(u32, usize)> {
    let first = if constructor { 0xff999999 } else { SECONDARY };
    let start = colours(code).into_iter().find(|c| c.value == first)?.low;
    let begin = code.iter().position(|i| i.address == start)?;
    let end = code[begin..].iter().position(|i| {
        if constructor {
            matches!(i.name.as_str(), "ldm" | "pop" | "bx")
        } else {
            i.name == "strb"
        }
    })? + begin;
    let block = &code[begin..end];
    let mut registers = BTreeMap::new();
    let mut stores = BTreeMap::new();
    let mut fade = false;
    if constructor {
        registers.extend([("r1", 0), ("r2", 1)]);
        for (register, assignment) in [("r0", "r0, r4"), ("r1", "r1, #0"), ("r2", "r2, #1")] {
            let last = code[..begin].iter().rev().find(|i| i.writes.iter().any(|r| r == register))?;
            if last.name != "mov" || last.text != assignment {
                return None;
            }
        }
    }
    let scratch = if constructor { ["r1", "r2", "r3"] } else { ["r0", "r3", "ip"] };
    for ins in block {
        let register = ins.ops.first()?.reg()?;
        match ins.name.as_str() {
            "mov" if ins.text == "r5, r1" && !constructor => fade = true,
            "mov" | "movw" if scratch.contains(&register) => {
                registers.insert(register, ins.ops[1].imm()?);
            }
            "movt" if scratch.contains(&register) => {
                let value = registers.get_mut(register)?;
                *value = (*value & 0xffff) | ins.ops[1].imm()? << 16;
            }
            "str" => {
                let ("r4", field, false) = ins.ops[1].mem()? else { return None };
                if stores.insert(field, *registers.get(register)?).is_some() {
                    return None;
                }
            }
            _ => return None,
        }
    }
    let mut expected = BTreeMap::from([
        (0x28, BLACK),
        (0x2c, SECONDARY),
        (0x30, 0xff999999),
        (0x34, LIGHT),
        (0x38, GREY),
        (0x3c, BLUE),
        (0x40, LIGHT),
        (0x44, BLUE),
        (0x48, LIGHT),
        (0x50, BLUE),
        (0x54, BLACK),
        (0x58, LIGHT),
    ]);
    if constructor {
        expected.extend([(0x68, 1), (0x14, 0), (0x10, 0), (0x18, 0), (0x64, 0), (0x1c, 0), (0x6c, 0), (0x24, 0)]);
    } else {
        expected.extend([(0x4c, 0xffb9e1f1), (0x5c, BLUE), (0x60, 0xffe0e0e0)]);
        if !fade || code[end].ops[0].reg() != Some("r2") {
            return None;
        }
    }
    (stores == expected).then_some((start, block.len()))
}

fn rewrite_block(edits: &mut Vec<(u32, u32)>, (address, len): (u32, usize), mut words: Vec<u32>) -> Result<()> {
    discovery::require(words.len() <= len, "dark theme palette block has insufficient space")?;
    words.resize(len, NOP);
    edits.extend(words.into_iter().enumerate().map(|(index, word)| (address + 4 * index as u32, word)));
    Ok(())
}

fn edits(elf: &Elf) -> Result<Vec<(u32, u32)>> {
    let arm = Arm::new(elf)?;
    // Carousel entry supplies the background setter used by the other screens.
    let (carousel, tile, background) =
        resolve(&arm, methods(elf, "20nlSettingsBaseScreen")?, "carousel", |address, code| {
            colours(code)
                .into_iter()
                .filter_map(|pair| {
                    let target = passed_to(code, pair)?;
                    (pair.value == BLUE && pair.reg == R1 && setter(&arm, target, 0x9c))
                        .then_some((address, pair, target))
                })
                .collect()
        })?;
    let mut edits = backgrounds(elf, &arm, background)?;
    edits.extend(tile.replace(0xff00a1f2));

    // Both fades construct RGB and alpha separately before calling SetColor.
    for class in ["17nlEquipmentScreen", "15nlProviewScreen"] {
        let sites = resolve(&arm, reachable(&arm, methods(elf, class)?, 1), "background fade", |_, code| {
            let mut found = Vec::new();
            if code.last().is_none_or(|i| i.name != "b" || i.ops[0].imm() != Some(background)) {
                return found;
            }
            let bits: Vec<_> = code
                .iter()
                .filter(|i| i.name == "bfi" && i.ops[0].reg() == Some("r1") && i.ops[3].imm() == Some(8))
                .collect();
            if bits.len() == 4
                && bits.iter().map(|i| i.ops[2].imm()).eq([Some(16), Some(8), Some(0), Some(24)])
                && !code.iter().any(|i| {
                    i.address > bits[0].address
                        && i.address < bits[3].address
                        && !bits.iter().any(|bit| bit.address == i.address)
                        && i.writes.iter().any(|reg| reg == "r1")
                })
            {
                found.push([bits[0].address, bits[1].address, bits[2].address]);
            }
            found
        })?;
        edits.extend([(sites[0], 0xe3a01000), (sites[1], NOP), (sites[2], NOP)]);
    }

    let (palette_method, block) = resolve(&arm, methods(elf, "7nlLayer")?, "layer palette", |address, code| {
        palette_block(code, false).map(|block| (address, block)).into_iter().collect()
    })?;
    rewrite_block(&mut edits, block, palette())?;
    let vtable = discovery::primary_vtable(elf, "7nlLayer")?;
    let mut ctors = discovery::referencing_functions(elf, &arm, vtable);
    ctors.extend(discovery::referencing_functions(elf, &arm, vtable - 8));
    let block = resolve(&arm, ctors, "layer constructor", |_, code| palette_block(code, true).into_iter().collect())?;
    rewrite_block(&mut edits, block, constructor())?;

    let letter = resolve(&arm, methods(elf, "19nlEncoderWheelLayer")?, "letter bubble", |_, code| {
        colours(code)
            .into_iter()
            .filter(|c| c.value == 0xff8eb1c0 && stored_at(code, c.high, c.reg).is_some())
            .collect()
    })?;
    edits.extend(letter.replace(0xff00a3d9));
    let equipment = resolve(&arm, methods(elf, "25nlEquipmentDetectionLayer")?, "equipment title", |_, code| {
        if !code.iter().any(|i| i.name == "bl" && i.ops[0].imm() == Some(palette_method)) {
            return Vec::new();
        }
        code.iter()
            .filter_map(|i| {
                let reg = (i.word >> 12) & 15;
                (i.name == "mov"
                    && i.ops[1].imm() == Some(BLACK)
                    && stored_at(code, i.address, reg) == Some(("r4", 0x28)))
                .then_some((i.address, reg))
            })
            .collect()
    })?;
    edits.push((equipment.0, 0xe3e004ff | equipment.1 << 12));

    let (black, message) = resolve(&arm, methods(elf, "15nlMessageScreen")?, "message colours", |_, code| {
        let pairs = colours(code);
        let mut matches = Vec::new();
        for group in pairs.windows(4) {
            if !group.iter().map(|c| c.value).eq([SECONDARY, LIGHT, BLUE, GREY]) {
                continue;
            }
            let Some(("sp", offset)) = stored_at(code, group[0].high, group[0].reg) else { continue };
            if !group
                .iter()
                .enumerate()
                .all(|(i, c)| stored_at(code, c.high, c.reg) == Some(("sp", offset + 4 * i as i32)))
            {
                continue;
            }
            for ins in code.iter().filter(|i| i.address < group[0].low && group[0].low - i.address <= 128) {
                let reg = (ins.word >> 12) & 15;
                if ins.name == "mov"
                    && ins.ops[1].imm() == Some(BLACK)
                    && stored_at(code, ins.address, reg).is_some_and(|(base, _)| base == "fp")
                {
                    matches.push(((ins.address, reg), group.to_vec()));
                }
            }
        }
        matches
    })?;
    edits.push((black.0, 0xe3e004ff | black.1 << 12));
    for (pair, value) in message.into_iter().zip([0xffc4c4c4, WHITE, 0xff00a3d9, 0xff666666]) {
        edits.extend(pair.replace(value));
    }

    let schedule =
        resolve(&arm, reachable(&arm, methods(elf, "14nlScheduleView")?, 2), "schedule colours", |_, code| {
            let pairs = colours(code);
            pairs
                .windows(2)
                .filter_map(|group| {
                    let (light, blue) = (group[0], group[1]);
                    (light.value == LIGHT
                        && blue.value == BLUE
                        && stored_at(code, light.high, light.reg).is_some_and(|(base, _)| base == "fp")
                        && stored_at(code, blue.high, blue.reg).is_some_and(|(base, _)| base == "fp")
                        && uses(code, light.high, light.reg)
                            .filter(|i| i.name == "str" && i.ops[0].reg() == Some(reg_name(light.reg)))
                            .count()
                            == 2)
                        .then_some((light, blue))
                })
                .collect()
        })?;
    edits.extend(schedule.0.replace(WHITE));
    edits.extend(schedule.1.replace(0xff00a3d9));

    // The carousel tail-calls its text renderer, which calls its title helper.
    let subtitle_function = arm
        .body(carousel)?
        .last()
        .and_then(|i| (i.name == "b").then(|| i.ops[0].imm()).flatten())
        .ok_or_else(|| crate::Error::Device("dark theme: carousel text renderer missing".into()))?;
    let (subtitle, transform) = resolve(&arm, [subtitle_function], "carousel subtitle", |_, code| {
        colours(code)
            .into_iter()
            .filter_map(|pair| {
                (pair.value == LIGHT && pair.reg == R0)
                    .then(|| passed_to(code, pair).map(|target| (pair, target)))
                    .flatten()
            })
            .collect()
    })?;
    edits.extend(subtitle.replace(0xffbfeaff));
    let titles = resolve(&arm, callees(&arm, subtitle_function)?, "carousel titles", |_, code| {
        let pairs: Vec<_> = colours(code)
            .into_iter()
            .filter(|pair| pair.value == LIGHT && pair.reg == R0 && passed_to(code, *pair) == Some(transform))
            .collect();
        (pairs.len() == 2).then_some(pairs).into_iter().collect()
    })?;
    edits.extend(titles[0].replace(WHITE));
    edits.extend(titles[1].replace(0xffbfeaff));

    let wiring = resolve(&arm, reachable(&arm, methods(elf, "12nlWiresLayer")?, 2), "wiring selection", |_, code| {
        colours(code)
            .into_iter()
            .filter(|pair| pair.value == BLUE && pair.reg == R0 && passed_to(code, *pair) == Some(transform))
            .collect()
    })?;
    edits.extend(wiring.replace(0xff00a3d9));

    // The progress renderer passes two colours to the same drawing function
    // through the same stack argument, and keeps its track colour locally.
    let progress = resolve(&arm, colour_functions(elf, &arm, LIGHT)?, "progress ring", |_, code| {
        let pairs = colours(code);
        let mut found = Vec::new();
        for group in pairs.windows(3) {
            let (track, first, second) = (group[0], group[1], group[2]);
            if !group.iter().map(|c| c.value).eq([LIGHT, 0xff00a3d9, 0xff007cec]) {
                continue;
            }
            if stored_at(code, track.high, track.reg).is_some_and(|(base, _)| base == "fp")
                && stored_at(code, first.high, first.reg).is_some_and(|(base, slot)| {
                    base == "sp" && stored_at(code, second.high, second.reg) == Some((base, slot))
                })
                && next_call(code, first.high).is_some_and(|target| next_call(code, second.high) == Some(target))
            {
                found.push((track, first, second));
            }
        }
        found
    })?;
    edits.extend(progress.0.replace(BLACK));
    edits.push((progress.1.low, movw(progress.1.reg, 0x7cec)));
    edits.push((progress.2.low, movw(progress.2.reg, 0xa3d9)));

    let pro = resolve(&arm, methods(elf, "14nlProInfoLayer")?, "Nest Pro colours", |_, code| {
        let pairs = colours(code);
        pairs
            .windows(2)
            .filter_map(|group| {
                let (first, second) = (group[0], group[1]);
                (first.value == 0xffffbb00
                    && second.value == 0xffe5e5e5
                    && stored_at(code, first.high, first.reg)
                        .is_some_and(|(base, slot)| stored_at(code, second.high, second.reg) == Some((base, slot + 4))))
                .then_some((first, second))
            })
            .collect()
    })?;
    edits.extend(pro.0.replace(0xff30b4e9));
    edits.extend(pro.1.replace(0xff103b4d));

    let chapter = resolve(&arm, methods(elf, "14nlChapterLayer")?, "chapter rows", |_, code| {
        colours(code)
            .into_iter()
            .filter(|pair| {
                pair.value == GREY && stored_at(code, pair.high, pair.reg).is_some_and(|(_, offset)| offset == 0x38)
            })
            .collect()
    })?;
    edits.extend(chapter.replace(0xff666666));

    // Startup draws the boot disc before the safety-switch layer is shown.
    let safety = discovery::string_address(elf, b"\0safetyswitchlayer\0")
        .map(|address| address + 1)
        .ok_or_else(|| crate::Error::Device("dark theme: safety-switch layer missing".into()))?;
    let boot = resolve(&arm, discovery::referencing_functions(elf, &arm, safety), "boot disc", |_, code| {
        let mut found = Vec::new();
        for pair in colours(code).into_iter().filter(|pair| pair.value == LIGHT && (4..=11).contains(&pair.reg)) {
            let Some(mov) = code.iter().find(|i| {
                i.address > pair.high
                    && i.name == "mov"
                    && i.ops[0].reg() == Some("r1")
                    && i.ops[1].reg() == Some(reg_name(pair.reg))
            }) else {
                continue;
            };
            if !code.iter().any(|i| {
                i.address > pair.high && i.address < mov.address && i.writes.iter().any(|reg| reg == reg_name(pair.reg))
            }) && uses(code, mov.address, R1)
                .find(|i| i.name == "bl")
                .and_then(|i| i.ops[0].imm())
                .is_some_and(|target| setter(&arm, target, 0xc))
            {
                found.push(mov.address);
            }
        }
        found
    })?;
    edits.push((boot, BLACK_R1));
    Ok(edits)
}

/// Whether the executable contains the supported stock palette operations.
pub fn reviewed(stock: &[u8]) -> bool {
    Elf::parse(stock).and_then(|elf| edits(&elf)).is_ok()
}

/// Validate every edit against the screen builder's output before writing.
pub fn patch(stock: &[u8], output: &mut [u8]) -> Result<()> {
    let elf = Elf::parse(stock)?;
    let mut writes = BTreeMap::new();
    for (address, after) in edits(&elf)? {
        let offset = elf.offset(address)?;
        let before = word(stock, offset)?;
        if word(output, offset)? != before {
            return device("the dark theme patch site changed");
        }
        if writes.insert(offset, after).is_some() {
            return device("overlapping dark theme palette operations");
        }
    }
    for (offset, after) in writes {
        output[offset..offset + 4].copy_from_slice(&after.to_le_bytes());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_firmware_is_rejected_without_changing_output() {
        let mut output = b"linked screen".to_vec();
        assert!(!reviewed(b"unknown firmware"));
        assert!(patch(b"unknown firmware", &mut output).is_err());
        assert_eq!(output, b"linked screen");
    }

    #[test]
    fn rewritten_palettes_fill_their_stock_regions_exactly() {
        assert_eq!(palette().len(), 31);
        assert_eq!(constructor().len(), 32);
        assert_eq!(palette()[0], 0xe1a05001);
    }

    #[test]
    fn reordered_palette_stores_produce_the_same_dark_palette() {
        let firmware = std::env::var_os("NLCLIENT_FIRMWARE_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("UNFR0ZEN_ROOT")).join(".work/firmware"));
        let Ok(stock) = std::fs::read(firmware.join("nlclient-gen2-2019")) else {
            assert!(std::env::var_os("NLCLIENT_REQUIRE_FIRMWARE").is_none(), "missing gen 2 firmware");
            return;
        };
        let elf = Elf::parse(&stock).unwrap();
        let arm = Arm::new(&elf).unwrap();
        let (start, len) = resolve(&arm, methods(&elf, "7nlLayer").unwrap(), "palette", |_, code| {
            palette_block(code, false).into_iter().collect()
        })
        .unwrap();
        let code = arm.code(start, Some(len * 4)).unwrap();
        let pair = code
            .windows(2)
            .find(|pair| {
                pair.iter().all(|i| {
                    i.name == "str" && i.ops[1].mem().is_some_and(|(base, _, indexed)| base == "r4" && !indexed)
                }) && pair[0].ops[0].reg() == pair[1].ops[0].reg()
                    && pair[0].word != pair[1].word
            })
            .unwrap();
        let offset = elf.offset(pair[0].address).unwrap();
        let mut reordered = stock.clone();
        reordered[offset..offset + 8].rotate_left(4);
        let mut expected = stock.clone();
        patch(&stock, &mut expected).unwrap();
        let mut output = reordered.clone();
        patch(&reordered, &mut output).unwrap();
        assert_eq!(output, expected);
    }

    #[test]
    fn instruction_encodings_match_the_assembler() {
        assert_eq!(movw(R1, 0xf1e7), 0xe30f11e7);
        assert_eq!(movt(R1, 0xffef), 0xe34f1fef);
        assert_eq!(store(R3, 0x2c), 0xe584302c);
    }
}
