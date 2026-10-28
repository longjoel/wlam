//! Public data types describing a Windows image.
//!
//! Kept separate from the parser so that the parsing code reads as logic
//! rather than as a wall of struct declarations.

use crate::ResourceTable;

/// PE image magic: PE32 (`0x10b`) or PE32+ (`0x20b`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeKind {
    /// 32-bit image. Addressing uses 4-byte fields.
    Pe32,
    /// 64-bit image. Address fields are 8 bytes.
    Pe32Plus,
}

/// COFF file machine type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Machine {
    I386,
    Amd64,
    Arm64,
    Other(u16),
}

impl Machine {
    pub(crate) fn from_raw(v: u16) -> Self {
        match v {
            0x014c => Machine::I386,
            0x8664 => Machine::Amd64,
            0xaa64 => Machine::Arm64,
            other => Machine::Other(other),
        }
    }
}

/// One section header plus the file range backing it.
#[derive(Debug, Clone)]
pub struct Section {
    pub name: String,
    pub virtual_address: u32,
    pub virtual_size: u32,
    pub file_offset: u32,
    pub file_size: u32,
    pub characteristics: u32,
    /// Bytes as laid out in the file (may be shorter than `virtual_size`;
    /// the remainder is zero-filled at load time).
    pub data: Vec<u8>,
}

impl Section {
    /// Whether the section is mapped readable.
    pub fn is_readable(&self) -> bool {
        self.characteristics & 0x4000_0000 != 0
    }
    /// Whether the section is mapped writable.
    pub fn is_writable(&self) -> bool {
        self.characteristics & 0x8000_0000 != 0
    }
    /// Whether the section holds executable code.
    pub fn is_executable(&self) -> bool {
        self.characteristics & 0x2000_0000 != 0
    }
    /// Virtual end address, excluding 32-bit wrap.
    pub fn virtual_end(&self) -> u32 {
        self.virtual_address.wrapping_add(self.virtual_size)
    }
    /// Slice of the section's *file* bytes starting at RVA `rva`, if in range.
    pub fn slice_at_rva(&self, rva: u32, len: u32) -> Option<&[u8]> {
        let off = rva.checked_sub(self.virtual_address)?;
        let start = off as usize;
        let end = start.checked_add(len as usize)?;
        self.data.get(start..end)
    }
}

/// A section that holds executable code.
#[derive(Debug, Clone)]
pub struct CodeSection {
    pub name: String,
    pub rva: u32,
    pub data: Vec<u8>,
}

/// One imported symbol and the IAT slot that receives its address.
#[derive(Debug, Clone)]
pub struct Import {
    /// The DLL name, as written in the image (e.g. `KERNEL32.dll`).
    pub library: String,
    /// One-based ordinal for ordinal imports.
    pub ordinal: Option<u16>,
    /// Symbol name for name-based imports.
    pub name: Option<String>,
    /// RVA of the IAT slot the loader writes the resolved address into.
    ///
    /// When the source thunk array (`OriginalFirstThunk`) and the IAT
    /// (`FirstThunk`) do not line up — mingw relocates them independently, so
    /// the name table can be longer than the IAT — this is the slot chosen by
    /// index and is flagged by [`Import::iat_slot_verified`].
    pub iat_rva: u32,
    /// Whether `iat_rva` was corroborated by a parallel address array. When
    /// false, the slot is a best-effort index and must not be relied on for
    /// IAT patching without independent evidence.
    pub iat_slot_verified: bool,
    /// Whether this entry came from a delay-load descriptor.
    pub delayed: bool,
}

impl Import {
    /// A stable, human readable identifier: `LIBRARY!Name` or `LIBRARY!#Ordinal`.
    pub fn symbol(&self) -> String {
        match (&self.name, self.ordinal) {
            (Some(n), _) => format!("{}!{}", self.library, n),
            (None, Some(o)) => format!("{}!#{}", self.library, o),
            (None, None) => format!("{}!<invalid>", self.library),
        }
    }
}

/// An exported symbol.
#[derive(Debug, Clone)]
pub struct Export {
    pub name: Option<String>,
    pub ordinal: u32,
    /// RVA of the exported code.
    pub rva: u32,
    /// Index of the defining section, if in range.
    pub section: Option<usize>,
}

/// A base relocation applied by the loader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Relocation {
    /// RVA where the fixup is applied.
    pub rva: u32,
    pub kind: RelocKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocKind {
    /// 32-bit absolute address (`IMAGE_REL_BASED_HIGHLOW`).
    Absolute,
    /// 64-bit absolute address.
    Dir64,
    /// No-op padding (`IMAGE_REL_BASED_ABSOLUTE`).
    Absolute32,
    /// High 16 bits only.
    High,
    /// Low 16 bits only.
    Low,
    /// Something we do not interpret yet.
    Other(u16),
}

/// A data directory from the optional header.
#[derive(Debug, Clone, Copy)]
pub struct Directory {
    pub index: usize,
    pub rva: u32,
    pub size: u32,
}

impl Directory {
    pub const EXPORT: usize = 0;
    pub const IMPORT: usize = 1;
    pub const RESOURCE: usize = 2;
    pub const EXCEPTION: usize = 3;
    pub const BASERELOC: usize = 5;
    pub const DEBUG: usize = 6;
    pub const TLS: usize = 9;
    pub const LOAD_CONFIG: usize = 10;
    pub const DELAY_IMPORT: usize = 13;
}

/// The complete, parsed image.
#[derive(Debug)]
pub struct Pe {
    pub kind: PeKind,
    pub machine: Machine,
    pub image_base: u64,
    pub entry_point_rva: u32,
    pub section_alignment: u32,
    pub file_alignment: u32,
    pub subsystem: u16,
    pub dll_characteristics: u16,
    pub size_of_image: u32,
    pub size_of_headers: u32,
    pub sections: Vec<Section>,
    pub directories: Vec<Directory>,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    pub relocations: Vec<Relocation>,
    pub resources: ResourceTable,
    /// RVA of the exception directory (`UNWIND_INFO` array), 64-bit images.
    pub unwind_info_rva: u32,
    pub unwind_info_size: u32,
    /// Whole file, retained for diagnostics.
    pub raw: Vec<u8>,
}
