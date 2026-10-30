//! Probe: count lifted blocks and operands that use a 10-byte (x87 80-bit) width.
//!
//! This measures the "unsupported operand width 10 bytes" blocker directly from
//! the lifted IR, without going through the emitter. It answers two questions the
//! roadmap needs to keep honest:
//!
//!   * how many blocks contain a 10-byte operand (the emitter will refuse these);
//!   * whether that number changed after the phantom-block fix.
//!
//! Usage: `cargo run --example probe_x87 -- [path.exe]`
use wlam_pe::Pe;
use wlam_sleigh::discover::discover;
use wlam_sleigh::IR;

/// Widths of every operand an IR op mentions.
fn widths_of(op: &IR) -> Vec<u32> {
    match op {
        IR::Copy { dst, src } => vec![dst.size(), src.size()],
        IR::Load { dst, addr, size } => vec![dst.size(), addr.size(), *size],
        IR::Store { addr, value, size } => vec![addr.size(), value.size(), *size],
        IR::Bin { dst, operands, .. } => vec![dst.size(), operands.0.size(), operands.1.size()],
        IR::Un { dst, src, .. } => vec![dst.size(), src.size()],
        IR::Float { dst, operands, .. } => {
            vec![dst.size(), operands.0.size(), operands.1.size()]
        }
        IR::Convert { dst, src, .. } => vec![dst.size(), src.size()],
        IR::Subpiece { dst, src, .. } => vec![dst.size(), src.size()],
        IR::Branch { target } | IR::IndirectJump { target } | IR::Call { target } => {
            vec![target.size()]
        }
        IR::CallInd { target } => vec![target.size()],
        IR::CBranch { cond, target } => vec![cond.size(), target.size()],
        IR::Ret { target } => target.iter().map(|t| t.size()).collect(),
        IR::Userop { args, dst, .. } => {
            let mut v: Vec<u32> = args.iter().map(|a| a.size()).collect();
            if let Some(d) = dst {
                v.push(d.size());
            }
            v
        }
        IR::Nop => Vec::new(),
    }
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "../../benchmarks/minesweeper/bin/minesweeper-x86.exe".into());
    let bytes = std::fs::read(&path).expect("image");
    let pe = Pe::parse(&bytes).expect("parse");
    let base = pe.image_base as u64;

    let (total, wide_blocks, wide_ops, examples, mnemonics, x87_addr_list, reach) = wlam_sleigh::with_big_stack(move || {
        let prog = discover(&pe, 64);
        let mut wide_blocks = 0usize;
        let mut wide_ops = 0usize;
        let mut examples: Vec<(u64, String)> = Vec::new();
        let mut distinct: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
        let mut mnemonics: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        let mut x87_addr_list: Vec<(u64, String)> = Vec::new(); // x87_addr_list
        for b in prog.blocks() {
            // Re-lift the block: discovery caches sizes, not full IR.
            let Some(rva) = b.start.checked_sub(base).and_then(|v| u32::try_from(v).ok()) else {
                continue;
            };
            let Some(idx) = pe.section_of(rva) else { continue };
            let sec = &pe.sections[idx];
            let off = (rva - sec.virtual_address) as usize;
            let Some(rest) = sec.data.get(off..) else { continue };
            let rest = rest.to_vec();
            let start = b.start;
            let block = wlam_sleigh::with_big_stack(move || wlam_sleigh::lift_block(&rest, start));
            let mut hit = false;
            for insn in &block.instructions {
                for op in &insn.ops {
                    for w in widths_of(op) {
                        if w == 10 {
                            wide_ops += 1;
                            hit = true;
                        }
                    }
                }
                if hit && examples.len() < 8 {
                    examples.push((insn.address, insn.disassembly.clone()));
                }
            }
            if hit {
                wide_blocks += 1;
                distinct.insert(b.start);
                for insn in &block.instructions {
                    let w = insn.disassembly.split_whitespace().next().unwrap_or("?").to_string();
                    *mnemonics.entry(w).or_insert(0) += 1;
                    let n = insn.disassembly.split_whitespace().next().unwrap_or("");
                    if n.starts_with('F') && insn.disassembly.contains('F') {
                        x87_addr_list.push((insn.address, insn.disassembly.clone()));
                    }
                }
            }
        }
        // Reachability from the entry point, over the discovered edges.
        let mut reach: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
        let entries: Vec<u64> = prog
            .blocks()
            .filter(|b| b.reason == "entry point")
            .map(|b| b.start)
            .collect();
        // Functions are entry points in their own right: `Edge::Call` records
        // only the *resume* address, so following edges alone under-approximates
        // reachability badly (78 of 4203 blocks). Seed every discovered function.
        let mut stack: Vec<u64> = entries.clone();
        stack.extend(prog.functions());
        while let Some(a) = stack.pop() {
            if !reach.insert(a) {
                continue;
            }
            let Some(b) = prog.blocks().find(|b| b.start == a) else { continue };
            use wlam_sleigh::discover::Edge;
            let succs: Vec<u64> = match &b.edge {
                Edge::FallsTo(n) => vec![*n],
                Edge::Jump(t) => vec![*t],
                Edge::Conditional { taken, not_taken } => {
                    let mut v = vec![*not_taken];
                    if let Some(t) = taken { v.push(*t); }
                    v
                }
                Edge::Call { resume } | Edge::CallInd { resume } => vec![*resume],
                _ => vec![],
            };
            for s2 in succs { stack.push(s2); }

        }
        (prog.block_count(), distinct.len(), wide_ops, examples, mnemonics, x87_addr_list, reach)
    });

    let pct = if total == 0 { 0.0 } else { wide_blocks as f64 * 100.0 / total as f64 };
    println!("{path}");
    println!("  image base      {base:#x}");
    println!("  blocks          {total}");
    println!("  x87 blocks      {wide_blocks} ({pct:.1}%) [distinct starts: {wide_blocks}]");
    println!("  x87 operands    {wide_ops}");
    println!("  --- mnemonics appearing in x87 blocks ---");
    for (m, n) in &mnemonics {
        println!("    {m:<28} {n}");
    }
    if std::env::var("X87_SITES").is_ok() {
        println!("  --- every x87 instruction site ---");
        let mut v = x87_addr_list;
        v.sort();
        v.dedup();
        for (a, d) in v {
            // `reach` holds block *starts*; an instruction is reachable if the
            // block containing it is. Checking the instruction address directly
            // reported every mid-block x87 as unreachable.
            let r = if reach.iter().any(|bs| *bs <= a && a < *bs + 0x400) {
                "REACHABLE"
            } else {
                "unreached"
            };
            println!("    {a:#x}  [{r}]  {d}");
        }
    }
    println!("  --- first instructions seen ---");
    for (addr, dis) in examples {
        println!("    {addr:#x}  {dis}");
    }
}
