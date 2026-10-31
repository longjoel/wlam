//! Which indirect transfers does the entry path hit, and through what?
//!
//! `probe_loop` strips every indirect jump/call before running a block, because
//! a stripped `call [slot]` leaves no way to know where it went. This prints
//! those sites with their operands so the question "is this the host API gap or
//! a guest function pointer" can be answered from the lifted IR.
use std::collections::BTreeMap;

use wlam_pe::Pe;
use wlam_sleigh::ir as sir;
use wlam_sleigh::{with_big_stack, Terminator};
use wlam_sleigh::discover::discover;

fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().to_path_buf();
    let path = std::env::args().nth(1).unwrap_or_else(|| root.join("north-star/bin/Uxn32.exe").display().to_string());
    let bytes = std::fs::read(&path).unwrap();
    let pe = Pe::parse(&bytes).unwrap();
    let base = pe.image_base as u64;
    let prog = { let b = bytes.clone(); with_big_stack(move || discover(&Pe::parse(&b).unwrap(), 64)) };
    let iat: BTreeMap<u64, String> = pe.imports.iter().filter(|i| i.iat_slot_verified)
        .map(|i| (base + i.iat_rva as u64, format!("{}!{}", i.library, i.name.clone().unwrap_or_default()))).collect();
    let mut n_ind = 0;
    for b in prog.blocks() {
        let rva = (b.start - base) as u32;
        let Some(idx) = pe.section_of(rva) else { continue };
        let sec = &pe.sections[idx];
        let off = (rva - sec.virtual_address) as usize;
        if off >= sec.data.len() { continue; }
        let slice = sec.data[off..].to_vec();
        let start = b.start;
        let lifted = with_big_stack(move || wlam_sleigh::lift_block(&slice, start));
        let mut ind: Vec<String> = Vec::new();
        for ins in &lifted.instructions {
            for op in &ins.ops {
                match op {
                    sir::IR::CallInd { target } => {
                        let mut s = format!("call_ind {target:?}");
                        if let sir::Operand::Ram { offset, .. } = target {
                            if let Some(n) = iat.get(offset) { s.push_str(&format!(" = IAT {n}")); }
                        }
                        ind.push(s);
                    }
                    sir::IR::IndirectJump { target } => ind.push(format!("jmp_ind {target:?}")),
                    _ => {}
                }
            }
        }
        if ind.is_empty() { continue; }
        n_ind += 1;
        println!("{:#x} term={:?}", lifted.start, lifted.terminator);
        for s in ind { println!("    {s}"); }
        let _ = &Terminator::Return;
    }
    println!("{n_ind} blocks with indirect transfers");
}
