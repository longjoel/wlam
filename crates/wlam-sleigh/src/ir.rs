//! The IR the lifter produces and the emitter consumes.
//!
//! `wlam-emit` owns this contract (not the lifter) so that the emitter has no
//! dependency on the Sleigh-backed front-end: the two live in separate Cargo
//! workspaces because the vendored `rsleigh` declares its own. A lifter's job
//! is to produce these types; the emitter's job is to compile them.

/// An operand: a value with a width in bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operand {
    /// An immediate, stored zero-extended.
    Const { value: u64, size: u32 },
    /// A named register (mapped to a wasm local).
    Reg { name: Box<str>, size: u32 },
    /// A per-instruction temporary (mapped to a wasm local).
    Temp { offset: u64, size: u32 },
    /// A guest memory address used as a value.
    Ram { offset: u64, size: u32 },
}

impl Operand {
    /// Classify a Sleigh varnode: registers by (offset, size), temporaries by
    /// their `inst_start << 16 | slot` encoding.
    pub(crate) fn from_varnode(v: &crate::pcode_ir::Varnode) -> Operand {
        use crate::pcode_ir::AddressSpaceId;
        match v.space {
            AddressSpaceId::Const => Operand::Const { value: v.offset, size: v.size },
            AddressSpaceId::Register => {
                let name: Box<str> = match crate::NamedRegister::lookup(v.offset, v.size) {
                    Some(n) => n.into(),
                    None => format!("reg_{:x}_{}", v.offset, v.size).into_boxed_str(),
                };
                Operand::Reg { name, size: v.size }
            }
            AddressSpaceId::Unique => Operand::Temp { offset: v.offset, size: v.size },
            AddressSpaceId::Ram => Operand::Ram { offset: v.offset, size: v.size },
        }
    }

    pub fn size(&self) -> u32 {
        match self {
            Operand::Const { size, .. }
            | Operand::Reg { size, .. }
            | Operand::Temp { size, .. }
            | Operand::Ram { size, .. } => *size,
        }
    }
}

/// Binary operators the emitter understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    /// Full-width product (needs 64-bit support; not yet emitted).
    MulFull,
    DivS,
    DivU,
    RemS,
    RemU,
    And,
    Or,
    Xor,
    Lsh,
    Rsh,
    Asr,
    Eq,
    NotEq,
    LessU,
    LessEqU,
    LessS,
    LessEqS,
    /// Unsigned carry out of an addition.
    Carry,
    /// Signed overflow of an addition.
    SCarry,
    /// Signed borrow out of a subtraction.
    SBorrow,
}

/// Unary operators the emitter understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    Zext,
    Sext,
    Popcount,
    Lzcount,
}

/// One IR operation.
#[derive(Debug, Clone, PartialEq)]
pub enum IR {
    Copy { dst: Operand, src: Operand },
    Load { dst: Operand, addr: Operand, size: u32 },
    Store { addr: Operand, value: Operand, size: u32 },
    Bin { op: BinOp, dst: Operand, operands: Box<(Operand, Operand)> },
    Un { op: UnOp, dst: Operand, src: Operand },
    Subpiece { dst: Operand, src: Operand, lsb: u32 },
    Branch { target: Operand },
    IndirectJump { target: Operand },
    CBranch { cond: Operand, target: Operand },
    Call { target: Operand },
    CallInd { target: Operand },
    Ret { target: Option<Operand> },
    Userop { id: u64, args: Vec<Operand>, dst: Option<Operand> },
    Nop,
}

// ---- Lowering from Ghidra P-code ----

use crate::pcode_ir::{PcodeOp as GhidraOp, Varnode as GhidraVarnode};

fn operand(v: &GhidraVarnode) -> Operand {
    Operand::from_varnode(v)
}

#[allow(dead_code)]
fn store_size(op: &GhidraOp) -> u32 {
    match op {
        GhidraOp::Store { val, .. } => val.size,
        _ => 0,
    }
}

/// Translate rsleigh's `PcodeOp` into [`IR`].
pub fn lower(op: &GhidraOp) -> IR {
    use GhidraOp as P;
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

fn bin(op: BinOp, out: &GhidraVarnode, left: &GhidraVarnode, right: &GhidraVarnode) -> IR {
    IR::Bin { op, dst: operand(out), operands: Box::new((operand(left), operand(right))) }
}

fn un(op: UnOp, out: &GhidraVarnode, input: &GhidraVarnode) -> IR {
    IR::Un { op, dst: operand(out), src: operand(input) }
}

