//! Print one block's lifted IR and its emitted straight-line ops.
//!
//! Usage: `cargo run -q --example dump_block -- 0x40ccb2 [image]`
use wlam_emit::translate_ops;
use wlam_emit::LocalMap;
use wlam_pe::Pe;
use wlam_sleigh::with_big_stack;

use wlam_sleigh::emit as shared;

fn main() {
    let addr: u64 = u64::from_str_radix(
        std::env::args().nth(1).unwrap_or_else(|| "0x40ccb2".into()).trim_start_matches("0x"),
        16,
    )
    .unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().to_path_buf();
    let path = std::env::args().nth(2).unwrap_or_else(|| root.join("north-star/bin/Uxn32.exe").display().to_string());
    let bytes = std::fs::read(&path).unwrap();
    let pe = Pe::parse(&bytes).unwrap();
    let base = pe.image_base as u64;
    let rva = (addr - base) as u32;
    let idx = pe.section_of(rva).unwrap();
    let sec = &pe.sections[idx];
    let off = (rva - sec.virtual_address) as usize;
    let slice = sec.data[off..].to_vec();
    let start = addr;
    let lifted = with_big_stack(move || wlam_sleigh::lift_block(&slice, start));
    println!("block {:#x}..{:#x} term={:?}", lifted.start, lifted.end, lifted.terminator);
    for insn in &lifted.instructions {
        if insn.unlowered > 0 {
            println!("  ({} unlowered op(s) at)", insn.unlowered);
        }
        println!("  @{:#x} {}", insn.address, insn.disassembly);
        for op in &insn.ops {
            println!("      {op:?}");
        }
    }
    let block = shared::convert(&lifted);
    let ops = shared::strip_control_flow(block.ops.clone());
    let regs = wlam_emit::seeded_registers();
    let locals = LocalMap::new(regs.iter().map(|r| (r.clone(), 4)));
    let t = translate_ops(&ops, locals).unwrap();
    println!("\nwasm body ({} bytes):", t.body.len());
    println!("{:02x?}", t.body);
}
