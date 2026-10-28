//! The translation IR contract shared with the lifter.
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
    Un {
        op: UnOp,
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
