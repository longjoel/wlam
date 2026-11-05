//! x86-32 lifting backed by Ghidra's Sleigh specification.
//!
//! Rather than re-deriving x86 semantics by hand (which produced a lifter that
//! passed only 16/29 semantic tests), this crate consumes the output of the
//! *same* Sleigh compiler Ghidra ships with, via `rsleigh`. The result is the
//! exact P-code Ghidra's decompiler sees: x87, flags, partial registers,
//! segment overrides and string operations are already correct there.
//!
//! ## What this crate adds
//!
//! 1. **Register naming.** Sleigh addresses registers by (offset, size);
//!    [`registers::X86_32_REGISTERS`] is generated from the live decoder so the
//!    mapping cannot drift from the specification.
//! 2. **A clean model with no ragged cases.** rsleigh's `Load`/`Store` carry a
//!    `ptr` operand that is *already the address* (verified: `mov eax,[esp+4]`
//!    yields `IntAdd` into a Unique, then `Load{ptr: that}`). [`IR`] wraps this
//!    so downstream code never re-derives it.
//! 3. **Trap and host-call surface.** Instructions whose P-code reaches the
//!    environment — `INT3` (106 sites in Uxn32), `in`/`outs`, `HLT` — are
//!    classified explicitly instead of being silently treated as no-ops.
//! 4. **Segment bases.** `mov eax, fs:[0]` lifts to
//!    `IntAdd(FS_OFFSET, 0)`; the loader supplies `FS_OFFSET = TIB_BASE`.
//!    Verified through the generated code, not assumed.
//!
//! ## User-defined ops
//!
//! Sleigh defers 257 operations to the environment (`define pcodeop ...`).
//! rsleigh exposes only a numeric `func_id` with no name table, so the names
//! here were recovered by decoding the *real* executable and reading the
//! mnemonics that produce each id. Unmapped ids are reported, never ignored.

#![forbid(unsafe_code)]

pub mod compile;
pub mod discover;
pub mod emit;
pub mod interp;
pub mod ir;
pub mod registers;

pub use registers::X86_32_REGISTERS;

use std::collections::BTreeMap;

pub use pcode_ir::{AddressSpaceId, PcodeOp, Varnode};
pub use ir::{BinOp, IR, Operand, UnOp};
pub use pcode_ir;
pub use rsleigh_api::{Architecture, Decoder};

/// Stack size used for lifting work: 64 MiB.
/// The interactive clang fixture exceeds 16 MiB on the generated decoder path.
///
/// The vendored rsleigh x86 decoder recurses on Sleigh sub-tables and needs
/// more than the default 2 MiB test-thread stack in debug builds (measured:
/// fails at 2 MiB, passes at 8 MiB for a single `call [IAT]`). Callers running
/// on a thread with the default stack should wrap lifting in
/// [`with_big_stack`] rather than silently depending on the build profile.
pub const LIFT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// Run `f` on a thread with a [`LIFT_STACK_SIZE`] stack and return its result.
///
/// # Panics
/// If the thread cannot be spawned, or if `f` panics (the panic is resumed on
/// the caller's thread).
pub fn with_big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(LIFT_STACK_SIZE)
        .spawn(f)
        .expect("spawn lifting thread")
        .join()
        .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
}

/// A user-defined pcodeop, identified by what the guest actually means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Userop {
    /// `in` / `in`-style port reads (`IN`, `INSB`, `INSD`). These reach real
    /// hardware; on the web there is nothing to read, so the adapter reports
    /// them so the host layer can decide (e.g. return 0xFF for the keyboard
    /// controller status port the classic console code polls).
    PortIn,
    /// `out` family.
    PortOut,
    /// Software breakpoint (`INT3`).
    Breakpoint,
    /// `xchg` on a memory operand, which needs a bus lock on real hardware.
    LockedXchg,
    /// `rdtsc`: a host-supplied cycle counter.
    Rdtsc,
    /// An id this build has not identified.
    Unknown(u64),
}

/// Map a Sleigh `func_id` to a userop.
///
/// The ids are not documented anywhere we can depend on: they are assigned by
/// the Sleigh compiler in declaration order and carry no names in generated
/// code. The mapping below was derived empirically (see `docs/sleigh-notes.md`)
/// and is guarded by a test that re-derives it from a real executable.
pub fn classify_userop(func_id: u64) -> Userop {
    match func_id {
        1 => Userop::PortIn,
        2 => Userop::PortOut,
        16 => Userop::Breakpoint,
        17 | 18 => Userop::LockedXchg,
        72 => Userop::Rdtsc,
        other => Userop::Unknown(other),
    }
}

/// A register operand resolved to a readable name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedRegister {
    pub offset: u64,
    pub size: u32,
    pub name: String,
}

impl NamedRegister {
    /// Look up the name of a (offset, size) pair.
    pub fn lookup(offset: u64, size: u32) -> Option<&'static str> {
        X86_32_REGISTERS
            .iter()
            .find(|(o, s, _)| *o == offset && *s == size)
            .map(|(_, _, n)| *n)
    }
}

/// One lifted instruction.
#[derive(Debug, Clone)]
pub struct LiftedInstruction {
    pub address: u64,
    pub length: u64,
    pub disassembly: String,
    pub ops: Vec<IR>,
    /// Ops that were not lowered, kept for diagnostics.
    pub unlowered: usize,
}

/// A decoded block and where it ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminator {
    /// Execution continues at `next`.
    FallsThrough,
    /// Unconditional jump.
    Jump(u64),
    /// Conditional jump. `taken` is `None` when the destination is computed at
    /// run time rather than fixed by the encoding.
    Conditional { taken: Option<u64>, not_taken: u64 },
    /// Indirect jump; the target lives in `EIP` after the block runs.
    IndirectJump,
    /// Return to the address on the stack.
    Return,
    /// A trap: `INT3` (a compiler patch point or breakpoint), `INT n`.
    Trap(u64),
    /// A `call` ends the block: control leaves for the callee and resumes at
    /// `resume`, the address of the instruction after the call.
    ///
    /// The lifted ops still contain the `Call`, so the block carries both the
    /// callee target (from that op) and the resume point.
    Call { resume: u64 },
    /// An indirect `call` (`call [slot]`, `call reg`) ends the block; control
    /// resumes at `resume`. The lifted ops carry the `CallInd` and the load
    /// that produced its target.
    CallInd { resume: u64 },
    /// Block ended because the next bytes did not decode.
    Undecodable(u64),
}

/// A basic block, ready for emission.
#[derive(Debug, Clone)]
pub struct Block {
    pub start: u64,
    pub end: u64,
    pub instructions: Vec<LiftedInstruction>,
    pub terminator: Terminator,
}

impl Block {
    /// Count of P-code ops in the block (the "P-U" measure).
    pub fn op_count(&self) -> usize {
        self.instructions.iter().map(|i| i.ops.len()).sum()
    }

    /// Count of *source* native instructions in the block (the "P-T" measure).
    ///
    /// The engineering gate for a static recompiler (as opposed to a
    /// decompiler, which is memoryless) is that every dynamic execution must
    /// correspond to at most one emitted lifted-code block. If `P-U/P-T` stays
    /// modest, a block-shaped interpreter is legitimate; if lifted ops vastly
    /// exceed native instructions, the design is really a trace interpreter.
    pub fn native_instruction_count(&self) -> usize {
        self.instructions.len()
    }
}

/// Decode and lower one basic block starting at `start`.
///
/// ## One `Decoder` per instruction
///
/// This used to decode the whole block with a single `Decoder`, and that was
/// wrong in a way that only showed up on real code. The x86 spec does not
/// decode `CALL [slot]`/`MOV reg,[abs]` (`mod=00 r/m=101`) as one internal
/// piece: `ff 15 <disp32>` splits into a one-byte `ff 15`, then a one-byte
/// `00`, then the four displacement bytes as a further piece, and each piece
/// **carries its own P-code**. Concatenating them attributed the `[disp32]`
/// load to the byte before it, so Uxn32's `CALL dword ptr [0x41106c]` at
/// 0x40ccd8 loaded from address `0x6d` (the ASCII `m`) and the guest jumped to
/// 109 -- a memory trap that looked like a loader problem.
///
/// Decoding each instruction with a fresh decoder makes every instruction's
/// P-code stand on its own, which is the only attribution that can be right.
/// It is not obviously slower: the same decode work happens, without the
/// multi-instruction state.
pub fn lift_block(bytes: &[u8], start: u64) -> Block {
    let mut instructions = Vec::new();
    let mut ip = start;
    let mut terminator = Terminator::Undecodable(start);
    loop {
        let index = match ip.checked_sub(start) {
            Some(i) => i as usize,
            None => {
                terminator = Terminator::Undecodable(ip);
                break;
            }
        };
        let rest = &bytes[index.min(bytes.len())..];
        if rest.is_empty() {
            terminator = Terminator::Undecodable(ip);
            break;
        }
        let mut decoder = Decoder::new(Architecture::X86_32);
        let mut insn = match decoder.decode_unoptimized(rest, ip) {
            Ok(i) if i.len > 0 => i,
            _ => {
                terminator = Terminator::Undecodable(ip);
                break;
            }
        };
        repair_x86_flagged_memory_address(&mut insn.ops);
        pcode_ir::optimize(&mut insn.ops);
        // The decoder spaces instruction temporaries by 64 KiB, but nested
        // Sleigh constructors allocate several such ranges. Adjacent native
        // instructions can therefore reuse the same numeric temp, even with
        // incompatible integer/float types. Give each x86 instruction a 4 GiB
        // namespace while retaining its internal subtable offsets.
        let namespace = ip.wrapping_shl(32).wrapping_sub(ip.wrapping_shl(16));
        let unique_outputs = insn.ops.iter().filter_map(pcode_ir::get_output)
            .filter(|v|v.space == AddressSpaceId::Unique).map(|v|v.offset)
            .collect::<std::collections::BTreeSet<_>>();
        for op in &mut insn.ops {
            // Indirect memory transfers may name a Unique pointer using a RAM
            // varnode. Keep that reference paired with its renamed producer.
            if let PcodeOp::BranchInd { dest } | PcodeOp::CallInd { dest } = op {
                if dest.space == AddressSpaceId::Ram && unique_outputs.contains(&dest.offset) {
                    dest.offset += namespace;
                }
            }
            pcode_ir::offset_unique_varnodes(op, namespace);
        }
        let mut ops = Vec::with_capacity(insn.ops.len());
        for op in &insn.ops {
            ops.push(crate::ir::lower(op));
        }
        // Sleigh's x87 transcendental templates sometimes return their
        // CallOther result under the following instruction's unique-temp
        // namespace, while the explicit ST0 write reads the current
        // instruction's namespace. Preserve the P-code data flow by wiring the
        // immediate ST0 copy to the CallOther output.
        for i in 0..ops.len().saturating_sub(1) {
            let result = match &ops[i] {
                IR::Userop { id: 99 | 102 | 103 | 105, dst: Some(dst), .. } => Some(dst.clone()),
                _ => None,
            };
            if let Some(result) = result {
                if let IR::Copy {
                    dst: Operand::Reg { name, .. },
                    src: Operand::Temp { .. },
                } = &ops[i + 1]
                {
                    if name.as_ref() == "ST0" {
                        if let IR::Copy { src, .. } = &mut ops[i + 1] {
                            *src = result;
                        }
                    }
                }
            }
        }
        let next = ip + insn.len;
        // `REP`-prefixed string instructions lift to a microcode loop whose
        // branches target the instruction's own start and `inst_next`, not
        // other blocks. Collapse that shape into a structured loop instead of
        // leaving both branches as unsupported ops.
        ops = collapse_micro_loop(&insn.ops, ops, ip, next);
        let mnemonic=insn.disassembly.split_whitespace().next().unwrap_or("");
        if ["STOS","SCAS","CMPS","LODS"].iter().any(|name|mnemonic.starts_with(name))
            || mnemonic.starts_with("MOVS") && insn.disassembly.contains("ES:") {
            snapshot_string_addresses(&mut ops,ip);
        }
        let unlowered = count_unlowered(&ops);
        let mut term = None;
        for op in &insn.ops {
            match op {
                // A *genuine* relative branch target arrives as a RAM-space
                // varnode holding the already-computed absolute address
                // (verified: `jmp +5` at 0x1000 gives `Branch{dest: Ram 0x1007}`).
                //
                // A CONST-space destination is NOT an address. Sleigh's x86
                // spec implements several instructions as a conditional
                // microcode path and encodes the internal branch as a constant
                // varnode: `BSR EAX,EDX` emits `CBranch{dest: Const 2}`, which
                // means "the spec's internal label 2", not "address 2".
                // Treating it as an address invented 70 phantom branch targets
                // below the image base on Minesweeper (0x1 and 0x2), which made
                // unrelated functions appear to share blocks. Such an op is
                // internal control flow: it does not terminate the block, so
                // decoding continues past it.
                PcodeOp::Branch { dest } => match dest.space {
                    AddressSpaceId::Ram => term = Some(Terminator::Jump(dest.offset)),
                    AddressSpaceId::Const => {}
                    _ => term = Some(Terminator::IndirectJump),
                },
                PcodeOp::BranchInd { .. } => term = Some(Terminator::IndirectJump),
                PcodeOp::CBranch { dest, .. } => match dest.space {
                    AddressSpaceId::Ram => {
                        term = Some(Terminator::Conditional {
                            taken: Some(dest.offset),
                            not_taken: next,
                        });
                    }
                    AddressSpaceId::Const => {}
                    _ => {
                        term = Some(Terminator::Conditional {
                            taken: None,
                            not_taken: next,
                        });
                    }
                },
                PcodeOp::Return { .. } => term = Some(Terminator::Return),
                // A call leaves this block for the callee and resumes at the
                // next instruction, so it is a block boundary. Without this the
                // lifter ran straight through the call and the emitter had no
                // place to put the `call`.
                PcodeOp::Call { .. } => term = Some(Terminator::Call { resume: next }),
                // `next` is `ip + insn.len`, the instruction after the call,
                // because every instruction is decoded on its own (see the
                // note on `lift_block`). It used to be the end of the current
                // internal piece, which for `CALL [slot]` is one byte into the
                // displacement.
                PcodeOp::CallInd { .. } => term = Some(Terminator::CallInd { resume: next }),
                PcodeOp::CallOther { func_id, .. } if classify_userop(*func_id) == Userop::Breakpoint => {
                    term = Some(Terminator::Trap(next));
                }
                _ => {}
            }
            if term.is_some() {
                break;
            }
        }
        instructions.push(LiftedInstruction {
            address: ip,
            length: insn.len,
            disassembly: insn.disassembly,
            ops,
            unlowered,
        });
        ip = next;
        if term.is_some() {
            terminator = term.unwrap();
            break;
        }
    }
    Block { start, end: ip, instructions, terminator }
}

/// The x86-32 address width. A `Const` branch destination of this size is a
/// real instruction address; Sleigh encodes its own microcode labels as
/// 8-byte constants (see `const_space_branch_is_not_a_terminator`).
const ADDRESS_SIZE: u32 = 4;

/// A `Branch`/`CBranch` whose destination is an address-sized constant.
///
/// The returned condition is `None` for an unconditional branch.
fn const_address_branch(op: &PcodeOp) -> Option<(u64, Option<&Varnode>)> {
    match op {
        PcodeOp::Branch { dest }
            if dest.space == AddressSpaceId::Const && dest.size == ADDRESS_SIZE =>
        {
            Some((dest.offset, None))
        }
        PcodeOp::CBranch { dest, cond }
            if dest.space == AddressSpaceId::Const && dest.size == ADDRESS_SIZE =>
        {
            Some((dest.offset, Some(cond)))
        }
        _ => None,
    }
}

fn is_transfer(op: &PcodeOp) -> bool {
    matches!(
        op,
        PcodeOp::Branch { .. }
            | PcodeOp::CBranch { .. }
            | PcodeOp::BranchInd { .. }
            | PcodeOp::Call { .. }
            | PcodeOp::CallInd { .. }
            | PcodeOp::Return { .. }
    )
}

/// Turn Sleigh's flattened `REP` microcode loop into a structured loop.
///
/// `REP MOVS`/`STOS`/`LODS` and friends lift to:
///
/// ```text
/// head:     cond = (ECX == 0)
///           if cond goto inst_next      ; exit when the count is exhausted
/// body:     ...one element copy...
///           goto inst_start             ; loop back
/// ```
///
/// `REPE`/`REPNE CMPS`/`SCAS` replace the unconditional back edge with a
/// second conditional branch. Both shapes are reducible to
/// [`IR::MicroLoop`]. Anything else is left as the explicit
/// [`IR::Unsupported`] ops the lifter produced, so an unknown microcode shape
/// still traps by name rather than being executed with invented semantics.
fn collapse_micro_loop(raw: &[PcodeOp], lowered: Vec<IR>, start: u64, next: u64) -> Vec<IR> {
    let branches: Vec<(usize, u64, Option<&Varnode>)> = raw
        .iter()
        .enumerate()
        .filter_map(|(i, op)| const_address_branch(op).map(|(target, cond)| (i, target, cond)))
        .collect();
    if branches.is_empty() {
        return lowered;
    }
    // Only edges that stay inside this instruction are understood. A constant
    // address somewhere else is an unknown microcode shape, not a block edge.
    if branches.iter().any(|(_, target, _)| *target != start && *target != next) {
        return lowered;
    }
    if branches.len() != 2 {
        return lowered;
    }
    let (exit_index, exit_target, exit_cond) = branches[0];
    let (repeat_index, repeat_target, repeat_cond) = branches[1];
    // `REP` always tests the counter before the body; a forward branch with no
    // condition would be a different (skip) pattern.
    if exit_target != next || repeat_target != start || exit_index >= repeat_index {
        return lowered;
    }
    let Some(exit_cond) = exit_cond else {
        return lowered;
    };
    // No other transfer may sit in the middle: the two branches above are only
    // a complete loop when they are the only control flow in the instruction.
    if raw
        .iter()
        .enumerate()
        .any(|(i, op)| i != exit_index && i != repeat_index && is_transfer(op))
    {
        return lowered;
    }

    vec![IR::MicroLoop {
        head: lowered[..exit_index].to_vec(),
        exit: Some(Operand::from_varnode(exit_cond)),
        body: lowered[exit_index + 1..repeat_index].to_vec(),
        repeat: repeat_cond.map(Operand::from_varnode),
        tail: lowered[repeat_index + 1..].to_vec(),
    }]
}

/// String-address subtables update ESI/EDI before their exported memory
/// reference is consumed. Freeze their input values once per iteration, so
/// accesses use the old address while the architectural registers advance.
///
/// The generated x86 SBB/ADC P-code can reuse the same unique varnode for a
/// lazy memory operand's address and the widened carry flag. Preserve the
/// earlier effective address for RAM accesses after that flag copy; arithmetic
/// uses of the varnode still need the carry value.
fn repair_x86_flagged_memory_address(ops: &mut [PcodeOp]) {
    use pcode_ir::{AddressSpaceId, PcodeOp as P};

    let carry_temps = ops
        .iter()
        .filter_map(|op| match op {
            P::IntZext { out, input }
                if input.space == AddressSpaceId::Register && input.offset == 512 =>
            {
                Some(out.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    for i in 0..ops.len() {
        let (address_temp, carry_temp) = match &ops[i] {
            P::Copy { out, input }
                if out.space == AddressSpaceId::Unique && carry_temps.contains(input) =>
            {
                (out.clone(), input.clone())
            }
            _ => continue,
        };
        let Some(address) = ops[..i].iter().rev().find_map(|op| match op {
            P::Copy { out, input } if *out == address_temp && *input != carry_temp => {
                Some(input.clone())
            }
            _ => None,
        }) else {
            continue;
        };
        for op in &mut ops[i + 1..] {
            match op {
                P::Load { ptr, .. } | P::Store { ptr, .. } if *ptr == address_temp => {
                    *ptr = address.clone();
                }
                _ => {}
            }
        }
    }
}

fn snapshot_string_addresses(ops:&mut Vec<IR>,instruction:u64) {
    let mut used=std::collections::BTreeSet::new();
    let rewrite=|operand:&mut Operand,used:&mut std::collections::BTreeSet<&'static str>| {
        let register=match operand {Operand::Reg {name,..} if name.as_ref()=="ESI"=>"ESI",Operand::Reg {name,..} if name.as_ref()=="EDI"=>"EDI",_=>return};
        used.insert(register);
        *operand=Operand::Temp {offset:(instruction<<32)|if register=="ESI" {0x80000000} else {0x80000004},size:4};
    };
    for op in ops.iter_mut() {
        match op {
            IR::Copy {src,..}|IR::Un {src,..}|IR::Convert {src,..}|IR::FloatUnary {src,..}|IR::Subpiece {src,..}=>rewrite(src,&mut used),
            IR::Load {addr,..}=>rewrite(addr,&mut used),
            IR::Store {addr,value,..}=>{rewrite(addr,&mut used);rewrite(value,&mut used);}
            IR::Bin {operands,..}|IR::Float {operands,..}=>{rewrite(&mut operands.0,&mut used);rewrite(&mut operands.1,&mut used);}
            IR::MicroLoop {body,..}=>snapshot_string_addresses(body,instruction),
            _=>{}
        }
    }
    let snapshots=used.into_iter().map(|name|IR::Copy {
        dst:Operand::Temp {offset:(instruction<<32)|if name=="ESI" {0x80000000} else {0x80000004},size:4},
        src:Operand::Reg {name:name.into(),size:4},
    }).collect::<Vec<_>>();
    ops.splice(0..0,snapshots);
}

/// Count unsupported ops, including those nested in reconstructed microcode
/// loops, so per-instruction diagnostics stay truthful.
fn count_unlowered(ops: &[IR]) -> usize {
    ops.iter()
        .map(|op| match op {
            IR::Unsupported(_) => 1,
            IR::MicroLoop { head, body, tail, .. } => {
                count_unlowered(head) + count_unlowered(body) + count_unlowered(tail)
            }
            _ => 0,
        })
        .sum()
}

impl IR {
    /// Human-readable form, e.g. `EAX = EAX + 0x5` or `CF = Carry(EAX, 0x5)`.
    ///
    /// Used by discovery (surface syntax is far more robust to read than
    /// re-parsing it from operand fields) and by diagnostics.
    pub fn text(&self) -> String {
        fn o(op: &Operand) -> String {
            match op {
                Operand::Const { value, .. } => format!("{value:#x}"),
                Operand::Reg { name, .. } => name.to_string(),
                Operand::Temp { offset, .. } => format!("t{offset:x}"),
                Operand::Ram { offset, .. } => format!("ram:{offset:#x}"),
            }
        }
        match self {
            IR::Copy { dst, src } => format!("{} = {}", o(dst), o(src)),
            IR::Load { dst, addr, size } => format!("{} = *{}{{{size}}}", o(dst), o(addr)),
            IR::Store { addr, value, size } => format!("*{}{{{size}}} = {}", o(addr), o(value)),
            IR::Bin { op, dst, operands } => {
                format!("{} = {op:?}({}, {})", o(dst), o(&operands.0), o(&operands.1))
            }
            IR::Float { op, dst, operands } => {
                // `~` marks a real-number operation so a dumped block cannot be
                // mistaken for integer arithmetic with a same-named mnemonic.
                format!("{} = ~{op:?}({}, {})", o(dst), o(&operands.0), o(&operands.1))
            }
            IR::FloatUnary { op, dst, src } => format!("{} = ~{op:?}({})", o(dst), o(src)),
            IR::Un { op, dst, src } => format!("{} = {op:?}({})", o(dst), o(src)),
            IR::Convert { op, dst, src } => format!("{} = {op:?}({})", o(dst), o(src)),
            IR::Subpiece { dst, src, lsb } => format!("{} = {}.{lsb}", o(dst), o(src)),
            IR::Branch { target } => format!("goto {}", o(target)),
            IR::IndirectJump { target } => format!("goto *{}", o(target)),
            IR::CBranch { cond, target } => format!("if {} goto {}", o(cond), o(target)),
            IR::Call { target } => format!("call {}", o(target)),
            IR::CallInd { target } => format!("call *{}", o(target)),
            IR::Ret { target } => match target {
                Some(t) => format!("return {}", o(t)),
                None => "return".to_string(),
            },
            IR::Userop { args, dst, id, .. } => {
                let a: Vec<String> = args.iter().map(o).collect();
                // Classify here: the IR variant stays free of the userop table
                // so the emitter's copy of the contract does not need it.
                let call = format!("{:?}({})", crate::classify_userop(*id), a.join(", "));
                match dst {
                    Some(d) => format!("{} = {call}  ; userop {id}", o(d)),
                    None => format!("{call}  ; userop {id}"),
                }
            }
            IR::MicroLoop { head, exit, body, repeat, tail } => {
                let text = |ops: &[IR]| {
                    ops.iter().map(|op| op.text()).collect::<Vec<_>>().join("; ")
                };
                let exit = exit.as_ref().map_or_else(String::new, |c| {
                    format!(", exit if {}", o(c))
                });
                let repeat = repeat.as_ref().map_or_else(
                    || "repeat".to_string(),
                    |c| format!("repeat if {}", o(c)),
                );
                format!(
                    "micro-loop {{ {}{exit}; {}; {repeat} }} tail {{ {} }}",
                    text(head),
                    text(body),
                    text(tail)
                )
            }
            IR::Unsupported(op) => format!("unsupported: {op}"),
            IR::Nop => "nop".to_string(),
        }
    }
}

/// Statistics over a lifted image: which userops and which Sleigh ops appear.
#[derive(Debug, Default)]
pub struct Coverage {
    pub instructions: u64,
    pub undecodable: u64,
    pub userops: BTreeMap<u64, BTreeMap<String, u64>>,
}

impl Coverage {
    /// Userop ids seen, with the mnemonics that produced them.
    pub fn unknown_userops(&self) -> impl Iterator<Item = (&u64, &BTreeMap<String, u64>)> {
        self.userops
            .iter()
            .filter(|(id, _)| matches!(classify_userop(**id), Userop::Unknown(_)))
    }
}

/// Walk every executable byte range of `bytes` (starting at `base`) and report
/// coverage. Undecodable bytes are skipped one at a time so decoding resyncs.
pub fn scan(bytes: &[u8], base: u64) -> Coverage {
    let mut decoder = Decoder::new(Architecture::X86_32);
    let mut cov = Coverage::default();
    let mut index = 0usize;
    let mut ip = base;
    while index < bytes.len() {
        match decoder.decode(&bytes[index..], ip) {
            Ok(insn) if insn.len > 0 => {
                cov.instructions += 1;
                for op in &insn.ops {
                    if let PcodeOp::CallOther { func_id, .. } = op {
                        let mnemonic = insn.disassembly.split(' ').next().unwrap_or("?").to_string();
                        *cov.userops.entry(*func_id).or_default().entry(mnemonic).or_insert(0) += 1;
                    }
                }
                index += insn.len as usize;
                ip += insn.len;
            }
            _ => {
                cov.undecodable += 1;
                index += 1;
                ip += 1;
            }
        }
    }
    cov
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruction_temporaries_have_disjoint_namespaces() {
        let lifted = block(&[0xd9,0xeb,0xa5,0xa5,0xdd,0x5d,0x8a,0xc3], 0x1000);
        let mut count = 0;
        for instruction in &lifted.instructions {
            for op in &instruction.ops {
                let destination = match op {
                    IR::Copy { dst, .. } | IR::Load { dst, .. } |
                    IR::Bin { dst, .. } | IR::Convert { dst, .. } => Some(dst),
                    _ => None,
                };
                if let Some(Operand::Temp { offset, .. }) = destination {
                    assert_eq!(offset >> 32, instruction.address);
                    count += 1;
                }
            }
        }
        assert!(count > 3);
    }

    /// Bytes lifted in these tests are hand-assembled from the Intel encoding
    /// tables; expected results are derived from the ISA definition.
    fn lift(bytes: &[u8], at: u64) -> Vec<IR> {
        let owned = bytes.to_vec();
        with_big_stack(move || {
            lift_block(&owned, at).instructions.into_iter().flat_map(|i| i.ops).collect()
        })
    }

    /// Lift a block on a big-enough stack (see [`LIFT_STACK_SIZE`]).
    fn block(bytes: &[u8], at: u64) -> Block {
        let owned = bytes.to_vec();
        with_big_stack(move || lift_block(&owned, at))
    }

    #[test]
    fn mov_immediate_is_a_single_copy() {
        // mov eax, 0x11223344 (b8 imm32)
        let ops = lift(&[0xb8, 0x44, 0x33, 0x22, 0x11], 0x1000);
        assert_eq!(
            ops,
            vec![IR::Copy {
                dst: Operand::Reg { name: "EAX".into(), size: 4 },
                src: Operand::Const { value: 0x1122_3344, size: 4 },
            }]
        );
    }

    #[test]
    fn add_accumulator_updates_flags() {
        // add eax, 5 (83 c0 05): EAX is read, result written, flags updated.
        let ops = lift(&[0x83, 0xc0, 0x05], 0x1000);
        let writes_eax = ops.iter().any(|o| match o {
            IR::Bin { dst: Operand::Reg { name, .. }, op: BinOp::Add, .. } => &**name == "EAX",
            _ => false,
        });
        // Sleigh writes flags directly as arithmetic results (e.g. CF is the
        // IntCarry multiplier output), not through an intermediate Copy.
        let writes = |o: &IR| -> Option<Box<str>> {
            match o {
                IR::Copy { dst: Operand::Reg { name, .. }, .. }
                | IR::Bin { dst: Operand::Reg { name, .. }, .. }
                | IR::Un { dst: Operand::Reg { name, .. }, .. } => Some(name.clone()),
                _ => None,
            }
        };
        let flag_writes: Vec<Box<str>> = ops
            .iter()
            .filter_map(writes)
            .filter(|n| matches!(&**n, "ZF" | "SF" | "CF" | "OF" | "PF"))
            .collect();
        assert_eq!(flag_writes.len(), 5, "ADD must define all five flags: {ops:?}");
        assert!(writes_eax, "EAX must be written: {ops:?}");

    }

    #[test]
    fn memory_operand_lowers_to_load_with_computed_address() {
        // mov eax, [esp+4] (8b 44 24 04)
        //
        // Real lift (the address is threaded through two temps, which is what
        // the spec actually produces):
        //   t = Add(0x4, ESP)
        //   t2 = Zext(t)
        //   t3 = Add(0x0, t2)
        //   EAX = *t3{4}
        // So the test follows the address producers back until it finds the
        // base register, rather than assuming one particular temp shape.
        let ops = lift(&[0x8b, 0x44, 0x24, 0x04], 0x1000);
        let (addr, size) = ops
            .iter()
            .find_map(|o| match o {
                IR::Load { addr, size, .. } => Some((addr, *size)),
                _ => None,
            })
            .expect("a Load");
        assert_eq!(size, 4);
        assert!(
            address_uses(&ops, addr, "ESP"),
            "the computed address must derive from ESP: {ops:?}"
        );
    }

    /// True if `seed` and every op that produced it (transitively) mention `reg`.
    fn address_uses(ops: &[IR], seed: &Operand, reg: &str) -> bool {
        let mut pending = vec![seed.clone()];
        let mut seen = std::collections::BTreeSet::new();
        while let Some(op) = pending.pop() {
            if format!("{op:?}").contains(reg) {
                return true;
            }
            let Operand::Temp { offset, .. } = &op else { continue };
            if !seen.insert(*offset) {
                continue;
            }
            for ir in ops {
                let dst_is = |d: &Operand| matches!(d, Operand::Temp { offset: o, .. } if o == offset);
                match ir {
                    IR::Bin { dst, operands, .. } if dst_is(dst) => {
                        pending.push(operands.0.clone());
                        pending.push(operands.1.clone());
                    }
                    IR::Un { dst, src, .. } | IR::Subpiece { dst, src, .. } | IR::Copy { dst, src } if dst_is(dst) => {
                        pending.push(src.clone())
                    }
                    _ => {}
                }
            }
        }
        false
    }

    #[test]
    fn fs_relative_access_adds_a_segment_base() {
        // mov eax, fs:[0] (64 a1 00000000) -- the MSVC SEH prologue idiom.
        let ops = lift(&[0x64, 0xa1, 0x00, 0x00, 0x00, 0x00], 0x1000);
        let has_segment_base = ops.iter().any(|o| match o {
            IR::Bin { op: BinOp::Add, operands, .. } => {
                format!("{operands:?}").contains("FS_OFFSET")
            }
            _ => false,
        });
        assert!(has_segment_base, "fs: access must add FS_OFFSET, got {ops:?}");
        assert!(ops.iter().any(|o| matches!(o, IR::Load { .. })), "and must load");
    }

    #[test]
    fn an_fs_store_goes_through_the_tib() {
        // mov dword ptr fs:[0], esp (64 89 25 00000000) -- the SEH link.
        //
        // Sleigh's x86 spec adds the segment base only for the `seg:[base +
        // index*ss + disp]` forms (the `highseg` bit). `fs:[disp32]` is the
        // `seg:disp32` form, where upstream skips the base, so this store went
        // to address 0 -- 0xFFFFFFFF's neighbour rather than the TIB. The
        // spec is patched (see `LOCAL PATCH (wlam)` in `ia.sinc`); this pins
        // that the patch is still in force after a table regeneration.
        let ops = lift(&[0x64, 0x89, 0x25, 0x00, 0x00, 0x00, 0x00], 0x1000);
        let (addr, value) = ops
            .iter()
            .find_map(|o| match o {
                IR::Store { addr, value, size: 4 } => Some((addr.clone(), value.clone())),
                _ => None,
            })
            .expect("a Store");
        assert!(
            matches!(&value, Operand::Reg { name, .. } if &**name == "ESP"),
            "the stored value is ESP: {ops:?}"
        );
        assert!(
            address_uses(&ops, &addr, "FS_OFFSET"),
            "an fs: store must add FS_OFFSET: {ops:?}"
        );
    }

    #[test]
    fn every_fs_slot_is_relative_to_the_tib() {
        // Address 0 is the DOS interrupt vector area and 0x7FFE0000 is a real
        // x86 system page. Both are absolute displacements, and both are
        // exactly the places a Windows program addresses through fs:.
        for disp in [0x0000_0000u32, 0x7FFE_0000] {
            let bytes = [0x64, 0x8b, 0x05, disp.to_le_bytes()[0], disp.to_le_bytes()[1], disp.to_le_bytes()[2], disp.to_le_bytes()[3]];
            let ops = lift(&bytes, 0x1000);
            let (addr, _) = ops
                .iter()
                .find_map(|o| match o {
                    IR::Load { addr, size: 4, .. } => Some((addr.clone(), ())),
                    _ => None,
                })
                .expect("a Load");
            assert!(
                address_uses(&ops, &addr, "FS_OFFSET"),
                "fs:[{disp:#x}] must be relative to the TIB: {ops:?}"
            );
        }
    }

    #[test]
    fn an_absolute_store_is_not_redirected_to_address_zero() {
        // mov dword ptr [0x41106c], eax (89 05 6c 10 41 00). With a zero DS
        // base the result must still be the literal address -- the patch must
        // not turn every absolute access into a TIB-relative one.
        let ops = lift(&[0x89, 0x05, 0x6c, 0x10, 0x41, 0x00], 0x1000);
        let addr = ops
            .iter()
            .find_map(|o| match o {
                IR::Store { addr, .. } => Some(addr.clone()),
                _ => None,
            })
            .expect("a Store");
        // Either the address is literally the constant, or it is computed by
        // an op that adds zero to it (a zero DS base folds to a no-op).
        if let Operand::Const { value, .. } = &addr {
            assert_eq!(*value, 0x41106c, "the literal address must survive: {ops:?}");
            return;
        }
        assert!(
            address_uses(&ops, &addr, "0x41106c"),
            "the literal address must survive the segment patch: {ops:?}"
        );
    }

    #[test]
    fn call_through_iat_lowers_to_indirect_call() {
        // call dword ptr [0x41106c] (ff 15 6c104100)
        let block = block(&[0xff, 0x15, 0x6c, 0x10, 0x41, 0x00], 0x40ccd8);
        let is_call_ind = block.instructions.iter().flat_map(|i| &i.ops).any(|o| matches!(o, IR::CallInd { .. }));
        assert!(is_call_ind, "{:?}", block.instructions);
        // The return address is pushed, so ESP is adjusted by a Store.
        let stores = block.instructions.iter().flat_map(|i| &i.ops).filter(|o| matches!(o, IR::Store { .. })).count();
        assert_eq!(stores, 1, "call must push exactly one return address");
    }

    #[test]
    fn conditional_jump_yields_both_successors() {
        // cmp eax, 3 ; jb rel8 (83 f8 03 72 05) at 0x1000 -> taken 0x100a
        let block = block(&[0x83, 0xf8, 0x03, 0x72, 0x05], 0x1000);
        assert_eq!(
            block.terminator,
            // jb rel8 sits at 0x1003, is 2 bytes, rel8 = +5 -> taken 0x100a;
            // falling through continues at the jump's own end address, 0x1005.
            Terminator::Conditional { taken: Some(0x100a), not_taken: 0x1005 }
        );
    }

    #[test]
    fn ret_terminates_and_pops() {
        let block = block(&[0xc3], 0x1000);
        assert_eq!(block.terminator, Terminator::Return);
        let fills_eip = format!("{:?}", block.instructions);
        assert!(fills_eip.contains("EIP") || fills_eip.contains("ESP"), "{fills_eip}");
    }

    #[test]
    fn int3_is_a_trap_not_a_noop() {
        let block = block(&[0xcc], 0x1000);
        assert_eq!(block.terminator, Terminator::Trap(0x1001));
        let userop = block.instructions[0]
            .ops
            .iter()
            .find_map(|o| match o {
                IR::Userop { id, .. } => Some(crate::classify_userop(*id)),
                _ => None,
            })
            .expect("INT3 is an environment operation");
        assert_eq!(userop, Userop::Breakpoint);
    }

    #[test]
    fn undecodable_bytes_are_reported() {
        // 0x0f 0xff is not a valid x86 encoding.
        let block = block(&[0x0f, 0xff], 0x1000);
        assert_eq!(block.terminator, Terminator::Undecodable(0x1000));
        assert!(block.instructions.is_empty());
    }

    #[test]
    fn register_names_come_from_generated_metadata() {
        assert_eq!(NamedRegister::lookup(0x0, 4), Some("EAX"));
        assert_eq!(NamedRegister::lookup(0x10, 4), Some("ESP"));
        assert_eq!(NamedRegister::lookup(0x110, 4), Some("FS_OFFSET"));
        assert_eq!(NamedRegister::lookup(0x9999, 4), None);
    }

    #[test]
    fn every_register_entry_is_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for (off, size, name) in X86_32_REGISTERS {
            assert!(seen.insert((*off, *size)), "{name} duplicates offset {off:#x}/{size}");
            assert!(!name.is_empty());
            assert!(*size > 0);
        }
    }

    /// A `Const`-space branch destination is an internal label, not an address.
    ///
    /// Sleigh compiles several x86 instructions as a conditional microcode path
    /// and encodes the internal jump as a constant varnode. `BSR EAX,EDX`
    /// (`0f bd c2`) is the canonical case: the spec tests the source for zero
    /// and emits `CBranch { dest: Const 2 }`, meaning "the spec's own label 2".
    /// Reading that as address `0x2` invented blocks below the image base. The
    /// instruction is not a branch, so the block must keep decoding past it.
    #[test]
    fn const_space_branch_is_not_a_terminator() {
        // BSR EAX,EDX ; XOR EAX,0x1f ; RET
        let bytes = [0x0f, 0xbd, 0xc2, 0x83, 0xf0, 0x1f, 0xc3];
        let block = with_big_stack(move || lift_block(&bytes, 0x401000));

        // The block runs to the RET, not to a phantom branch after the BSR.
        match block.terminator {
            Terminator::Return => {}
            ref other => panic!("BSR must not terminate the block; got {other:?}"),
        }
        assert_eq!(block.instructions.len(), 3, "BSR, XOR and RET must all be lifted");
        assert_eq!(block.end, 0x401007);
    }

    /// The complement of the test above: a real relative branch *is* a
    /// terminator, and its target is the resolved absolute address.
    ///
    /// Without this, "ignore every branch" would pass the test above while
    /// breaking all control flow.
    #[test]
    fn ram_space_branch_is_still_a_terminator() {
        // JNZ rel8 -0x04 at 0x401000 -> target 0x400ffe ; plus padding.
        let bytes = [0x75, 0xfc, 0x90, 0x90];
        let block = with_big_stack(move || lift_block(&bytes, 0x401000));
        match block.terminator {
            Terminator::Conditional { taken: Some(t), not_taken } => {
                assert_eq!(t, 0x400ffe, "the branch target must be the resolved absolute address");
                assert_eq!(not_taken, 0x401002, "fall-through is the next instruction");
            }
            ref other => panic!("JNZ must be a conditional terminator; got {other:?}"),
        }
        assert_eq!(block.instructions.len(), 1, "the jump ends the block");
    }

    /// x87 arithmetic must survive lifting as a real operation.
    ///
    /// `FMUL ST1` (`d8 c9`) lowers to `FloatMult { ST0 = ST0 * ST1 }`. Before
    /// the `IR::Float` arms existed it became `Nop`, so the multiplication
    /// silently did not happen and the guest read a stale `ST0`. The block
    /// reported `unlowered = 1`, which is the signal this asserts on.
    #[test]
    fn x87_multiply_lifts_to_a_real_operation() {
        let bytes = [0xd8, 0xc9, 0xc3]; // FMUL ST1 ; RET
        let block = with_big_stack(move || lift_block(&bytes, 0x401000));

        let mul = block
            .instructions
            .iter()
            .flat_map(|i| i.ops.iter())
            .find_map(|op| match op {
                IR::Float { op, dst, operands } => Some((*op, dst, operands)),
                _ => None,
            })
            .expect("FMUL must lift to IR::Float, not Nop");

        assert_eq!(mul.0, crate::ir::FloatOp::Mul, "the operation is a multiply");
        assert!(
            matches!(mul.1, Operand::Reg { ref name, .. } if &**name == "ST0"),
            "result goes to ST0, got {:?}",
            mul.1
        );
        assert!(
            matches!(mul.2 .1, Operand::Reg { ref name, .. } if &**name == "ST1"),
            "right operand is ST1, got {:?}",
            mul.2 .1
        );
        assert_eq!(
            block.instructions[0].unlowered, 0,
            "no op may be dropped: that is the bug this guards"
        );
    }

    /// `FSTP m64` must produce a real -> real conversion, not a bare store.
    ///
    /// It lowers to `Float2Float { tmp(64) = ST0(80) }` followed by a store.
    /// Without the `Convert` arm the store received an untyped temp and the
    /// emitter reported "i64 store".
    #[test]
    fn x87_store_lifts_to_a_float_conversion() {
        // FSTP qword ptr [esp] ; RET
        let bytes = [0xdd, 0x1c, 0x24, 0xc3];
        let block = with_big_stack(move || lift_block(&bytes, 0x401000));

        let conv = block
            .instructions
            .iter()
            .flat_map(|i| i.ops.iter())
            .find_map(|op| match op {
                IR::Convert { op, src, .. } => Some((*op, src)),
                _ => None,
            })
            .expect("FSTP must lift to IR::Convert");

        assert_eq!(conv.0, crate::ir::ConvertOp::FloatToFloat);
        assert!(
            matches!(conv.1, Operand::Reg { ref name, .. } if &**name == "ST0"),
            "the source is the x87 stack top, got {:?}",
            conv.1
        );
        assert_eq!(block.instructions[0].unlowered, 0);
    }
}
