//! ARM decoding is statically linked; no process, Python or shared library.
use super::elf::{bytes, word, Elf};
use crate::shell::sha256_hex;
use crate::{device, Result};
use capstone::{arch::arm::ArmOperandType, prelude::*, AccessType};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug)]
pub(super) enum Op {
    Reg(String),
    Imm(u32),
    Mem {
        base: String,
        disp: i32,
        indexed: bool,
    },
    Other,
}
impl Op {
    pub fn imm(&self) -> Option<u32> {
        if let Self::Imm(v) = self {
            Some(*v)
        } else {
            None
        }
    }
    pub fn reg(&self) -> Option<&str> {
        if let Self::Reg(v) = self {
            Some(v)
        } else {
            None
        }
    }
    pub fn mem(&self) -> Option<(&str, i32, bool)> {
        if let Self::Mem {
            base,
            disp,
            indexed,
        } = self
        {
            Some((base, *disp, *indexed))
        } else {
            None
        }
    }
}
#[derive(Clone, Debug)]
pub(super) struct Ins {
    pub address: u32,
    pub word: u32,
    pub name: String,
    pub text: String,
    pub ops: Vec<Op>,
    pub writes: Vec<String>,
}
type ConstructorStores = (BTreeMap<i32, Value>, Vec<Option<Value>>);

pub(super) struct Arm<'a, 'b> {
    pub elf: &'a Elf<'b>,
    cs: Capstone,
    starts: Vec<u32>,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Value {
    Number(u32),
    Object(i32),
}
impl<'a, 'b> Arm<'a, 'b> {
    pub fn new(elf: &'a Elf<'b>) -> Result<Self> {
        let cs = Capstone::new()
            .arm()
            .mode(capstone::arch::arm::ArchMode::Arm)
            .detail(true)
            .build()
            .map_err(|e| crate::Error::Device(e.to_string()))?;
        let s = elf.section(".ARM.exidx")?;
        if s.size % 8 != 0 {
            return device("invalid ARM unwind table");
        }
        let mut starts = Vec::new();
        for o in (s.offset..s.offset + s.size).step_by(8) {
            let relative = ((word(elf.data, o)? << 1) as i32) >> 1;
            starts.push((s.address + (o - s.offset) as u32).wrapping_add(relative as u32));
        }
        if !starts.windows(2).all(|w| w[0] <= w[1]) {
            return device("unsorted ARM unwind table");
        }
        Ok(Self { elf, cs, starts })
    }
    pub fn decode(&self, address: u32, data: &[u8]) -> Result<Vec<Ins>> {
        let insns = self
            .cs
            .disasm_all(data, address as u64)
            .map_err(|e| crate::Error::Device(e.to_string()))?;
        let mut result = Vec::new();
        for i in insns.iter() {
            if i.bytes().len() != 4 {
                return device("non-ARM instruction");
            }
            let detail = self
                .cs
                .insn_detail(i)
                .map_err(|e| crate::Error::Device(e.to_string()))?;
            let arch = detail.arch_detail();
            let arm = arch
                .arm()
                .ok_or_else(|| crate::Error::Device("non-ARM detail".into()))?;
            let name = |reg| self.cs.reg_name(reg).unwrap_or_default();
            let mut writes = detail
                .regs_write()
                .iter()
                .map(|&reg| name(reg))
                .collect::<Vec<_>>();
            let ops = arm
                .operands()
                .map(|op| {
                    if matches!(
                        op.access,
                        Some(AccessType::WriteOnly | AccessType::ReadWrite)
                    ) {
                        if let ArmOperandType::Reg(r) = op.op_type {
                            writes.push(name(r));
                        }
                    }
                    match op.op_type {
                        ArmOperandType::Reg(r) => Op::Reg(name(r)),
                        ArmOperandType::Imm(n) => Op::Imm(n as u32),
                        ArmOperandType::Mem(m) => {
                            if arm.writeback() {
                                writes.push(name(m.base()));
                            }
                            Op::Mem {
                                base: name(m.base()),
                                disp: m.disp(),
                                indexed: m.index().0 != 0,
                            }
                        }
                        _ => Op::Other,
                    }
                })
                .collect();
            result.push(Ins {
                address: i.address() as u32,
                word: u32::from_le_bytes(i.bytes().try_into().unwrap()),
                name: i.mnemonic().unwrap_or_default().into(),
                text: i.op_str().unwrap_or_default().into(),
                ops,
                writes,
            });
        }
        Ok(result)
    }
    pub fn code(&self, address: u32, size: Option<usize>) -> Result<Vec<Ins>> {
        if address & 3 != 0 || !self.elf.text(address) {
            return device(format!("not ARM executable code: {address:#x}"));
        }
        let size = match size {
            Some(size) => size,
            None => self
                .starts
                .get(self.starts.partition_point(|&a| a <= address))
                .and_then(|&end| end.checked_sub(address))
                .ok_or_else(|| crate::Error::Device("missing ARM end boundary".into()))?
                as usize,
        };
        if !(1..=0x10000).contains(&size) {
            return device("implausible ARM function length");
        }
        let section = self
            .elf
            .at(address)
            .ok_or_else(|| crate::Error::Device("ARM function outside sections".into()))?;
        let code_bytes = bytes(
            self.elf.contents(section),
            (address - section.address) as usize,
            size,
        )?;
        let mut code = self.decode(address, code_bytes)?;
        let mut end = address
            .checked_add(size as u32)
            .ok_or_else(|| crate::Error::Device("ARM function address overflow".into()))?;
        for ins in &code {
            for op in &ins.ops {
                if let Some(("pc", disp, _)) = op.mem() {
                    let pool = ins.address.wrapping_add(8).wrapping_add(disp as u32);
                    if address < pool && pool < end {
                        end = pool;
                    }
                }
            }
        }
        code.retain(|i| i.address < end);
        if code.is_empty() {
            return device("no ARM instructions");
        }
        Ok(code)
    }
    pub fn calls(&self, address: u32) -> Result<Vec<u32>> {
        Ok(self
            .code(address, None)?
            .iter()
            .filter(|i| i.name == "bl")
            .filter_map(|i| i.ops.first()?.imm())
            .collect())
    }
    pub fn tails(&self, address: u32) -> Result<Vec<u32>> {
        let code = self.code(address, None)?;
        let end = code.last().unwrap().address + 4;
        Ok(code
            .iter()
            .filter(|i| i.name == "b")
            .filter_map(|i| i.ops.first()?.imm())
            .filter(|t| !(address..end).contains(t))
            .collect())
    }
    pub fn fingerprint(&self, address: u32) -> Result<String> {
        let code = self.code(address, None)?;
        let end = code.last().unwrap().address + 4;
        let mut halves = BTreeSet::new();
        let mut pending = BTreeMap::new();
        for i in &code {
            match i.name.as_str() {
                "movw" => {
                    pending.insert(i.ops[0].reg().unwrap().to_owned(), i);
                }
                "movt" => {
                    if let Some(low) = pending.remove(i.ops[0].reg().unwrap()) {
                        let value = low.ops[1].imm().unwrap() | (i.ops[1].imm().unwrap() << 16);
                        if self.elf.sections.iter().any(|s| {
                            s.flags & 2 != 0
                                && value >= s.address
                                && value - s.address < s.size as u32
                        }) {
                            halves.extend([low.address, i.address]);
                        }
                    }
                }
                _ => {
                    for reg in &i.writes {
                        pending.remove(reg);
                    }
                }
            }
        }
        let mut output = Vec::new();
        for i in &code {
            let mut w = i.word;
            if w & 0x0e000000 == 0x0a000000
                && i.ops
                    .first()
                    .and_then(Op::imm)
                    .is_some_and(|t| !(address..end).contains(&t))
            {
                w &= 0xff000000;
            }
            if halves.contains(&i.address) {
                w &= !0x000f0fff;
            }
            if i.ops
                .iter()
                .any(|op| op.mem().is_some_and(|(b, _, _)| b == "pc"))
            {
                w &= !0x00800fff;
            }
            output.extend(w.to_le_bytes());
        }
        Ok(sha256_hex(&output))
    }
    pub fn stores(&self, address: u32, allocator: Option<u32>) -> Result<ConstructorStores> {
        let mut regs: BTreeMap<String, Value> = BTreeMap::new();
        if allocator.is_none() {
            regs.insert("r0".into(), Value::Object(0));
        }
        let mut stores = BTreeMap::new();
        let mut allocations = Vec::new();
        fn value(op: &Op, ins: &Ins, regs: &BTreeMap<String, Value>) -> Option<Value> {
            match op {
                Op::Imm(n) => Some(Value::Number(*n)),
                Op::Reg(r) if r == "pc" => Some(Value::Number(ins.address + 8)),
                Op::Reg(r) => regs.get(r).copied(),
                _ => None,
            }
        }
        fn assign(regs: &mut BTreeMap<String, Value>, op: &Op, v: Option<Value>) {
            if let Some(reg) = op.reg() {
                if let Some(v) = v {
                    regs.insert(reg.to_owned(), v);
                } else {
                    regs.remove(reg);
                }
            }
        }
        for i in self.code(address, None)? {
            let o = &i.ops;
            if matches!(i.name.as_str(), "pop" | "ldm") && i.text.contains("pc") {
                break;
            }
            match i.name.as_str() {
                "bl" => {
                    let allocated = o[0].imm() == allocator;
                    if allocated {
                        allocations.push(regs.get("r0").copied());
                    }
                    for reg in ["r0", "r1", "r2", "r3", "ip", "lr"] {
                        regs.remove(reg);
                    }
                    if allocated {
                        regs.insert("r0".into(), Value::Object(0));
                    }
                }
                "mov" | "movw" | "movt" => {
                    let mut v = value(&o[1], &i, &regs);
                    if i.name == "movt" {
                        v = match (value(&o[0], &i, &regs), v) {
                            (Some(Value::Number(a)), Some(Value::Number(b))) => {
                                Some(Value::Number((a & 0xffff) | (b << 16)))
                            }
                            _ => None,
                        };
                    }
                    assign(&mut regs, &o[0], v);
                }
                "add" | "sub" => {
                    let v = match (value(&o[1], &i, &regs), value(&o[2], &i, &regs)) {
                        (Some(a), Some(Value::Number(mut b))) => {
                            if i.name == "sub" {
                                b = b.wrapping_neg();
                            }
                            Some(match a {
                                Value::Number(a) => Value::Number(a.wrapping_add(b)),
                                Value::Object(a) => Value::Object(a.wrapping_add(b as i32)),
                            })
                        }
                        _ => None,
                    };
                    assign(&mut regs, &o[0], v);
                }
                "ldr" => {
                    let v = o[1]
                        .mem()
                        .filter(|(b, _, _)| *b == "pc")
                        .and_then(|(_, d, _)| {
                            self.elf
                                .read(i.address.wrapping_add(8).wrapping_add(d as u32))
                                .ok()
                        })
                        .map(Value::Number);
                    assign(&mut regs, &o[0], v);
                }
                "str" | "strd" => {
                    if let Some((base, disp, false)) = o.last().and_then(Op::mem) {
                        if let Some(Value::Object(base)) = regs.get(base).copied() {
                            for (n, op) in o[..o.len() - 1].iter().enumerate() {
                                let offset = base + disp + n as i32 * 4;
                                if let Some(v) = value(op, &i, &regs) {
                                    stores.insert(offset, v);
                                } else {
                                    stores.remove(&offset);
                                }
                            }
                        }
                    }
                }
                "stm" | "stmib" => {
                    if let Some(Value::Object(base)) = o
                        .first()
                        .and_then(Op::reg)
                        .and_then(|r| regs.get(r))
                        .copied()
                    {
                        for (n, op) in o[1..].iter().enumerate() {
                            let offset =
                                base + if i.name == "stmib" { 4 } else { 0 } + n as i32 * 4;
                            if let Some(v) = value(op, &i, &regs) {
                                stores.insert(offset, v);
                            } else {
                                stores.remove(&offset);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Ok((stores, allocations))
    }
}
