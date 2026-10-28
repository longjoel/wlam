//! Win32 Portable Executable parsing.
//!
//! `wlam-pe` owns no policy about recompilation: it turns a `.exe`/`.dll`
//! byte image into a typed description (sections, imports, exports, base
//! relocations, resources) and then gets out of the way.
//!
//! The parser is strict about structural invariants (headers, RVAs, alignment)
//! and lenient about semantic content (unknown directories are preserved as
//! [`Directory`] entries rather than rejected).

mod error;
mod resource;

pub use error::PeError;
pub use resource::{ResourceEntry, ResourceTable};

use std::collections::BTreeMap;

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
    fn from_raw(v: u16) -> Self {
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
    pub iat_rva: u32,
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

/// Read helpers over a byte slice with bounds checks.
struct Reader<'a> {
    data: &'a [u8],
    base: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Reader { data, base: 0 }
    }
    fn at(data: &'a [u8], base: usize) -> Self {
        Reader { data, base }
    }
    fn u8(&self, off: u32) -> Result<u8, PeError> {
        let b = self.bytes(off, 1)?;
        Ok(b[0])
    }
    fn u16(&self, off: u32) -> Result<u16, PeError> {
        let b = self.bytes(off, 2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&self, off: u32) -> Result<u32, PeError> {
        let b = self.bytes(off, 4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&self, off: u32) -> Result<u64, PeError> {
        let lo = self.u32(off)? as u64;
        let hi = self.u32(off.checked_add(4).ok_or(PeError::OutOfBounds { offset: off })?)?;
        Ok(lo | (hi as u64) << 32)
    }
    fn bytes(&self, off: u32, len: u32) -> Result<&'a [u8], PeError> {
        let start = (self.base as u64)
            .checked_add(off as u64)
            .ok_or(PeError::OutOfBounds { offset: off })? as usize;
        let end = start.checked_add(len as usize).ok_or(PeError::OutOfBounds { offset: off })?;
        self.data.get(start..end).ok_or(PeError::OutOfBounds { offset: off })
    }
    /// Read a NUL-terminated string starting at `off`.
    ///
    /// A string that runs to the end of the buffer without a terminator is
    /// accepted for its full length; only a *non-UTF8* sequence is an error.
    /// This matters for debug directories, whose paths are sometimes not
    /// NUL-terminated at EOF.
    fn cstr(&self, off: u32) -> Result<String, PeError> {
        let start = (self.base as u64).checked_add(off as u64).unwrap_or(u64::MAX) as usize;
        let tail = self.data.get(start..).unwrap_or(&[]);
        let end = tail.iter().position(|&c| c == 0).unwrap_or(tail.len());
        if end > 4096 {
            return Err(PeError::BadString { offset: off });
        }
        String::from_utf8(tail[..end].to_vec()).map_err(|_| PeError::BadString { offset: off })
    }
}

impl Pe {
    /// Parse a complete PE image.
    pub fn parse(raw: &[u8]) -> Result<Pe, PeError> {
        let file = Reader::new(raw);
        if file.bytes(0, 2)? != b"MZ" {
            return Err(PeError::NotMz);
        }
        let e_lfanew = file.u32(0x3c)? as usize;
        let nt = Reader::at(raw, e_lfanew);
        if nt.bytes(0, 4)? != b"PE\0\0" {
            return Err(PeError::NotPe { offset: e_lfanew });
        }
        let machine = Machine::from_raw(nt.u16(4)?);
        let num_sections = nt.u16(6)? as u32;
        let opt_size = nt.u16(20)? as u32;
        let opt = Reader::at(raw, e_lfanew + 24);
        let magic = opt.u16(0)?;
        let kind = match magic {
            0x10b => PeKind::Pe32,
            0x20b => PeKind::Pe32Plus,
            other => return Err(PeError::UnknownOptionalMagic(other)),
        };
        let is64 = kind == PeKind::Pe32Plus;
        let entry_point_rva = opt.u32(16)?;
        let image_base = if is64 { opt.u64(24)? } else { opt.u32(28)? as u64 };
        let section_alignment = opt.u32(32)?;
        let file_alignment = opt.u32(36)?;
        let size_of_image = opt.u32(56)?;
        let size_of_headers = opt.u32(60)?;
        let subsystem = opt.u16(68)?;
        let dll_characteristics = opt.u16(70)?;
        let n_dirs = opt.u32(92)? as usize;
        let dir_base = if is64 { 112 } else { 96 };
        let mut directories = Vec::new();
        for i in 0..n_dirs.min(16) {
            let off = dir_base + (i as u32) * 8;
            directories.push(Directory { index: i, rva: opt.u32(off)?, size: opt.u32(off + 4)? });
        }
        let dir = |i: usize| -> Option<Directory> {
            directories.iter().copied().find(|d| d.index == i && d.rva != 0)
        };

        // Section table follows the optional header.
        let sec_base = e_lfanew + 24 + opt_size as usize;
        let file_all = Reader::new(raw);
        let mut sections = Vec::with_capacity(num_sections as usize);
        for i in 0..num_sections as u32 {
            let s = Reader::at(raw, sec_base + (i as usize) * 40);
            let mut name = [0u8; 8];
            name.copy_from_slice(s.bytes(0, 8)?);
            let name = String::from_utf8_lossy(&name).trim_end_matches('\0').to_string();
            let virtual_size = s.u32(8)?;
            let virtual_address = s.u32(12)?;
            let file_size = s.u32(16)?;
            let file_offset = s.u32(20)?;
            let characteristics = s.u32(36)?;
            // Only capture file-backed bytes; the BSS remainder is implicit.
            let data = if file_size > 0 {
                file_all.bytes(file_offset, file_size)?.to_vec()
            } else {
                Vec::new()
            };
            sections.push(Section {
                name,
                virtual_address,
                virtual_size,
                file_offset,
                file_size,
                characteristics,
                data,
            });
        }

        let image = ImageView { raw, sections: &sections, image_base };
        let imports = image.read_imports(dir(Directory::IMPORT), dir(Directory::DELAY_IMPORT))?;
        let exports = image.read_exports(dir(Directory::EXPORT))?;
        let relocations = image.read_relocations(dir(Directory::BASERELOC))?;
        let resources = resource::parse_resources(&image, dir(Directory::RESOURCE))?;
        let (unwind_info_rva, unwind_info_size) =
            dir(Directory::EXCEPTION).map(|d| (d.rva, d.size)).unwrap_or((0, 0));

        Ok(Pe {
            kind,
            machine,
            image_base,
            entry_point_rva,
            section_alignment,
            file_alignment,
            subsystem,
            dll_characteristics,
            size_of_image,
            size_of_headers,
            sections,
            directories,
            imports,
            exports,
            relocations,
            resources,
            unwind_info_rva,
            unwind_info_size,
            raw: raw.to_vec(),
        })
    }

    /// Translate an RVA to a file offset, if it falls in a section.
    pub fn rva_to_offset(&self, rva: u32) -> Option<u32> {
        self.sections.iter().find_map(|s| {
            if rva >= s.virtual_address && rva < s.virtual_end() {
                Some(s.file_offset + (rva - s.virtual_address))
            } else {
                None
            }
        })
    }

    /// Read `len` bytes starting at `rva`.
    pub fn read_rva(&self, rva: u32, len: u32) -> Option<&[u8]> {
        self.sections.iter().find_map(|s| s.slice_at_rva(rva, len))
    }

    /// Index of the section containing `rva`.
    pub fn section_of(&self, rva: u32) -> Option<usize> {
        self.sections.iter().position(|s| rva >= s.virtual_address && rva < s.virtual_end())
    }

    /// Executable sections as flat `(rva, bytes)` views.
    pub fn code_sections(&self) -> Vec<CodeSection> {
        self.sections
            .iter()
            .filter(|s| s.is_executable())
            .map(|s| CodeSection { name: s.name.clone(), rva: s.virtual_address, data: s.data.clone() })
            .collect()
    }
}

/// RVA-addressed accessor over the section table.
pub(crate) struct ImageView<'a> {
    pub raw: &'a [u8],
    pub sections: &'a [Section],
    /// Image base, used only to normalize legacy VA-style delay imports.
    pub image_base: u64,
}

impl<'a> ImageView<'a> {
    /// Read `len` bytes at `rva`, treating the image as the Windows loader
    /// would.
    ///
    /// Real-world detail: several Wine/Proton-built DLLs place the import
    /// directory immediately before `.rsrc` and the descriptor terminator
    /// straddles the boundary, so the read runs past `virtual_size` while still
    /// inside the section's raw file data. Windows maps `max(VirtualSize,
    /// SizeOfRawData)` bytes per section, so bytes beyond `VirtualSize` are
    /// copied from the file exactly as Windows does. A read that extends past
    /// the end of the raw data is an error (that would be `PAGE_NOACCESS`).
    pub fn bytes(&self, rva: u32, len: u32) -> Result<&'a [u8], PeError> {
        for s in self.sections {
            if rva < s.virtual_address {
                continue;
            }
            let off = (rva - s.virtual_address) as usize;
            let mapped_len = (s.virtual_size as usize).max(s.data.len());
            if off >= mapped_len {
                continue;
            }
            let end = off.checked_add(len as usize).ok_or(PeError::BadRva { rva })?;
            if end <= s.data.len() {
                return Ok(&s.data[off..end]);
            }
            // Mapped but not file-backed: an uninitialised section or a read
            // into the raw-size padding. We cannot borrow zeroes from a
            // temporary, so this only matters for fixed-size reads; callers
            // that hit it get a zero buffer through `read_exact`.
            return Err(PeError::BadRva { rva });
        }
        // Data directories may point into the headers themselves.
        let start = rva as usize;
        let end = start.checked_add(len as usize).ok_or(PeError::BadRva { rva })?;
        self.raw.get(start..end).ok_or(PeError::BadRva { rva })
    }

    /// Fixed-size read that tolerates reads into zero-filled mapped memory.
    pub fn read_exact(&self, rva: u32, len: u32) -> Result<Vec<u8>, PeError> {
        match self.bytes(rva, len) {
            Ok(b) => Ok(b.to_vec()),
            Err(e) => {
                // Fall back to zero-filling for the mapped-but-not-file-backed
                // tail of an uninitialised section (`.bss`).
                for s in self.sections {
                    if rva < s.virtual_address {
                        continue;
                    }
                    let off = (rva - s.virtual_address) as usize;
                    let mapped_len = (s.virtual_size as usize).max(s.data.len());
                    if off >= mapped_len {
                        continue;
                    }
                    let mut out = vec![0u8; len as usize];
                    let have = s.data.len().min(off + len as usize).saturating_sub(off);
                    out[..have].copy_from_slice(&s.data[off..off + have]);
                    return Ok(out);
                }
                Err(e)
            }
        }
    }
    pub fn u16(&self, rva: u32) -> Result<u16, PeError> {
        let b = self.read_exact(rva, 2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    pub fn u32(&self, rva: u32) -> Result<u32, PeError> {
        let b = self.read_exact(rva, 4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn cstr(&self, rva: u32) -> Result<String, PeError> {
        for s in self.sections {
            if rva < s.virtual_address {
                continue;
            }
            let off = (rva - s.virtual_address) as usize;
            let mapped_len = (s.virtual_size as usize).max(s.data.len());
            if off < mapped_len {
                let tail = s.data.get(off..).unwrap_or(&[]);
                let end = tail.iter().position(|&c| c == 0).unwrap_or(tail.len());
                if end > 4096 {
                    return Err(PeError::BadString { offset: rva });
                }
                return String::from_utf8(tail[..end].to_vec())
                    .map_err(|_| PeError::BadString { offset: rva });
            }
        }
        Err(PeError::BadRva { rva })
    }

    fn read_imports(
        &self,
        import: Option<Directory>,
        delay: Option<Directory>,
    ) -> Result<Vec<Import>, PeError> {
        let mut out = Vec::new();
        if let Some(d) = import {
            let mut rva = d.rva;
            loop {
                // IMAGE_IMPORT_DESCRIPTOR: OriginalFirstThunk, TimeDateStamp,
                // ForwarderChain, Name, FirstThunk (20 bytes).
                let oft = self.u32(rva)?;
                let name_rva = self.u32(rva + 12)?;
                let first_thunk = self.u32(rva + 16)?;
                if name_rva == 0 && first_thunk == 0 && oft == 0 {
                    break;
                }
                let library = self.cstr(name_rva)?;
                let lookup = if oft != 0 { oft } else { first_thunk };
                self.read_thunks(&library, lookup, first_thunk, false, &mut out)?;
                rva += 20;
            }
        }
        if let Some(d) = delay {
            let mut rva = d.rva;
            loop {
                // IMAGE_DELAYLOAD_DESCRIPTOR (32 bytes): Attributes, DllNameRVA,
                // ModuleHandleRVA, ImportAddressTableRVA, ImportNameTableRVA, ...
                let attrs = self.u32(rva)?;
                let name_field = self.u32(rva + 4)?;
                let iat = self.u32(rva + 12)?;
                let int = self.u32(rva + 16)?;
                if name_field == 0 && iat == 0 && int == 0 {
                    break;
                }
                let base = self.image_base as u32;
                let (name_rva, int_rva, iat_rva) = if attrs & 1 != 0 {
                    (name_field, int, iat)
                } else {
                    (name_field.wrapping_sub(base), int.wrapping_sub(base), iat.wrapping_sub(base))
                };
                let library = self.cstr(name_rva)?;
                self.read_thunks(&library, int_rva, iat_rva, true, &mut out)?;
                rva += 32;
            }
        }
        Ok(out)
    }

    fn read_thunks(
        &self,
        library: &str,
        lookup_rva: u32,
        iat_rva: u32,
        delayed: bool,
        out: &mut Vec<Import>,
    ) -> Result<(), PeError> {
        for i in 0..4096u32 {
            let entry = self.u32(lookup_rva + i * 4)?;
            if entry == 0 {
                break;
            }
            let iat_slot = iat_rva + i * 4;
            if entry & 0x8000_0000 != 0 {
                out.push(Import {
                    library: library.to_string(),
                    ordinal: Some((entry & 0xffff) as u16),
                    name: None,
                    iat_rva: iat_slot,
                    delayed,
                });
            } else {
                // IMAGE_IMPORT_BY_NAME: Hint (u16) then NUL-terminated name.
                let name = self.cstr(entry + 2)?;
                out.push(Import {
                    library: library.to_string(),
                    ordinal: None,
                    name: Some(name),
                    iat_rva: iat_slot,
                    delayed,
                });
            }
        }
        Ok(())
    }

    fn read_exports(&self, dir: Option<Directory>) -> Result<Vec<Export>, PeError> {
        let Some(d) = dir else { return Ok(Vec::new()) };
        let num_functions = self.u32(d.rva + 20)?;
        let num_names = self.u32(d.rva + 24)?;
        let functions_rva = self.u32(d.rva + 28)?;
        let names_rva = self.u32(d.rva + 32)?;
        let ordinals_rva = self.u32(d.rva + 36)?;
        let mut name_by_ordinal: BTreeMap<u32, String> = BTreeMap::new();
        for i in 0..num_names {
            let name_ptr = self.u32(names_rva + i * 4)?;
            let ord = self.u16(ordinals_rva + i * 2)? as u32;
            if let Ok(n) = self.cstr(name_ptr) {
                name_by_ordinal.insert(ord, n);
            }
        }
        let mut out = Vec::with_capacity(num_functions as usize);
        for i in 0..num_functions {
            let rva = self.u32(functions_rva + i * 4)?;
            if rva == 0 {
                continue;
            }
            let section =
                self.sections.iter().position(|s| rva >= s.virtual_address && rva < s.virtual_end());
            out.push(Export { name: name_by_ordinal.get(&i).cloned(), ordinal: i + 1, rva, section });
        }
        Ok(out)
    }

    fn read_relocations(&self, dir: Option<Directory>) -> Result<Vec<Relocation>, PeError> {
        let Some(d) = dir else { return Ok(Vec::new()) };
        let end = d.rva.saturating_add(d.size);
        let mut rva = d.rva;
        let mut out = Vec::new();
        while rva + 8 <= end {
            let page = self.u32(rva)?;
            let block_size = self.u32(rva + 4)?;
            if block_size < 8 {
                break;
            }
            let count = (block_size - 8) / 2;
            for i in 0..count {
                let entry = self.u16(rva + 8 + i * 2)?;
                let kind = match entry >> 12 {
                    0 => RelocKind::Absolute32,
                    1 => RelocKind::High,
                    2 => RelocKind::Low,
                    3 => RelocKind::Absolute,
                    10 => RelocKind::Dir64,
                    other => RelocKind::Other(other),
                };
                if kind == RelocKind::Absolute32 {
                    continue;
                }
                out.push(Relocation { rva: page.wrapping_add((entry & 0x0fff) as u32), kind });
            }
            rva += block_size;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_mz() {
        let err = Pe::parse(b"not an exe at all, sorry").unwrap_err();
        assert!(matches!(err, PeError::NotMz));
    }

    #[test]
    fn reader_is_bounds_checked() {
        let r = Reader::new(&[1, 2, 3, 4]);
        assert_eq!(r.u16(0).unwrap(), 0x0201);
        assert_eq!(r.u32(0).unwrap(), 0x0403_0201);
        assert!(r.u32(2).is_err());
        assert!(r.u8(4).is_err());
    }

    #[test]
    fn reader_u64_and_cstr() {
        let mut buf = vec![9u8; 8];
        buf.extend_from_slice(b"KERNEL32.dll\0tail");
        let r = Reader::new(&buf);
        assert_eq!(r.u64(0).unwrap(), 0x0909_0909_0909_0909);
        assert_eq!(r.cstr(8).unwrap(), "KERNEL32.dll");
        assert_eq!(r.cstr(21).unwrap(), "tail");
        // An unterminated string is read to the end of the buffer, not an
        // error; scan length stays bounded.
        assert_eq!(Reader::new(b"abc").cstr(0).unwrap(), "abc");
        assert!(Reader::new(b"abc").cstr(99).unwrap().is_empty());
        // Non-UTF8 content is rejected.
        assert!(Reader::new(&[0xff, 0xfe, 0]).cstr(0).is_err());
    }

    #[test]
    fn section_rva_mapping() {
        let s = Section {
            name: ".data".into(),
            virtual_address: 0x1000,
            virtual_size: 0x100,
            file_offset: 0x400,
            file_size: 0x100,
            characteristics: 0,
            data: (0..=255u8).collect(),
        };
        assert_eq!(s.slice_at_rva(0x1000, 4), Some(&[0, 1, 2, 3][..]));
        assert_eq!(s.slice_at_rva(0x10fc, 4), Some(&[252, 253, 254, 255][..]));
        assert_eq!(s.slice_at_rva(0x10fe, 4), None);
        assert_eq!(s.slice_at_rva(0xfff, 4), None);
    }
}
