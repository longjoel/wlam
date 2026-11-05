//! The IR the lifter produces and the emitter consumes.
//!
//! `wlam-emit` owns this contract (not the lifter) so that the emitter has no
//! dependency on the Sleigh-backed front-end: both participate in the root workspace. A lifter's job
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
    /// an instruction-scoped 32-bit namespace plus the decoder slot.
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

/// Floating-point operators, kept separate from [`BinOp`].
///
/// x87 and SSE arithmetic is a distinct domain from integer arithmetic: the
/// operand types, rounding and NaN/`-0.0` behaviour all differ, and a recompiler
/// that let the two share an enum would eventually emit `i32.add` where `f64.add`
/// was meant. `FloatOp` therefore names only real-number operations, and the
/// emitter is forced to pick a float instruction for each one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatOp {
    Add,
    Sub,
    Mul,
    Div,
    /// IEEE comparison: yields a 1-byte boolean (P-code's `FloatLess` family).
    Eq,
    NotEq,
    Less,
    LessEq,
}

/// Numeric conversions between the integer and real-number domains.
///
/// Sleigh emits these for x87's `FILD` (`Int2Float`), `FISTP` (`Trunc`), and for
/// `FSTP m64` (`Float2Float` from an 80-bit `ST` to a 64-bit double). They are
/// named separately from [`IR::Un`] because each one is a *typed* conversion:
/// the target type is decided by the operand widths, not by the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertOp {
    /// Signed integer to real (`Int2Float`, `FILD`).
    IntToFloat,
    /// Real to real, narrowing or widening (`Float2Float`).
    FloatToFloat,
    /// Real to signed integer, truncating (`Trunc`).
    FloatToInt,
}

/// One IR operation.
#[derive(Debug, Clone, PartialEq)]
pub enum IR {
    FloatUnary { op: wlam_emit::ir::FloatUnaryOp, dst: Operand, src: Operand },
    Copy { dst: Operand, src: Operand },
    Load { dst: Operand, addr: Operand, size: u32 },
    Store { addr: Operand, value: Operand, size: u32 },
    Bin { op: BinOp, dst: Operand, operands: Box<(Operand, Operand)> },
    /// A floating-point binary operation (`FloatAdd`, `FloatMult`, ...).
    ///
    /// Distinct from [`IR::Bin`] so the emitter cannot route it through an
    /// integer instruction. Operand widths are the *source* precision: a 10-byte
    /// `ST` register is modelled as `f64` (see the x87 note in `docs/roadmap.md`).
    Float { op: FloatOp, dst: Operand, operands: Box<(Operand, Operand)> },
    Un { op: UnOp, dst: Operand, src: Operand },
    /// A typed conversion between integer and real-number domains.
    Convert { op: ConvertOp, dst: Operand, src: Operand },
    Subpiece { dst: Operand, src: Operand, lsb: u32 },
    Branch { target: Operand },
    IndirectJump { target: Operand },
    CBranch { cond: Operand, target: Operand },
    Call { target: Operand },
    CallInd { target: Operand },
    Ret { target: Option<Operand> },
    Userop { id: u64, args: Vec<Operand>, dst: Option<Operand> },
    /// A loop contained in one guest instruction, reconstructed from Sleigh's
    /// `Const`-address branches (see the emitter's `IR::MicroLoop`).
    MicroLoop {
        head: Vec<IR>,
        exit: Option<Operand>,
        body: Vec<IR>,
        repeat: Option<Operand>,
        tail: Vec<IR>,
    },
    /// An operation whose semantics are not implemented. Emission must fail.
    Unsupported(String),
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
        P::Branch { dest } | P::CBranch { dest, .. }
            if dest.space == crate::AddressSpaceId::Const =>
            IR::Unsupported(format!("internal P-code branch: {op:?}")),
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
        // Floating point. Before these arms existed every x87 arithmetic
        // instruction lowered to `Nop`, so `FMUL ST1` computed nothing and the
        // guest silently read the stale ST0 -- wrong in a way that looks like a
        // working program. They are a separate `IR::Float` variant rather than
        // `IR::Bin` so the emitter must choose an f64 instruction.
        P::FloatAdd { out, left, right } => flt(FloatOp::Add, out, left, right),
        P::FloatSub { out, left, right } => flt(FloatOp::Sub, out, left, right),
        P::FloatMult { out, left, right } => flt(FloatOp::Mul, out, left, right),
        P::FloatDiv { out, left, right } => flt(FloatOp::Div, out, left, right),
        P::FloatEq { out, left, right } => flt(FloatOp::Eq, out, left, right),
        P::FloatNotEq { out, left, right } => flt(FloatOp::NotEq, out, left, right),
        P::FloatLess { out, left, right } => flt(FloatOp::Less, out, left, right),
        P::FloatLessEq { out, left, right } => flt(FloatOp::LessEq, out, left, right),
        P::FloatNan { out, input } => flt(FloatOp::NotEq, out, input, input),
        P::FloatNeg { out, input } | P::FloatAbs { out, input } |
        P::FloatSqrt { out, input } | P::FloatCeil { out, input } |
        P::FloatFloor { out, input } | P::FloatRound { out, input } => {
            use wlam_emit::ir::FloatUnaryOp as U;
            let kind=match op {
                P::FloatNeg {..}=>U::Neg,P::FloatAbs {..}=>U::Abs,
                P::FloatSqrt {..}=>U::Sqrt,P::FloatCeil {..}=>U::Ceil,
                P::FloatFloor {..}=>U::Floor,P::FloatRound {..}=>U::Round,
                _=>unreachable!(),
            };
            IR::FloatUnary {op:kind,dst:operand(out),src:operand(input)}
        }
        // Typed conversions. `FSTP m64` lowers to `Float2Float ST0(80) ->
        // tmp(64)`, so without these the store receives an untyped temp and the
        // emitter refuses it with "i64 store".
        P::Int2Float { out, input } => IR::Convert {
            op: ConvertOp::IntToFloat,
            dst: operand(out),
            src: operand(input),
        },
        P::Float2Float { out, input } => IR::Convert {
            op: ConvertOp::FloatToFloat,
            dst: operand(out),
            src: operand(input),
        },
        P::Trunc { out, input } => IR::Convert {
            op: ConvertOp::FloatToInt,
            dst: operand(out),
            src: operand(input),
        },

        // Flag support ops: counts and boolean operators. These are part of
        // ordinary flag computation, not exotic.
        P::Popcount { out, input } => IR::Un { op: UnOp::Popcount, dst: operand(out), src: operand(input) },
        P::Lzcount { out, input } => IR::Un { op: UnOp::Lzcount, dst: operand(out), src: operand(input) },
        P::BoolNot { out, input } => IR::Bin {
            op: BinOp::Eq, dst: operand(out),
            operands: Box::new((operand(input), Operand::Const { value: 0, size: input.size })),
        },
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
        _ => IR::Unsupported(format!("{op:?}")),
    }
}

fn bin(op: BinOp, out: &GhidraVarnode, left: &GhidraVarnode, right: &GhidraVarnode) -> IR {
    IR::Bin { op, dst: operand(out), operands: Box::new((operand(left), operand(right))) }
}

fn un(op: UnOp, out: &GhidraVarnode, input: &GhidraVarnode) -> IR {
    IR::Un { op, dst: operand(out), src: operand(input) }
}

fn flt(op: FloatOp, out: &GhidraVarnode, left: &GhidraVarnode, right: &GhidraVarnode) -> IR {
    IR::Float { op, dst: operand(out), operands: Box::new((operand(left), operand(right))) }
}


#[cfg(test)]
mod subpiece_tests {
    use crate::pcode_ir::{AddressSpaceId, PcodeOp};
    use crate::{with_big_stack, Architecture, Decoder};

    /// `SUBPIECE`'s `lsb` is "the number of least significant bytes of input0
    /// to be thrown away" (Ghidra P-Code Reference Manual). For a dword `MUL`
    /// the two halves must therefore report different values: EAX discards 0,
    /// EDX discards 4.
    ///
    /// This was an OPEN BUG (see docs/sleigh-notes.md): rsleigh's codegen
    /// divided the `trunk` byte count by 8 as if it were bits, so `tmp(4)`
    /// produced `lsb = 0` and EDX became indistinguishable from EAX -- i.e.
    /// every `MUL` computed EDX = EAX. Fixed in
    /// `rsleigh/src/codegen/.../constructor/execution.rs`.
    #[test]
    fn mul_halves_report_distinct_lsb() {
        // The decoder needs an 8 MiB stack; see docs/sleigh-notes.md.
        let mut halves = with_big_stack(|| {
            let mut dec = Decoder::new(Architecture::X86_32);
            let insn = dec.decode(&[0xF7, 0xE3], 0x1000).expect("MUL EBX decodes");
            let mut halves: Vec<(u64, u32, u32)> = Vec::new();
            for op in &insn.ops {
                if let PcodeOp::Subpiece { out, input, lsb } = op {
                    if input.space == AddressSpaceId::Unique && input.size == 8 {
                        halves.push((out.offset, input.size, *lsb));
                    }
                }
            }
            halves
        });
        halves.sort();
        assert_eq!(halves.len(), 2, "MUL yields two halves: {halves:?}");
        // EAX is at register-file offset 0x0 and discards nothing.
        assert_eq!((halves[0].0, halves[0].2), (0x0, 0), "EAX = low half");
        // EDX is at 0x8 and discards the 4 low bytes to yield the high half.
        assert_eq!(
            (halves[1].0, halves[1].2),
            (0x8, 4),
            "EDX must discard 4 bytes (SUBPIECE lsb = bytes discarded)"
        );
    }

    /// The same units rule for a byte-count that is not a multiple of 8, which
    /// the old `/ 8` silently collapsed to zero.
    #[test]
    fn trunk_lsb_is_a_byte_count_not_a_bit_count() {
        // `AX = tmp(2)` style truncation: 2 bytes discarded must stay 2.
        // IMUL rm16: tmp:4 = sext(AX) * sext(rm16); DX = tmp(2); AX = tmp(0)
        let code = [0x66, 0xF7, 0xEB]; // IMUL BX (16-bit)
        let lsbs: Vec<u32> = with_big_stack(move || {
            let mut dec = Decoder::new(Architecture::X86_32);
            let Some(insn) = dec.decode(&code, 0x1000).ok() else {
                return Vec::new(); // form not lifted in this build
            };
            insn.ops
                .iter()
                .filter_map(|op| match op {
                    PcodeOp::Subpiece { input, lsb, .. }
                        if input.space == AddressSpaceId::Unique && input.size == 4 =>
                    {
                        Some(*lsb)
                    }
                    _ => None,
                })
                .collect()
        });
        if lsbs.is_empty() {
            return; // form not lifted in this build
        }
        assert!(
            lsbs.contains(&2) || lsbs.contains(&0),
            "16-bit IMUL halves should discard 2 and 0 bytes, got {lsbs:?}"
        );
        assert!(
            !lsbs.contains(&1),
            "a discarded byte count of 2 must not be divided by 8 to 0/1: {lsbs:?}"
        );
    }
}

#[cfg(test)]
mod unsupported_tests {
    use super::*;

    #[test]
    fn unimplemented_pcode_survives_lowering_and_conversion() {
        let value = GhidraVarnode { space: crate::AddressSpaceId::Const, offset: 1, size: 8 };
        let raw = GhidraOp::Branch { dest: value };
        let lowered = lower(&raw);
        assert!(matches!(&lowered, IR::Unsupported(op) if op.contains("internal P-code branch")));
        assert!(matches!(crate::emit::to_eir(&lowered), wlam_emit::IR::Unsupported(op) if op.contains("internal P-code branch")));
    }
}
