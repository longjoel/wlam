//! Print the raw Sleigh P-code for one instruction, before lowering.
//! Usage: `cargo run -q --example dump_raw -- 0x40ccc8`
use wlam_sleigh::{with_big_stack, Architecture, Decoder};

fn main() {
    let addr: u64 = u64::from_str_radix(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "0x40ccc8".into())
            .trim_start_matches("0x"),
        16,
    )
    .unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let bytes = std::fs::read(root.join("north-star/bin/Uxn32.exe")).unwrap();
    let base = 0x400000u64;
    let off = (addr - base) as usize;
    let slice = bytes[off..].to_vec();
    with_big_stack(move || {
        let mut d = Decoder::new(Architecture::X86_32);
        let _ = &mut d;
        let mut ip = addr;
        let mut index = 0usize;
        let mut insns = Vec::new();
        while let Ok(i) = d.decode(&slice[index..], ip) {
            if i.len == 0 {
                break;
            }
            ip += i.len;
            index += i.len as usize;
            insns.push(i);
        }
        for i in &insns {
            println!("{} ({} bytes)", i.disassembly, i.len);
            for op in &i.ops {
                println!("    {op:?}");
            }
        }
    });
}
