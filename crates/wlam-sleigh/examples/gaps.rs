//! Diagnostic: where is discovery incomplete, and why?
use std::collections::{BTreeMap, BTreeSet};

use wlam_pe::Pe;
use wlam_sleigh::discover::{discover, prologue_candidates, Edge};
use wlam_sleigh::with_big_stack;

fn main() {
    let bytes = std::fs::read("../../north-star/bin/Uxn32.exe").expect("north star");
    let pe = Pe::parse(&bytes).expect("parse");
    let entries = prologue_candidates(&pe);
    let prog = with_big_stack(move || discover(&pe, 64));
    let starts: BTreeSet<u64> = prog.block_starts().into_iter().collect();

    let missing: Vec<u64> = entries.iter().copied().filter(|a| !starts.contains(a)).collect();
    println!(
        "prologue candidates: {}, covered: {}, missing: {}",
        entries.len(),
        entries.len() - missing.len(),
        missing.len()
    );
    for a in &missing {
        println!("  missing entry {a:#x}");
    }

    let mut by_kind: BTreeMap<&str, usize> = BTreeMap::new();
    for b in prog.blocks() {
        let k = match &b.edge {
            Edge::Jump(_) => "jump",
            Edge::Conditional { .. } => "cond",
            Edge::FallsTo(_) => "falls",
            Edge::Call { .. } => "call",
            Edge::CallInd { .. } => "callind",
            Edge::IndirectJump { .. } => "indirect",
            Edge::Return => "return",
            Edge::Trap(_) => "trap",
            Edge::Bad(_) => "bad",
        };
        *by_kind.entry(k).or_default() += 1;
    }
    println!("block ends: {by_kind:?}");
    println!("unresolved indirect sites: {}", prog.unresolved_indirect().len());
    for s in prog.unresolved_indirect().iter().take(20) {
        // Show the block's last instruction to explain each one.
        if let Some(b) = prog.blocks().find(|b| b.start == *s) {
            println!("  {s:#x}");
        }
    }

    // How many blocks are reachable only because they are call targets?
    let mut reasons: BTreeMap<&str, usize> = BTreeMap::new();
    for b in prog.blocks() {
        *reasons.entry(b.reason).or_default() += 1;
    }
    println!("block provenance: {reasons:?}");
}
