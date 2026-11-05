//! Print the lifted P-code and emitted straight-line ops of a guest block, for
//! any PE image and address.
//!
//! cargo run -p wlam-sleigh --example lift_image_block -- image.exe ADDRESS
//!
//! `dump_block` in `wlam-sleigh` does the same for the North Star image; this
//! one takes the image as an argument so the hover target can be inspected.
use wlam_emit::{translate_ops, LocalMap};
use wlam_sleigh::emit as shared;
use wlam_sleigh::with_big_stack;

fn main() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("usage: lift_block image.exe ADDRESS".into());
    }
    let image = std::fs::read(&args[0]).map_err(|e| e.to_string())?;
    let pe = wlam_pe::Pe::parse(&image).map_err(|e| e.to_string())?;
    let base = pe.image_base as u64;
    let address =
        u64::from_str_radix(args[1].trim_start_matches("0x"), 16).map_err(|e| e.to_string())?;
    let rva = address
        .checked_sub(base)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or("address is below the image base")?;
    let section = pe.section_of(rva).map(|i| &pe.sections[i]).ok_or("address is not in a section")?;
    let slice = section.data[(rva - section.virtual_address) as usize..].to_vec();
    let lifted = with_big_stack(move || wlam_sleigh::lift_block(&slice, address));
    println!("block {:#x}..{:#x} term={:?}", lifted.start, lifted.end, lifted.terminator);
    for insn in &lifted.instructions {
        if insn.unlowered > 0 {
            println!("      ({} unlowered op(s))", insn.unlowered);
        }
        println!("  {:#010x} {}", insn.address, insn.disassembly);
        for op in &insn.ops {
            println!("      {op:?}");
        }
    }
    let block = shared::convert(&lifted);
    let ops = shared::strip_control_flow(block.ops.clone());
    println!("emitted IR:");
    for op in &ops {
        println!("      {op:?}");
    }
    let regs = wlam_emit::seeded_registers();
    let locals = LocalMap::new(regs.iter().map(|r| (r.clone(), 4)));
    match translate_ops(&ops, locals) {
        Ok(_) => println!("translate_ops: ok"),
        Err(e) => println!("translate_ops: {e:?}"),
    }
    Ok(())
}
