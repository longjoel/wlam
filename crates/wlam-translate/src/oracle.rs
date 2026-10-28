//! Resolving ordinal imports through reference DLLs.
//!
//! Windows executables frequently import by **ordinal** (no name), which is the
//! one case where the import table alone cannot tell us what an API is. The
//! names are recoverable from any real DLL of the same version: its export
//! directory maps ordinal → name. This module uses a directory of reference
//! DLLs as that oracle, taking the one fact we need (names) without treating
//! the DLLs as build inputs — we never recompile or bundle them.

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
            let is_dll = path
                .extension()
                .map(|e| e.eq_ignore_ascii_case("dll"))
                .unwrap_or(false);
            if !is_dll {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
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
        // comctl32 ordinal 17 is CreatePropertySheetPageA in Wine's exports.
        assert_eq!(
            oracle.resolve("COMCTL32.dll", 17),
            Some("CreatePropertySheetPageA")
        );
        assert!(oracle.has_library("kernel32.dll"));
    }
}
