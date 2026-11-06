//! Resolving ordinal imports through reference DLLs and import libraries.
//!
//! Windows executables frequently import by **ordinal** (no name), which is the
//! one case where the import table alone cannot tell us what an API is. The
//! names are recoverable from any real DLL of the same version: its export
//! directory maps ordinal → name. Some DLLs (notably MFC 3.x) only export by
//! ordinal, so their export tables cannot help. For those, the matching MSVC
//! import library (`.lib`) contains name → ordinal records; this module accepts
//! both forms in an oracle directory. Neither form is treated as a build input
//! — we never recompile or bundle the reference binaries.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use wlam_pe::Pe;

/// Ordinal → export name, per library, loaded from reference DLLs.
#[derive(Debug, Default)]
pub struct OrdinalOracle {
    /// lowercased library stem -> (ordinal -> export name)
    tables: BTreeMap<String, BTreeMap<u16, String>>,
    /// Files that actually contributed a table, for honest reporting.
    loaded: Vec<PathBuf>,
}

fn library_stem(library: &str) -> String {
    let lower = library.to_ascii_lowercase();
    lower.strip_suffix(".dll").unwrap_or(&lower).to_string()
}

impl OrdinalOracle {
    /// Index every `.dll` in `dir`.
    pub fn from_dir(dir: &Path) -> std::io::Result<Self> {
        let mut oracle = OrdinalOracle::default();
        for entry in std::fs::read_dir(dir)?.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let extension = path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            let is_dll = extension == "dll";
            let is_lib = extension == "lib";
            if !is_dll && !is_lib {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if is_lib {
                if let Some((library, table)) = parse_import_library(&bytes) {
                    if !table.is_empty() {
                        let stem = library_stem(&library);
                        oracle.tables.entry(stem).or_default().extend(table);
                        oracle.loaded.push(path);
                    }
                }
                continue;
            }
            let Ok(pe) = Pe::parse(&bytes) else { continue };
            let Some(stem) = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_ascii_lowercase())
            else {
                continue;
            };
            let table = oracle.tables.entry(stem).or_default();
            for e in &pe.exports {
                if let Some(name) = &e.name {
                    if let Ok(ord) = u16::try_from(e.ordinal) {
                        table.insert(ord, name.clone());
                    }
                }
            }
            oracle.loaded.push(path);
        }
        Ok(oracle)
    }

    /// Look up the name for an imported ordinal.
    pub fn resolve(&self, library: &str, ordinal: u16) -> Option<&str> {
        self.tables
            .get(&library_stem(library))
            .and_then(|t| t.get(&ordinal))
            .map(|s| s.as_str())
    }

    /// Fold another oracle into this one (later entries win).
    pub fn merge(&mut self, other: OrdinalOracle) {
        for (lib, table) in other.tables {
            self.tables.entry(lib).or_default().extend(table);
        }
        self.loaded.extend(other.loaded);
    }

    /// Reference DLLs loaded.
    pub fn loaded(&self) -> &[PathBuf] {
        &self.loaded
    }

    /// Whether any reference DLL for `library` was available. Distinguishes
    /// "we have no oracle for this library" from "this ordinal does not exist",
    /// which callers must not conflate.
    pub fn has_library(&self, library: &str) -> bool {
        self.tables.contains_key(&library_stem(library))
    }
}

fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}
fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}
fn cstr(bytes: &[u8], at: usize) -> Option<String> {
    let tail = bytes.get(at..)?;
    let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
    Some(String::from_utf8_lossy(&tail[..end]).to_string())
}

/// Parse one MSVC import-library archive. An import library is a COFF archive
/// whose members are short import objects. For an ordinal import, `.idata$4`
/// (or `.idata$5`) holds `0x8000_0000 | ordinal`, and the COFF symbol table
/// names the `__imp_` thunk. That pair is exactly the oracle mapping.
fn parse_import_library(bytes: &[u8]) -> Option<(String, BTreeMap<u16, String>)> {
    if !bytes.starts_with(b"!<arch>\n") {
        return None;
    }
    let mut at = 8;
    let mut library: Option<String> = None;
    let mut table = BTreeMap::new();
    while at + 60 <= bytes.len() {
        let header = bytes.get(at..at + 60)?;
        let raw_name = std::str::from_utf8(&header[..16]).ok()?.trim();
        let size = std::str::from_utf8(&header[48..58])
            .ok()?
            .trim()
            .parse::<usize>()
            .ok()?;
        at += 60;
        let body = bytes.get(at..at.checked_add(size)?)?;
        at = at.checked_add(size + (size & 1))?;
        if raw_name.is_empty() || raw_name == "/" || raw_name == "//" {
            continue;
        }
        let member = raw_name.trim_end_matches('/');
        if !member.to_ascii_lowercase().ends_with(".dll") {
            continue;
        }
        let Some((ordinal, name)) = parse_import_object(body) else {
            continue;
        };
        library.get_or_insert_with(|| member.to_ascii_lowercase());
        table.insert(ordinal, name);
    }
    Some((library?, table))
}

fn parse_import_object(body: &[u8]) -> Option<(u16, String)> {
    let sections = read_u16(body, 2)? as usize;
    let symbol_table = read_u32(body, 8)? as usize;
    let symbols = read_u32(body, 12)? as usize;
    let optional = read_u16(body, 16)? as usize;
    let section_table = 20usize.checked_add(optional)?;

    let mut ordinal = None;
    for index in 0..sections {
        let at = section_table.checked_add(index.checked_mul(40)?)?;
        let name_bytes = body.get(at..at + 8)?;
        let name = String::from_utf8_lossy(name_bytes)
            .trim_end_matches('\0')
            .to_string();
        if name != ".idata$4" && name != ".idata$5" {
            continue;
        }
        let raw_offset = read_u32(body, at + 20)? as usize;
        let value = read_u32(body, raw_offset)?;
        if value & 0x8000_0000 != 0 {
            ordinal = Some((value & 0xffff) as u16);
            break;
        }
    }
    let ordinal = ordinal?;

    let string_offset = symbol_table.checked_add(symbols.checked_mul(18)?)?;
    let string_size = read_u32(body, string_offset)? as usize;
    let strings = body.get(string_offset + 4..string_offset + string_size)?;
    for index in 0..symbols {
        let at = symbol_table.checked_add(index.checked_mul(18)?)?;
        let raw = body.get(at..at + 8)?;
        let name = if raw[..4] == [0, 0, 0, 0] {
            let offset = u32::from_le_bytes(raw[4..8].try_into().ok()?) as usize;
            cstr(strings, offset.checked_sub(4)?)?
        } else {
            cstr(raw, 0)?
        };
        if let Some(name) = name.strip_prefix("__imp_") {
            if !name.is_empty() {
                return Some((ordinal, name.to_string()));
            }
        }
    }
    None
}

/// Common locations for a reference DLL corpus on a Steam/Proton machine.
/// Explicit configuration always wins over these guesses.
pub fn default_reference_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(home) = std::env::var("HOME") else {
        return out;
    };
    let steam = PathBuf::from(home).join(".local/share/Steam/steamapps/common");
    let Ok(entries) = std::fs::read_dir(&steam) else {
        return out;
    };
    let mut protons: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().starts_with("Proton"))
                .unwrap_or(false)
        })
        .collect();
    // Prefer "Proton - Experimental", then anything else, deterministically.
    protons.sort();
    for proton in protons {
        for sub in ["i386-windows", "x86_64-windows"] {
            let d = proton.join("files/lib/wine").join(sub);
            if d.is_dir() {
                out.push(d);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_stem_normalizes() {
        assert_eq!(library_stem("KERNEL32.dll"), "kernel32");
        assert_eq!(library_stem("comctl32"), "comctl32");
    }

    #[test]
    fn missing_library_is_distinguishable_from_missing_ordinal() {
        let o = OrdinalOracle::default();
        assert!(!o.has_library("comctl32.dll"));
        assert_eq!(o.resolve("comctl32.dll", 17), None);
    }

    #[test]
    fn resolves_known_ordinals_from_proton() {
        let dirs = default_reference_dirs();
        let Some(dir) = dirs.first() else {
            eprintln!("skipping: no Proton reference DLLs available");
            return;
        };
        let oracle = OrdinalOracle::from_dir(dir).expect("index reference DLLs");
        assert!(!oracle.loaded().is_empty());
        // Wine's comctl32 export base is 2: ordinal 17 is InitCommonControls
        // and ordinal 18 is CreatePropertySheetPageA.
        assert_eq!(
            oracle.resolve("COMCTL32.dll", 17),
            Some("InitCommonControls")
        );
        assert_eq!(
            oracle.resolve("COMCTL32.dll", 18),
            Some("CreatePropertySheetPageA")
        );
        assert!(oracle.has_library("kernel32.dll"));
    }
}
