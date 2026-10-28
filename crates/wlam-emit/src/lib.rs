//! P-code to WebAssembly emission.
//!
//! This crate is the back end of the recompiler: it takes basic blocks of
//! [`wlam_pcode`] IR and produces a wasm module. `wlam_pcode` owns the type
//! definitions; this crate owns their translation, so the two can evolve
//! independently.
//!
//! ## Memory model (fixed now, so the ABI does not drift)
//!
//! | guest address range | contents |
//! |---|---|
//! | `0x0000_0000 .. 0x0000_ffff` | reserved null page; any access traps |
//! | `0x0040_0000 ..` | the PE image at its preferred base |
//! | `TEB.stack_base` | guest stack |
//! | `0x7ffd_e000 ..` | TIB / TEB; `FS:[0]` points at the exception list head |
//! | `0xf000_0000 ..` | x87 register file (80-bit values live in memory) |
//! | existing size | one guard page past the highest used address |
//!
//! The guest's whole 32-bit address space is one linear memory; nothing is ever
//! addressed through a host pointer.
//!
//! ## Registers
//!
//! P-code registers become wasm *locals* so lifted code stays fast, with a
//! memory-backed snapshot maintained for host calls and trap diagnostics.
//! wasm has no 80-bit type, so the x87 register file is guest memory.
//!
//! ## Not emitted yet
//!
//! Indirect branches need a jump-table resolution pass and are reported as
//! [`EmitError::IndirectBranch`] rather than guessed; real x86 code uses them,
//! so this is the largest known gap (`docs/roadmap.md`). Block translation
//! will land on the encoder below.

pub mod chain;
pub mod encode;
pub mod ir;
pub mod translate;

pub use ir::{BinOp, Operand, UnOp, IR};
pub use translate::{
    module_with_snapshot, translate_ops, LocalMap, TranslateError, TranslatedBlock,
    REG_SNAPSHOT_BASE,
};

pub use encode::{leb_u32, module_header, section_id, Encoder, SectionKind};

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
