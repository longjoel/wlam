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

pub mod discover;
pub mod registers;

pub use registers::X86_32_REGISTERS;

use std::collections::BTreeMap;

pub use pcode_ir::{AddressSpaceId, PcodeOp, Varnode};
pub use rsleigh_api::{Architecture, Decoder};

/// Stack size used for lifting work: 16 MiB.
///
/// The vendored rsleigh x86 decoder recurses on Sleigh sub-tables and needs
/// more than the default 2 MiB test-thread stack in debug builds (measured:
/// fails at 2 MiB, passes at 8 MiB for a single `call [IAT]`). Callers running
/// on a thread with the default stack should wrap lifting in
/// [`with_big_stack`] rather than silently depending on the build profile.
pub const LIFT_STACK_SIZE: usize = 16 * 1024 * 1024;

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

/// Send-side view of the IR: every varnode is classified once, so consumers
/// (emitter, interpreter) do not repeat the "is this a register / a temp / an
/// address" branching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operand {
    /// An immediate.
    Const { value: u64, size: u32 },
    /// A register local, by name. The name is boxed deliberately: `Operand`
    /// appears inline several times per `IR` value, and a large inline
    /// variant multiplies the size (and stack usage) of every IR node.
    Reg { name: Box<str>, size: u32 },
    /// A per-instruction temporary.
    Temp { offset: u64, size: u32 },
    /// A guest memory location, used directly as an address.
    ///
    /// Sleigh keeps memory operands in the RAM space; such a varnode *is* an
    /// address, so there is no pointer indirection to model here. (An earlier
    /// version wrapped a nested `Operand`, which made `Debug`/`PartialEq`
    /// recurse forever — a stack overflow, not a theoretical concern.)
    Ram { offset: u64, size: u32 },
}

impl Operand {
    /// Size in bytes.
    pub fn size(&self) -> u32 {
        match self {
            Operand::Const { size, .. }
            | Operand::Reg { size, .. }
            | Operand::Temp { size, .. }
            | Operand::Ram { size, .. } => *size,
        }
    }

    /// Is this an address in guest memory (as opposed to a value)?
    pub fn is_memory(&self) -> bool {
        matches!(self, Operand::Ram { .. })
    }

    fn from_varnode(v: &Varnode) -> Operand {
        match v.space {
            AddressSpaceId::Const => Operand::Const { value: v.offset, size: v.size },
            AddressSpaceId::Register => {
                let name: Box<str> = match NamedRegister::lookup(v.offset, v.size) {
                    Some(n) => n.into(),
                    None => format!("reg_{:x}_{}", v.offset, v.size).into_boxed_str(),
                };
                Operand::Reg { name, size: v.size }
            }
            AddressSpaceId::Unique => Operand::Temp { offset: v.offset, size: v.size },
            AddressSpaceId::Ram => Operand::Ram { offset: v.offset, size: v.size },
        }
    }
}

/// Sleigh-space IR: every op with its operands classified.
#[derive(Debug, Clone, PartialEq)]
pub enum IR {
    Copy { dst: Operand, src: Operand },
    Load { dst: Operand, addr: Operand, size: u32 },
    Store { addr: Operand, value: Operand, size: u32 },
    /// Three-operand and two-operand forms are boxed as a pair: inlining them
    /// would make every `IR` value large enough to exhaust a 2 MiB test-thread
    /// stack during recursion in `Debug`/`PartialEq`.
    Bin { op: BinOp, dst: Operand, operands: Box<(Operand, Operand)> },
    Un { op: UnOp, dst: Operand, src: Operand },
    /// Extract bytes `[lsb, lsb+sizeof(dst))` of `src`.
    Subpiece { dst: Operand, src: Operand, lsb: u32 },
    /// Unconditional branch to a destination fixed by the encoding.
    Branch { target: Operand },
    /// Branch through a register or memory cell: a jump table, a computed
    /// goto, or an IAT-resolved thunk tail. Requires target resolution.
    IndirectJump { target: Operand },
    /// Conditional branch.
    CBranch { cond: Operand, target: Operand },
    /// Call to a constant address (a known guest function).
    Call { target: Operand },
    /// Call through a computed destination, typically `call [IAT]`.
    CallInd { target: Operand },
    /// Return; `target` is the popped return address.
    Ret { target: Option<Operand> },
    /// An environment operation.
    Userop { id: u64, kind: Userop, args: Vec<Operand>, dst: Option<Operand> },
    /// No-observable-effect operation.
    Nop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    /// Multiplier result: the full product, twice the operand width.
    MulFull,
    /// Carry out of an unsigned addition.
    Carry,
    /// Signed carry (overflow) out of an addition.
    SCarry,
    /// Signed borrow out of a subtraction.
    SBorrow,
    DivU,
    RemU,
    Sub,
    Mul,
    DivS,
    RemS,
    Eq,
    NotEq,
    LessU,
    LessEqU,
    LessS,
    LessEqS,
    And,
    Or,
    Xor,
    Lsh,
    Rsh,
    Asr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Popcount,
    Lzcount,
    Not,
    Zext,
    Sext,
}

fn operand(v: &Varnode) -> Operand {
    Operand::from_varnode(v)
}

fn store_size(op: &PcodeOp) -> u32 {
    match op {
        PcodeOp::Store { val, .. } => val.size,
        _ => 0,
    }
}

/// Translate rsleigh's `PcodeOp` into [`IR`].
pub fn lower(op: &PcodeOp) -> IR {
    use PcodeOp as P;
    match op {
        P::Copy { out, input } => IR::Copy { dst: operand(out), src: operand(input) },
        P::Load { out, ptr, .. } => IR::Load { dst: operand(out), addr: operand(ptr), size: out.size },
        P::Store { ptr, val, .. } => {
            IR::Store { addr: operand(ptr), value: operand(val), size: val.size }
        }
        P::Branch { dest } => IR::Branch { target: operand(dest) },
        P::CBranch { dest, cond } => IR::CBranch { cond: operand(cond), target: operand(dest) },
        // An *indirect* branch takes its destination from a register or
        // memory, which is a jump table or a computed goto. Lowering it as a
        // plain Branch would silently drop the indirection.
        P::BranchInd { dest } => IR::IndirectJump { target: operand(dest) },
        P::Call { dest } => IR::Call { target: operand(dest) },
        P::CallInd { dest } => IR::CallInd { target: operand(dest) },
        P::Return { dest } => IR::Ret { target: Some(operand(dest)) },
        P::CallOther { out, func_id, inputs } => IR::Userop {
            id: *func_id,
            kind: classify_userop(*func_id),
            args: inputs.iter().map(operand).collect(),
            dst: out.as_ref().map(operand),
        },
        P::IntAdd { out, left, right } => bin(BinOp::Add, out, left, right),
        P::IntSub { out, left, right } => bin(BinOp::Sub, out, left, right),
        P::IntMult { out, left, right } => bin(BinOp::Mul, out, left, right),
        P::IntSDiv { out, left, right } => bin(BinOp::DivS, out, left, right),
        P::IntSRem { out, left, right } => bin(BinOp::RemS, out, left, right),
        P::IntEq { out, left, right } => bin(BinOp::Eq, out, left, right),
        P::IntNotEq { out, left, right } => bin(BinOp::NotEq, out, left, right),
        P::IntLess { out, left, right } => bin(BinOp::LessU, out, left, right),
        P::IntLessEq { out, left, right } => bin(BinOp::LessEqU, out, left, right),
        P::IntSLess { out, left, right } => bin(BinOp::LessS, out, left, right),
        P::IntSLessEq { out, left, right } => bin(BinOp::LessEqS, out, left, right),
        P::IntAnd { out, left, right } => bin(BinOp::And, out, left, right),
        P::IntOr { out, left, right } => bin(BinOp::Or, out, left, right),
        P::IntXor { out, left, right } => bin(BinOp::Xor, out, left, right),
        P::IntLsl { out, left, right } => bin(BinOp::Lsh, out, left, right),
        P::IntLsr { out, left, right } => bin(BinOp::Rsh, out, left, right),
        P::IntAsr { out, left, right } => bin(BinOp::Asr, out, left, right),
        P::IntNeg { out, input } => un(UnOp::Neg, out, input),
        // Flag support ops: counts and boolean operators. These are part of
        // ordinary flag computation, not exotic.
        P::Popcount { out, input } => IR::Un { op: UnOp::Popcount, dst: operand(out), src: operand(input) },
        P::Lzcount { out, input } => IR::Un { op: UnOp::Lzcount, dst: operand(out), src: operand(input) },
        P::BoolNot { out, input } => IR::Un { op: UnOp::Not, dst: operand(out), src: operand(input) },
        P::BoolAnd { out, left, right } => bin(BinOp::And, out, left, right),
        P::BoolOr { out, left, right } => bin(BinOp::Or, out, left, right),
        P::BoolXor { out, left, right } => bin(BinOp::Xor, out, left, right),
        P::IntCarry { out, left, right } => bin(BinOp::Carry, out, left, right),
        P::IntSCarry { out, left, right } => bin(BinOp::SCarry, out, left, right),
        P::IntSBorrow { out, left, right } => bin(BinOp::SBorrow, out, left, right),
        P::IntRem { out, left, right } => bin(BinOp::RemU, out, left, right),
        P::IntDiv { out, left, right } => bin(BinOp::DivU, out, left, right),
        P::IntNot { out, input } => un(UnOp::Not, out, input),
        P::IntZext { out, input } => un(UnOp::Zext, out, input),
        P::IntSext { out, input } => un(UnOp::Sext, out, input),
        P::Subpiece { out, input, lsb } => {
            IR::Subpiece { dst: operand(out), src: operand(input), lsb: *lsb }
        }
        // Anything else is not yet lowered; reported honestly rather than
        // silently turned into a no-op. See `docs/sleigh-notes.md` for the
        // list of ops that still need lowering (float, multipliers, etc).
        _ => IR::Nop,
    }
}

fn bin(op: BinOp, out: &Varnode, left: &Varnode, right: &Varnode) -> IR {
    IR::Bin { op, dst: operand(out), operands: Box::new((operand(left), operand(right))) }
}

fn un(op: UnOp, out: &Varnode, input: &Varnode) -> IR {
    IR::Un { op, dst: operand(out), src: operand(input) }
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
pub fn lift_block(bytes: &[u8], start: u64) -> Block {
    let mut decoder = Decoder::new(Architecture::X86_32);
    let mut instructions = Vec::new();
    let mut ip = start;
    let mut index = 0usize;
    let terminator;
    loop {
        let Some(rest) = bytes.get(index..) else {
            terminator = Terminator::Undecodable(ip);
            break;
        };
        if rest.is_empty() {
            terminator = Terminator::Undecodable(ip);
            break;
        }
        let insn = match decoder.decode(rest, ip) {
            Ok(i) if i.len > 0 => i,
            _ => {
                terminator = Terminator::Undecodable(ip);
                break;
            }
        };
        let mut ops = Vec::with_capacity(insn.ops.len());
        let mut unlowered = 0usize;
        for op in &insn.ops {
            let lowered = lower(op);
            if matches!(lowered, IR::Nop) && !matches!(op, PcodeOp::CallOther { .. }) {
                unlowered += 1;
            }
            ops.push(lowered);
        }
        let next = ip + insn.len;
        let mut term = None;
        for op in &insn.ops {
            match op {
                // Sleigh emits a *relative* branch target as a RAM-space
                // varnode holding the already-computed absolute address
                // (verified: `jmp +5` at 0x1000 gives `Branch{dest: Ram 0x1007}`).
                // Only a non-constant destination is a genuine indirect jump.
                PcodeOp::Branch { dest } => {
                    term = Some(match dest.space {
                        AddressSpaceId::Ram | AddressSpaceId::Const => Terminator::Jump(dest.offset),
                        _ => Terminator::IndirectJump,
                    });
                }
                PcodeOp::BranchInd { .. } => term = Some(Terminator::IndirectJump),
                PcodeOp::CBranch { dest, .. } => {
                    let taken = match dest.space {
                        AddressSpaceId::Ram | AddressSpaceId::Const => Some(dest.offset),
                        _ => None,
                    };
                    term = Some(Terminator::Conditional { taken, not_taken: next });
                }
                PcodeOp::Return { .. } => term = Some(Terminator::Return),
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
        index += insn.len as usize;
        ip = next;
        if term.is_some() {
            terminator = term.unwrap();
            break;
        }
    }
    Block { start, end: ip, instructions, terminator }
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
            IR::Un { op, dst, src } => format!("{} = {op:?}({})", o(dst), o(src)),
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
            IR::Userop { kind, args, dst, id } => {
                let a: Vec<String> = args.iter().map(o).collect();
                let call = format!("{kind:?}({})", a.join(", "));
                match dst {
                    Some(d) => format!("{} = {call}  ; userop {id}", o(d)),
                    None => format!("{call}  ; userop {id}"),
                }
            }
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
    let _ = store_size;
    cov
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let ops = lift(&[0x8b, 0x44, 0x24, 0x04], 0x1000);
        let (addr, size) = ops
            .iter()
            .find_map(|o| match o {
                IR::Load { addr, size, .. } => Some((addr, *size)),
                _ => None,
            })
            .expect("a Load");
        assert_eq!(size, 4);
        // The address is computed into a temp, so the ESP use is in the op
        // that produced that temp rather than in the Load itself.
        let Operand::Temp { offset, .. } = addr else {
            panic!("expected the address to be a computed temp, got {addr:?}");
        };
        let producer = ops
            .iter()
            .find(|o| matches!(o, IR::Bin { dst: Operand::Temp { offset: d, .. }, .. } if d == offset))
            .expect("an op must produce the address temp");
        assert!(format!("{producer:?}").contains("ESP"), "address must use ESP: {producer:?}");
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
                IR::Userop { kind, .. } => Some(*kind),
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
}
