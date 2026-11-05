//! Architecture-neutral P-code, modelled on Ghidra's IR.
//!
//! The IR is deliberately close to Ghidra's so that:
//!  * the lifting rules for x86 read like the well-understood Sleigh
//!    specifications rather than something invented here, and
//!  * an independent IR interpreter/emulator can be checked against the same
//!    semantics the wasm emitter implements.
//!
//! A *basic block* is the unit the recompiler emits as one wasm function. It
//! ends at any control transfer; the emitter turns [`Op::Branch`] into a jump
//! to the target block's function.

pub mod regfile;

use std::fmt;

/// Number of bytes of the breakpoint we inject as the unique terminator of
/// every block.
pub const BRKPSIZE: u32 = 1;

/// CPU state addressed by P-code. The *names* are exactly Ghidra's so that
/// vocabulary transfers between the two systems.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Space {
    /// Temporary variables, valid within one basic block.
    Unique,
    /// Guest memory (the linear memory of the emitted module).
    Ram,
    /// Registers.
    Register,
}

/// Size in bytes of a value (1, 2, 4, 8, or 0 for a "void" operand slot).
pub type ByteSize = u8;

/// A register operand. `name` is a stable identifier used for the symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Register {
    pub name: &'static str,
    pub size: ByteSize,
}

macro_rules! regs {
    ($($ident:ident => ($name:literal, $size:literal)),* $(,)?) => {
        impl Register {
            $(#[allow(non_upper_case_globals)] pub const $ident: Register = Register { name: $name, size: $size };)*
        }
        /// All named registers, in declaration order (also the index order
        /// used by [`Address::register`]).
        pub const REGISTERS: &[Register] = &[$(Register { name: $name, size: $size }),*];
    };
}

// The 32-bit subset needed first: general purpose, flags, x87 status, and SSE
// control. 8- and 16-bit views are separate registers; the lifter preserves
// the upper bits when writing them.
regs! {
    EAX => ("EAX", 4), ECX => ("ECX", 4), EDX => ("EDX", 4), EBX => ("EBX", 4),
    ESP => ("ESP", 4), EBP => ("EBP", 4), ESI => ("ESI", 4), EDI => ("EDI", 4),
    AX => ("AX", 2), CX => ("CX", 2), DX => ("DX", 2), BX => ("BX", 2),
    SP => ("SP", 2), BP => ("BP", 2), SI => ("SI", 2), DI => ("DI", 2),
    AL => ("AL", 1), CL => ("CL", 1), DL => ("DL", 1), BL => ("BL", 1),
    AH => ("AH", 1), CH => ("CH", 1), DH => ("DH", 1), BH => ("BH", 1),
    EIP => ("EIP", 4),
    EFLAGS => ("EFLAGS", 4),
    // Flags are separate variables so dependency analysis stays precise and
    // combined EFLAGS is only synthesized where pushf/popf needs it.
    ZE => ("ZF", 1), SE => ("SF", 1), OE => ("OF", 1), CE => ("CF", 1), PE => ("PF", 1),
    AC => ("AF", 1),
    // x87: top-of-stack pointer and tag word; the register stack lives in RAM.
    FTOP => ("FTOP", 4),
    FTAG => ("FTAG", 2),
    FCTRL => ("FCTRL", 2),
    FSTATUS => ("FSTATUS", 2),
    // SSE control register.
    MXCSR => ("MXCSR", 4),
}

/// A register slot meaning "no register".
pub fn reg_none() -> Register {
    Register { name: "", size: 0 }
}

pub fn is_reg_none(r: &Register) -> bool {
    r.size == 0
}

impl fmt::Display for Register {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if is_reg_none(self) {
            f.write_str("void")
        } else {
            f.write_str(self.name)
        }
    }
}

/// A P-code address: an offset into a space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Address {
    pub space: Space,
    /// Byte offset. For [`Space::Register`] this is an index into
    /// [`REGISTERS`]; for [`Space::Unique`] a temporary index; for RAM a guest
    /// address (used symbolically).
    pub offset: u64,
}

impl Address {
    pub const fn ram(offset: u64) -> Address {
        Address {
            space: Space::Ram,
            offset,
        }
    }
    pub const fn unique(offset: u64) -> Address {
        Address {
            space: Space::Unique,
            offset,
        }
    }
    /// Index of `r` in [`REGISTERS`], or `None` if it is not a named register.
    pub fn register_index(r: Register) -> Option<u64> {
        REGISTERS.iter().position(|x| *x == r).map(|i| i as u64)
    }
    /// Address of a named register. Panics if `r` is not in [`REGISTERS`]:
    /// that would be a lifter bug, and silently using a bogus index would be
    /// far worse than failing loudly.
    pub fn register(r: Register) -> Address {
        let idx = Self::register_index(r)
            .unwrap_or_else(|| panic!("register {} is not in REGISTERS", r.name));
        Address {
            space: Space::Register,
            offset: idx,
        }
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.space {
            Space::Unique => write!(f, "u{:x}", self.offset),
            Space::Ram => write!(f, "ram:{:x}", self.offset),
            Space::Register => match REGISTERS.get(self.offset as usize) {
                Some(r) => write!(f, "{}", r.name),
                None => write!(f, "reg#{}", self.offset),
            },
        }
    }
}

/// An operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Varnode {
    /// A value stored at an address: register, unique temp, or memory location
    /// whose base is a RAM address. This is always an *lvalue*: when it appears
    /// as an op input the consumer loads from it.
    Addr(Address, ByteSize),
    /// A constant, stored zero-extended in a `u64`.
    Const(u64, ByteSize),
}

impl Varnode {
    pub fn ram(offset: u64, size: ByteSize) -> Varnode {
        Varnode::Addr(Address::ram(offset), size)
    }
    pub fn unique(offset: u64, size: ByteSize) -> Varnode {
        Varnode::Addr(Address::unique(offset), size)
    }
    pub fn reg(r: Register) -> Varnode {
        Varnode::Addr(Address::register(r), r.size)
    }
    pub fn constant(value: u64, size: ByteSize) -> Varnode {
        Varnode::Const(value & mask(size), size)
    }
    pub fn size(&self) -> ByteSize {
        match self {
            Varnode::Addr(_, s) | Varnode::Const(_, s) => *s,
        }
    }
    pub fn is_const(&self) -> bool {
        matches!(self, Varnode::Const(..))
    }
    pub fn const_value(&self) -> Option<u64> {
        match self {
            Varnode::Const(v, _) => Some(*v),
            _ => None,
        }
    }
    /// Resize a varnode: constants are truncated or zero-extended; addresses
    /// change declared width only.
    pub fn truncate(&self, size: ByteSize) -> Varnode {
        match self {
            Varnode::Const(v, _) => Varnode::Const(*v & mask(size), size),
            Varnode::Addr(a, _) => Varnode::Addr(*a, size),
        }
    }
}

/// Bit mask for a value of `size` bytes.
pub fn mask(size: ByteSize) -> u64 {
    if size >= 8 {
        u64::MAX
    } else {
        (1u64 << (size * 8)) - 1
    }
}

/// Binary operator opcodes, mirroring Ghidra's `CPUI_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    IntAdd,
    IntSub,
    IntXor,
    IntAnd,
    IntOr,
    IntLeft,
    IntRight,
    IntSRight,
    IntEqual,
    IntNotEqual,
    IntLess,
    IntLessEqual,
    IntSLess,
    IntSLessEqual,
    IntMult,
    FloatEqual,
    FloatNotEqual,
    FloatLess,
    FloatLessEqual,
    FloatAdd,
    FloatSub,
    FloatMult,
    FloatDiv,
}

/// Unary operator opcodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    BoolNegate,
    Int2Comp,
    IntNegate,
    IntZext,
    IntSext,
    FloatNeg,
    FloatAbs,
    FloatSqrt,
    Popcount,
}

/// How an operand position is consumed: as an lvalue that is loaded, or as an
/// rvalue whose value is used directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadKind {
    /// Load `size` bytes from the address in the operand.
    Load,
    /// Use the operand's value directly (branch destinations, store address).
    Rvalue,
}

/// One P-code operation, corresponding to Ghidra's `PcodeOp` kinds.
///
/// Operand semantics follow Ghidra's model precisely: `Addr` inputs are
/// *always* lvalues. A binary op computes `out = input[0] <op> input[1]`, `Copy`
/// is `out = *input[0]`, `Call` jumps to the value of `input[0]`, and a store
/// writes `*input[1]` to the address in `input[0]`. Any tool that consumes this
/// IR (emitter, reference interpreter, pretty printer) must agree on this, so
/// the rule is encoded in [`Op::reads`].
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// `out = in[0]`, possibly resizing (zero-extend or truncate).
    Copy { out: Varnode, input: Varnode },
    /// Load `size` bytes from memory at `input` into `out`.
    Load {
        out: Varnode,
        input: Varnode,
        size: ByteSize,
    },
    /// Store `value` (`size` bytes) to memory at `address`.
    Store {
        address: Varnode,
        value: Varnode,
        size: ByteSize,
    },
    /// `out = a <op> b`.
    Binary {
        out: Varnode,
        a: Varnode,
        b: Varnode,
        op: BinaryOp,
    },
    /// `out = <op> input`.
    Unary {
        out: Varnode,
        input: Varnode,
        op: UnaryOp,
    },
    /// `out = if(cond != 0) { a } else { b }`.
    BranchIndirect {
        out: Varnode,
        cond: Varnode,
        taken: Varnode,
        not_taken: Varnode,
    },
    /// Unconditional branch to a constant guest address.
    Branch { target: u64 },
    /// Conditional branch on `cond` to a constant guest address.
    CBranch { cond: Varnode, target: u64 },
    /// Call to a known guest function; the return address is recorded so a
    /// lifted callee can return to it.
    Call { target: u64, ret: u64 },
    /// Call through `input` (IAT slot, function pointer).
    CallInd { input: Varnode, ret: u64 },
    /// Return from the current function: control flow only.
    Return,
    /// Return, also carrying the popped return address in `input` (the lifter
    /// emits this; the emitter reads `input` as the branch target).
    ReturnTo { input: Varnode },
    /// A call into the host: `name` identifies the Win32 import.
    CallOther {
        name: String,
        inputs: Vec<Varnode>,
        output: Option<Varnode>,
    },
    /// No-op (nop, prefix quirks, padding).
    Nop,
}

impl Op {
    /// The inputs of this op with how each is read, in order.
    ///
    /// This is the single source of truth for operand semantics; consumers
    /// (the emitter and the reference interpreter) iterate it rather than
    /// pattern-matching operand fields, so they cannot drift apart.
    pub fn reads(&self) -> Vec<(&Varnode, ReadKind)> {
        match self {
            // `out = *input`: Copy input is an lvalue.
            Op::Copy { input, .. } => vec![(input, ReadKind::Load)],
            // Unary operators read their input's value at its own width.
            Op::Unary { input, .. } => vec![(input, ReadKind::Load)],
            // The address is used directly; the loaded value is the input.
            Op::Load { input, .. } => vec![(input, ReadKind::Rvalue)],
            Op::Store { address, value, .. } => {
                vec![(address, ReadKind::Rvalue), (value, ReadKind::Load)]
            }
            Op::Binary { a, b, .. } => vec![(a, ReadKind::Load), (b, ReadKind::Load)],
            Op::BranchIndirect {
                cond,
                taken,
                not_taken,
                ..
            } => {
                vec![
                    (cond, ReadKind::Load),
                    (taken, ReadKind::Rvalue),
                    (not_taken, ReadKind::Rvalue),
                ]
            }
            // The branch condition is loaded; the target is an immediate.
            Op::CBranch { cond, .. } => vec![(cond, ReadKind::Load)],
            // The callee address is loaded from the IAT slot / register.
            Op::CallInd { input, .. } => vec![(input, ReadKind::Load)],
            Op::CallOther { inputs, .. } => inputs.iter().map(|v| (v, ReadKind::Load)).collect(),
            Op::Call { .. } | Op::Branch { .. } | Op::Nop | Op::Return | Op::ReturnTo { .. } => {
                vec![]
            }
        }
    }

    /// The destination varnode, if the operation writes one.
    pub fn output(&self) -> Option<&Varnode> {
        match self {
            Op::Copy { out, .. }
            | Op::Load { out, .. }
            | Op::Binary { out, .. }
            | Op::Unary { out, .. }
            | Op::BranchIndirect { out, .. } => Some(out),
            Op::CallOther { output, .. } => output.as_ref(),
            Op::Store { .. } | Op::CBranch { .. } | Op::Call { .. } | Op::CallInd { .. } => None,
            Op::Branch { .. } | Op::Nop | Op::Return | Op::ReturnTo { .. } => None,
        }
    }
}

/// One lifted instruction.
#[derive(Debug, Clone, PartialEq)]
pub struct PcodeInstruction {
    /// Guest address of the instruction.
    pub address: u64,
    /// Length in bytes of the original instruction.
    pub length: u8,
    /// Disassembly text for diagnostics and golden tests.
    pub disassembly: String,
    /// Operations in execution order.
    pub ops: Vec<Op>,
}

impl PcodeInstruction {
    pub fn new(address: u64, length: u8, disassembly: String, ops: Vec<Op>) -> Self {
        PcodeInstruction {
            address,
            length,
            disassembly,
            ops,
        }
    }
}

/// How a basic block ends. This is what the emitter needs to chain blocks.
#[derive(Debug, Clone, PartialEq)]
pub enum Terminator {
    /// Falls through to `next`.
    Fallthrough { next: u64 },
    /// Unconditional jump.
    Jump { target: u64 },
    /// Conditional jump.
    Conditional { taken: u64, not_taken: u64 },
    /// Indirect jump through a computed address (switch or computed goto).
    IndirectJump,
    /// Call to a known guest function returning to `ret`.
    Call { target: u64, ret: u64 },
    /// Call into the Win32 host layer, returning to `ret`.
    HostCall { symbol: String, ret: u64 },
    /// Indirect call (IAT thunk or function pointer), returning to `ret`.
    IndirectCall { ret: u64 },
    /// Function return.
    Return,
    /// Unsupported instruction or missing code: stop and report.
    Trap { reason: String },
}

/// A basic block: the emission unit.
#[derive(Debug, Clone, PartialEq)]
pub struct BasicBlock {
    /// Guest address of the first instruction.
    pub start: u64,
    /// Guest address one past the last instruction.
    pub end: u64,
    pub instructions: Vec<PcodeInstruction>,
    pub terminator: Terminator,
}

impl BasicBlock {
    pub fn new(start: u64) -> Self {
        BasicBlock {
            start,
            end: start,
            instructions: Vec::new(),
            terminator: Terminator::Trap {
                reason: "block has no terminator".into(),
            },
        }
    }

    /// Append an instruction, advancing `end`.
    pub fn push(&mut self, insn: PcodeInstruction) {
        self.end = insn.address + insn.length as u64;
        self.instructions.push(insn);
    }

    /// All direct successors of this block. A call's *code* successor is the
    /// return address: the callee is a separate function, not a successor of
    /// this block.
    pub fn successors(&self) -> Vec<u64> {
        match &self.terminator {
            Terminator::Fallthrough { next } => vec![*next],
            Terminator::Jump { target } => vec![*target],
            Terminator::Conditional { taken, not_taken } => vec![*taken, *not_taken],
            Terminator::Call { ret, .. }
            | Terminator::HostCall { ret, .. }
            | Terminator::IndirectCall { ret } => vec![*ret],
            Terminator::IndirectJump | Terminator::Return | Terminator::Trap { .. } => vec![],
        }
    }

    /// The guest function this block calls, when it is a known direct call.
    pub fn call_target(&self) -> Option<u64> {
        match self.terminator {
            Terminator::Call { target, .. } => Some(target),
            _ => None,
        }
    }
}

/// Render a block as C-like text. Used by golden tests and for readable
/// diffs: a behaviour change shows as a changed line, not a byte offset.
pub struct CPrinter;

impl CPrinter {
    pub fn print_block(block: &BasicBlock) -> String {
        let mut out = String::new();
        out.push_str(&format!("// block {:#x}..{:#x}\n", block.start, block.end));
        for insn in &block.instructions {
            out.push_str(&format!("@{:#08x}: {}\n", insn.address, insn.disassembly));
            for op in &insn.ops {
                out.push_str(&format!("    {}\n", Self::print_op(op)));
            }
        }
        out.push_str(&format!("    // end: {:?}\n", block.terminator));
        out
    }

    pub fn print_op(op: &Op) -> String {
        match op {
            Op::Copy { out, input } => format!("{out} = {input}"),
            Op::Load { out, input, size } => format!("{out} = {size}:{input}"),
            Op::Store {
                address,
                value,
                size,
            } => format!("*{size}:{address} = {value}"),
            Op::Binary { out, a, b, op } => format!("{out} = {a} {op:?} {b}"),
            Op::Unary { out, input, op } => format!("{out} = {op:?}({input})"),
            Op::BranchIndirect {
                out,
                cond,
                taken,
                not_taken,
            } => {
                format!("{out} = {cond} ? {taken} : {not_taken}")
            }
            Op::Branch { target } => format!("goto {target:#x}"),
            Op::CBranch { cond, target } => format!("if ({cond}) goto {target:#x}"),
            Op::Call { target, ret } => format!("call {target:#x} -> {ret:#x}"),
            Op::CallInd { input, ret } => format!("call *{input} -> {ret:#x}"),
            Op::Return => "return".to_string(),
            Op::ReturnTo { input } => format!("return {input}"),
            Op::CallOther {
                name,
                inputs,
                output,
            } => {
                let ins: Vec<String> = inputs.iter().map(|v| format!("{v}")).collect();
                match output {
                    Some(o) => format!("{o} = callother({name}, [{}])", ins.join(", ")),
                    None => format!("callother({name}, [{}])", ins.join(", ")),
                }
            }
            Op::Nop => "nop".to_string(),
        }
    }
}

impl fmt::Display for Varnode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Varnode::Const(v, s) => write!(f, "{v:#x}:{s}"),
            Varnode::Addr(a, s) => write!(f, "{a}:{s}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_handles_all_sizes() {
        assert_eq!(mask(1), 0xff);
        assert_eq!(mask(2), 0xffff);
        assert_eq!(mask(4), 0xffff_ffff);
        assert_eq!(mask(8), u64::MAX);
    }

    #[test]
    fn truncate_zero_extends_and_never_sign_extends() {
        assert_eq!(
            Varnode::constant(0x1_2345, 4).truncate(2),
            Varnode::constant(0x2345, 2)
        );
        assert_eq!(
            Varnode::constant(0xffff, 4).truncate(1),
            Varnode::constant(0xff, 1)
        );
        assert_eq!(Varnode::unique(7, 4).truncate(1), Varnode::unique(7, 1));
    }

    #[test]
    fn constants_are_stored_masked() {
        // A 4-byte constant cannot carry a 64-bit value.
        assert_eq!(Varnode::constant(0x1_0000_0001, 4).const_value(), Some(1));
    }

    #[test]
    fn register_addresses_round_trip() {
        let a = Address::register(Register::EAX);
        assert_eq!(a.space, Space::Register);
        assert_eq!(REGISTERS[a.offset as usize], Register::EAX);
        assert_eq!(format!("{a}"), "EAX");
    }

    #[test]
    fn every_register_has_a_unique_index() {
        for (i, r) in REGISTERS.iter().enumerate() {
            assert_eq!(
                REGISTERS.iter().position(|x| x == r),
                Some(i),
                "{} is duplicated",
                r.name
            );
            assert!(r.size > 0, "{} has zero size", r.name);
        }
    }

    #[test]
    fn address_of_unknown_register_panics_rather_than_lying() {
        let bogus = Register {
            name: "NOPE",
            size: 4,
        };
        let r = std::panic::catch_unwind(|| Address::register(bogus));
        assert!(r.is_err());
    }

    #[test]
    fn reg_none_is_distinguishable() {
        assert!(is_reg_none(&reg_none()));
        assert!(!is_reg_none(&Register::EAX));
        assert_eq!(format!("{}", reg_none()), "void");
    }

    #[test]
    fn block_push_advances_end() {
        let mut b = BasicBlock::new(0x1000);
        b.push(PcodeInstruction::new(
            0x1000,
            3,
            "nop".into(),
            vec![Op::Nop],
        ));
        b.push(PcodeInstruction::new(0x1003, 2, "ret".into(), vec![]));
        assert_eq!(b.end, 0x1005);
        assert_eq!(b.instructions.len(), 2);
    }

    #[test]
    fn successors_cover_every_terminator() {
        let t = |term| BasicBlock {
            terminator: term,
            ..BasicBlock::new(0)
        };
        assert_eq!(t(Terminator::Fallthrough { next: 4 }).successors(), vec![4]);
        assert_eq!(t(Terminator::Jump { target: 8 }).successors(), vec![8]);
        assert_eq!(
            t(Terminator::Conditional {
                taken: 1,
                not_taken: 2
            })
            .successors(),
            vec![1, 2]
        );
        assert_eq!(
            t(Terminator::Call { target: 9, ret: 5 }).successors(),
            vec![5]
        );
        assert!(t(Terminator::Return).successors().is_empty());
        assert!(t(Terminator::Trap { reason: "x".into() })
            .successors()
            .is_empty());
    }

    #[test]
    fn printer_is_readable() {
        let b = BasicBlock {
            start: 0x1000,
            end: 0x1003,
            instructions: vec![PcodeInstruction::new(
                0x1000,
                3,
                "mov EAX, 1".into(),
                vec![Op::Copy {
                    out: Varnode::reg(Register::EAX),
                    input: Varnode::constant(1, 4),
                }],
            )],
            terminator: Terminator::Return,
        };
        let text = CPrinter::print_block(&b);
        assert!(text.contains("EAX:4 = 0x1:4"), "{text}");
        assert!(text.contains("mov EAX, 1"));
    }
}

pub mod abi;
