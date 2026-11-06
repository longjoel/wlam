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
mod types;

pub use error::PeError;
pub use resource::{ResourceEntry, ResourceName, ResourceTable};

pub use types::*;

use std::collections::BTreeMap;

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
    #[cfg(test)]
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
        let hi = self.u32(
            off.checked_add(4)
                .ok_or(PeError::OutOfBounds { offset: off })?,
        )?;
        Ok(lo | (hi as u64) << 32)
    }
    fn bytes(&self, off: u32, len: u32) -> Result<&'a [u8], PeError> {
        let start = (self.base as u64)
            .checked_add(off as u64)
            .ok_or(PeError::OutOfBounds { offset: off })? as usize;
        let end = start
            .checked_add(len as usize)
            .ok_or(PeError::OutOfBounds { offset: off })?;
        self.data
            .get(start..end)
            .ok_or(PeError::OutOfBounds { offset: off })
    }
    /// Read a NUL-terminated string starting at `off`.
    ///
    /// A string that runs to the end of the buffer without a terminator is
    /// accepted for its full length; only a *non-UTF8* sequence is an error.
    /// This matters for debug directories, whose paths are sometimes not
    /// NUL-terminated at EOF.
    #[cfg(test)]
    fn cstr(&self, off: u32) -> Result<String, PeError> {
        let start = (self.base as u64)
            .checked_add(off as u64)
            .unwrap_or(u64::MAX) as usize;
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
        let image_base = if is64 {
            opt.u64(24)?
        } else {
            opt.u32(28)? as u64
        };
        let section_alignment = opt.u32(32)?;
        let file_alignment = opt.u32(36)?;
        let size_of_image = opt.u32(56)?;
        let size_of_headers = opt.u32(60)?;
        let subsystem = opt.u16(68)?;
        let dll_characteristics = opt.u16(70)?;
        let declared_dirs = opt.u32(92)? as usize;
        let dir_base = if is64 { 112 } else { 96 };
        // `NumberOfRvaAndSizes` is not always trustworthy: bins produced by
        // some linkers leave it at 0 while still emitting a full header. Trust
        // the *header size* to bound what could be present, but only read
        // entries the declared count admits, then recover structural
        // directories afterwards.
        let capacity = ((opt_size as usize).saturating_sub(dir_base as usize) / 8).min(16);
        let n_dirs = declared_dirs.min(capacity);
        let mut directories = Vec::new();
        for i in 0..n_dirs {
            let off = dir_base + (i as u32) * 8;
            directories.push(Directory {
                index: i,
                rva: opt.u32(off)?,
                size: opt.u32(off + 4)?,
            });
        }
        fn dir_of(dirs: &[Directory], i: usize) -> Option<Directory> {
            dirs.iter().copied().find(|d| d.index == i && d.rva != 0)
        }

        // Section table follows the optional header.
        let sec_base = e_lfanew + 24 + opt_size as usize;
        let file_all = Reader::new(raw);
        let mut sections = Vec::with_capacity(num_sections as usize);
        for i in 0..num_sections as u32 {
            let s = Reader::at(raw, sec_base + (i as usize) * 40);
            let mut name = [0u8; 8];
            name.copy_from_slice(s.bytes(0, 8)?);
            let name = String::from_utf8_lossy(&name)
                .trim_end_matches('\0')
                .to_string();
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

        let image = ImageView {
            raw,
            sections: &sections,
            image_base,
            is64,
        };
        // Some linkers emit an intact import table with `NumberOfRvaAndSizes`
        // = 0. Trusting the header alone would report a GUI program with no
        // imports, which never reflects reality; recover it structurally.
        let recovered_imports = if dir_of(&directories, Directory::IMPORT).is_none() {
            image.find_import_directory()
        } else {
            None
        };
        if let Some(rva) = recovered_imports {
            directories.push(Directory {
                index: Directory::IMPORT,
                rva,
                size: 0,
            });
        }
        let imports = image.read_imports(
            dir_of(&directories, Directory::IMPORT),
            dir_of(&directories, Directory::DELAY_IMPORT),
        )?;
        let exports = image.read_exports(dir_of(&directories, Directory::EXPORT))?;
        let relocations = image.read_relocations(dir_of(&directories, Directory::BASERELOC))?;
        let resources =
            resource::parse_resources(&image, dir_of(&directories, Directory::RESOURCE))?;
        let (unwind_info_rva, unwind_info_size) = dir_of(&directories, Directory::EXCEPTION)
            .map(|d| (d.rva, d.size))
            .unwrap_or((0, 0));

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
        self.sections
            .iter()
            .position(|s| rva >= s.virtual_address && rva < s.virtual_end())
    }

    /// Executable sections as flat `(rva, bytes)` views.
    pub fn code_sections(&self) -> Vec<CodeSection> {
        self.sections
            .iter()
            .filter(|s| s.is_executable())
            .map(|s| CodeSection {
                name: s.name.clone(),
                rva: s.virtual_address,
                data: s.data.clone(),
            })
            .collect()
    }
}

/// RVA-addressed accessor over the section table.
pub(crate) struct ImageView<'a> {
    pub raw: &'a [u8],
    pub sections: &'a [Section],
    /// Image base, used only to normalize legacy VA-style delay imports.
    pub image_base: u64,
    /// Whether this is a PE32+ image (8-byte thunks and pointers).
    pub is64: bool,
}

impl<'a> ImageView<'a> {
    /// Read a NUL-terminated ASCII string at `rva`, if it is printable.
    fn ascii_at(&self, rva: u32) -> Option<String> {
        let mut out = Vec::new();
        for i in 0..=260u32 {
            let c = *self.bytes(rva + i, 1).ok()?.first()?;
            if c == 0 {
                return Some(String::from_utf8_lossy(&out).into_owned());
            }
            if !c.is_ascii_graphic() || out.len() > 255 {
                return None;
            }
            out.push(c);
        }
        None
    }

    /// Validate `cand` as the start of an `IMAGE_IMPORT_DESCRIPTOR` list.
    ///
    /// Structural signature: a plausible module name ending in `.dll`, a thunk
    /// array with at least one well-formed entry and a terminator, and a list
    /// terminated by an all-zero descriptor. This is intentionally strict so
    /// that the scan cannot invent imports out of unrelated data.
    fn import_table_at(&self, cand: u32) -> bool {
        let u32_at = |rva: u32| {
            self.bytes(rva, 4)
                .ok()
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        };
        let (Some(oft), Some(name_rva), Some(first_thunk)) =
            (u32_at(cand), u32_at(cand + 12), u32_at(cand + 16))
        else {
            return false;
        };
        if name_rva == 0 || (oft == 0 && first_thunk == 0) {
            return false;
        }
        match self.ascii_at(name_rva) {
            Some(n) if n.to_ascii_lowercase().ends_with(".dll") => {}
            _ => return false,
        }
        let lookup = if oft != 0 { oft } else { first_thunk };
        let mut entries = 0u32;
        for i in 0..64u32 {
            let Some(entry) = u32_at(lookup + i * 4) else {
                return false;
            };
            if entry == 0 {
                break;
            }
            if entry & 0x8000_0000 == 0 && self.bytes(entry + 2, 1).is_err() {
                return false;
            }
            entries += 1;
        }
        if entries == 0 {
            return false;
        }
        for i in 1..=64u32 {
            let d = cand + i * 20;
            match (u32_at(d), u32_at(d + 12), u32_at(d + 16)) {
                (Some(0), Some(0), Some(0)) => return true,
                (Some(_), Some(_), Some(_)) => continue,
                _ => return false,
            }
        }
        false
    }

    /// Locate an import descriptor list by scanning sections that contain a
    /// `.dll` string, when the header does not declare one.
    pub fn find_import_directory(&self) -> Option<u32> {
        for sec in self.sections {
            if !sec.is_readable() {
                continue;
            }
            if !sec.data.windows(4).any(|w| w.eq_ignore_ascii_case(b".dll")) {
                continue;
            }
            for off in (0..sec.data.len().saturating_sub(20)).step_by(4) {
                let cand = sec.virtual_address + off as u32;
                if self.import_table_at(cand) {
                    return Some(cand);
                }
            }
        }
        None
    }

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
            let end = off
                .checked_add(len as usize)
                .ok_or(PeError::BadRva { rva })?;
            if end <= s.data.len() {
                return Ok(&s.data[off..end]);
            }
            // The RVA is inside this section but the read runs past its
            // file-backed bytes (uninitialised tail or raw-size padding).
            // Another section may own the target bytes: real mingw images can
            // write `.idata` *after* `.rdata` in the section table even when
            // `.rdata` sits above it, so returning an error here would hide
            // valid data. Keep scanning other sections instead.
            continue;
        }
        // Data directories may point into the headers themselves.
        let start = rva as usize;
        let end = start
            .checked_add(len as usize)
            .ok_or(PeError::BadRva { rva })?;
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
                // A descriptor whose module name is not a plausible DLL name
                // ends the chain: it is not an import descriptor.
                let Some(library) = self.plausible_module(name_rva) else {
                    eprintln!("DBG import break at rva {rva:#x}: oft={oft:#x} name={name_rva:#x} ft={first_thunk:#x}");
                    break;
                };
                eprintln!(
                    "DBG import {library} oft={oft:#x} name={name_rva:#x} ft={first_thunk:#x}"
                );
                let lookup = if oft != 0 { oft } else { first_thunk };
                self.read_thunks(&library, lookup, first_thunk, false, 4096, &mut out)?;
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
                    (
                        name_field.wrapping_sub(base),
                        int.wrapping_sub(base),
                        iat.wrapping_sub(base),
                    )
                };
                let Some(library) = self.plausible_module(name_rva) else {
                    break;
                };
                self.read_thunks(&library, int_rva, iat_rva, true, 4096, &mut out)?;
                rva += 32;
            }
        }
        Ok(out)
    }

    /// Read a thunk array into `out`.
    ///
    /// The distance between thunk arrays is not fixed: some linkers emit a
    /// compact table made only of name/ordinal entries, while others (mingw
    /// with a delay-load style layout) interleave the *bound* address values
    /// in the IAT right after the name entries, making the name table longer
    /// than the import count. Walking to the first NULL is therefore correct
    /// only when the NULL is part of the table; when the array runs straight
    /// into the next library's thunks the walk must not invent imports.
    ///
    /// Termination and validity rules:
    ///  * a NULL entry ends the array;
    ///  * an ordinal entry (high bit set) is accepted;
    ///  * a name entry is accepted only when the hint/name record it points at
    ///    is readable and its name is printable ASCII;
    ///  * anything else ends the array instead of being misread as an import.
    /// Thunk width: 4 bytes in a PE32 image, 8 in a PE32+ image.
    fn thunk_width(&self) -> u32 {
        if self.is64 {
            8
        } else {
            4
        }
    }

    fn read_thunk(&self, rva: u32) -> Result<u32, PeError> {
        // Names and ordinals only ever use the low 32 bits of a thunk.
        self.u32(rva)
    }

    /// Length of the name/ordinal array at `lookup_rva` (entries before the
    /// first terminator).
    fn thunk_count(&self, lookup_rva: u32, limit: u32) -> Result<u32, PeError> {
        let width = self.thunk_width();
        for i in 0..limit {
            if self.read_thunk(lookup_rva + i * width)? == 0 {
                return Ok(i);
            }
        }
        Ok(limit)
    }

    /// Whether an IAT *slot* plausibly holds a loader-resolved address, judged
    /// from the file image.
    ///
    /// On disk an unbound IAT usually mirrors the name table, but linkers may
    /// rebase the two independently (mingw does), so a slot in another
    /// section holds an arbitrary RVA. A slot is trusted when either its low
    /// half points inside the image, or — for a 64-bit image — it reads as a
    /// full pointer into the image.
    fn value_inside_image(&self, value: u32) -> bool {
        self.sections.iter().any(|s| {
            value >= s.virtual_address
                && value
                    < s.virtual_address
                        .saturating_add(s.virtual_size.max(s.data.len() as u32))
        })
    }

    fn slot_points_inside_image(&self, rva: u32) -> bool {
        if self
            .bytes(rva, 4)
            .map(|b| self.value_inside_image(u32::from_le_bytes([b[0], b[1], b[2], b[3]])))
            .unwrap_or(false)
        {
            return true;
        }
        self.is64
            && self
                .bytes(rva, 8)
                .map(|b| {
                    let lo = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                    self.value_inside_image(lo)
                })
                .unwrap_or(false)
    }

    /// Count consecutive IAT slots that pass [`Self::slot_points_inside_image`].
    fn address_count(&self, iat_rva: u32, limit: u32) -> u32 {
        let width = self.thunk_width();
        let mut n = 0;
        for i in 0..limit.min(4096) {
            if !self.slot_points_inside_image(iat_rva + i * width) {
                break;
            }
            n += 1;
        }
        n
    }

    fn read_thunks(
        &self,
        library: &str,
        lookup_rva: u32,
        iat_rva: u32,
        delayed: bool,
        limit: u32,
        out: &mut Vec<Import>,
    ) -> Result<(), PeError> {
        let width = self.thunk_width();
        let names = self.thunk_count(lookup_rva, limit)?;
        // Bind IAT slots only where the two arrays are demonstrably parallel.
        // mingw can place the IAT in a different section from the name table
        // and leave stale values in it, so misaligned slots must be flagged
        // rather than trusted.
        let slots = self.address_count(iat_rva, names);
        // An unbound IAT may mirror ordinal entries as well as name RVAs.
        // Ordinal flags are not image addresses, but equality with the lookup
        // array independently proves that these slots are parallel.
        let mirrors = (0..names).all(|i| {
            self.read_thunk(lookup_rva + i * width).ok()
                == self.read_thunk(iat_rva + i * width).ok()
                && self.read_thunk(iat_rva + i * width).is_ok()
        });
        // A bound image stores resolved addresses (usually into another DLL)
        // in the IAT instead of name RVAs, so those slots are not inside this
        // image and `slots` is zero. Every slot being a non-zero address
        // outside this image is the bound-import signature.
        let bound = (0..names).all(|i| {
            self.read_thunk(iat_rva + i * width)
                .map(|value| value != 0 && !self.value_inside_image(value))
                .unwrap_or(false)
        });
        let aligned = mirrors || slots >= names || bound;
        for i in 0..names {
            let entry = self.read_thunk(lookup_rva + i * width)?;
            let iat_slot = if aligned {
                iat_rva + i * width
            } else {
                // Unaligned: address the IAT by the index we can justify, and
                // callers see `iat_slot_verified == false`.
                iat_rva + i.min(slots.saturating_sub(1)) * width
            };
            if entry & 0x8000_0000 != 0 {
                out.push(Import {
                    library: library.to_string(),
                    ordinal: Some((entry & 0xffff) as u16),
                    name: None,
                    iat_rva: iat_slot,
                    iat_slot_verified: aligned,
                    delayed,
                });
                continue;
            }
            let Some(name) = self.plausible_name(entry + 2) else {
                // A malformed entry ends the array; never misread the rest as
                // imports.
                break;
            };
            out.push(Import {
                library: library.to_string(),
                ordinal: None,
                name: Some(name),
                iat_rva: iat_slot,
                iat_slot_verified: aligned,
                delayed,
            });
        }
        Ok(())
    }

    /// Read a library name at `rva`, accepting only plausible module names
    /// (`name.dll`, printable). Descriptors are validated with this before
    /// being trusted, because a zeroed or stale 16-entry directory table can
    /// point the delay-import index at unrelated data (as mingw's PE32+
    /// images do), and following it produces garbage names.
    fn plausible_module(&self, rva: u32) -> Option<String> {
        let name = self.cstr(rva).ok()?;
        if name.is_empty() || name.len() > 255 {
            return None;
        }
        if !name.bytes().all(|c| c.is_ascii_graphic()) {
            return None;
        }
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".dll") || lower.ends_with(".drv") {
            Some(name)
        } else {
            None
        }
    }

    /// Read a symbol name at `rva` only if it looks like one: non-empty,
    /// bounded, and printable ASCII.
    fn plausible_name(&self, rva: u32) -> Option<String> {
        // The 16-bit hint must be readable for the record to be well-formed.
        let mut out = Vec::new();
        for i in 0..=256u32 {
            let c = *self.bytes(rva + i, 1).ok()?.first()?;
            if c == 0 {
                break;
            }
            if !c.is_ascii_graphic() || out.len() > 255 {
                return None;
            }
            out.push(c);
        }
        if out.is_empty() {
            return None;
        }
        String::from_utf8(out).ok()
    }

    fn read_exports(&self, dir: Option<Directory>) -> Result<Vec<Export>, PeError> {
        let Some(d) = dir else { return Ok(Vec::new()) };
        let ordinal_base = self.u32(d.rva + 16)?;
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
            let section = self
                .sections
                .iter()
                .position(|s| rva >= s.virtual_address && rva < s.virtual_end());
            out.push(Export {
                name: name_by_ordinal.get(&i).cloned(),
                // `Base` is not always 1: MFC30.DLL starts at 268.
                ordinal: ordinal_base + i,
                rva,
                section,
            });
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
                out.push(Relocation {
                    rva: page.wrapping_add((entry & 0x0fff) as u32),
                    kind,
                });
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
    fn drv_descriptors_do_not_hide_later_ordinal_imports() {
        let mut data = vec![0; 512];
        let put = |data: &mut [u8], at: usize, words: &[u32]| {
            for (i, value) in words.iter().enumerate() {
                data[at + i * 4..at + i * 4 + 4].copy_from_slice(&value.to_le_bytes());
            }
        };
        put(&mut data, 0, &[0x1080, 0, 0, 0x1100, 0x10a0]);
        put(&mut data, 20, &[0x1090, 0, 0, 0x1120, 0x10b0]);
        put(&mut data, 0x80, &[0x1140, 0]);
        put(&mut data, 0xa0, &[0x1140, 0]);
        put(&mut data, 0x90, &[0x80000011, 0]);
        put(&mut data, 0xb0, &[0x80000011, 0]);
        data[0x100..0x10d].copy_from_slice(b"WINSPOOL.DRV\0");
        data[0x120..0x12d].copy_from_slice(b"COMCTL32.dll\0");
        data[0x142..0x14f].copy_from_slice(b"OpenPrinterA\0");
        let section = Section {
            name: ".idata".into(),
            virtual_address: 0x1000,
            virtual_size: 512,
            file_offset: 0,
            file_size: 512,
            characteristics: 0x40000040,
            data,
        };
        let sections = [section];
        let image = ImageView {
            raw: &[],
            sections: &sections,
            image_base: 0x400000,
            is64: false,
        };
        let imports = image
            .read_imports(
                Some(Directory {
                    index: 1,
                    rva: 0x1000,
                    size: 60,
                }),
                None,
            )
            .unwrap();
        assert_eq!(imports.len(), 2);
        assert_eq!(imports[1].ordinal, Some(17));
        assert_eq!(imports[1].iat_rva, 0x10b0);
        assert!(imports.iter().all(|import| import.iat_slot_verified));
    }

    #[test]
    fn bound_iat_slots_are_verified_even_when_addresses_are_outside_the_image() {
        let mut data = vec![0; 0x200];
        let put = |data: &mut [u8], at: usize, words: &[u32]| {
            for (i, value) in words.iter().enumerate() {
                data[at + i * 4..at + i * 4 + 4].copy_from_slice(&value.to_le_bytes());
            }
        };
        // Descriptor at RVA 0x1000: lookup 0x1140, name 0x1100, IAT 0x10a0.
        put(&mut data, 0, &[0x1140, 0, 0, 0x1100, 0x10a0]);
        // The IAT is already bound to addresses in another module.
        put(&mut data, 0xa0, &[0x7fd81000, 0x7fd82000]);
        data[0x100..0x10b].copy_from_slice(b"MSVCRT.dll\0");
        put(&mut data, 0x140, &[0x1160, 0x1170]);
        data[0x162..0x166].copy_from_slice(b"Foo\0");
        data[0x172..0x176].copy_from_slice(b"Bar\0");
        let section = Section {
            name: ".idata".into(),
            virtual_address: 0x1000,
            virtual_size: 0x200,
            file_offset: 0,
            file_size: 0x200,
            characteristics: 0x40000040,
            data,
        };
        let sections = [section];
        let image = ImageView {
            raw: &[],
            sections: &sections,
            image_base: 0x400000,
            is64: false,
        };
        let imports = image
            .read_imports(
                Some(Directory {
                    index: 1,
                    rva: 0x1000,
                    size: 20,
                }),
                None,
            )
            .unwrap();
        assert_eq!(imports.len(), 2);
        assert!(imports.iter().all(|import| import.iat_slot_verified));
        assert_eq!(imports[0].iat_rva, 0x10a0);
        assert_eq!(imports[1].iat_rva, 0x10a4);
        assert_eq!(imports[0].name.as_deref(), Some("Foo"));
        assert_eq!(imports[1].name.as_deref(), Some("Bar"));
    }

    #[test]
    fn export_ordinals_use_the_declared_base() {
        let mut data = vec![0; 0x200];
        let put = |data: &mut [u8], at: usize, words: &[u32]| {
            for (i, value) in words.iter().enumerate() {
                data[at + i * 4..at + i * 4 + 4].copy_from_slice(&value.to_le_bytes());
            }
        };
        put(&mut data, 16, &[268]); // Base
        put(&mut data, 20, &[2]); // NumberOfFunctions
        put(&mut data, 24, &[1]); // NumberOfNames
        put(&mut data, 28, &[0x1040]); // AddressOfFunctions
        put(&mut data, 32, &[0x1050]); // AddressOfNames
        put(&mut data, 36, &[0x1054]); // AddressOfNameOrdinals
        put(&mut data, 0x40, &[0x1100, 0x1110]);
        put(&mut data, 0x50, &[0x1060]);
        data[0x54..0x56].copy_from_slice(&1u16.to_le_bytes());
        data[0x60..0x64].copy_from_slice(b"Foo\0");
        let section = Section {
            name: ".text".into(),
            virtual_address: 0x1000,
            virtual_size: 0x200,
            file_offset: 0,
            file_size: 0x200,
            characteristics: 0x60000020,
            data,
        };
        let sections = [section];
        let image = ImageView {
            raw: &[],
            sections: &sections,
            image_base: 0x400000,
            is64: false,
        };
        let exports = image
            .read_exports(Some(Directory {
                index: 0,
                rva: 0x1000,
                size: 40,
            }))
            .unwrap();
        assert_eq!(exports.len(), 2);
        assert_eq!(exports[0].ordinal, 268);
        assert_eq!(exports[0].name, None);
        assert_eq!(exports[1].ordinal, 269);
        assert_eq!(exports[1].name.as_deref(), Some("Foo"));
    }

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
