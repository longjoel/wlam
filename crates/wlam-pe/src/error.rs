//! Typed PE parsing failures.

/// Something went wrong while reading a portable executable.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PeError {
    #[error("not a DOS executable (missing MZ signature)")]
    NotMz,
    #[error("missing PE signature at e_lfanew {offset:#x}")]
    NotPe { offset: usize },
    #[error("unsupported optional header magic {0:#x} (expected PE32 or PE32+)")]
    UnknownOptionalMagic(u16),
    #[error("read past end of file at offset {offset:#x}")]
    OutOfBounds { offset: u32 },
    #[error("RVA {rva:#x} is not mapped by any section")]
    BadRva { rva: u32 },
    #[error("unterminated or non-UTF8 string at RVA/offset {offset:#x}")]
    BadString { offset: u32 },
    #[error("malformed resource tree: {reason}")]
    BadResources { reason: String },
}
