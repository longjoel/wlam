//! Print every parsed import and whether its IAT slot was verified.
//!
//! A recompiled `jmp [slot]` can only become a host call when the slot is in
//! the verified map. When a guest transfer lands on a poisoned IAT entry, this
//! shows whether the import was missed or merely mis-addressed.
//!
//! Usage: `cargo run --example dump_imports -- path/to/image.exe [name-filter]`
use wlam_pe::Pe;

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("usage: dump_imports <exe> [filter]")?;
    let filter = args.next();
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    let pe = Pe::parse(&bytes).map_err(|e| e.to_string())?;
    let base = pe.image_base as u32;
    let verified = pe.imports.iter().filter(|import|import.iat_slot_verified).count();
    for import in &pe.imports {
        let name = import
            .name
            .clone()
            .unwrap_or_else(|| format!("#{}", import.ordinal.unwrap_or(0)));
        if let Some(filter) = &filter {
            if !name.contains(filter.as_str()) {
                continue;
            }
        }
        println!(
            "{:#010x} {:<16} {:<40} verified={}",
            base.wrapping_add(import.iat_rva),
            import.library,
            name,
            import.iat_slot_verified
        );
    }
    println!(
        "{} imports, {verified} verified, {} unverified",
        pe.imports.len(),
        pe.imports.len() - verified
    );
    Ok(())
}
