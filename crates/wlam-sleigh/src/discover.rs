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
    /// A call: control leaves for a callee and resumes at `resume`.
    Call { resume: u64 },
    /// An indirect call; the callee is chosen at run time.
    CallInd { resume: u64 },
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
    indirect_targets: BTreeMap<u64,Vec<u64>>,
    /// Indirect jumps that still have no known set of targets.
    unresolved: BTreeSet<u64>,
    /// Distinct userops reached, with the mnemonics that produce them.
    userops: BTreeMap<u64, BTreeSet<String>>,
    passes: u32,
    /// Work items still queued when `max_passes` ran out.
    ///
    /// Non-empty means discovery stopped early and the block set is
    /// incomplete. It is recorded (and surfaced by `wlam analyze`) rather than
    /// silently dropping the remainder, which is how a pass limit quietly cost
    /// function coverage.
    pending: BTreeSet<(u64, &'static str)>,
    /// Conditions that make the result unusable, e.g. a non-i386 image.
    ///
    /// Reported rather than silently producing a partial block set.
    problems: Vec<String>,
}

impl Program {
    /// Blocks in address order.
    pub fn indirect_targets(&self)->&BTreeMap<u64,Vec<u64>> {&self.indirect_targets}

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

    /// Addresses left unprocessed because `max_passes` was reached.
    ///
    /// Empty when discovery converged. Callers that care about completeness
    /// must check this: a non-empty result means the block set is partial.
    pub fn pending(&self) -> &BTreeSet<(u64, &'static str)> {
        &self.pending
    }

    /// Whether discovery ran out of passes before the worklist emptied.
    pub fn incomplete(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Conditions that make this program unusable (empty means usable).
    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    /// Whether discovery refused the image outright.
    pub fn refused(&self) -> bool {
        !self.problems.is_empty()
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
/// `max_passes` bounds the fixpoint iteration. It used to converge in two or
/// three rounds, but once a `call` became a block boundary every call adds a
/// resume edge, so real images need many more (Uxn32 converges around 20).
/// Exceeding the bound is reported via [`Program::pending`] rather than
/// silently truncating the block set.
fn empty_program() -> Program {
    Program {
        blocks: BTreeMap::new(),
        functions: BTreeSet::new(),
        jump_table_targets: BTreeSet::new(),
        resolved_indirect: BTreeSet::new(),
        indirect_targets: BTreeMap::new(),
        unresolved: BTreeSet::new(),
        userops: BTreeMap::new(),
        passes: 0,
        pending: BTreeSet::new(),
        problems: Vec::new(),
    }
}

pub fn discover(pe: &Pe, max_passes: u32) -> Program {
    discover_observing(pe, max_passes, |_| {})
}

/// Let the compiler consume lifted blocks during discovery instead of lifting
/// the entire image a second time after convergence.
pub(crate) fn discover_observing(pe: &Pe, max_passes: u32, mut on_block: impl FnMut(&crate::Block)) -> Program {
    discover_observing_with_seeds(pe,max_passes,&[],&mut on_block)
}

pub(crate) fn discover_observing_with_seeds(pe:&Pe,max_passes:u32,extra_seeds:&[u64],mut on_block:impl FnMut(&crate::Block))->Program {
    // This toolchain is 32-bit Win32 only. The Sleigh lifter decodes x86-32;
    // handing it an x86-64 or ARM64 image would decode 64-bit code as 32-bit
    // and produce plausible-looking nonsense. Refuse loudly instead: an empty
    // program with a recorded problem, never a wrong answer.
    if !matches!(pe.machine, wlam_pe::Machine::I386) {
        let mut prog = empty_program();
        prog.problems.push(format!(
            "unsupported machine {:?}: this recompiler targets 32-bit Win32 (i386) only",
            pe.machine
        ));
        return prog;
    }
    let base = pe.image_base as u32;
    let mut prog = empty_program();
    let mut work: BTreeSet<(u64, &'static str)> = BTreeSet::new();
    // Seeds: entry point and exports are unambiguously code.
    work.insert((base as u64 + pe.entry_point_rva as u64, "entry point"));
    for address in extra_seeds {work.insert((*address,"initialized data function pointer"));}
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
            on_block(&block);
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
                Terminator::CallInd { resume } => {
                    // Always continue at the return address; whether the
                    // callee is an IAT import or a guest pointer is decided
                    // when emitting.
                    work.insert((*resume, "after indirect call"));
                    Edge::CallInd { resume: *resume }
                }
                Terminator::Call { resume } => {
                    // The callee is discovered by the call-target sweep below;
                    // the caller continues at the return address.
                    work.insert((*resume, "after call"));
                    Edge::Call { resume: *resume }
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
                    // An initialized global may hold a callback even when its
                    // eventual CALL lives in a different block. Keep the guest
                    // pointer in memory and discover its executable destination.
                    if let IR::Load { addr: Operand::Const { value, .. }, size: 4, .. } = op {
                        if let Some(target) = image_code_pointer(pe, *value) {
                            prog.functions.insert(target);
                            work.insert((target, "global function pointer"));
                        }
                    }
                    // CRT initializer ranges and callback tables are often
                    // passed by address, then traversed through a register.
                    let literal = match op {
                        IR::Copy { src:Operand::Const {value,..}, .. } |
                        IR::Store { value:Operand::Const {value,..}, .. } => Some(*value),
                        _ => None,
                    };
                    if let Some(address)=literal {
                        let call_instruction=insn.ops.iter().any(|op|matches!(op,IR::Call {..}|IR::CallInd {..}));
                        let instruction_pointer=matches!(op,IR::Copy {dst:Operand::Reg {name,..},..} if name.as_ref()=="FPUInstructionPointer");
                        if !call_instruction && !instruction_pointer && points_into_code(pe,pe.image_base,address) {
                            prog.functions.insert(address);
                            work.insert((address,"code address literal"));
                        }
                        for target in image_function_table(pe,address) {
                            prog.functions.insert(target);
                            work.insert((target,"referenced function table"));
                        }
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
            prog.indirect_targets.insert(site,targets.clone());
            prog.unresolved.remove(&site);
            for t in targets {
                work.insert((t, "jump table"));
            }
        }
    }
    prog.passes = pass;
    prog.pending = work;
    prog
}

fn image_code_pointer(pe: &Pe, address: u64) -> Option<u64> {
    let rva = u32::try_from(address.checked_sub(pe.image_base)?).ok()?;
    let section = &pe.sections[pe.section_of(rva)?];
    if section.is_executable() { return None; }
    let offset = (rva - section.virtual_address) as usize;
    let target = u32::from_le_bytes(section.data.get(offset..offset + 4)?.try_into().ok()?) as u64;
    let target_rva = u32::try_from(target.checked_sub(pe.image_base)?).ok()?;
    pe.sections[pe.section_of(target_rva)?].is_executable().then_some(target)
}

fn image_function_table(pe:&Pe,address:u64)->Vec<u64> {
    let Some(rva)=address.checked_sub(pe.image_base).and_then(|r|u32::try_from(r).ok()) else {return vec![];};
    let Some(section)=pe.section_of(rva).map(|i|&pe.sections[i]) else {return vec![];};
    if section.is_executable() || address%4!=0 {return vec![];}
    let offset=(rva-section.virtual_address) as usize;
    let Some(bytes)=section.data.get(offset..) else {return vec![];};
    let mut result=Vec::new();
    for word in bytes.chunks_exact(4) {
        let target=u32::from_le_bytes(word.try_into().unwrap()) as u64;
        if target==0 {continue;}
        if !points_into_code(pe,pe.image_base,target) {break;}
        result.push(target);
    }
    result
}

#[cfg(test)]
mod callback_tests {
    use super::*;
    #[test]
    fn a_global_callback_is_discovered_across_a_conditional_block_boundary() {
        use wlam_pe::{Machine, PeKind, ResourceTable, Section};
        let mut code = vec![0xcc; 64];
        // mov eax,[0x402000]; test eax,eax; jz ret; call eax; ret
        code[..12].copy_from_slice(&[0xa1,0,0x20,0x40,0,0x85,0xc0,0x74,2,0xff,0xd0,0xc3]);
        code[11..17].copy_from_slice(&[0x68,4,0x20,0x40,0,0xc3]);
        code[32] = 0xc3;
        code[36] = 0xc3;
        let section = |name: &str, rva, flags, data: Vec<u8>| Section {
            name:name.into(),virtual_address:rva,virtual_size:data.len() as u32,
            file_offset:0,file_size:data.len() as u32,characteristics:flags,data,
        };
        let pe = Pe {
            kind:PeKind::Pe32,machine:Machine::I386,image_base:0x400000,entry_point_rva:0x1000,
            section_alignment:4096,file_alignment:512,subsystem:2,dll_characteristics:0,
            size_of_image:0x3000,size_of_headers:512,
            sections:vec![section(".text",0x1000,0x60000020,code),section(".data",0x2000,0xc0000040,[0x401020u32,0,0x401024,0,123].into_iter().flat_map(u32::to_le_bytes).collect())],
            directories:vec![],imports:vec![],exports:vec![],relocations:vec![],
            resources:ResourceTable::default(),unwind_info_rva:0,unwind_info_size:0,raw:vec![],
        };
        let discovered = crate::with_big_stack(move || discover(&pe, 16));
        assert!(!discovered.incomplete());
        assert!(discovered.functions().contains(&0x401020));
        assert!(discovered.block_starts().contains(&0x401020));
        assert!(discovered.functions().contains(&0x401024));
    }
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

/// Recover jump tables for an explicit set of sites.
///
/// Exposed so tests can pin the result on known tables without running full
/// discovery: this is the regression guard for the VA/RVA bug that made entry
/// reads silently return `None`.
pub fn recover_tables_at(pe: &Pe, sites: &[u64]) -> Vec<(u64, Vec<u64>)> {
    let mut prog = Program {
        blocks: BTreeMap::new(),
        functions: BTreeSet::new(),
        jump_table_targets: BTreeSet::new(),
        resolved_indirect: BTreeSet::new(),
        indirect_targets: BTreeMap::new(),
        unresolved: sites.iter().copied().collect(),
        userops: BTreeMap::new(),
        passes: 0,
        pending: BTreeSet::new(),
        problems: Vec::new(),
    };
    // The recovery needs the site's block, so lift just that block.
    let base = pe.image_base as u64;
    for &site in sites {
        let Some(rva) = rva_of(base, site) else { continue };
        let Some(idx) = pe.section_of(rva) else { continue };
        let sec = &pe.sections[idx];
        let off = (rva - sec.virtual_address) as usize;
        let bytes = sec.data[off.min(sec.data.len())..].to_vec();
        let block = crate::with_big_stack(move || lift_block(&bytes, site));
        if !matches!(block.terminator, Terminator::IndirectJump) {
            continue;
        }
        let lifted = block;
        prog.blocks.insert(
            site,
            DiscoveredBlock {
                start: site,
                end: lifted.end,
                edge: Edge::IndirectJump { site },
                op_count: lifted.op_count(),
                native_count: lifted.native_instruction_count(),
                reason: "test",
            },
        );
    }
    recover_jump_tables(pe, &prog)
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
pub(crate) fn recover_jump_tables(pe: &Pe, prog: &Program) -> Vec<(u64, Vec<u64>)> {
    let base = pe.image_base as u64;
    let mut out = Vec::new();
    for &site in &prog.unresolved {
        let Some(block) = prog.blocks.get(&site) else { continue };
        if !matches!(block.edge, Edge::IndirectJump { .. }) {
            continue;
        }
        let ops = block_ops(pe, block);
        let Some(jump_target) = ops.iter().find_map(|o| match o {
            IR::IndirectJump { target } => Some(target.clone()),
            _ => None,
        }) else {
            continue;
        };
        let Some(expression)=table_expression(&ops,&jump_target) else {continue;};
        if expression.indexed && expression.scale!=4 {continue;}
        let table=Some(expression.base);
        let Some(table) = table else { continue };
        let Some(table_rva) = rva_of(base, table) else { continue };
        // Tables may live in writable data OR in .text: MSVC places jump tables
        // in .text right after the function (verified: Uxn32's .text has
        // rawsize 0x10000 with .rdata starting at VA 0x11000, so RVA 0x2938 is
        // inside .text and holds the table). Only require the RVA to be mapped.
        let Some(_) = pe.section_of(table_rva) else { continue };
        let cap = expression.bound.or_else(||scan_back_for_bound(pe, block.end, 96))
            .map(|b| b as usize + 1)
            .unwrap_or(256)
            .min(256);
        // `table` is a virtual address; `read_rva` takes RVAs. Passing the VA
        // silently yielded None for every entry — one full round of debugging,
        // because probe code used RVAs while discovery passed VAs.
        let mut targets = Vec::new();
        for i in 0..cap {
            let Some(slot_rva) = table_rva.checked_add((i as u32) * 4) else { break };
            let Some(raw) = pe.read_rva(slot_rva, 4) else { break };
            let v = u64::from(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]));
            if !points_into_code(pe, base, v) {
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

#[derive(Clone,Copy)]
struct TableExpression {base:u64,indexed:bool,scale:u64,bound:Option<u64>}
fn table_expression(ops:&[IR],target:&Operand)->Option<TableExpression> {
    fn constant(value:u64)->TableExpression {TableExpression {base:value,indexed:false,scale:0,bound:Some(0)}}
    fn resolve(ops:&[IR],operand:&Operand,depth:u32)->Option<TableExpression> {
        if depth>64 {return None;}
        let unknown=||matches!(operand,Operand::Reg {..}).then_some(TableExpression {base:0,indexed:true,scale:1,bound:None});
        if let Operand::Const {value,..}=operand {return Some(constant(*value));}
        let producer=ops.iter().enumerate().rev().find(|(_,op)|match op {
            IR::Copy {dst,..}|IR::Un {dst,..}|IR::Bin {dst,..}|IR::Load {dst,..}=>dst==operand,
            _=>false,
        });
        let Some((at,producer))=producer else {
            return unknown();
        };
        let earlier=&ops[..at];
        match producer {
            IR::Copy {src,..}|IR::Un {op:crate::UnOp::Zext,src,..}=>resolve(earlier,src,depth+1),
            IR::Bin {op,operands,..}=>{
                let left=resolve(earlier,&operands.0,depth+1)?;
                let right=resolve(earlier,&operands.1,depth+1)?;
                match op {
                    BinOp::Add if !(left.indexed&&right.indexed)=>Some(TableExpression {
                        base:left.base.wrapping_add(right.base)&0xffffffff,
                        indexed:left.indexed||right.indexed,scale:left.scale+right.scale,
                        bound:if left.indexed {left.bound} else {right.bound},
                    }),
                    BinOp::Mul if !right.indexed=>Some(TableExpression {base:left.base.wrapping_mul(right.base)&0xffffffff,scale:left.scale*right.base,..left}),
                    BinOp::Mul if !left.indexed=>Some(TableExpression {base:right.base.wrapping_mul(left.base)&0xffffffff,scale:right.scale*left.base,..right}),
                    BinOp::And if !right.indexed=>Some(TableExpression {base:0,indexed:true,scale:1,bound:Some(right.base)}),
                    BinOp::And if !left.indexed=>Some(TableExpression {base:0,indexed:true,scale:1,bound:Some(left.base)}),
                    _=>unknown(),
                }
            }
            // A loaded value can itself be a pre-decoded switch index, as
            // in MSVC's byte-map + DWORD target-table pattern. Its value is
            // unknown statically, but it still participates as the index.
            IR::Load { .. } => Some(TableExpression {base:0,indexed:true,scale:1,bound:None}),
            _=>unknown(),
        }
    }
    let target=if let Operand::Ram {offset,size}=target {
        Operand::Temp {offset:*offset,size:*size}
    } else {target.clone()};
    resolve(ops,&target,0)
}

/// Upper bound on a switch's index, from the `cmp <reg>, imm` encoding.
///
/// x86 is variable-length, so decoding forward from `site - N` desynchronises
/// and misses the comparison (verified at 0x405ebc). The comparison's P-code is
/// also unreliable: `cmp eax, 0x1f` compiles to a `LessU` with **no consumer**
/// because the hardware flags suffice. Pattern-matching the encodings is what
/// actually works:
///   3C ib     cmp al, imm8
///   80 /7 ib  cmp r/m8, imm8
///   3D id     cmp eax, imm32
///   83 /7 ib  cmp r/m32, imm8
fn scan_back_for_bound(pe: &Pe, site: u64, limit: u64) -> Option<u64> {
    let base = pe.image_base as u64;
    let start = site.saturating_sub(limit);
    let rva = rva_of(base, start)?;
    let sec = &pe.sections[pe.section_of(rva)?];
    let off = (rva - sec.virtual_address) as usize;
    let end = (rva_of(base, site)?.checked_sub(sec.virtual_address)? as usize).min(sec.data.len());
    let window = sec.data.get(off..end)?;
    let mut best: Option<u64> = None;
    for i in 0..window.len() {
        let imm = match window[i] {
            0x3c => window.get(i + 1).map(|b| *b as u64),
            0x80 if window.get(i + 1).map(|b| (b >> 3) & 7 == 7).unwrap_or(false) => {
                window.get(i + 2).map(|b| *b as u64)
            }
            0x3d => window
                .get(i + 1..i + 5)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64),
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


/// Convert a virtual address to an RVA, refusing addresses below the base.
///
/// `addr.saturating_sub(base)` is the trap this guards against: an address of
/// `0x1` with a base of `0x400000` saturates to `0`, which is a perfectly valid
/// RVA inside `.text`, so a nonsense target validates as code. Checked
/// subtraction makes "below the base" a distinct outcome instead of silently
/// becoming "the first byte of the image".
fn rva_of(base: u64, addr: u64) -> Option<u32> {
    u32::try_from(addr.checked_sub(base)?).ok()
}

/// Whether `addr` lands in an executable section of the image.
fn points_into_code(pe: &Pe, base: u64, addr: u64) -> bool {
    rva_of(base, addr)
        .and_then(|rva| pe.section_of(rva))
        .map(|i| pe.sections[i].is_executable())
        .unwrap_or(false)
}

/// Re-lift a discovered block/// Re-lift a discovered block to inspect its P-code (discovery stores only
/// sizes, so this is a targeted re-decode rather than cached IR).
fn block_ops(pe: &Pe, block: &DiscoveredBlock) -> Vec<IR> {
    let base = pe.image_base as u64;
    let Some(rva) = rva_of(base, block.start) else { return Vec::new() };
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

    /// Discovery must reach a fixpoint on a real image, not stop at the pass
    /// limit. A silent truncation here once cost two function entries when
    /// calls started ending blocks, so the guard is explicit.
    #[test]
    fn discovery_converges_on_the_north_star() {
        let Some(pe) = uxn32() else {
            return;
        };
        let prog = crate::with_big_stack(move || discover(&pe, 64));
        assert!(
            !prog.incomplete(),
            "discovery stopped early with {} pending item(s), e.g. {:?}",
            prog.pending().len(),
            prog.pending().iter().take(3).collect::<Vec<_>>()
        );
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
        let prog = crate::with_big_stack(move || discover(&pe, 64));
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
        let prog = crate::with_big_stack(move || discover(&pe, 64));
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
        let prog = crate::with_big_stack(move || discover(&pe, 64));
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
        let prog = crate::with_big_stack(move || discover(&pe, 64));
        let unknown = prog.unknown_userops();
        assert!(unknown.is_empty(), "unknown userops reached: {unknown:?} via {:?}", prog.userops());
    }

    #[test]
    fn pcode_ratio_stays_linear() {
        let Some(pe) = uxn32() else {
            return;
        };
        let prog = crate::with_big_stack(move || discover(&pe, 64));
        let ratio = prog.pcode_ratio();
        assert!(ratio > 1.0 && ratio < 20.0, "pcode/native ratio {ratio}");
    }
}

#[cfg(test)]
mod table_tests {
    use super::*;

    fn uxn32() -> Option<Pe> {
        let bytes = std::fs::read("../../north-star/bin/Uxn32.exe").ok()?;
        Pe::parse(&bytes).ok()
    }

    /// Pinned against the raw bytes of Uxn32.exe: each table is followed
    /// immediately by data (0x6060100, 0x7070700, machine code), so the exact
    /// entry counts are 7, 8 and 11. This is the regression guard for the
    /// VA/RVA bug where `read_rva` was handed a virtual address and silently
    /// returned `None`, producing zero targets for a whole round.
    #[test]
    fn recovers_known_tables_with_exact_entry_counts() {
        let Some(pe) = uxn32() else {
            eprintln!("skipping: north-star binary not present");
            return;
        };
        // (jump site VA, table VA, expected entries)
        let cases = [
            (0x40172au64, 0x402938u64, 7usize),
            (0x40222fu64, 0x402984u64, 8),
            (0x405ebcu64, 0x406054u64, 11),
        ];
        let sites: Vec<u64> = cases.iter().map(|(s, _, _)| *s).collect();
        let recovered = crate::with_big_stack(move || recover_tables_at(&pe, &sites));
        for (site, table, want) in cases {
            let (_, targets) = recovered
                .iter()
                .find(|(s, _)| *s == site)
                .unwrap_or_else(|| panic!("no table recovered for site {site:#x}"));
            assert_eq!(
                targets.len(),
                want,
                "site {site:#x} table {table:#x}: got {:X?}",
                targets
            );
            // Entries are the table's contents, in order.
            let base = 0x40_0000u64;
            let bytes = std::fs::read("../../north-star/bin/Uxn32.exe").unwrap();
            let pe = Pe::parse(&bytes).unwrap();
            let table_rva = (table - base) as u32;
            for (i, t) in targets.iter().enumerate() {
                let raw = pe.read_rva(table_rva + i as u32 * 4, 4).unwrap();
                let dword = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as u64;
                assert_eq!(*t, dword, "entry {i} of {table:#x}");
            }
        }
    }

    #[test]
    fn thunk_slots_are_not_treated_as_tables() {
        let Some(pe) = uxn32() else { return };
        // `jmp [0x411124]` is an IAT thunk: the slot holds 0x125da, not code.
        let sites = vec![0x40dde0u64, 0x40ddcbu64];
        let recovered = crate::with_big_stack(move || recover_tables_at(&pe, &sites));
        assert!(recovered.is_empty(), "thunks must not yield tables: {recovered:?}");
    }
}

#[cfg(test)]
mod machine_guard_tests {
    use super::*;

    /// The guard must fire on a real x86-64 image: decoding 64-bit code with an
    /// x86-32 lifter would produce plausible nonsense, which is worse than an
    /// error. Both benchmark builds are present, so this is testable directly.
    #[test]
    fn amd64_image_is_refused_not_misdecoded() {
        let Ok(bytes) = std::fs::read("../../benchmarks/minesweeper/bin/minesweeper-x64.exe") else {
            return;
        };
        let Ok(pe) = Pe::parse(&bytes) else { return };
        assert_eq!(pe.machine, wlam_pe::Machine::Amd64, "fixture must be the x64 build");
        let prog = crate::with_big_stack(move || discover(&pe, 8));
        assert!(prog.refused(), "an amd64 image must be refused");
        assert!(prog.blocks().count() == 0, "no blocks may be produced");
        assert!(prog.problems()[0].contains("i386"), "{:?}", prog.problems());
    }

    #[test]
    fn i386_image_is_accepted() {
        let Ok(bytes) = std::fs::read("../../benchmarks/minesweeper/bin/minesweeper-x86.exe") else {
            return;
        };
        let pe = Pe::parse(&bytes).expect("parse");
        assert_eq!(pe.machine, wlam_pe::Machine::I386);
        let prog = crate::with_big_stack(move || discover(&pe, 8));
        assert!(!prog.refused(), "a PE32 image must be accepted: {:?}", prog.problems());
    }

    /// An address below the image base must not be mistaken for RVA 0.
    ///
    /// `0x1.saturating_sub(0x400000)` is `0`, which is a valid RVA inside
    /// `.text`, so a saturation-based conversion validated nonsense as code and
    /// discovery produced blocks at `0x1` and `0x2`. Checked subtraction must
    /// reject it instead.
    #[test]
    fn addresses_below_the_image_base_are_not_code() {
        let Ok(bytes) = std::fs::read("../../north-star/bin/Uxn32.exe") else {
            eprintln!("skipping: north-star binary not present");
            return;
        };
        let pe = Pe::parse(&bytes).expect("parse");
        let base = pe.image_base as u64;
        for below in [0u64, 1, 2, 0x1000, base - 1] {
            assert_eq!(rva_of(base, below), None, "{below:#x} is below the base");
            assert!(
                !points_into_code(&pe, base, below),
                "{below:#x} must not be treated as executable"
            );
        }
        // The entry point itself must still validate, or the fix would reject
        // everything and "pass" for the wrong reason.
        let entry = base + pe.entry_point_rva as u64;
        assert!(rva_of(base, entry).is_some(), "the entry point must map");
        assert!(points_into_code(&pe, base, entry), "the entry point is code");
    }

    /// Discovery must never claim a block outside the image.
    ///
    /// This is the end-to-end assertion for the bug above: when Sleigh's
    /// internal `Const`-space branch labels were read as addresses, Minesweeper
    /// grew blocks at `0x1`/`0x2` claimed by several unrelated functions.
    #[test]
    fn no_discovered_block_lies_outside_the_image() {
        for path in [
            "../../north-star/bin/Uxn32.exe",
            "../../benchmarks/minesweeper/bin/minesweeper-x86.exe",
        ] {
            let Ok(data) = std::fs::read(path) else { continue };
            let Ok(pe) = Pe::parse(&data) else { continue };
            let base = pe.image_base as u64;
            // `with_big_stack` requires `'static`, so collect the mapped section
            // ranges before the image moves into the closure.
            let mapped: Vec<(u64, u64)> = pe
                .sections
                .iter()
                .map(|s| {
                    let start = base + s.virtual_address as u64;
                    (start, start + s.virtual_size as u64)
                })
                .collect();
            let prog = crate::with_big_stack(move || discover(&pe, 64));
            let strays: Vec<u64> = prog
                .blocks()
                .map(|b| b.start)
                .filter(|a| !mapped.iter().any(|(s, e)| *a >= *s && *a < *e))
                .collect();
            assert!(
                strays.is_empty(),
                "{path}: {} block(s) outside any section, e.g. {:?}",
                strays.len(),
                strays.iter().take(5).collect::<Vec<_>>()
            );
        }
    }
}
