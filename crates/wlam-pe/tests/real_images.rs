//! Parse real Windows images if a Proton/Wine tree is present.
//!
//! This is the integration test that matters: the unit tests use hand-made
//! buffers, but the parser must survive 800+ real `i386-windows` DLLs without
//! panicking and must recover plausible imports/exports from them.
//!
//! Set `WLAM_TEST_WIN_DIR` to a directory of Windows DLLs, or rely on the
//! default Steam/Proton location. The test is skipped when neither exists so
//! it works on machines without Proton.

use std::path::{Path, PathBuf};
use wlam_pe::Pe;

fn corpus_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(d) = std::env::var("WLAM_TEST_WIN_DIR") {
        dirs.push(PathBuf::from(d));
    }
    let home = std::env::var("HOME").unwrap_or_default();
    for base in [
        format!("{home}/.local/share/Steam/steamapps/common/Proton - Experimental/files/lib/wine"),
        format!("{home}/.local/share/Steam/steamapps/common/Proton - Experimental/files/share"),
    ] {
        for sub in ["i386-windows", "x86_64-windows"] {
            let p = Path::new(&base).join(sub);
            if p.is_dir() {
                dirs.push(p);
            }
        }
    }
    dirs
}

#[test]
fn parses_real_windows_images_without_panicking() {
    let dirs = corpus_dirs();
    if dirs.is_empty() {
        eprintln!("skipping: no Windows DLL corpus found (set WLAM_TEST_WIN_DIR)");
        return;
    }
    let mut total = 0usize;
    let mut parsed = 0usize;
    let mut failures = Vec::new();
    let mut total_imports = 0usize;
    let mut total_exports = 0usize;
    for dir in &dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_dll = path
                .extension()
                .map(|e| e.eq_ignore_ascii_case("dll") || e.eq_ignore_ascii_case("exe"))
                .unwrap_or(false);
            if !is_dll {
                continue;
            }
            total += 1;
            let bytes = std::fs::read(&path).expect("readable");
            match Pe::parse(&bytes) {
                Ok(pe) => {
                    parsed += 1;
                    total_imports += pe.imports.len();
                    total_exports += pe.exports.len();
                    // Every import must be attributable to a library and have
                    // an IAT slot inside a mapped section.
                    for imp in &pe.imports {
                        assert!(!imp.library.is_empty(), "{path:?}: empty library name");
                        assert!(
                            pe.section_of(imp.iat_rva).is_some(),
                            "{path:?}: IAT rva {:#x} not in any section",
                            imp.iat_rva
                        );
                    }
                    // Exports must point into mapped memory.
                    for e in &pe.exports {
                        assert!(
                            pe.section_of(e.rva).is_some(),
                            "{path:?}: export rva {:#x} not mapped",
                            e.rva
                        );
                    }
                }
                Err(err) => failures.push((path.display().to_string(), err.to_string())),
            }
        }
    }
    println!(
        "parsed {parsed}/{total} images, {total_imports} imports, {total_exports} exports; \
         {} failures",
        failures.len()
    );
    // Real-world DLLs are all well-formed PE images; any failure is a bug.
    assert!(failures.is_empty(), "parse failures: {failures:#?}");
    assert!(parsed > 0);
    assert!(total_imports > 0);
}

#[test]
fn kernel32_imports_resolve_to_known_symbols() {
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        format!(
            "{home}/.local/share/Steam/steamapps/common/Proton - Experimental/files/lib/wine/i386-windows/kernel32.dll"
        ),
        format!(
            "{home}/.local/share/Steam/steamapps/common/Proton - Experimental/files/share/default_pfx/drive_c/windows/system32/kernel32.dll"
        ),
    ];
    let Some(path) = candidates.iter().map(PathBuf::from).find(|p| p.is_file()) else {
        eprintln!("skipping: kernel32.dll not found");
        return;
    };
    let pe = Pe::parse(&std::fs::read(&path).unwrap()).expect("kernel32 parses");
    let symbols: Vec<String> = pe.exports.iter().filter_map(|e| e.name.clone()).collect();
    for expected in ["CreateFileW", "VirtualAlloc", "GetTickCount", "ExitProcess"] {
        assert!(
            symbols.iter().any(|s| s == expected),
            "kernel32 should export {expected}"
        );
    }
    assert!(pe.imports.iter().any(|i| i.library.eq_ignore_ascii_case("ntdll.dll")));
}
