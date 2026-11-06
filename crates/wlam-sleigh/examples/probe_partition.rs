//! Diagnostic: report how `partition` assigns blocks to functions for an image.
//!
//! Written after a hand-rolled probe claimed 161 blocks belong to more than one
//! function. This uses the project's own `partition` so the answer is the one
//! the emitter actually acts on.

use std::collections::{BTreeMap, BTreeSet};
use wlam_emit::chain::partition;
use wlam_pe::Pe;
use wlam_sleigh::emit as shared;
use wlam_sleigh::{discover::discover, with_big_stack};

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "../../benchmarks/minesweeper/bin/minesweeper-x86.exe".into());
    let bytes = std::fs::read(&path).unwrap();
    let prog = {
        let b = bytes.clone();
        with_big_stack(move || discover(&Pe::parse(&b).unwrap(), 64))
    };
    let pe = Pe::parse(&bytes).unwrap();
    let base = pe.image_base as u64;

    let mut blocks: BTreeMap<u64, wlam_emit::ir::Block> = BTreeMap::new();
    for b in prog.blocks() {
        let rva = (b.start - base) as u32;
        let Some(idx) = pe.section_of(rva) else {
            continue;
        };
        let sec = &pe.sections[idx];
        let off = (rva - sec.virtual_address) as usize;
        if off >= sec.data.len() {
            continue;
        }
        let slice = sec.data[off..].to_vec();
        let start = b.start;
        blocks.insert(
            start,
            shared::convert(&with_big_stack(move || {
                wlam_sleigh::lift_block(&slice, start)
            })),
        );
    }
    let regs: Vec<String> = wlam_emit::seeded_registers();
    let p = partition(&[base + pe.entry_point_rva as u64], &blocks, &regs);

    let mut owners: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    for f in &p.functions {
        for addr in &f.blocks {
            owners.entry(*addr).or_default().push(f.entry);
        }
    }
    let multi: Vec<_> = owners.iter().filter(|(_, v)| v.len() > 1).collect();
    println!("functions: {}", p.functions.len());
    println!("distinct blocks claimed: {}", owners.len());
    println!("claimed by >1 function: {}", multi.len());
    for (a, v) in multi.iter().take(8) {
        // Guard the subtraction: a block below the image base has no section.
        let in_section = if **a >= base {
            pe.section_of((**a - base) as u32)
                .map(|i| pe.sections[i].name.clone())
        } else {
            None
        };
        println!("  {a:#x} -> {:x?} (section {:?})", v, in_section);
    }
    // Blocks that are not valid code addresses at all.
    let weird: Vec<_> = owners.keys().filter(|a| **a < base).collect();
    println!(
        "claimed blocks below the image base: {:?}",
        weird.iter().take(8).collect::<Vec<_>>()
    );
    let total_uniq: BTreeSet<u64> = owners.keys().copied().collect();
    println!(
        "emitted block count (with duplication): {}",
        p.functions.iter().map(|f| f.blocks.len()).sum::<usize>()
    );
    println!("unique blocks: {}", total_uniq.len());
    // Is the suspect a discovered FUNCTION entry (as opposed to reached by a branch)?
    let funcs = prog.functions();
    for suspect in [0x40dfb0u64, 0x40dfd4, 0x4153f8, 0x40dfa4] {
        println!(
            "  {suspect:#x}: function-entry={} block-present={}",
            funcs.contains(&suspect),
            blocks.contains_key(&suspect)
        );
    }
    // Who reaches the blocks that produce bogus targets?
    for suspect in [0x40dfb0u64, 0x40dfd4, 0x4153f8] {
        for (a, b) in &blocks {
            use wlam_emit::ir::Terminator as T;
            let targets: Vec<u64> = match &b.terminator {
                T::Jump(t) => vec![*t],
                T::Conditional {
                    taken: Some(t),
                    not_taken,
                } => vec![*t, *not_taken],
                T::FallsThrough => vec![b.end],
                T::Call { resume } => vec![*resume],
                T::CallInd { resume } => vec![*resume],
                T::Return | T::Trap(_) | T::IndirectJump | T::Undecodable(_) => vec![],
                T::Conditional { taken: None, .. } => vec![],
            };
            if targets.contains(&suspect) {
                println!("  {suspect:#x} reached from {a:#x} via {:?}", b.terminator);
            }
        }
    }
    // Which block terminators point at the bogus low addresses?
    for (a, b) in &blocks {
        use wlam_emit::ir::Terminator as T;
        let targets: Vec<u64> = match &b.terminator {
            T::Jump(t) => vec![*t],
            T::Conditional {
                taken: Some(t),
                not_taken,
            } => vec![*t, *not_taken],
            T::Conditional {
                taken: None,
                not_taken,
            } => vec![*not_taken],
            T::FallsThrough => vec![b.end],
            _ => vec![],
        };
        for t in targets {
            if t < base {
                let dis = blocks
                    .get(a)
                    .map(|bb| format!("{:?}", bb.terminator))
                    .unwrap_or_default();
                println!(
                    "  low target {t:#x} from block {a:#x}: {}",
                    &dis[..dis.len().min(90)]
                );
            }
        }
    }
}
