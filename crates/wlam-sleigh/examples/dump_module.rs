//! Debug helper: write the emitted module for an image to /tmp so the wabt
//! tools can inspect it. Usage: `--example dump_module [path.exe] [out.wasm]`

#[path = "../tests/shared/mod.rs"]
mod shared;

use std::collections::BTreeMap;
use wlam_emit::chain::{build_module_with_memory, partition, HostImport};
use wlam_pe::Pe;
use wlam_sleigh::discover::discover;
use wlam_sleigh::{lift_block, with_big_stack};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let path = args
        .get(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join("north-star/bin/Uxn32.exe"));
    let out = args.get(2).cloned().unwrap_or_else(|| "/tmp/guest.wasm".into());
    let bytes = std::fs::read(&path).expect("read image");
    let prog = {
        let b2 = bytes.clone();
        with_big_stack(move || discover(&Pe::parse(&b2).expect("parse"), 64))
    };
    let pe = Pe::parse(&bytes).expect("parse");
    let base = pe.image_base as u64;
    let by_slot: BTreeMap<u64, HostImport> = pe
        .imports
        .iter()
        .filter(|i| i.iat_slot_verified)
        .map(|i| {
            (
                base + i.iat_rva as u64,
                HostImport {
                    library: i.library.clone(),
                    name: i.name.clone().unwrap_or_else(|| format!("#{}", i.ordinal.unwrap_or(0))),
                },
            )
        })
        .collect();
    let mut blocks = BTreeMap::new();
    for b in prog.blocks() {
        let rva = (b.start - base) as u32;
        let Some(idx) = pe.section_of(rva) else { continue };
        let sec = &pe.sections[idx];
        let off = (rva - sec.virtual_address) as usize;
        if off >= sec.data.len() {
            continue;
        }
        let slice = sec.data[off..].to_vec();
        let start = b.start;
        let lifted = with_big_stack(move || lift_block(&slice, start));
        if b.start == 0x40ccb2 {
            println!("DISCOVERED BLOCK AT {:#x}: start={:#x} end={:#x} terminator={:?}", b.start, lifted.start, lifted.end, lifted.terminator);
        }
        blocks.insert(b.start, shared::convert(&lifted));
        if b.start == 0x40ccb2 {
            println!("  -> inserted with key {:#x}", b.start);
        }
    }
    let mut regs: Vec<String> = wlam_emit::seeded_registers();
    let mut used = std::collections::BTreeSet::new();
    for b in blocks.values() {
        for op in &b.ops {
            shared::collect_regs(op, &mut used);
        }
    }
    for r in used {
        if !regs.contains(&r) {
            regs.push(r);
        }
    }
    let pages = wlam_emit::chain::pages_for_image(base + pe.size_of_image as u64);
    let mut p = partition(&[base + pe.entry_point_rva as u64], &blocks, &regs);
    p.register_host_imports(&blocks, |slot| by_slot.get(&slot).cloned());
    let m = build_module_with_memory(&p, &blocks, pages).expect("module");
    std::fs::write(&out, &m).expect("write");
    // Report which function holds the interesting block, so the dump can be
    // narrowed without guessing.
    for probe in [0x40ccd8u64, 0x40ccde, 0x40cce2] {
        let mut found = false;
        for (i, f) in p.functions.iter().enumerate() {
            if let Some(id) = f.ids.get(&probe) {
                println!("{probe:#x}: function {i} entry={:#x} id {id}", f.entry);
                found = true;
            }
        }
        if !found {
            println!("{probe:#x}: NOT IN ANY FUNCTION (block exists: {})", blocks.contains_key(&probe));
        }
    }
    println!("blocks map contains 0x40ccb2? {}", blocks.contains_key(&0x40ccb2));
    println!("discovery: block_starts contains 0x40ccd8? {}", prog.block_starts().contains(&0x40ccd8));
    let near: Vec<u64> = prog.block_starts().iter().copied().filter(|a| (0x40ccb0..0x40cd20).contains(a)).collect();
    let near: Vec<String> = near.iter().map(|a| format!("{a:#x}")).collect();
    println!("blocks near: {}", near.join(" "));
    println!("total functions={} total blocks in functions={}", p.functions.len(), p.functions.iter().map(|f| f.blocks.len()).sum::<usize>());
    println!("wrote {} bytes to {out}", m.len());
}
