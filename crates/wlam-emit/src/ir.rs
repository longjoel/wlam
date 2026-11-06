//! The translation IR contract shared with the lifter.
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

/// Floating-point operators the emitter understands.
///
/// Mirrors `wlam_sleigh::ir::FloatOp`. Kept separate from [`BinOp`] so a float
/// operation can never reach an integer instruction: the two enums share no
/// variants, and the emitter must handle `IR::Float` explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatOp {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    NotEq,
    Less,
    LessEq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatUnaryOp {
    Neg,
    Abs,
    Sqrt,
    Ceil,
    Floor,
    Round,
}

/// Typed integer <-> real conversions. Mirrors `wlam_sleigh::ir::ConvertOp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertOp {
    /// Signed integer to real (`FILD`).
    IntToFloat,
    /// Real to real, narrowing or widening (`FSTP m64` from an x87 register).
    FloatToFloat,
    /// Real to signed integer, truncating (`FISTP`).
    FloatToInt,
}

/// One IR operation.
#[derive(Debug, Clone, PartialEq)]
pub enum IR {
    FloatUnary {
        op: FloatUnaryOp,
        dst: Operand,
        src: Operand,
    },
    Copy {
        dst: Operand,
        src: Operand,
    },
    Load {
        dst: Operand,
        addr: Operand,
        size: u32,
    },
    Store {
        addr: Operand,
        value: Operand,
        size: u32,
    },
    Bin {
        op: BinOp,
        dst: Operand,
        operands: Box<(Operand, Operand)>,
    },
    /// A real-number binary operation (x87 `FADD`/`FMUL`/...).
    ///
    /// Operands are `f64` locals for x87 `ST` registers; a 1-byte destination
    /// means the operation was a comparison yielding a boolean.
    Float {
        op: FloatOp,
        dst: Operand,
        operands: Box<(Operand, Operand)>,
    },
    Un {
        op: UnOp,
        dst: Operand,
        src: Operand,
    },
    /// A typed conversion between integer and real domains.
    Convert {
        op: ConvertOp,
        dst: Operand,
        src: Operand,
    },
    Subpiece {
        dst: Operand,
        src: Operand,
        lsb: u32,
    },
    Branch {
        target: Operand,
    },
    IndirectJump {
        target: Operand,
    },
    CBranch {
        cond: Operand,
        target: Operand,
    },
    Call {
        target: Operand,
    },
    CallInd {
        target: Operand,
    },
    Ret {
        target: Option<Operand>,
    },
    Userop {
        id: u64,
        args: Vec<Operand>,
        dst: Option<Operand>,
    },
    /// A loop entirely inside one guest instruction, reconstructed from
    /// Sleigh's `Const`-address branches.
    ///
    /// `REP`-prefixed string instructions are the motivating case: the x86
    /// Sleigh spec implements them as a microcode loop whose back-edge targets
    /// the instruction's own start and whose exit edge targets the next
    /// instruction. Sleigh flattens that loop into its op list, so the branch
    /// targets are instruction addresses, not block targets. Flattening it
    /// again into the block list would invent a block that the discovery pass
    /// never asked for; representing it as a structured loop keeps it inside
    /// the block that contains the instruction.
    ///
    /// Semantics:
    /// ```text
    /// loop {
    ///     head
    ///     if exit != 0 { break }
    ///     body
    ///     if repeat is Some(c) { if c == 0 { break } } else { continue }
    /// }
    /// tail
    /// ```
    MicroLoop {
        /// Ops re-executed at the top of every iteration (the counter test).
        head: Vec<IR>,
        /// Exit condition: `goto inst_next` when non-zero.
        exit: Option<Operand>,
        /// Ops executed after the exit test.
        body: Vec<IR>,
        /// Back-edge condition: `goto inst_start` when non-zero. `None` means
        /// the back edge is unconditional.
        repeat: Option<Operand>,
        /// Ops after the back edge, executed once when the loop exits.
        tail: Vec<IR>,
    },
    /// An operation whose semantics are not implemented. Emission must fail.
    Unsupported(String),
    Nop,
}

// ---- Block contract ----

/// How a block ends. Produced by the lifter, consumed by [`crate::chain`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminator {
    /// Execution continues at the next instruction.
    FallsThrough,
    /// Unconditional jump.
    Jump(u64),
    /// Conditional jump; `taken` is `None` when computed at run time.
    Conditional { taken: Option<u64>, not_taken: u64 },
    /// Indirect jump (computed goto / jump table).
    IndirectJump,
    /// A `call`: control leaves for a callee and resumes at `resume`.
    Call { resume: u64 },
    /// An indirect `call`: control leaves for a target computed at run time.
    CallInd { resume: u64 },
    /// Return to the guest caller.
    Return,
    /// A trap by design (`INT3`), carrying the address after the instruction.
    Trap(u64),
    /// The bytes at this address are not valid code.
    Undecodable(u64),
}

/// A lifted basic block.
#[derive(Debug, Clone)]
pub struct Block {
    pub start: u64,
    pub end: u64,
    pub ops: Vec<IR>,
    pub terminator: Terminator,
}
