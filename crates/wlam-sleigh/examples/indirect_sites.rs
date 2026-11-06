//! Diagnostic: list every indirect branch site discovery found, whether its
//! targets were recovered from a table, and optionally print each block's
//! lifted P-code.
//!
//! cargo run -p wlam-sleigh --example indirect_sites -- image.exe [--dump ADDR ...]
use wlam_sleigh::discover::{discover, recover_tables_at, Edge};

fn parse_address(text: &str) -> Option<u64> {
    u64::from_str_radix(text.trim_start_matches("0x"), 16).ok()
}

fn main() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let Some(image_path) = args.first() else {
        return Err("usage: indirect_sites image.exe [--dump ADDR ...]".into());
    };
    let image = std::fs::read(image_path).map_err(|e| e.to_string())?;
    // `discover` needs ownership of the image; the outer binary runs with a
    // normal stack, and the lifting thread started per block below covers the
    // recursion depth Sleigh needs.
    let program = std::thread::Builder::new()
        .stack_size(wlam_sleigh::LIFT_STACK_SIZE)
        .spawn(move || {
            let pe = wlam_pe::Pe::parse(&image).ok()?;
            Some(discover(&pe, 64))
        })
        .map_err(|e| e.to_string())?
        .join()
        .map_err(|_| "discovery thread panicked".to_string())?
        .ok_or("image did not parse as PE")?;
    let image = std::fs::read(image_path).map_err(|e| e.to_string())?;
    let pe = wlam_pe::Pe::parse(&image).map_err(|e| e.to_string())?;
    let base = pe.image_base as u64;
    println!(
        "blocks={} functions={} unresolved={} jump_table_targets={} passes={} incomplete={}",
        program.block_count(),
        program.functions().len(),
        program.unresolved_indirect().len(),
        program.jump_table_targets().len(),
        program.passes(),
        program.incomplete()
    );
    for block in program.blocks() {
        let (kind, site) = match block.edge {
            Edge::IndirectJump { site } => ("jump-ind", site),
            Edge::CallInd { resume } => ("call-ind", resume),
            _ => continue,
        };
        let resolved = !program.unresolved_indirect().contains(&block.start);
        println!(
            "{kind} {site:#010x}: {} (reason {})",
            if resolved { "resolved" } else { "UNRESOLVED" },
            block.reason
        );
    }
    let dump = args.iter().skip(1).skip_while(|a| *a != "--dump").skip(1);
    for text in dump {
        let Some(address) = parse_address(text) else {
            eprintln!("skipping unparsable address {text}");
            continue;
        };
        let Some(rva) = address
            .checked_sub(base)
            .and_then(|v| u32::try_from(v).ok())
        else {
            eprintln!("{address:#010x}: below the image base");
            continue;
        };
        let Some(section) = pe.section_of(rva).map(|i| &pe.sections[i]) else {
            eprintln!("{address:#010x}: not in a section");
            continue;
        };
        let bytes = section.data[(rva - section.virtual_address) as usize..].to_vec();
        let lifted = wlam_sleigh::with_big_stack(move || wlam_sleigh::lift_block(&bytes, address));
        println!(
            "--- recovered table: {:?}",
            recover_tables_at(&pe, &[address])
        );
        println!(
            "--- {address:#010x}..{:#010x} term={:?}",
            lifted.end, lifted.terminator
        );
        for insn in &lifted.instructions {
            println!("  {:#010x} {}", insn.address, insn.disassembly);
            for op in &insn.ops {
                println!("      {op:?}");
            }
        }
    }
    Ok(())
}
