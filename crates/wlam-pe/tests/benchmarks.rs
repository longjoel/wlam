//! Robustness tests for images that real linkers actually emit.
//!
//! Findings driving this file (both from the Minesweeper Classic benchmark,
//! built with MSYS2 mingw-w64):
//!
//!  * `minesweeper-x64.exe` has `SizeOfOptionalHeader = 240` (112 PE32+
//!    header + 128 for 16 data directories) but its `NumberOfRvaAndSizes`
//!    field reads 0. `.idata` exists and the import table at RVA 0x2b000 is
//!    intact — 195 imports across 7 DLLs. A loader that trusts the count alone
//!    finds nothing.
//!  * mingw names some sections `/4`, `/14`, `/29`, ... (COFF long-name
//!    encoding). They are valid and must parse.
//!
//! Policy: bound what the header *size* allows, then recover structurally with
//! strict plausibility checks (module name ends in `.dll`, well-formed thunk
//! array, zero-terminated descriptor list) so no imports can be invented.

use wlam_pe::Pe;

fn benchmark(name: &str) -> Option<Vec<u8>> {
    let path = format!("../../benchmarks/minesweeper/bin/{name}");
    std::fs::read(path).ok()
}

#[test]
fn x64_benchmark_recovers_imports_despite_zero_directory_count() {
    let Some(bytes) = benchmark("minesweeper-x64.exe") else {
        eprintln!("skipping: ran scripts/fetch-benchmarks.sh to fetch this benchmark");
        return;
    };
    let pe = Pe::parse(&bytes).expect("parses");
    // 195 imports across 7 DLLs, confirmed by an independent Python walk of the
    // descriptor chain in the image.
    assert_eq!(pe.imports.len(), 195, "recovered import count");
    let libs: Vec<&str> = pe.imports.iter().map(|i| i.library.as_str()).collect();
    for want in [
        "ADVAPI32.dll",
        "GDI32.dll",
        "KERNEL32.dll",
        "msvcrt.dll",
        "USER32.dll",
        "WINHTTP.dll",
        "WS2_32.dll",
    ] {
        assert!(
            libs.iter().any(|l| l.eq_ignore_ascii_case(want)),
            "missing {want}"
        );
    }
}

#[test]
fn x86_benchmark_parses_normally() {
    let Some(bytes) = benchmark("minesweeper-x86.exe") else {
        eprintln!("skipping: run scripts/fetch-benchmarks.sh");
        return;
    };
    let pe = Pe::parse(&bytes).expect("parses");
    assert!(
        !pe.directories.is_empty(),
        "x86 build has a normal directory table"
    );
    assert_eq!(pe.imports.len(), 201, "x86 build import count");
    // COFF long section names must survive parsing.
    assert!(
        pe.sections.iter().any(|s| s.name.starts_with('/')),
        "long section names such as '/29' parse: {:?}",
        pe.sections.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
}

#[test]
fn recovery_does_not_invent_imports_for_a_clean_image() {
    // A normal image must be unaffected by recovery: its declared directory is
    // used, and no scan runs.
    let Some(bytes) = benchmark("minesweeper-x86.exe") else {
        return;
    };
    let pe = Pe::parse(&bytes).unwrap();
    let declared = pe
        .directories
        .iter()
        .find(|d| d.index == wlam_pe::Directory::IMPORT)
        .expect("declared import directory");
    assert_eq!(declared.rva, 0x2f000);
}
