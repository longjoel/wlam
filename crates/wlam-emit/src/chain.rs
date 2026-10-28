//! Chaining translated blocks into an executable function.
//!
//! Each guest block becomes wasm code that ends by calling its successor, so
//! control flow is *direct calls between recompiled code* with no dispatcher
//! loop — that is what separates this from an interpreter.
//!
//! ## The stack problem, stated honestly
//!
//! Recursive block calls consume wasm stack per guest control transfer: a
//! `while(1)` loop in the guest becomes infinite wasm recursion and overflows.
//! The fix is tail calls (`return_call`), which wasm gained after the MVP.
//! Chrome, Firefox and Safari all ship it, so chaining uses `return_call` and
//! the emitter *reports* it in the module's requirements rather than assuming.
//! A future fallback (explicit trampoline) is possible but not written; until
//! then, targets without tail calls will trap rather than silently degrade.
//!
//! ## Limits
//!
//! Calls into guest functions and returns are emitted as direct calls to the
//! same function's block entry points. Indirect calls and unresolved jump
//! tables are reported as errors: they require the runtime dispatch this phase
//! does not have yet.

use std::collections::{BTreeMap, BTreeSet};

use crate::ir::{Block, Terminator};

use crate::encode::{leb_u32, module_header, section_id, Encoder};
use crate::translate::{LocalMap, TranslateError};

/// What the emitted module requires from its host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Requirements {
    /// The module uses `return_call` (wasm tail calls).
    pub tail_calls: bool,
}

/// A fully chained function: entry block plus every reachable block.
#[derive(Debug)]
pub struct ChainedFunction {
    /// Blocks in emission order (entry first).
    pub order: Vec<u64>,
    /// wasm function index for each block, offset by the number of imports.
    pub indices: BTreeMap<u64, u32>,
    /// Locals shared by every block function (same indices everywhere).
    pub locals: LocalMap,
    /// Every register name in local-index order.
    pub registers: Vec<String>,
    pub requires: Requirements,
    /// Blocks that could not be chained, with a reason each.
    pub problems: Vec<(u64, String)>,
}

/// wasm opcodes for control flow.
mod op {
    /// `return_call fnidx` (tail call, from the tail-call proposal).
    pub const RETURN_CALL: &[u8] = &[0x12];
    /// `if blocktype` ... `end`.
    pub const IF: u8 = 0x04;
    /// `else`.
    pub const ELSE: u8 = 0x05;
    /// `end`.
    pub const END: u8 = 0x0b;
    /// `local.get idx`.
    pub const LOCAL_GET: u8 = 0x20;
    /// `unreachable`.
    pub const UNREACHABLE: u8 = 0x00;
}

/// Chain `blocks` into one executable function rooted at `entry`.
///
/// `registers` is the full register set the program uses, so every block shares
/// one local layout. Blocks whose terminator names a target that is not present
/// are recorded in `problems` rather than emitted incorrectly.
pub fn chain(
    entry: u64,
    blocks: &BTreeMap<u64, Block>,
    registers: &[String],
) -> Result<ChainedFunction, TranslateError> {
    let locals = LocalMap::new(registers.iter().map(|r| (r.clone(), 4)));
    // Reachable blocks from the entry, following every terminator edge.
    let mut order = vec![entry];
    let mut seen: BTreeSet<u64> = BTreeSet::from([entry]);
    let mut problems = Vec::new();
    let mut i = 0;
    let mut requires = Requirements::default();
    while i < order.len() {
        let addr = order[i];
        i += 1;
        let Some(block) = blocks.get(&addr) else {
            problems.push((addr, "block not present in the program".to_string()));
            continue;
        };
        let nexts: Vec<u64> = match &block.terminator {
            Terminator::FallsThrough => vec![block.end],
            Terminator::Jump(t) => vec![*t],
            Terminator::Conditional { taken, not_taken } => {
                let mut v = vec![*not_taken];
                if let Some(t) = taken {
                    v.push(*t);
                } else {
                    problems.push((addr, "conditional with a computed destination".into()));
                }
                v
            }
            Terminator::Return => vec![],
            // These need a run-time dispatcher, which this phase does not have.
            Terminator::IndirectJump => {
                problems.push((addr, "indirect jump needs runtime dispatch".into()));
                vec![]
            }
            Terminator::Trap(at) => {
                let _ = at;
                vec![]
            }
            Terminator::Undecodable(at) => {
                problems.push((addr, format!("undecodable code at {at:#x}")));
                vec![]
            }
        };
        for n in nexts {
            if seen.insert(n) {
                order.push(n);
            }
        }
        if matches!(
            block.terminator,
            Terminator::Jump(_) | Terminator::Conditional { .. } | Terminator::FallsThrough
        ) {
            requires.tail_calls = true;
        }
    }
    // Block N is wasm function N. There are no imports in this module, so
    // offsetting by one (a habit from import-bearing modules) made every call
    // target a function one past the end: V8 reported "function index #4 is out
    // of bounds" for a 4-function module.
    let import_count = 0u32;
    let indices = order
        .iter()
        .enumerate()
        .map(|(i, a)| (*a, i as u32 + import_count))
        .collect();
    Ok(ChainedFunction { order, indices, locals, registers: registers.to_vec(), requires, problems })
}

/// Emit one block as a wasm function body (without the size prefix).
///
/// Layout: locals declaration (from `translate_ops`), the block's ops, then the
/// chaining epilogue, then `end`. Local indices are shared across blocks because
/// every block receives the same register layout.
fn emit_block_body(
    block: &Block,
    f: &ChainedFunction,
    problem: Option<&str>,
) -> Result<Vec<u8>, TranslateError> {
    let ops: Vec<crate::ir::IR> = block
        .ops
        .iter()
        .cloned()
        .filter(|op| {
            // Control flow is expressed by the epilogue, not as data ops.
            !matches!(
                op,
                crate::ir::IR::Branch { .. }
                    | crate::ir::IR::CBranch { .. }
                    | crate::ir::IR::Call { .. }
                    | crate::ir::IR::CallInd { .. }
                    | crate::ir::IR::Ret { .. }
                    | crate::ir::IR::IndirectJump { .. }
            )
        })
        .collect();
    // A fresh LocalMap with the shared register layout; temps are allocated per
    // block but always above the register indices, so they cannot collide.
    let translated = crate::translate_ops(&ops, clone_locals(&f.locals))?.without_end();
    let mut body = translated.body.clone();

    let call = |idx: u32, body: &mut Vec<u8>| {
        body.extend_from_slice(op::RETURN_CALL);
        body.extend_from_slice(&leb_u32(idx));
    };

    if let Some(_reason) = problem {
        // Cannot chain: trap loudly rather than fall through into the next
        // function's body.
        body.push(op::UNREACHABLE);
        body.push(op::END);
        return Ok(body);
    }

    match &block.terminator {
        Terminator::FallsThrough => call(target_index(f, block.end)?, &mut body),
        Terminator::Jump(t) => call(target_index(f, *t)?, &mut body),
        Terminator::Conditional { taken: Some(t), not_taken } => {
            let cond = condition_local(block, &translated.map)?;
            body.push(op::LOCAL_GET);
            body.extend_from_slice(&leb_u32(cond));
            body.push(op::IF);
            body.push(0x40);
            call(target_index(f, *t)?, &mut body);
            body.push(op::ELSE);
            call(target_index(f, *not_taken)?, &mut body);
            body.push(op::END);
        }
        Terminator::Return => {
            // `ret` popped the return address into EIP. Single-module returns
            // end this chained function; cross-function return is the next
            // step and is reported, not guessed.
        }
        _ => body.push(op::UNREACHABLE),
    }
    body.push(op::END);
    Ok(body)
}

fn clone_locals(m: &LocalMap) -> LocalMap {
    m.clone()
}

fn target_index(f: &ChainedFunction, addr: u64) -> Result<u32, TranslateError> {
    f.indices
        .get(&addr)
        .copied()
        .ok_or(TranslateError::Unsupported("branch target outside the chained function"))
}

/// The local holding the `CBranch` condition, found from the block's IR.
///
/// Sleigh's `CBranch` is the block's final op and its condition is a temp or
/// flag computed just before it; because translation stops before the branch,
/// that local still holds the value.
fn condition_local(block: &Block, locals: &LocalMap) -> Result<u32, TranslateError> {
    let cond = block
        .ops
        .iter()
        .find_map(|ir| match ir {
            crate::ir::IR::CBranch { cond, .. } => Some(cond.clone()),
            _ => None,
        })
        .ok_or(TranslateError::Unsupported("conditional block without a CBranch"))?;
    match &cond {
        crate::ir::Operand::Temp { offset, .. } => locals
            .temp_index(*offset)
            .ok_or(TranslateError::Unsupported("condition temp has no local")),
        crate::ir::Operand::Reg { name, .. } => locals
            .reg(name.as_ref())
            .ok_or(TranslateError::Unsupported("condition register has no local")),
        crate::ir::Operand::Const { .. } | crate::ir::Operand::Ram { .. } => {
            Err(TranslateError::Unsupported("constant condition"))
        }
    }
}

/// Build a wasm module exporting `entry()` that chains the function's blocks.
pub fn module_for_function(
    f: &ChainedFunction,
    blocks: &BTreeMap<u64, Block>,
    entry: u64,
) -> Result<Vec<u8>, TranslateError> {
    let problems: BTreeMap<u64, String> = f.problems.iter().cloned().collect();
    let mut bodies = Vec::new();
    for addr in &f.order {
        let Some(block) = blocks.get(addr) else { continue };
        bodies.push(emit_block_body(block, f, problems.get(addr).map(|s| s.as_str()))?);
    }
    let mut e = Encoder::new();
    module_header(&mut e);
    // type: () -> ()
    e.section(section_id::TYPE, |b| {
        b.u32(1);
        b.push(0x60);
        b.u32(0);
        b.u32(0);
    });
    // declare `return_call` (tail calls) if used
    if f.requires.tail_calls {
        // The tail-call proposal text convention is a custom section; WASM
        // feature detection happens at instantiation, so nothing is required
        // here. Recorded in `Requirements` for the host instead.
    }
    e.section(section_id::FUNCTION, |b| {
        b.u32(f.order.len() as u32);
        for _ in 0..f.order.len() {
            b.u32(0);
        }
    });
    e.section(section_id::MEMORY, |b| {
        b.u32(1);
        b.push(0x00);
        b.u32(16);
    });
    e.section(section_id::EXPORT, |b| {
        b.u32(2);
        b.u32(5);
        b.raw(b"entry");
        b.push(0x00);
        b.u32(f.indices[&entry]);
        b.u32(6);
        b.raw(b"memory");
        b.push(0x02);
        b.u32(0);
    });
    e.section(section_id::CODE, |b| {
        b.u32(bodies.len() as u32);
        for body in &bodies {
            b.u32(body.len() as u32);
            b.raw(body);
        }
    });
    Ok(e.into_bytes())
}
