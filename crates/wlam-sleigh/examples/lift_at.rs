//! Disassemble + lift one guest address (or a short run of instructions).
//!
//! Usage: `cargo run --example lift_at -- 40e070 [path.exe]`

use wlam_pe::Pe;
use wlam_sleigh::{lift_block, with_big_stack};

fn main() {
    let a = std::env::args().nth(1).expect("hex address");
    let start = u64::from_str_radix(a.trim_start_matches("0x"), 16).expect("hex address");
    let path = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "../../north-star/bin/Uxn32.exe".into());
    let bytes = std::fs::read(&path).expect("image");
    let pe = Pe::parse(&bytes).expect("parse");
    let base = pe.image_base as u64;
    let rva = (start - base) as u32;
    let idx = pe.section_of(rva).expect("address not in a section");
    let sec = &pe.sections[idx];
    let off = (rva - sec.virtual_address) as usize;
    let slice = sec.data[off..].to_vec();
    let lb = with_big_stack(move || lift_block(&slice, start));
    println!(
        "{start:#x}: end={:#x} terminator={:?}",
        lb.end, lb.terminator
    );
    for ins in &lb.instructions {
        println!(
            "  {:#x}  {:<38} unlowered={}",
            ins.address, ins.disassembly, ins.unlowered
        );
        if std::env::var("OPS").is_ok() {
            for op in &ins.ops {
                println!("        {op:?}");
            }
        }
    }
}
