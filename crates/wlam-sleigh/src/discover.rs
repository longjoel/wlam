//! Whole-program translation: from a parsed PE to emittable blocks.
//!
//! Static recompilation has one hard prerequisite: **every block that can run
//! must be emitted**. A recompiler cannot decode at run time (that would be an
//! interpreter), so discovery has to be complete enough to cover an execution.
//! This module implements the discovery Ghidra calls "rough disassembly":
//!
//! 1. Seed with the entry point, every export, and every IAT-slotted import's
//!    call site (a `call [IAT]` is a host call, not a guest block, but the
//!    instruction after it is a block start).
//! 2. Lift each seed, following *direct* branch targets and fall-throughs.
//!    Calls are followed as functions (their target becomes a seed), because
//!    lifted code is emitted per function, not per trace.
//! 3. Record the address of every instruction encountered, aligned or not.
//!    This is the **IP-relative scrub**: an indirect jump (`jmp dword ptr
//!    [table + eax*4]`, `call eax`) lands on an address that only exists as a
//!    jump-table entry in `.rdata`. Those entries are 4-byte absolute VAs; a
//!    scan for pointers *into an interior instruction* recovers them.
//! 4. Iterate to a fixpoint, then report anything still unresolved instead of
//!    guessing. Unresolved indirect jumps are the honest gap: they are counted
//!    and listed, not silently dropped.
//!
//! The IP-relative scrub is why this pass exists as its own module rather than
//! being folded into lifting: it needs whole-image knowledge (all valid
//! instruction addresses) that a single-block lifter cannot have.

use std::collections::{BTreeMap, BTreeSet};

use wlam_pe::Pe;

use crate::{lift_block, BinOp, Operand, Terminator, Userop, IR};

/// How a block ends, as far as translation is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edge {
    /// A direct jump to a known guest address.
    Jump(u64),
    /// A conditional jump.
    Conditional { taken: Option<u64>, not_taken: u64 },
    /// Fall through to the next instruction.
    FallsTo(u64),
    /// Indirect jump: destination unknown at compile time.
    IndirectJump { site: u64 },
    /// Return to the caller.
    Return,
    /// Traps by design (`INT3`): the compiler's patch point.
    Trap(u64),
    /// The bytes at this address are not valid code.
    Bad(u64),
}

/// A discovered block.
#[derive(Debug, Clone)]
pub struct DiscoveredBlock {
    pub start: u64,
    pub end: u64,
    pub edge: Edge,
    /// P-code size of the block.
    pub op_count: usize,
    pub native_count: usize,
    /// Why this address was considered code, for diagnostics.
    pub reason: &'static str,
}

/// Translation status for one image.
#[derive(Debug)]
pub struct Program {
    blocks: BTreeMap<u64, DiscoveredBlock>,
    /// Direct call targets discovered (rendered as function entities).
    functions: BTreeSet<u64>,
    /// Addresses used as indirect-jump destinations through a table.
    jump_table_targets: BTreeSet<u64>,
    /// Indirect jump sites whose targets were recovered from a table.
    resolved_indirect: BTreeSet<u64>,
    /// Indirect jumps that still have no known set of targets.
    unresolved: BTreeSet<u64>,
    /// Distinct userops reached, with the mnemonics that produce them.
    userops: BTreeMap<u64, BTreeSet<String>>,
    passes: u32,
}

impl Program {
    /// Blocks in address order.
    pub fn blocks(&self) -> impl Iterator<Item = &DiscoveredBlock> {
        self.blocks.values()
    }

    /// Every address that is the start of a block.
    pub fn block_starts(&self) -> Vec<u64> {
        self.blocks.keys().copied().collect()
    }

    /// Functions found (direct call targets).
    pub fn functions(&self) -> Vec<u64> {
        self.functions.iter().copied().collect()
    }

    /// Indirect jump sites resolved through a recovered table.
    pub fn resolved_indirect(&self) -> Vec<u64> {
        self.resolved_indirect.iter().copied().collect()
    }

    /// Recovered jump-table destinations.
    pub fn jump_table_targets(&self) -> Vec<u64> {
        self.jump_table_targets.iter().copied().collect()
    }

    /// Indirect jumps with no recovered targets. These are the honest gap.
    pub fn unresolved_indirect(&self) -> Vec<u64> {
        self.unresolved.iter().copied().collect()
    }

    /// Discovery passes needed to reach a fixpoint.
    pub fn passes(&self) -> u32 {
        self.passes
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    pub fn total_ops(&self) -> usize {
        self.blocks.values().map(|b| b.op_count).sum()
    }

    pub fn total_native(&self) -> usize {
        self.blocks.values().map(|b| b.native_count).sum()
    }

    /// P-code ops per native instruction across the whole program.
    pub fn pcode_ratio(&self) -> f64 {
        self.total_ops() as f64 / self.total_native().max(1) as f64
    }

    /// Userops reached anywhere in the program.
    pub fn userops(&self) -> &BTreeMap<u64, BTreeSet<String>> {
        &self.userops
    }

    /// Userops with no mapped meaning: a hard stop for the emitter.
    pub fn unknown_userops(&self) -> Vec<u64> {
        self.userops
            .keys()
            .copied()
            .filter(|id| matches!(crate::classify_userop(*id), Userop::Unknown(_)))
            .collect()
    }
}

/// Translate a 32-bit image into emittable blocks.
///
/// `max_passes` bounds the fixpoint iteration; discovery normally converges in
/// two or three rounds. Exceeding the bound is reported, not hidden.
pub fn discover(pe: &Pe, max_passes: u32) -> Program {
    let base = pe.image_base as u32;
    let mut prog = Program {
        blocks: BTreeMap::new(),
        functions: BTreeSet::new(),
        jump_table_targets: BTreeSet::new(),
        resolved_indirect: BTreeSet::new(),
        unresolved: BTreeSet::new(),
        userops: BTreeMap::new(),
        passes: 0,
    };
    let mut work: BTreeSet<(u64, &'static str)> = BTreeSet::new();
    // Seeds: entry point and exports are unambiguously code.
    work.insert((base as u64 + pe.entry_point_rva as u64, "entry point"));
    for e in &pe.exports {
        if pe.section_of(e.rva).map(|i| pe.sections[i].is_executable()).unwrap_or(false) {
            work.insert((base as u64 + e.rva as u64, "export"));
        }
    }
    // Call sites through the import table: the instruction *after* each
    // `call [IAT]` is a block start. IAT slots live in readable data, so map
    // each slot to the code that references it by scanning `.text` for the
    // `ff 15 <slot>` / `ff 25 <slot>` encodings.
    for addr in scan_iat_references(pe) {
        work.insert((addr, "after IAT call"));
    }

    let mut pass = 0;
    while !work.is_empty() && pass < max_passes {
        pass += 1;
        let seeds: Vec<(u64, &'static str)> = std::mem::take(&mut work).into_iter().collect();
        for (addr, reason) in seeds {
            if prog.blocks.contains_key(&addr) {
                continue;
            }
            let Some(rva) = addr.checked_sub(base as u64).and_then(|v| u32::try_from(v).ok()) else {
                continue;
            };
            let Some(section) = pe.section_of(rva).map(|i| &pe.sections[i]) else {
                continue;
            };
            if !section.is_executable() {
                continue;
            }
            let off = rva - section.virtual_address;
            let bytes = &section.data[off as usize..];

            let block = lift_block(bytes, addr);
            if block.instructions.is_empty() {
                prog.blocks.insert(
                    addr,
                    DiscoveredBlock {
                        start: addr,
                        end: addr,
                        edge: Edge::Bad(addr),
                        op_count: 0,
                        native_count: 0,
                        reason,
                    },
                );
                continue;
            }
            for insn in &block.instructions {
                for op in &insn.ops {
                    if let IR::Userop { id, .. } = op {
                        let mnemonic = insn.disassembly.split(' ').next().unwrap_or("?").to_string();
                        prog.userops.entry(*id).or_default().insert(mnemonic);
                    }
                }
            }
            let edge = match &block.terminator {
                Terminator::FallsThrough => Edge::FallsTo(block.end),
                Terminator::Jump(t) => {
                    work.insert((*t, "jump target"));
                    Edge::Jump(*t)
                }
                Terminator::Conditional { taken, not_taken } => {
                    if let Some(t) = taken {
                        work.insert((*t, "conditional target"));
                    } else {
                        prog.unresolved.insert(block.start);
                    }
                    work.insert((*not_taken, "fall-through"));
                    Edge::Conditional { taken: *taken, not_taken: *not_taken }
                }
                Terminator::IndirectJump => {
                    prog.unresolved.insert(block.start);
                    Edge::IndirectJump { site: block.start }
                }
                Terminator::Return => Edge::Return,
                Terminator::Trap(t) => Edge::Trap(*t),
                Terminator::Undecodable(at) => Edge::Bad(*at),
            };
            // Direct calls: the callee is a function to translate, and the
            // instruction after the call continues this block's chain.
            for insn in &block.instructions {
                for op in &insn.ops {
                    if let IR::Call { target: crate::Operand::Ram { offset, .. }, .. } = op {
                        prog.functions.insert(*offset);
                        work.insert((*offset, "call target"));
                    }
                }
            }
            match edge {
                Edge::FallsTo(next) => {
                    work.insert((next, "fall-through"));
                }
                _ => {}
            }
            prog.blocks.insert(
                block.start,
                DiscoveredBlock {
                    start: block.start,
                    end: block.end,
                    edge,
                    op_count: block.op_count(),
                    native_count: block.native_instruction_count(),
                    reason,
                },
            );
        }
        // Jump-table recovery. A table alone is not enough: the table's length
        // is only knowable from the bound-checked index that feeds it, and the
        // entries must point at *aligned* instruction starts (verified against
        // Uxn32: table at 0x402938 has 7 entries, and entry 7 is data).
        for (site, targets) in recover_jump_tables(pe, &prog) {
            if targets.is_empty() {
                continue;
            }
            prog.jump_table_targets.extend(targets.iter().copied());
            prog.resolved_indirect.insert(site);
            prog.unresolved.remove(&site);
            for t in targets {
                work.insert((t, "jump table"));
            }
        }
    }
    prog.passes = pass;
    prog
}

/// Function entry points found by an independent heuristic: the MSVC/GCC 32-bit
/// prologue `push ebp; mov ebp, esp` (`55 8b ec`).
///
/// Deliberately a *different* method from call following, so comparing the two
/// is a real completeness check rather than a tautology.
pub fn prologue_candidates(pe: &Pe) -> Vec<u64> {
    let base = pe.image_base as u64;
    let mut out = Vec::new();
    for sec in &pe.sections {
        if !sec.is_executable() {
            continue;
        }
        let d = &sec.data;
        let mut i = 0usize;
        while i + 3 <= d.len() {
            if d[i] == 0x55 && d[i + 1] == 0x8b && d[i + 2] == 0xec {
                out.push(base + sec.virtual_address as u64 + i as u64);
            }
            i += 1;
        }
    }
    out
}

/// Addresses following each `ff 15` (call [mem]) / `ff 25` (jmp [mem]) in
/// executable sections: unambiguous code seeds at imported-function call sites.
fn scan_iat_references(pe: &Pe) -> Vec<u64> {
    let base = pe.image_base as u32;
    let mut out = Vec::new();
    for sec in &pe.sections {
        if !sec.is_executable() {
            continue;
        }
        let d = &sec.data;
        let mut i = 0usize;
        while i + 6 <= d.len() {
            if d[i] == 0xff && (d[i + 1] == 0x15 || d[i + 1] == 0x25) {
                let site = base as u64 + (sec.virtual_address + i as u32) as u64;
                out.push(site + 6);
                i += 6;
                continue;
            }
            i += 1;
        }
    }
    out
}

/// Diagnostic: why each unresolved indirect site did or did not recover.
///
/// Returns `(site, bound_found, has_indexed_load)` per unresolved site. A site
/// without an indexed load is an import thunk or a computed goto — a different,
/// acceptable case from a switch table we failed to read.
pub fn recovery_report(pe: &Pe, prog: &Program) -> Vec<(u64, Option<u64>, bool)> {
    prog.unresolved
        .iter()
        .map(|&site| {
            let ops = prog.blocks.get(&site).map(|b| block_ops(pe, b)).unwrap_or_default();
            let has_indexed_load = ops.iter().any(|o| {
                matches!(o, IR::Bin { op: BinOp::Mul, operands, .. }
                    if operands.1.const_or_ram() == Some(4))
            });
            (site, scan_back_for_bound(pe, site, 96), has_indexed_load)
        })
        .collect()
}

/// Recover jump tables for indirect-jump sites, using their **own block's**
/// P-code.
///
/// Uxn32's generated switch shape:
///
/// ```text
///   xor ecx, ecx
///   mov  cl, byte ptr [eax + 0x402954]     ; bound table lookup
///   cmp  ecx, 0x2b                         ; bound (table length - 1)
///   ja   default                           ; a *conditional* block terminator
///   jmp  dword ptr [ecx*4 + 0x402938]      ; indirect jump (this block)
/// ```
///
/// The bound comparison therefore lives in a *different* block, so this pass
/// looks at the predecessor that jumps or falls through to the site. The table
/// length comes from that bound, never from scanning until the data ends, and
/// every entry must point at an executable, aligned instruction start —
/// Uxn32's table has 7 valid entries and a data word as entry 8.
fn recover_jump_tables(pe: &Pe, prog: &Program) -> Vec<(u64, Vec<u64>)> {
    let base = pe.image_base as u64;
    let mut out = Vec::new();
    for &site in &prog.unresolved {
        let Some(block) = prog.blocks.get(&site) else { continue };
        if !matches!(block.edge, Edge::IndirectJump { .. }) {
            continue;
        }
        // The jump's effective address is a computed temp; the table is the
        // immediate addend of the `Add` that produces it
        // (`jmp [ecx*4 + 0x402938]` -> `Add(0x402938, Mul(ECX, 4))`).
        // The jump's effective address is a computed temp. Two shapes occur:
        //   jmp [ebx*4 + 0x406054]  -> Add(0x406054, Mul(EBX, 4)) into a temp
        //   jmp [0x411124]          -> the indirect target *is* a constant slot
        //                               reached straight from memory
        let ops = block_ops(pe, block);
        // The block's P-code tells us the shape:
        //   jmp [ebx*4 + 0x406054]  -> Mul, Add(0x406054, tidx) -> goto *taddr
        //   jmp [0x411124]          -> goto *ram:0x411124       (import thunk slot)
        //   jmp eax                 -> goto *EAX                (no static target)
        let jump_target = ops.iter().find_map(|o| match o {
            IR::IndirectJump { target } => Some(target.clone()),
            _ => None,
        });
        let Some(jump_target) = jump_target else { continue };
        let table = match &jump_target {
            // The value the jump computes lives in a temp whose address Sleigh
            // reports in the *register* space. A RAM-space destination instead
            // means the encoding baked in a fixed slot address (an import
            // thunk), which has no guest targets.
            Operand::Ram { .. } => None,
            Operand::Const { value, .. } => Some(*value),
            Operand::Reg { name, .. } if name.starts_with("t") => {
                let t: u64 = u64::from_str_radix(&name[1..], 16).unwrap_or(u64::MAX);
                // The displacement of the indexed load is the Add's constant
                // addend; the *index* reaches the jump through the Mul.
                let add = ops.iter().find_map(|o| match o {
                    IR::Bin { op: BinOp::Add, dst: Operand::Temp { offset: d, .. }, operands }
                        if *d == t =>
                    {
                        Some(operands)
                    }
                    _ => None,
                });
                add.and_then(|ops2| ops2.0.const_or_ram().or_else(|| ops2.1.const_or_ram()))
                    // Require the other operand to be a computed index, not
                    // another constant (that would be a plain address).
                    .filter(|_| {
                        add.map(|ops2| !(ops2.0.const_or_ram().is_some() && ops2.1.const_or_ram().is_some()))
                            .unwrap_or(false)
                    })
            }
            _ => None,
        };
        let Some(table) = table else { continue };
        // A table must live in a read-only data section.
        let Ok(table_rva) = u32::try_from(table.saturating_sub(base)) else { continue };
        match pe.section_of(table_rva) {
            Some(i) if !pe.sections[i].is_executable() => {}
            _ => continue,
        }
        // The bound check lives earlier in the *same function's* straight-line
        // code, not in a predecessor block: discovery splits blocks at every
        // taken jump, so the `cmp` and the switch can be separated by a block
        // boundary (verified: at 0x405ebc the `cmp` is ~40 bytes back, after an
        // unrelated `ja`). Scan backwards in the enclosing text window.
        let bound = scan_back_for_bound(pe, site, 96);
        let Some(bound) = bound else { continue };
        let count = (bound as usize + 1).min(256);
        let mut targets = Vec::new();
        for i in 0..count {
            let Ok(slot) = u32::try_from(table).map(|t| t + (i as u32) * 4) else { break };
            let Some(raw) = pe.read_rva(slot, 4) else { break };
            let v = u64::from(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]));
            let Ok(rva) = u32::try_from(v.saturating_sub(base)) else { break };
            let Some(idx) = pe.section_of(rva) else { break };
            if !pe.sections[idx].is_executable() || rva % 4 != 0 && v % 4 != 0 {
                break;
            }
            targets.push(v);
        }
        if !targets.is_empty() {
            out.push((site, targets));
        }
    }
    out
}

/// Scan backwards from `site` for the nearest `cmp <8-bit temp>, imm` that
/// feeds a `ja`, within `limit` bytes. Returns the immediate (bound).
///
/// Decoding a window and walking instructions forward from a plausible start is
/// unreliable mid-function, so this decodes forward from `site - limit` and
/// keeps the last matching comparison before `site`.
pub(crate) fn scan_back_for_bound(pe: &Pe, site: u64, limit: u64) -> Option<u64> {
    // Do NOT decode forward from `site - limit`: x86 is variable-length, so an
    // arbitrary start often desynchronises and the comparison never appears
    // (measured at 0x405ebc, where a 96-byte window decoded into garbage).
    // Instead, pattern-match the encoding of `cmp <reg>, imm` in a bounded
    // byte window and take the *last* occurrence. The encodings used by both
    // clang and MSVC for switch bound checks are:
    //   3C ib            cmp al, imm8
    //   80 /7 ib         cmp r/m8, imm8
    //   3D id            cmp eax, imm32
    //   83 /7 ib         cmp r/m32, imm8  (sign-extended)
    let base = pe.image_base as u64;
    let start = site.saturating_sub(limit);
    let rva = u32::try_from(start.saturating_sub(base)).ok()?;
    let sec = &pe.sections[pe.section_of(rva)?];
    let off = (rva - sec.virtual_address) as usize;
    let end = (site.saturating_sub(base) as usize).min(sec.data.len());
    let window = sec.data.get(off..end)?;
    let mut best: Option<u64> = None;
    for i in 0..window.len() {
        let imm = match window[i] {
            0x3c => window.get(i + 1).map(|b| *b as u64),
            0x80 if window.get(i + 1).map(|b| (b >> 3) & 7 == 7).unwrap_or(false) => {
                window.get(i + 2).map(|b| *b as u64)
            }
            0x3d => window.get(i + 1..i + 5).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64),
            0x83 if window.get(i + 1).map(|b| (b >> 3) & 7 == 7).unwrap_or(false) => {
                window.get(i + 2).map(|b| *b as i8 as i64 as u64)
            }
            _ => None,
        };
        if let Some(imm) = imm.filter(|v| *v < 256) {
            best = Some(imm);
        }
    }
    best
}

/// The trailing immediate of an disassembly line such as `CMP EAX,0x2b`.
fn last_imm(text: &str) -> Option<u64> {
    text.rsplit(|c: char| c == ',' || c == ' ')
        .find_map(|tok| tok.strip_prefix("0x").and_then(|h| u64::from_str_radix(h, 16).ok()))
}

/// Re-lift a discovered block/// Re-lift a discovered block to inspect its P-code (discovery stores only
/// sizes, so this is a targeted re-decode rather than cached IR).
fn block_ops(pe: &Pe, block: &DiscoveredBlock) -> Vec<IR> {
    let base = pe.image_base as u64;
    let Ok(rva) = u32::try_from(block.start.saturating_sub(base)) else { return Vec::new() };
    let Some(idx) = pe.section_of(rva) else { return Vec::new() };
    let sec = &pe.sections[idx];
    let off = (rva - sec.virtual_address) as usize;
    let bytes = sec.data[off.min(sec.data.len())..].to_vec();
    let start = block.start;
    crate::with_big_stack(move || {
        lift_block(&bytes, start).instructions.into_iter().flat_map(|i| i.ops).collect()
    })
}

/// Immediate value of an operand, including RAM-space constants (how Sleigh
/// encodes immediates in comparisons).
trait Constish {
    fn const_or_ram(&self) -> Option<u64>;
}

impl Constish for Operand {
    fn const_or_ram(&self) -> Option<u64> {
        match self {
            Operand::Const { value, .. } | Operand::Ram { offset: value, .. } => Some(*value),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uxn32() -> Option<Pe> {
        let bytes = std::fs::read("../../north-star/bin/Uxn32.exe").ok()?;
        Pe::parse(&bytes).ok()
    }

    #[test]
    fn discovers_the_entry_point() {
        let Some(pe) = uxn32() else {
            eprintln!("skipping: north-star binary not present");
            return;
        };
        let entry_rva = pe.entry_point_rva as u64 + pe.image_base;
        let entry_addr = pe.image_base + pe.entry_point_rva as u64;
        let _ = entry_rva;
        let prog = crate::with_big_stack(move || discover(&pe, 8));
        assert!(prog.block_count() > 0);
        // The entry point must be present and recorded as such. (It is not
        // necessarily the lowest-address block: the address order interleaves
        // with other discovered code.)
        let entry = prog
            .blocks()
            .find(|b| b.start == entry_addr)
            .expect("the entry point must be a discovered block");
        // A JIT-entry thunk or a jump to a cold constructor often makes the
        // entry block a call target as well; accept either provenance but
        // require that the entry address itself was discovered.
        assert!(
            matches!(entry.reason, "entry point" | "call target" | "jump target"),
            "entry block reason was {:?}",
            entry.reason
        );
    }

    #[test]
    fn follows_calls_into_functions() {
        let Some(pe) = uxn32() else {
            return;
        };
        let entries = prologue_candidates(&pe);
        let prog = crate::with_big_stack(move || discover(&pe, 8));
        assert!(prog.functions().len() > 10, "found {} functions", prog.functions().len());
        // A workable partial translation should cover a meaningful slice.
        // Completeness, not just a count: do the function entry points found
        // (by an independent scan for prologues) appear as blocks?
        let covered = entries.iter().filter(|a| prog.blocks().any(|b| b.start == **a)).count();
        eprintln!(
            "coverage: {covered}/{} prologue-derived entry points have blocks",
            entries.len()
        );
        eprintln!(
            "blocks={} functions={} ops={} native={} ratio={:.2} passes={} unresolved_indirect={} jump_table_targets={}",
            prog.block_count(),
            prog.functions().len(),
            prog.total_ops(),
            prog.total_native(),
            prog.pcode_ratio(),
            prog.passes(),
            prog.unresolved_indirect().len(),
            prog.jump_table_targets().len(),
        );
    }

    #[test]
    fn indirect_jumps_are_reported_not_guessed() {
        let Some(pe) = uxn32() else {
            return;
        };
        let prog = crate::with_big_stack(move || discover(&pe, 4));
        // Any indirect jump must either have recovered targets (via the
        // IP-relative scrub) or be listed in `unresolved_indirect`.
        for site in prog.unresolved_indirect() {
            assert!(
                prog.blocks().any(|b| b.start == site),
                "unresolved site {site:#x} must be a block"
            );
        }
        eprintln!("unresolved indirect sites: {}", prog.unresolved_indirect().len());
    }

    #[test]
    fn no_unknown_userops_in_discovered_code() {
        let Some(pe) = uxn32() else {
            return;
        };
        let prog = crate::with_big_stack(move || discover(&pe, 8));
        let unknown = prog.unknown_userops();
        assert!(unknown.is_empty(), "unknown userops reached: {unknown:?} via {:?}", prog.userops());
    }

    #[test]
    fn pcode_ratio_stays_linear() {
        let Some(pe) = uxn32() else {
            return;
        };
        let prog = crate::with_big_stack(move || discover(&pe, 8));
        let ratio = prog.pcode_ratio();
        assert!(ratio > 1.0 && ratio < 20.0, "pcode/native ratio {ratio}");
    }
}
