//! Ahead-of-time NE/P-code to C frontend. Clang lowers the generated operations
//! to Wasm; runtime imports only access checked segmented memory and report faults.
use crate::{NeAccess, NeDecodedInstruction, NeError, NeTask};
use rsleigh_api::{AddressSpaceId as Space, Decoder, PcodeOp as Op, Varnode};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

pub struct NeCompilation {
    pub source: String,
    pub instructions: usize,
    pub unsupported: Vec<(u32, String)>,
}

pub fn compile(task: &NeTask) -> Result<NeCompilation, NeError> {
    let mut decoder = Decoder::new(rsleigh_api::Architecture::X86_16Protected);
    let mut pending = BTreeSet::new();
    pending.insert((u32::from(task.machine.cs()) << 16) | u32::from(task.machine.ip()));
    for entry in &task.image.entry_points {
        if let Some(selector) = task.loaded.selector_for_segment(entry.segment) {
            pending.insert((u32::from(selector) << 16) | u32::from(entry.offset));
        }
    }
    // Code pointers in data (including callback tables) need not be reachable
    // by a direct branch from the entry point.
    for segment in &task.image.segments {
        for r in &segment.relocations {
            if r.flags & 3 != 0 {
                continue;
            }
            let target = if r.target1 & 0xff == 0xff {
                task.image
                    .entry_points
                    .iter()
                    .find(|e| e.ordinal == r.target2)
                    .map(|e| (e.segment, e.offset))
            } else {
                Some((r.target1 & 0xff, r.target2))
            };
            if let Some((segment, offset)) = target {
                if let Some(selector) = task.loaded.selector_for_segment(segment) {
                    if task
                        .loaded
                        .translate(selector, u32::from(offset), 1, NeAccess::Execute)
                        .is_ok()
                    {
                        pending.insert((u32::from(selector) << 16) | u32::from(offset));
                    }
                }
            }
        }
    }
    let mut instructions: BTreeMap<u32, NeDecodedInstruction> = BTreeMap::new();
    while let Some(pc) = pending.pop_first() {
        if instructions.contains_key(&pc) {
            continue;
        }
        let cs = (pc >> 16) as u16;
        let ip = pc as u16;
        if task
            .loaded
            .translate(cs, u32::from(ip), 1, NeAccess::Execute)
            .is_err()
            || task.is_api_address(cs)
        {
            continue;
        }
        let Ok(decoded) = task.loaded.decode_next(&mut decoder, cs, ip) else {
            continue;
        };
        pending.extend(indirect_table_targets(&task.loaded, &decoded));
        let mut fallthrough = true;
        for op in &decoded.instruction.ops {
            match op {
                Op::Branch { dest } => {
                    if dest.space == Space::Ram {
                        pending.insert(dest.offset as u32);
                    }
                    fallthrough = false;
                }
                Op::CBranch { dest, .. } | Op::Call { dest } => {
                    if dest.space == Space::Ram {
                        pending.insert(dest.offset as u32);
                    }
                }
                Op::BranchInd { .. } | Op::Return { .. } => fallthrough = false,
                _ => {}
            }
        }
        if fallthrough {
            pending.insert((pc & 0xffff0000) | u32::from(decoded.next_ip));
        }
        instructions.insert(pc, decoded);
    }
    let mut source = String::from(PRELUDE);
    let mut unsupported = Vec::new();
    for (&pc, instruction) in &instructions {
        let _ = writeln!(
            source,
            "static u32 i_{pc:08x}(void) {{ /* {} */",
            instruction.instruction.disassembly.replace("*/", "")
        );
        let mut temps = BTreeSet::new();
        for op in &instruction.instruction.ops {
            visit_nodes(op, |n| {
                if n.space == Space::Unique {
                    for i in 0..n.size {
                        temps.insert(n.offset + u64::from(i));
                    }
                }
            });
        }
        let temps: BTreeMap<u64, usize> =
            temps.into_iter().enumerate().map(|(i, p)| (p, i)).collect();
        let _ = writeln!(source, "u8 t[{}]={{0}};", temps.len().max(1));
        let mut emitter = Emitter {
            out: &mut source,
            temps: &temps,
            pc,
        };
        for (index, op) in instruction.instruction.ops.iter().enumerate() {
            if let Err(reason) = emitter.emit(op) {
                let _ = writeln!(emitter.out, "fault({pc}u,{index}u); return {pc}u;");
                unsupported.push((pc, reason));
                break;
            }
        }
        let next = (pc & 0xffff0000) | u32::from(instruction.next_ip);
        let _ = writeln!(source, "return {next}u; }}");
    }
    source.push_str("typedef u32 (*code_fn)(void); struct site {u32 pc;code_fn code;}; static const struct site sites[]={\n");
    for pc in instructions.keys() {
        let _ = writeln!(source, "{{{pc}u,i_{pc:08x}}},");
    }
    source.push_str("};\n__attribute__((export_name(\"ne_step\"))) u32 ne_step(u32 pc) { u32 lo=0,hi=sizeof(sites)/sizeof(sites[0]);while(lo<hi){u32 mid=lo+(hi-lo)/2;if(sites[mid].pc<pc)lo=mid+1;else hi=mid;}if(lo==sizeof(sites)/sizeof(sites[0])||sites[lo].pc!=pc){fault(pc,0xffffffffu);return pc;}u32 next=sites[lo].code();wr(0x102,2,next>>16);wr(0x284,4,next);return next;}\n");
    Ok(NeCompilation {
        source,
        instructions: instructions.len(),
        unsupported,
    })
}

#[derive(Clone, Copy)]
enum AddressValue {
    Offset {
        base: u16,
        indexed: bool,
    },
    Pointer {
        selector: u16,
        base: u16,
        indexed: bool,
    },
}
fn indirect_table_targets(
    memory: &crate::NeLoadedImage,
    instruction: &NeDecodedInstruction,
) -> BTreeSet<u32> {
    if !instruction
        .instruction
        .ops
        .iter()
        .any(|op| matches!(op, Op::BranchInd { .. } | Op::CallInd { .. }))
    {
        return BTreeSet::new();
    }
    let mut values = BTreeMap::new();
    let mut tables = BTreeSet::new();
    let value =
        |node: &Varnode, values: &BTreeMap<(u64, u32), AddressValue>| -> Option<AddressValue> {
            match node.space {
                Space::Const => Some(AddressValue::Offset {
                    base: node.offset as u16,
                    indexed: false,
                }),
                Space::Register if node.offset == 0x102 => Some(AddressValue::Offset {
                    base: instruction.cs,
                    indexed: false,
                }),
                Space::Register => Some(AddressValue::Offset {
                    base: 0,
                    indexed: true,
                }),
                Space::Unique => values.get(&(node.offset, node.size)).copied(),
                _ => None,
            }
        };
    for op in &instruction.instruction.ops {
        let output = match op {
            Op::CallOther { out, .. } => *out,
            _ => pcode_ir::get_output(op),
        };
        let Some(out) = output else {
            continue;
        };
        let result = match op {
            Op::Copy { input, .. }
            | Op::IntZext { input, .. }
            | Op::IntSext { input, .. }
            | Op::Subpiece { input, lsb: 0, .. } => value(input, &values),
            Op::IntAdd { left, right, .. } => match (value(left, &values), value(right, &values)) {
                (
                    Some(AddressValue::Offset {
                        base: a,
                        indexed: ai,
                    }),
                    Some(AddressValue::Offset {
                        base: b,
                        indexed: bi,
                    }),
                ) => Some(AddressValue::Offset {
                    base: a.wrapping_add(b),
                    indexed: ai || bi,
                }),
                _ => None,
            },
            Op::CallOther {
                func_id: 0, inputs, ..
            } if inputs.len() == 2 => {
                match (value(&inputs[0], &values), value(&inputs[1], &values)) {
                    (
                        Some(AddressValue::Offset {
                            base: selector,
                            indexed: false,
                        }),
                        Some(AddressValue::Offset { base, indexed }),
                    ) => Some(AddressValue::Pointer {
                        selector,
                        base,
                        indexed,
                    }),
                    _ => None,
                }
            }
            Op::Load {
                ptr,
                space: Space::Ram,
                ..
            } if out.size == 2 => {
                if let Some(AddressValue::Pointer {
                    selector,
                    base,
                    indexed: true,
                }) = value(ptr, &values)
                {
                    if selector == instruction.cs {
                        tables.insert((selector, base));
                    }
                }
                None
            }
            _ => None,
        };
        if out.space == Space::Unique {
            if let Some(result) = result {
                values.insert((out.offset, out.size), result);
            } else {
                values.remove(&(out.offset, out.size));
            }
        }
    }
    let mut targets = BTreeSet::new();
    for (selector, base) in tables {
        // Dense 16-bit code-offset tables terminate when an entry cannot name
        // executable memory. This is bounded discovery only; executing an
        // indirect branch still uses the guest's actual computed destination.
        for index in 0..512u32 {
            let offset = u32::from(base) + index * 2;
            let Ok(bytes) = memory.read(selector, offset, 2, NeAccess::Read) else {
                break;
            };
            let target = u16::from_le_bytes([bytes[0], bytes[1]]);
            if memory
                .translate(selector, u32::from(target), 1, NeAccess::Execute)
                .is_err()
            {
                break;
            }
            targets.insert((u32::from(selector) << 16) | u32::from(target));
        }
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovers_checked_word_targets_of_cs_relative_indexed_jumps() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let mut memory = crate::NeLoadedImage {
                    selectors: Vec::new(),
                    storage: Vec::new(),
                    next_allocation_id: 1,
                };
                memory.add_region(0x28, 256, true, true).unwrap();
                memory.write(0x28, 0, &[0x2e, 0xff, 0xa7, 0x40, 0]).unwrap();
                memory
                    .write(0x28, 0x40, &[0x20, 0, 0x30, 0, 0xff, 0xff])
                    .unwrap();
                let mut decoder = Decoder::new(rsleigh_api::Architecture::X86_16Protected);
                let instruction = memory.decode_next(&mut decoder, 0x28, 0).unwrap();
                assert_eq!(
                    indirect_table_targets(&memory, &instruction),
                    BTreeSet::from([0x00280020, 0x00280030])
                );
                memory.write(0x28, 0, &[0xff, 0xa7, 0x40, 0]).unwrap(); // unknown DS: do not assume CS
                let instruction = memory.decode_next(&mut decoder, 0x28, 0).unwrap();
                assert!(indirect_table_targets(&memory, &instruction).is_empty());
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

struct Emitter<'a> {
    out: &'a mut String,
    temps: &'a BTreeMap<u64, usize>,
    pc: u32,
}
impl Emitter<'_> {
    fn value(&self, n: &Varnode) -> Result<String, String> {
        if !(1..=8).contains(&n.size) {
            return Err(format!("unsupported width {}", n.size));
        }
        Ok(match n.space {
            Space::Const | Space::Ram => format!("{}ull", n.offset & mask(n.size)),
            Space::Register => format!("rr({}u,{}u)", n.offset, n.size),
            Space::Unique => {
                let terms: Vec<_> = (0..n.size)
                    .map(|i| {
                        format!(
                            "((u64)t[{}]<<{})",
                            self.temps[&(n.offset + u64::from(i))],
                            i * 8
                        )
                    })
                    .collect();
                format!("({})", terms.join("|"))
            }
        })
    }
    fn set(&mut self, n: &Varnode, value: &str) -> Result<(), String> {
        if !(1..=8).contains(&n.size) {
            return Err(format!("unsupported width {}", n.size));
        }
        match n.space {
            Space::Register => {
                let _ = writeln!(self.out, "wr({}u,{}u,{value});", n.offset, n.size);
            }
            Space::Unique => {
                self.out.push_str("{u64 v=");
                self.out.push_str(value);
                self.out.push_str(";\n");
                for i in 0..n.size {
                    let _ = writeln!(
                        self.out,
                        "t[{}]=(u8)(v>>{});",
                        self.temps[&(n.offset + u64::from(i))],
                        i * 8
                    );
                }
                self.out.push_str("}\n");
            }
            _ => return Err("write to literal".into()),
        }
        Ok(())
    }
    fn emit(&mut self, op: &Op) -> Result<(), String> {
        macro_rules! binary {
            ($out:expr,$left:expr,$right:expr,$f:expr) => {{
                let a = self.value($left)?;
                let b = self.value($right)?;
                let bits = $left.size * 8;
                self.set($out, &($f)(a, b, bits))
            }};
        }
        match op {
            Op::Copy { out, input } | Op::IntZext { out, input } => {
                self.set(out, &self.value(input)?)
            }
            Op::IntSext { out, input } => self.set(
                out,
                &format!("(u64)sx({},{}u)", self.value(input)?, input.size * 8),
            ),
            Op::IntNeg { out, input } => self.set(out, &format!("(0ull-{})", self.value(input)?)),
            Op::IntNot { out, input } => self.set(out, &format!("(~{})", self.value(input)?)),
            Op::BoolNot { out, input } => self.set(out, &format!("(!{})", self.value(input)?)),
            Op::Popcount { out, input } => self.set(
                out,
                &format!("(u64)__builtin_popcountll({})", self.value(input)?),
            ),
            Op::Subpiece { out, input, lsb } => self.set(
                out,
                &if *lsb >= 8 {
                    "0ull".into()
                } else {
                    format!("({}>>{})", self.value(input)?, lsb * 8)
                },
            ),
            Op::IntAdd { out, left, right } => {
                binary!(out, left, right, |a, b, _| format!("({a}+{b})"))
            }
            Op::IntSub { out, left, right } => {
                binary!(out, left, right, |a, b, _| format!("({a}-{b})"))
            }
            Op::IntMult { out, left, right } => {
                binary!(out, left, right, |a, b, _| format!("({a}*{b})"))
            }
            Op::IntAnd { out, left, right } => {
                binary!(out, left, right, |a, b, _| format!("({a}&{b})"))
            }
            Op::IntOr { out, left, right } => {
                binary!(out, left, right, |a, b, _| format!("({a}|{b})"))
            }
            Op::IntXor { out, left, right } => {
                binary!(out, left, right, |a, b, _| format!("({a}^{b})"))
            }
            Op::IntEq { out, left, right } => {
                binary!(out, left, right, |a, b, _| format!("({a}=={b})"))
            }
            Op::IntNotEq { out, left, right } => {
                binary!(out, left, right, |a, b, _| format!("({a}!={b})"))
            }
            Op::IntLess { out, left, right } => {
                binary!(out, left, right, |a, b, _| format!("({a}<{b})"))
            }
            Op::IntLessEq { out, left, right } => {
                binary!(out, left, right, |a, b, _| format!("({a}<={b})"))
            }
            Op::IntSLess { out, left, right } => binary!(out, left, right, |a, b, n| format!(
                "(sx({a},{n})<sx({b},{n}))"
            )),
            Op::IntSLessEq { out, left, right } => binary!(out, left, right, |a, b, n| format!(
                "(sx({a},{n})<=sx({b},{n}))"
            )),
            Op::BoolAnd { out, left, right } => {
                binary!(out, left, right, |a, b, _| format!("({a}&&{b})"))
            }
            Op::BoolOr { out, left, right } => {
                binary!(out, left, right, |a, b, _| format!("({a}||{b})"))
            }
            Op::BoolXor { out, left, right } => {
                binary!(out, left, right, |a, b, _| format!("((!!{a})!=(!!{b}))"))
            }
            Op::IntCarry { out, left, right } => binary!(out, left, right, |a, b, n| if n == 64 {
                format!("(({a}+{b})<{a})")
            } else {
                format!("(({a}+{b})>{}ull)", mask(n / 8))
            }),
            Op::IntSCarry { out, left, right } => binary!(out, left, right, |a, b, n| format!(
                "((~({a}^{b})&({a}^({a}+{b}))&{}ull)!=0)",
                1u64 << (n - 1)
            )),
            Op::IntSBorrow { out, left, right } => binary!(out, left, right, |a, b, n| format!(
                "((({a}^{b})&({a}^({a}-{b}))&{}ull)!=0)",
                1u64 << (n - 1)
            )),
            Op::IntLsl { out, left, right } => binary!(out, left, right, |a, b, n| format!(
                "({b}>={n}?0ull:({a}<<{b}))"
            )),
            Op::IntLsr { out, left, right } => binary!(out, left, right, |a, b, n| format!(
                "({b}>={n}?0ull:({a}>>{b}))"
            )),
            Op::IntAsr { out, left, right } => binary!(out, left, right, |a, b, n| format!(
                "(u64)({b}>={n}?(sx({a},{n})<0?-1ll:0ll):(sx({a},{n})>>{b}))"
            )),
            Op::IntDiv { out, left, right } => {
                binary!(out, left, right, |a, b, _| format!("({a}/{b})"))
            }
            Op::IntRem { out, left, right } => {
                binary!(out, left, right, |a, b, _| format!("({a}%{b})"))
            }
            Op::IntSDiv { out, left, right } => {
                binary!(out, left, right, |a, b, n| format!("sdiv({a},{b},{n})"))
            }
            Op::IntSRem { out, left, right } => {
                binary!(out, left, right, |a, b, n| format!("srem({a},{b},{n})"))
            }
            Op::Load {
                out,
                space: Space::Ram,
                ptr,
            } => self.set(
                out,
                &format!("mem_read((u32){},{}u)", self.value(ptr)?, out.size),
            ),
            Op::Store {
                space: Space::Ram,
                ptr,
                val,
            } => {
                let _ = writeln!(
                    self.out,
                    "mem_write((u32){},{}u,{});",
                    self.value(ptr)?,
                    val.size,
                    self.value(val)?
                );
                Ok(())
            }
            Op::CallOther {
                out: Some(out),
                func_id: 0,
                inputs,
            } if inputs.len() == 2 => self.set(
                out,
                &format!(
                    "(({}<<16)|{})",
                    self.value(&inputs[0])?,
                    self.value(&inputs[1])?
                ),
            ),
            Op::CallOther {
                out: None,
                func_id: 17 | 18,
                inputs,
            } if inputs.is_empty() => Ok(()),
            Op::Branch { dest } | Op::Call { dest } | Op::Return { dest } => {
                if dest.space == Space::Const {
                    return Err("internal P-code branch".into());
                }
                let _ = writeln!(self.out, "return (u32){};", self.value(dest)?);
                Ok(())
            }
            Op::BranchInd { dest } | Op::CallInd { dest } => {
                let mut dest = *dest;
                if dest.space == Space::Ram && self.temps.contains_key(&dest.offset) {
                    dest.space = Space::Unique;
                }
                let _ = writeln!(self.out, "return (u32){};", self.value(&dest)?);
                Ok(())
            }
            Op::CBranch { dest, cond } => {
                if dest.space == Space::Const {
                    return Err("internal P-code conditional branch".into());
                }
                let _ = writeln!(
                    self.out,
                    "if({}) return (u32){};",
                    self.value(cond)?,
                    self.value(dest)?
                );
                Ok(())
            }
            _ => Err(format!("{op:?} at {:08x}", self.pc)),
        }
    }
}

fn mask(size: u32) -> u64 {
    if size >= 8 {
        u64::MAX
    } else {
        (1u64 << (size * 8)) - 1
    }
}
// The pcode crate exposes read visitation; outputs deliberately include userop
// results as well, even though they cannot be removed by dead-code elimination.
fn visit_nodes(op: &Op, mut visit: impl FnMut(&Varnode)) {
    // All operands are plain varnodes; read visitation is exported by rsleigh.
    pcode_ir::visit_reads(op, &mut |n| visit(n));
    if let Some(out) = pcode_ir::get_output(op) {
        visit(&out);
    }
    if let Op::CallOther { out: Some(out), .. } = op {
        visit(&out);
    }
}

const PRELUDE: &str = r#"
typedef unsigned char u8; typedef unsigned int u32;
typedef unsigned long long u64; typedef long long i64;
__attribute__((import_module("ne"),import_name("read"))) extern u64 mem_read(u32,u32);
__attribute__((import_module("ne"),import_name("write"))) extern void mem_write(u32,u32,u64);
__attribute__((import_module("ne"),import_name("fault"))) extern void fault(u32,u32);
static u8 registers[8192];
static u64 rr(u32 p,u32 n){u64 v=0;for(u32 i=0;i<n;i++)v|=(u64)registers[p+i]<<(i*8);return v;}
static void wr(u32 p,u32 n,u64 v){for(u32 i=0;i<n;i++)registers[p+i]=(u8)(v>>(i*8));}
static i64 sx(u64 v,u32 bits){if(bits==64)return (i64)v;return ((i64)(v<<(64-bits)))>>(64-bits);}
static u64 sdiv(u64 a,u64 b,u32 n){i64 x=sx(a,n),y=sx(b,n);if(x==(-9223372036854775807ll-1)&&y==-1)return (u64)x;return (u64)(x/y);}
static u64 srem(u64 a,u64 b,u32 n){i64 x=sx(a,n),y=sx(b,n);if(x==(-9223372036854775807ll-1)&&y==-1)return 0;return (u64)(x%y);}
__attribute__((export_name("ne_registers"))) u32 ne_registers(void){return (u32)registers;}
"#;
