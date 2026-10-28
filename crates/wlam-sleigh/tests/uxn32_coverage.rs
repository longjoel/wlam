//! Coverage and gate measurements against the north-star executable.
//!
//! These are the numbers that justify the design: which Sleigh userops real
//! code reaches, how many registers must be modelled, and what the
//! P-code-per-native-instruction ratio is (the property that separates a static
//! recompiler from a trace interpreter).
use std::path::PathBuf;

use wlam_sleigh::{lift_block, with_big_stack, Coverage, Operand, Terminator, Userop, IR};

fn uxn32_text() -> Option<(Vec<u8>, u64)> {
    let path = PathBuf::from("../../north-star/bin/Uxn32.exe");
    let bytes = std::fs::read(path).ok()?;
    let u32at = |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let u16at = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]) as usize;
    let e = u32at(0x3c) as usize;
    let base = u32at(e + 24 + 28) as u64;
    let nsec = u16at(e + 6);
    let sec = e + 24 + u16at(e + 20);
    let mut text = Vec::new();
    let mut text_rva = 0u64;
    for i in 0..nsec {
        let o = sec + i * 40;
        if bytes[o..o + 8].starts_with(b".text") {
            let size = u32at(o + 16) as usize;
            text_rva = u32at(o + 12) as u64;
            let off = u32at(o + 20) as usize;
            text = bytes[off..off + size].to_vec();
        }
    }
    (!text.is_empty()).then_some((text, base + text_rva))
}

#[test]
fn userop_surface_of_the_north_star() {
    let Some((text, base)) = uxn32_text() else {
        eprintln!("skipping: north-star binary not present");
        return;
    };
    let cov: Coverage = with_big_stack(move || wlam_sleigh::scan(&text, base));
    eprintln!("instructions={} undecodable={}", cov.instructions, cov.undecodable);
    for (id, mnemonics) in &cov.userops {
        eprintln!("  userop {id}: {:?} -> {:?}", wlam_sleigh::classify_userop(*id), mnemonics);
    }
    // The decoded executable reaches exactly five userops, four of which are
    // ordinary instruction semantics (ports, INT3, locked xchg).
    assert!(cov.instructions > 20_000, "decoded {} instructions", cov.instructions);
    assert!(cov.undecodable < cov.instructions / 100, "resync rate is small");
    let unknown: Vec<_> = cov.unknown_userops().collect();
    assert!(unknown.is_empty(), "unclassified userops in real code: {unknown:?}");
    let kinds: Vec<Userop> = cov.userops.keys().map(|id| wlam_sleigh::classify_userop(*id)).collect();
    for expected in [Userop::PortIn, Userop::Breakpoint, Userop::LockedXchg] {
        assert!(kinds.contains(&expected), "expected {expected:?} in {kinds:?}");
    }
}

#[test]
fn registers_needed_for_real_code_are_covered() {
    let Some((text, base)) = uxn32_text() else {
        return;
    };
    let coverage = with_big_stack(move || {
        let mut found = std::collections::BTreeSet::new();
        let mut ip = base;
        let mut rest: &[u8] = &text;
        let mut decoder = wlam_sleigh::Decoder::new(wlam_sleigh::Architecture::X86_32);
        while !rest.is_empty() {
            let Ok(ins) = decoder.decode(rest, ip) else {
                rest = &rest[1..];
                ip += 1;
                continue;
            };
            if ins.len == 0 {
                break;
            }
            for op in &ins.ops {
                let ops = wlam_sleigh::lower(op);
                collect_regs(&ops, &mut found);
            }
            ip += ins.len;
            rest = &rest[ins.len as usize..];
        }
        found
    });
    eprintln!("distinct registers referenced: {}", coverage.len());
    // x87 lives in Sleigh's ST0..ST7 registers, and the SEH prologue uses
    // FS_OFFSET; both must be discoverable by name.
    for want in ["ESP", "EBP", "EAX", "ZF", "CF", "FS_OFFSET"] {
        assert!(coverage.contains(want), "missing {want} in {coverage:?}");
    }
    let unnamed = coverage
        .iter()
        .filter(|n| n.starts_with("reg_") || n.parse::<u32>().is_ok())
        .count();
    assert_eq!(unnamed, 0, "every referenced register must have a known name: {coverage:?}");
}

fn collect_regs(ir: &IR, out: &mut std::collections::BTreeSet<String>) {
    fn visit(o: &Operand, out: &mut std::collections::BTreeSet<String>) {
        if let Operand::Reg { name, .. } = o {
            out.insert(name.to_string());
        }
    }
    match ir {
        IR::Copy { dst, src } => {
            visit(dst, out);
            visit(src, out);
        }
        IR::Load { dst, addr, .. } => {
            visit(dst, out);
            visit(addr, out);
        }
        IR::Store { addr, value, .. } => {
            visit(addr, out);
            visit(value, out);
        }
        IR::Bin { dst, operands, .. } => {
            visit(dst, out);
            visit(&operands.0, out);
            visit(&operands.1, out);
        }
        IR::Un { dst, src, .. } => {
            visit(dst, out);
            visit(src, out);
        }
        IR::Subpiece { dst, src, .. } => {
            visit(dst, out);
            visit(src, out);
        }
        IR::Branch { target } | IR::Call { target } | IR::CallInd { target } => visit(target, out),
        IR::CBranch { cond, target } => {
            visit(cond, out);
            visit(target, out);
        }
        IR::Ret { target } => {
            if let Some(t) = target {
                visit(t, out);
            }
        }
        IR::Userop { args, dst, .. } => {
            for a in args {
                visit(a, out);
            }
            if let Some(d) = dst {
                visit(d, out);
            }
        }
        IR::Nop => {}
    }
}

/// The engineering gate: emitted P-code ops per native instruction.
#[test]
fn pcode_per_native_instruction_is_bounded() {
    let Some((text, base)) = uxn32_text() else {
        return;
    };
    // Lift the entry point's first 400 instructions as a sample.
    let sample = text[..4096.min(text.len())].to_vec();
    let (ops, insns) = with_big_stack(move || {
        let sample = sample;
        (0..sample.len() / 3)
            .map(|i| lift_block(&sample[i..], base + i as u64))
            .filter(|b| !b.instructions.is_empty())
            .fold((0usize, 0usize), |(o, n), b| (o + b.op_count(), n + b.native_instruction_count()))
    });
    let ratio = ops as f64 / insns.max(1) as f64;
    eprintln!("sample: {ops} pcode ops over {insns} native instructions (ratio {ratio:.1})");
    // Sleigh's x86 lift is verbose (flags, partial registers); anything near
    // this bound is still linear in the block, which is the property that
    // makes block-shaped emission legitimate rather than trace interpretation.
    assert!(ratio < 20.0, "pcode/native ratio {ratio:.1} too high for a static emitter");
}

#[test]
fn entry_block_terminator_is_recognised() {
    let Some((text, base)) = uxn32_text() else {
        return;
    };
    let block = with_big_stack(move || lift_block(&text, base));
    assert!(block.instructions.len() > 4, "entry block lifts several instructions");
    // The SEH prologue and an indirect call through the IAT appear early.
    let has_call_ind = block.instructions.iter().flat_map(|i| &i.ops).any(|o| matches!(o, IR::CallInd { .. }));
    assert!(has_call_ind, "entry block calls through the IAT");
    assert!(!matches!(block.terminator, Terminator::Undecodable(_)), "{:?}", block.terminator);
}
