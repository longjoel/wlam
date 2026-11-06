//! P-code to WebAssembly emission.
//!
//! This crate is the back end of the recompiler: it takes basic blocks of
//! [`wlam_pcode`] IR and produces a wasm module. `wlam_pcode` owns the type
//! definitions; this crate owns their translation, so the two can evolve
//! independently.
//!
//! The active emitter uses Wasm locals for registers and shared guest memory
//! for loader state and register snapshots. Each snapshot slot occupies eight
//! bytes; use [`snapshot_address`] instead of calculating offsets in callers.
//! x87 registers are an f64 approximation, not full 80-bit emulation.
//!
//! Whole programs report unsupported operations through named diagnostic
//! imports and trap when reached. Standalone translation returns an error.
//! See `docs/abi.md` and `docs/roadmap.md` for the current contract and limits.

mod callback;
pub mod chain;
pub mod encode;
pub mod ir;
pub mod translate;

pub use ir::{BinOp, Operand, UnOp, IR};

pub use translate::{
    module_with_snapshot, snapshot_address, translate_ops, LocalMap, TranslateError,
    TranslatedBlock, REG_SNAPSHOT_BASE,
};
/// Resolve an x86-32 register name to its byte window in the register file.
///
/// Re-exported from `wlam-pcode::regfile` so the emitter and the lifter agree on
/// which byte of which parent a name like `AH` refers to.
pub use wlam_pcode::regfile::{
    is_partial, slot_of as regfile_slot, Parent as RegParent, Slot as RegSlot,
};

pub use encode::{leb_s32, leb_u32, module_header, section_id, Encoder, SectionKind};

use thiserror::Error;

/// Everything that can make emission fail. Each variant names the block, so a
/// failure is always attributable to a place in the guest program.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum EmitError {
    #[error("block {block:#x}: unsupported P-code operation: {op}")]
    Unsupported { block: u64, op: String },
    #[error("block {block:#x}: indirect branch requires jump-table resolution")]
    IndirectBranch { block: u64 },
    #[error("block {block:#x}: malformed IR ({reason})")]
    Malformed { block: u64, reason: String },
}

/// WebAssembly value types used by the emitter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValType {
    I32,
    I64,
    F32,
    F64,
}

impl ValType {
    /// Binary encoding byte.
    pub fn byte(self) -> u8 {
        match self {
            ValType::I32 => 0x7f,
            ValType::I64 => 0x7e,
            ValType::F32 => 0x7d,
            ValType::F64 => 0x7c,
        }
    }
}

/// The guest registers exposed as wasm locals, in a stable order.
///
/// The order is part of the ABI: the host snapshots registers in this order
/// when reporting a trap.
pub const GUEST_LOCALS: &[&str] = &[
    "EAX",
    "ECX",
    "EDX",
    "EBX",
    "ESP",
    "EBP",
    "ESI",
    "EDI",
    "EIP",
    "EFLAGS",
    "FS_OFFSET",
    "GS_OFFSET",
];

/// The x87 register stack.
///
/// Sleigh's 80-bit x87 stack, currently represented by f64 locals.
/// This is an approximation; full extended precision is not implemented.
pub const X87_LOCALS: &[&str] = &["ST0", "ST1", "ST2", "ST3", "ST4", "ST5", "ST6", "ST7"];

/// Every register seeded as a wasm local, in a fixed order.
///
/// `GUEST_LOCALS` first, then the x87 stack. The x87 registers MUST be seeded
/// here rather than left to discovery order: the emitter types a local from the
/// register *name*, so an `ST` register that discovery happened to place among
/// temporaries would sit in a slot other code writes with 32-bit values. Seeding
/// them at a fixed index range keeps the f64 locals in a block of their own.
pub fn seeded_registers() -> Vec<String> {
    GUEST_LOCALS
        .iter()
        .chain(X87_LOCALS.iter())
        .map(|s| s.to_string())
        .collect()
}

/// Guest TIB base: the value the loader stores in `FS_OFFSET`.
pub const TIB_BASE: u64 = 0x7ffd_e000;

/// Address of the exception-registration-list head inside the TIB.
///
/// This is the field the MSVC SEH prologue touches (`mov eax, fs:[0]` /
/// `mov fs:[0], esp`). Sleigh lifts those as `IntAdd(FS_OFFSET, 0)` followed by
/// a load/store, so modelling the TIB explicitly is all that is needed.
pub const TIB_EXCEPTION_LIST: u64 = TIB_BASE;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_local_order_is_pinned() {
        assert_eq!(GUEST_LOCALS[0], "EAX");
        assert_eq!(GUEST_LOCALS[4], "ESP");
        assert_eq!(GUEST_LOCALS.len(), 12);
        assert!(GUEST_LOCALS.contains(&"FS_OFFSET"));
    }

    #[test]
    fn tib_base_is_in_the_windows_range() {
        assert_eq!(TIB_EXCEPTION_LIST, TIB_BASE);
        assert!(TIB_BASE >= 0x7ffd_0000 && TIB_BASE + 0x1000 <= 0x8000_0000);
    }

    #[test]
    fn valtype_bytes_match_the_spec() {
        assert_eq!(ValType::I32.byte(), 0x7f);
        assert_eq!(ValType::I64.byte(), 0x7e);
        assert_eq!(ValType::F32.byte(), 0x7d);
        assert_eq!(ValType::F64.byte(), 0x7c);
    }

    #[test]
    fn emit_errors_name_their_block() {
        let e = EmitError::Unsupported {
            block: 0x400010,
            op: "x".into(),
        };
        assert!(e.to_string().contains("0x400010"), "{e}");
    }
}
