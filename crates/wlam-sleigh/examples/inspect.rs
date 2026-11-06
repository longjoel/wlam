//! Print the decoded form of specific addresses, to explain discovery gaps.
use wlam_pe::Pe;
use wlam_sleigh::discover::discover;
use wlam_sleigh::{lift_block, with_big_stack, IR};

fn disasm(pe: &Pe, addr: u64, n: usize) -> String {
    let base = pe.image_base as u64;
    let rva = (addr - base) as u32;
    let Some(idx) = pe.section_of(rva) else {
        return "<unmapped>\n".into();
    };
    let sec = &pe.sections[idx];
    let off = (rva - sec.virtual_address) as usize;
    let bytes = sec.data[off..].to_vec();
    let block = with_big_stack(move || lift_block(&bytes, addr));
    let mut s = String::new();
    for (i, insn) in block.instructions.iter().take(n).enumerate() {
        if i == n.saturating_sub(1) {
            for op in &insn.ops {
                if matches!(
                    op,
                    IR::Call { .. }
                        | IR::CallInd { .. }
                        | IR::Branch { .. }
                        | IR::IndirectJump { .. }
                ) {
                    s.push_str(&format!("    [{}] {:?}\n", insn.disassembly, op));
                }
            }
        }
        s.push_str(&format!("  {:#x}: {}\n", insn.address, insn.disassembly));
    }
    s.push_str(&format!("  -- end: {:?}\n", block.terminator));
    s
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bytes = std::fs::read("../../north-star/bin/Uxn32.exe").unwrap();
    let pe = Pe::parse(&bytes).unwrap();
    let prog = with_big_stack({
        let pe_bytes = bytes.clone();
        move || {
            let pe2 = Pe::parse(&pe_bytes).unwrap();
            discover(&pe2, 8)
        }
    });
    let targets: Vec<u64> = if args.is_empty() {
        prog.unresolved_indirect().into_iter().take(3).collect()
    } else {
        args.iter()
            .map(|a| u64::from_str_radix(a.trim_start_matches("0x"), 16).unwrap())
            .collect()
    };
    for t in targets {
        println!("== block at {t:#x}");
        print!("{}", disasm(&pe, t, 4));
    }
    // Where do the back-to-back prologue sequences start?
    println!("\n== prologue cluster start candidates");
    for a in [0x40d884u64, 0x40d97c, 0x40ea32, 0x40ed5b] {
        print!("{}", disasm(&pe, a - 8, 4));
    }
}
