//! Verify the resource tree on real DLLs: every Proton `i386-windows` DLL
//! that ships resources must yield at least one leaf entry whose data lies in
//! mapped memory. This is the regression guard for the root-relative offset
//! bug that previously made 165 real images fail to parse.
use std::path::PathBuf;
use wlam_pe::Pe;

#[test]
fn real_dll_resources_resolve() {
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = PathBuf::from(format!(
        "{home}/.local/share/Steam/steamapps/common/Proton - Experimental/files/lib/wine/i386-windows"
    ));
    if !dir.is_dir() {
        eprintln!("skipping: no Proton i386-windows DLLs");
        return;
    }
    let mut with_resources = 0usize;
    let mut entries = 0usize;
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let p = e.path();
        if !p
            .extension()
            .map(|x| x.to_string_lossy().to_lowercase() == "dll")
            .unwrap_or(false)
        {
            continue;
        }
        let pe =
            Pe::parse(&std::fs::read(&p).unwrap()).unwrap_or_else(|err| panic!("{p:?}: {err}"));
        if pe.resources.entries.is_empty() {
            continue;
        }
        with_resources += 1;
        entries += pe.resources.entries.len();
        for r in &pe.resources.entries {
            assert!(
                pe.section_of(r.data_rva).is_some(),
                "{p:?}: resource data rva {:#x} unmapped",
                r.data_rva
            );
            assert!(
                r.size < 0x1000_0000,
                "{p:?}: implausible resource size {}",
                r.size
            );
        }
        // Icon-bearing DLLs should expose RT_ICON (3) leaves.
        if p.file_name().unwrap().to_string_lossy() == "user32.dll" {
            assert!(
                pe.resources.by_type(3).count() > 0 || pe.resources.by_type(14).count() > 0,
                "user32 should have icon or group-icon resources"
            );
        }
    }
    println!("{with_resources} DLLs with resources, {entries} leaf entries");
    assert!(
        with_resources > 200,
        "expected resources in many DLLs, got {with_resources}"
    );
}
