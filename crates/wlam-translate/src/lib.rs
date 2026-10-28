//! Translation pipeline: discovery, call resolution, and compat reporting.
//!
//! The lifter (`wlam-lift-x86`) knows individual instructions; this crate only
//! orchestrates it and answers whole-program questions. The first answer it
//! needs to give is "what does this program actually import, and are we
//! committed to providing it?" — see [`compat`].

pub mod compat;
pub mod oracle;

pub use compat::{Implementation, Matrix};
pub use oracle::OrdinalOracle;

use wlam_pe::Pe;

/// Coarse provenance class of an import's identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// The name was in the import table.
    Named,
    /// An ordinal import resolved through a reference DLL's export table.
    RecoveredFromReferenceDll,
    /// An ordinal that no available reference DLL could resolve.
    UnresolvedOrdinal,
}

/// Where an import's identity came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOrigin {
    /// The name was in the import table.
    Named(String),
    /// An ordinal import resolved through a reference DLL's export table.
    RecoveredFromReferenceDll { ordinal: u16, name: String },
    /// An ordinal that no available reference DLL could resolve.
    UnresolvedOrdinal {
        ordinal: u16,
        reference_dll_available: bool,
    },
}

impl ImportOrigin {
    /// Provenance class, for callers that only need the category.
    pub fn provenance(&self) -> Provenance {
        match self {
            ImportOrigin::Named(_) => Provenance::Named,
            ImportOrigin::RecoveredFromReferenceDll { .. } => Provenance::RecoveredFromReferenceDll,
            ImportOrigin::UnresolvedOrdinal { .. } => Provenance::UnresolvedOrdinal,
        }
    }

    /// The API name, if any.
    pub fn name(&self) -> Option<&str> {
        match self {
            ImportOrigin::Named(n) | ImportOrigin::RecoveredFromReferenceDll { name: n, .. } => {
                Some(n)
            }
            ImportOrigin::UnresolvedOrdinal { .. } => None,
        }
    }
}

/// One analyzed import, joining the PE table entry to its resolved identity.
#[derive(Debug, Clone)]
pub struct ResolvedImport {
    pub library: String,
    /// The import-table symbol (`LIBRARY!Name` or `LIBRARY!#Ordinal`).
    pub symbol: String,
    pub origin: ImportOrigin,
    pub implementation: Implementation,
    /// Subsystem, or the reason no classification was possible.
    pub area: String,
}

/// Intern distinct area strings once per process. Bounded by the number of
/// distinct reasons, not by the number of imports.
fn intern_area(s: &str) -> &'static str {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static AREAS: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let set = AREAS.get_or_init(|| Mutex::new(HashSet::new()));
    let mut set = set.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = set.get(s) {
        return existing;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    set.insert(leaked);
    leaked
}

/// A high-level view of a Windows image, ready for a human to inspect.
#[derive(Debug)]
pub struct Analysis {
    pub image: Pe,
    pub matrix: Matrix,
    /// Every import with its resolved identity, in import-table order.
    pub imports: Vec<ResolvedImport>,
}

impl Analysis {
    /// Analyze an image without an ordinal oracle: unresolvable ordinals are
    /// reported as such rather than guessed.
    pub fn new(image: Pe) -> Self {
        Self::with_oracle(image, &OrdinalOracle::default())
    }

    /// Analyze an image, resolving ordinal imports through `oracle` when it has
    /// the library.
    pub fn with_oracle(image: Pe, oracle: &OrdinalOracle) -> Self {
        let mut matrix = Matrix::default();
        let mut imports = Vec::with_capacity(image.imports.len());
        for imp in &image.imports {
            let origin = match (&imp.name, imp.ordinal) {
                (Some(n), _) => ImportOrigin::Named(n.clone()),
                (None, Some(ord)) => match oracle.resolve(&imp.library, ord) {
                    Some(n) => ImportOrigin::RecoveredFromReferenceDll {
                        ordinal: ord,
                        name: n.to_string(),
                    },
                    None => ImportOrigin::UnresolvedOrdinal {
                        ordinal: ord,
                        reference_dll_available: oracle.has_library(&imp.library),
                    },
                },
                (None, None) => ImportOrigin::UnresolvedOrdinal {
                    ordinal: 0,
                    reference_dll_available: false,
                },
            };
            // Classification is by symbol, so a recovered ordinal name is
            // classified exactly like a named import of the same API.
            let (implementation, area) = Matrix::classify(&imp.library, origin.name());
            let area = match &origin {
                ImportOrigin::UnresolvedOrdinal {
                    reference_dll_available: true,
                    ..
                } => "ordinal not exported by the reference DLL (version mismatch?)",
                ImportOrigin::UnresolvedOrdinal {
                    reference_dll_available: false,
                    ..
                } => "ordinal import with no reference DLL available to resolve it",
                _ => area,
            };
            let area: String = if implementation == Implementation::Unknown {
                match &origin {
                    // The name is known; only its classification is missing,
                    // so say which API is missing rather than just "unknown".
                    ImportOrigin::RecoveredFromReferenceDll { name, .. } => {
                        format!("unclassified API: {name}")
                    }
                    _ => area.to_string(),
                }
            } else {
                area.to_string()
            };
            // The matrix stores a `&'static str` area; intern it once per
            // distinct string to avoid leaking on every import.
            let area_static: &'static str = intern_area(&area);
            matrix.entries.insert((
                imp.library.clone(),
                imp.symbol(),
                implementation,
                area_static,
            ));
            imports.push(ResolvedImport {
                library: imp.library.clone(),
                symbol: imp.symbol(),
                origin,
                implementation,
                area,
            });
        }
        Analysis {
            image,
            matrix,
            imports,
        }
    }

    /// Provenance notes for the matrix: how ordinals were resolved, and why
    /// unsupported APIs are unsupported. Keyed by `(library, symbol)`.
    pub fn matrix_notes(&self) -> std::collections::BTreeMap<(String, String), String> {
        let mut notes = std::collections::BTreeMap::new();
        for imp in &self.imports {
            if let ImportOrigin::RecoveredFromReferenceDll { ordinal, name } = &imp.origin {
                // The matrix is keyed by `Imp!symbol`; match that exactly.
                let key = (imp.library.clone(), imp.symbol.clone());
                let note = format!("ordinal {ordinal} = `{name}` (resolved via reference DLL)");
                if matches!(
                    imp.implementation,
                    Implementation::Unsupported | Implementation::Unknown
                ) {
                    notes.insert(
                        key,
                        format!("{note}; cannot run until `{name}` is implemented"),
                    );
                } else {
                    notes.insert(key, note);
                }
            }
        }
        for (lib, sym, state, area) in self.matrix.blockers() {
            let reason = match state {
                Implementation::Unsupported => format!("deliberate gap: {area}"),
                Implementation::Unknown => format!("unclassified: {area}"),
                _ => continue,
            };
            // Ordinal provenance is more informative than a generic reason,
            // and the ordinal entry already names the API.
            notes.entry((lib.clone(), sym.clone())).or_insert(reason);
        }
        notes
    }

    /// Ordinal imports that could not be resolved.
    pub fn unresolved_ordinals(&self) -> impl Iterator<Item = &ResolvedImport> {
        self.imports
            .iter()
            .filter(|i| i.origin.provenance() == Provenance::UnresolvedOrdinal)
    }

    /// Libraries with their symbol counts, in import-table order.
    pub fn libraries(&self) -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> = Vec::new();
        for imp in &self.image.imports {
            match out
                .iter_mut()
                .find(|(l, _)| l.eq_ignore_ascii_case(&imp.library))
            {
                Some((_, n)) => *n += 1,
                None => out.push((imp.library.clone(), 1)),
            }
        }
        out
    }

    /// A compact human report for `wlam analyze`.
    pub fn report(&self) -> String {
        let pe = &self.image;
        let mut s = String::new();
        s.push_str(&format!(
            "kind:            {}\n",
            match pe.kind {
                wlam_pe::PeKind::Pe32 => "PE32 (32-bit)",
                wlam_pe::PeKind::Pe32Plus => "PE32+ (64-bit)",
            }
        ));
        s.push_str(&format!("machine:         {:?}\n", pe.machine));
        s.push_str(&format!("image base:      {:#x}\n", pe.image_base));
        s.push_str(&format!("entry point:     {:#x}\n", pe.entry_point_rva));
        s.push_str(&format!(
            "subsystem:       {}\n",
            subsystem_name(pe.subsystem)
        ));
        s.push_str(&format!("sections:        {}\n", pe.sections.len()));
        for sec in &pe.sections {
            let flags = format!(
                "{}{}{}",
                if sec.is_readable() { "r" } else { "-" },
                if sec.is_writable() { "w" } else { "-" },
                if sec.is_executable() { "x" } else { "-" },
            );
            s.push_str(&format!(
                "  {:<8} {flags} rva={:#010x} vsize={:#08x} raw={:#08x}\n",
                sec.name, sec.virtual_address, sec.virtual_size, sec.file_size
            ));
        }
        s.push_str(&format!(
            "imports:         {} symbols from {} libraries\n",
            pe.imports.len(),
            self.libraries().len()
        ));
        for (lib, n) in self.libraries() {
            s.push_str(&format!("  {lib:<20} {n}\n"));
        }
        s.push_str(&format!("exports:         {}\n", pe.exports.len()));
        s.push_str(&format!("relocations:     {}\n", pe.relocations.len()));
        s.push_str(&format!(
            "resources:       {}\n",
            pe.resources.entries.len()
        ));
        let recovered = self
            .imports
            .iter()
            .filter(|i| i.origin.provenance() == Provenance::RecoveredFromReferenceDll)
            .count();
        let unresolved = self.unresolved_ordinals().count();
        if recovered > 0 || unresolved > 0 {
            s.push_str(&format!(
                "ordinals:        {recovered} resolved via reference DLLs, {unresolved} unresolved\n"
            ));
            for imp in self.unresolved_ordinals() {
                s.push_str(&format!(
                    "  UNRESOLVED   {}  ({})\n",
                    imp.symbol, imp.library
                ));
            }
        }
        if pe.unwind_info_rva != 0 {
            s.push_str(&format!(
                "unwind info:     rva {:#x}, {} bytes\n",
                pe.unwind_info_rva, pe.unwind_info_size
            ));
        }
        s.push_str("\ncompatibility:\n");
        for (state, n) in self.matrix.summary() {
            s.push_str(&format!("  {:<12} {}\n", state.label(), n));
        }
        let blockers: Vec<_> = self.matrix.blockers().collect();
        if !blockers.is_empty() {
            s.push_str(&format!("\n{} blocking import(s):\n", blockers.len()));
            for (lib, sym, state, area) in blockers.iter().take(40) {
                s.push_str(&format!(
                    "  {:<10} {:<40} {lib}  ({area})\n",
                    state.label(),
                    sym
                ));
            }
            if blockers.len() > 40 {
                s.push_str(&format!("  ... and {} more\n", blockers.len() - 40));
            }
        }
        s
    }
}

fn subsystem_name(s: u16) -> &'static str {
    match s {
        1 => "NATIVE",
        2 => "GUI (user32)",
        3 => "CONSOLE",
        5 => "OS/2",
        7 => "POSIX",
        9 => "WINDOWS CE",
        10 => "EFI APPLICATION",
        14 => "XBOX",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsystem_names_cover_common_cases() {
        assert_eq!(subsystem_name(2), "GUI (user32)");
        assert_eq!(subsystem_name(3), "CONSOLE");
        assert_eq!(subsystem_name(999), "unknown");
    }
    #[test]
    fn origin_reports_name_and_provenance() {
        let named = ImportOrigin::Named("Sleep".into());
        assert_eq!(named.name(), Some("Sleep"));
        assert_eq!(named.provenance(), Provenance::Named);

        let rec = ImportOrigin::RecoveredFromReferenceDll {
            ordinal: 17,
            name: "Foo".into(),
        };
        assert_eq!(rec.name(), Some("Foo"));
        assert_eq!(rec.provenance(), Provenance::RecoveredFromReferenceDll);

        let un = ImportOrigin::UnresolvedOrdinal {
            ordinal: 3,
            reference_dll_available: true,
        };
        assert_eq!(un.name(), None);
        assert_eq!(un.provenance(), Provenance::UnresolvedOrdinal);
    }

    #[test]
    fn intern_area_is_stable_for_equal_strings() {
        let a = intern_area("same reason");
        let b = intern_area("same reason");
        assert!(std::ptr::eq(a, b));
        assert_ne!(
            intern_area("reason one").as_ptr(),
            intern_area("reason two").as_ptr()
        );
    }
}
