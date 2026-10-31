//! Print the lifted ops at an address, formatted for reading.
//! Usage: `cargo run -q --example dump_ops -- 0x401058`
use wlam_pe::Pe;
use wlam_sleigh::with_big_stack;

fn main() {
    let addr: u64 = u64::from_str_radix(
        std::env::args().nth(1).unwrap_or_else(|| "0x401058".into()).trim_start_matches("0x"),
        16,
    )
    .unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().to_path_buf();
    let bytes = std::fs::read(root.join("north-star/bin/Uxn32.exe")).unwrap();
    let pe = Pe::parse(&bytes).unwrap();
    let base = pe.image_base as u64;
    let rva = (addr - base) as u32;
    let idx = pe.section_of(rva).unwrap();
    let off = (rva - pe.sections[idx].virtual_address) as usize;
    let slice = pe.sections[idx].data[off..].to_vec();
    let start = addr;
    let blk = with_big_stack(move || wlam_sleigh::lift_block(&slice, start));
    for i in &blk.instructions {
        println!("{:#x} {}{}", i.address, i.disassembly, if i.unlowered > 0 { "   [UNLOWERED]" } else { "" });
        for op in &i.ops {
            println!("    {}", op.text());
        }
    }
}
