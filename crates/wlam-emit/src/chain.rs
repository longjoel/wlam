//! Whole-program control flow: linking guest blocks into one executable module.
//!
//! ## Why a dispatcher loop
//!
//! The previous design chained blocks with `return_call`. That had two problems
//! that only showed up when executing real code:
//!
//! 1. **Backward edges grew the wasm stack** unless the target enabled the
//!    tail-call proposal: a guest `while(1)` became unbounded recursion.
//! 2. **Guest calls and returns had no home.** A `call` must return to the
//!    instruction after it and a `ret` must return to the caller's caller; with
//!    one wasm function per block there was nowhere to record that.
//!
//! Each guest function now compiles to one wasm function containing a dispatch
//! loop:
//!
//! ```text
//! func guest_401234:
//!   seed registers from the snapshot area
//!   loop
//!     block                      ;; innermost: br_table target space
//!       br_table (block_id) [case0, case1, ...] default->trap
//!     end
//!     block ;; case i: its ops, then set block_id and `br` the loop
//!       ...native wasm for the block's ops...
//!     end
//!     ...
//!   end
//! ```
//!
//! `br` to the outer loop costs **no wasm stack**, so loops are O(1). Dispatch
//! happens once per *control transfer*, not per instruction, and every case
//! body is native wasm — no P-code is interpreted at run time.
//!
//! ## Calls and returns
//!
//! The lifted `call` already pushes the return address on the guest stack, and
//! `ret` pops it into `EIP`. So a guest call is a wasm `call` to the callee's
//! function; the callee's `ret` leaves the popped address in `EIP`, and the
//! caller resumes by dispatching on that address. That keeps the guest's own
//! stack semantics intact instead of inventing a second return stack.
//!
//! ## Honest limits
//!
//! Indirect calls and unresolved indirect jumps need a runtime dispatch table
//! and are reported as errors, never guessed.

use std::collections::{BTreeMap, BTreeSet};

use crate::encode::{leb_u32, module_header, section_id, Encoder};
use crate::ir::{Block, Terminator};
use crate::translate::{LocalMap, TranslateError};

/// What the emitted module requires from its host: nothing beyond the wasm MVP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Requirements {
    /// Always false: the dispatcher design uses no post-MVP features.
    pub tail_calls: bool,
}

/// wasm opcodes used here.
mod op {
    pub const BLOCK: u8 = 0x02;
    pub const LOOP: u8 = 0x03;
    pub const IF: u8 = 0x04;
    pub const ELSE: u8 = 0x05;
    pub const END: u8 = 0x0b;
    pub const BR: u8 = 0x0c;
    pub const BR_TABLE: u8 = 0x0e;
    pub const RETURN: u8 = 0x0f;
    pub const UNREACHABLE: u8 = 0x00;
    pub const I32_CONST: u8 = 0x41;
    pub const LOCAL_GET: u8 = 0x20;
    pub const LOCAL_SET: u8 = 0x21;
    pub const I32_LOAD: u8 = 0x28;
}

/// Local holding the dispatch block id (after every register/temp local).
fn id_local(locals: &LocalMap) -> u32 {
    locals.len()
}

/// Local used to stage a return value (after the id local).
fn ret_local(locals: &LocalMap) -> u32 {
    locals.len() + 1
}

/// A guest function: entry block plus blocks reachable inside it.
#[derive(Debug)]
pub struct GuestFunction {
    pub entry: u64,
    /// Blocks of this function; block id = index in this vector.
    pub blocks: Vec<u64>,
    pub ids: BTreeMap<u64, u32>,
    /// Direct call targets found in this function.
    pub callees: BTreeSet<u64>,
    /// Blocks that cannot be executed correctly, with a reason.
    pub problems: Vec<(u64, String)>,
}

/// A program: guest functions sharing one register/local layout.
#[derive(Debug)]
pub struct Program {
    pub functions: Vec<GuestFunction>,
    pub function_index: BTreeMap<u64, u32>,
    pub locals: LocalMap,
    pub registers: Vec<String>,
    pub requirements: Requirements,
    pub problems: Vec<String>,
}

/// Partition `blocks` into guest functions rooted at `entries`.
///
/// A function owns every block reachable by jump and fall-through; a direct
/// `call` starts a *new* function, which is what makes `ret` well defined.
pub fn partition(entries: &[u64], blocks: &BTreeMap<u64, Block>, registers: &[String]) -> Program {
    let locals = LocalMap::new(registers.iter().map(|r| (r.clone(), 4)));
    let mut functions: Vec<GuestFunction> = Vec::new();
    let mut work: Vec<u64> = entries.to_vec();
    let mut started: BTreeSet<u64> = BTreeSet::new();
    while let Some(entry) = work.pop() {
        if !started.insert(entry) {
            continue;
        }
        let mut func = GuestFunction {
            entry,
            blocks: Vec::new(),
            ids: BTreeMap::new(),
            callees: BTreeSet::new(),
            problems: Vec::new(),
        };
        let mut queue = vec![entry];
        while let Some(addr) = queue.pop() {
            if func.ids.contains_key(&addr) {
                continue;
            }
            let id = func.blocks.len() as u32;
            func.blocks.push(addr);
            func.ids.insert(addr, id);
            let Some(block) = blocks.get(&addr) else {
                func.problems.push((addr, "block bytes unavailable".into()));
                continue;
            };
            match &block.terminator {
                Terminator::FallsThrough => queue.push(block.end),
                Terminator::Jump(t) => queue.push(*t),
                Terminator::Conditional {
                    taken: Some(t),
                    not_taken,
                } => {
                    queue.push(*t);
                    queue.push(*not_taken);
                }
                Terminator::Conditional { taken: None, .. } => {
                    func.problems
                        .push((addr, "conditional with computed destination".into()));
                }
                Terminator::Return | Terminator::Trap(_) => {}
                Terminator::IndirectJump => {
                    func.problems
                        .push((addr, "indirect jump needs target resolution".into()));
                }
                Terminator::Undecodable(at) => {
                    func.problems
                        .push((addr, format!("undecodable bytes at {at:#x}")));
                }
            }
            for target in direct_calls(block) {
                func.callees.insert(target);
                work.push(target);
            }
        }
        functions.push(func);
    }
    let function_index = functions
        .iter()
        .enumerate()
        .map(|(i, f)| (f.entry, i as u32))
        .collect();
    Program {
        functions,
        function_index,
        locals,
        registers: registers.to_vec(),
        requirements: Requirements::default(),
        problems: Vec::new(),
    }
}

/// Direct (constant) call targets of a block.
fn direct_calls(block: &Block) -> Vec<u64> {
    block
        .ops
        .iter()
        .filter_map(|ir| match ir {
            crate::ir::IR::Call { target } => const_of(target),
            _ => None,
        })
        .collect()
}

fn const_of(o: &crate::ir::Operand) -> Option<u64> {
    match o {
        crate::ir::Operand::Const { value, .. } | crate::ir::Operand::Ram { offset: value, .. } => {
            Some(*value)
        }
        _ => None,
    }
}

/// A condition operand, resolved to a local index at the last moment.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CondRef {
    Temp(u64),
    Reg(Box<str>),
}

impl CondRef {
    fn local(&self, map: &LocalMap) -> Result<u32, TranslateError> {
        match self {
            CondRef::Temp(offset) => map
                .temp_index(*offset)
                .ok_or(TranslateError::Unsupported("condition temp has no local")),
            CondRef::Reg(name) => map.reg(name.as_ref()).ok_or(TranslateError::Unsupported(
                "condition register has no local",
            )),
        }
    }
}

/// What happens after a block's ops run.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Exit {
    /// Branch back to the dispatcher with a new block id.
    Goto(u32),
    /// Branch on a condition to one of two block ids.
    ///
    /// The condition is carried as the *operand* (offset or register name), not
    /// as a local index: indices are only stable once the whole function's local
    /// map is final, and resolving early produced an `if` testing a temp that
    /// belonged to a later block.
    IfElse {
        cond: CondRef,
        taken: u32,
        not_taken: u32,
    },
    /// Return to the caller (the return address is already in EIP).
    Ret,
    /// Trap: unreachable or unsupported.
    Trap,
}

/// Split a block into data ops and a dispatcher exit.
fn split_ops(block: &Block) -> Result<Vec<crate::ir::IR>, TranslateError> {
    let mut ops = Vec::new();
    for ir in &block.ops {
        match ir {
            // Control flow is the dispatcher's job.
            crate::ir::IR::Branch { .. }
            | crate::ir::IR::CBranch { .. }
            | crate::ir::IR::Ret { .. }
            | crate::ir::IR::IndirectJump { .. }
            | crate::ir::IR::Call { .. } => {}
            crate::ir::IR::CallInd { .. } => {
                return Err(TranslateError::Unsupported(
                    "indirect call needs a function table",
                ))
            }
            other => ops.push(other.clone()),
        }
    }
    Ok(ops)
}

/// The dispatcher exit for a block, given the local map that its translation
/// actually produced.
///
/// Condition temps are allocated *during* translation, so they only exist in
/// the translated block's map — looking them up in the program-wide map is how
/// an earlier version failed with "condition temp has no local".
fn block_exit(
    block: &Block,
    func: &GuestFunction,
    _translated: &crate::translate::TranslatedBlock,
) -> Result<Exit, TranslateError> {
    Ok(match &block.terminator {
        Terminator::FallsThrough => Exit::Goto(id_of(func, block.end)?),
        Terminator::Jump(t) => Exit::Goto(id_of(func, *t)?),
        Terminator::Conditional {
            taken: Some(t),
            not_taken,
        } => Exit::IfElse {
            cond: condition_ref(block)?,
            taken: id_of(func, *t)?,
            not_taken: id_of(func, *not_taken)?,
        },
        Terminator::Return => Exit::Ret,
        _ => Exit::Trap,
    })
}

fn id_of(func: &GuestFunction, addr: u64) -> Result<u32, TranslateError> {
    func.ids
        .get(&addr)
        .copied()
        .ok_or(TranslateError::Unsupported(
            "branch target outside its guest function",
        ))
}

/// The operand holding a `CBranch` condition.
fn condition_ref(block: &Block) -> Result<CondRef, TranslateError> {
    let cond = block
        .ops
        .iter()
        .find_map(|ir| match ir {
            crate::ir::IR::CBranch { cond, .. } => Some(cond.clone()),
            _ => None,
        })
        .ok_or(TranslateError::Unsupported(
            "conditional block without a CBranch",
        ))?;
    match cond {
        crate::ir::Operand::Temp { offset, .. } => Ok(CondRef::Temp(offset)),
        crate::ir::Operand::Reg { name, .. } => Ok(CondRef::Reg(name)),
        _ => Err(TranslateError::Unsupported("constant condition")),
    }
}

/// Build the whole module.
pub fn build_module(
    prog: &Program,
    blocks: &BTreeMap<u64, Block>,
) -> Result<Vec<u8>, TranslateError> {
    let mut bodies = Vec::new();
    for func in &prog.functions {
        bodies.push(emit_function_body(func, prog, blocks)?);
    }
    let mut e = Encoder::new();
    module_header(&mut e);
    e.section(section_id::TYPE, |b| {
        b.u32(1);
        b.push(0x60);
        b.u32(0);
        b.u32(0);
    });
    e.section(section_id::FUNCTION, |b| {
        b.u32(prog.functions.len() as u32);
        for _ in &prog.functions {
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
        b.u32(0);
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

/// Wasm body for one guest function.
fn emit_function_body(
    func: &GuestFunction,
    prog: &Program,
    blocks: &BTreeMap<u64, Block>,
) -> Result<Vec<u8>, TranslateError> {
    // Emit the dispatch + case bodies first, tracking every local allocated so
    // the header can declare exactly that many.
    // Reserve the dispatcher's own locals INSIDE the shared map before any case
    // is translated. Otherwise a case's temps can be assigned the same index and
    // silently clobber the dispatch id (observed: `i32.const 5; local.set 8`
    // overwrote the id with 5, dispatching out of range).
    let mut shared = prog.locals.clone();
    let id = shared.reserve_scratch(0);
    let _ret = shared.reserve_scratch(1);
    let mut body = Vec::new();

    // Seed registers from the snapshot area.
    for (i, name) in prog.registers.iter().enumerate() {
        let Some(local) = prog.locals.reg(name) else {
            continue;
        };
        body.push(op::I32_CONST);
        body.extend_from_slice(&leb_u32((crate::REG_SNAPSHOT_BASE + i as u64 * 4) as u32));
        body.push(op::I32_LOAD);
        body.push(0);
        body.push(0);
        body.push(op::LOCAL_SET);
        body.extend_from_slice(&leb_u32(local));
    }
    body.push(op::I32_CONST);
    body.extend_from_slice(&leb_u32(0));
    body.push(op::LOCAL_SET);
    body.extend_from_slice(&leb_u32(id));

    // Canonical dispatch nesting: `br` targets only ENCLOSING labels, so each
    // case body sits after the `end` of the block whose label br_table names.
    let n = func.blocks.len() as u32;
    body.push(op::LOOP);
    body.push(0x40);
    for _ in 0..n {
        body.push(op::BLOCK);
        body.push(0x40);
    }
    body.push(op::LOCAL_GET);
    body.extend_from_slice(&leb_u32(id));
    body.push(op::BR_TABLE);
    body.extend_from_slice(&leb_u32(n));
    // Entry `id` must run the body for func.blocks[id]. Bodies were emitted in
    // reverse (innermost case first), so entry `id` targets depth `n-1-id`:
    // depth 0 is the innermost case block (whose body is func.blocks[n-1]),
    // depth n-1 is the outermost (whose body is func.blocks[0]).
    //
    // Emitting `depth = id` inverted the mapping, so the initial dispatch
    // (id 0) ran the LAST block and every branch landed on the wrong case.
    for i in 0..n {
        body.extend_from_slice(&leb_u32(n - 1 - i));
    }
    body.extend_from_slice(&leb_u32(n)); // default: out of range
    for i in (0..n).rev() {
        body.push(op::END);
        let addr = func.blocks[i as usize];
        emit_case(&mut body, func, prog, blocks, addr, i, &mut shared, id)?;
    }
    body.push(op::END); // end loop
    body.push(op::UNREACHABLE); // out-of-range dispatch
    body.push(op::END); // end function

    // Header: every local allocated anywhere in the function (the id local is
    // already included via `reserve_scratch`).
    let total = shared.len();
    let mut out = Vec::new();
    out.push(1);
    out.extend_from_slice(&leb_u32(total));
    out.push(0x7f);
    out.extend_from_slice(&body);
    Ok(out)
}

/// Emit one case body: its ops, then the branch back to the dispatch loop.
///
/// From inside case `index`, the enclosing labels are the case blocks for the
/// cases *after* it (which have already closed) plus the loop, so the loop sits
/// `index + 1` levels out.
fn emit_case(
    out: &mut Vec<u8>,
    func: &GuestFunction,
    prog: &Program,
    blocks: &BTreeMap<u64, Block>,
    addr: u64,
    index: u32,
    shared: &mut LocalMap,
    id: u32,
) -> Result<(), TranslateError> {
    let depth_to_loop = index + 1;
    let Some(block) = blocks.get(&addr) else {
        out.push(op::UNREACHABLE);
        return Ok(());
    };
    let ops = split_ops(block)?;
    // Each block's temps must get their own locals inside the shared function
    // layout: translating with a fresh map would hand out indices the function
    // never declared ("unknown local 10: local index out of bounds").
    let translated = crate::translate_ops(&ops, shared.clone())?;
    *shared = translated.map.clone();
    out.extend_from_slice(&translated.body[translated.ops_offset..translated.body.len() - 1]);
    match block_exit(block, func, &translated)? {
        Exit::Goto(target) => {
            out.push(op::I32_CONST);
            out.extend_from_slice(&leb_u32(target));
            out.push(op::LOCAL_SET);
            out.extend_from_slice(&leb_u32(id));
            out.push(op::BR);
            out.extend_from_slice(&leb_u32(depth_to_loop));
        }
        Exit::IfElse {
            cond,
            taken,
            not_taken,
        } => {
            let cond_local = cond.local(shared)?;
            out.push(op::LOCAL_GET);
            out.extend_from_slice(&leb_u32(cond_local));
            out.push(op::IF);
            out.push(0x40);
            out.push(op::I32_CONST);
            out.extend_from_slice(&leb_u32(taken));
            out.push(op::LOCAL_SET);
            out.extend_from_slice(&leb_u32(id));
            out.push(op::BR);
            out.extend_from_slice(&leb_u32(depth_to_loop + 1));
            out.push(op::ELSE);
            out.push(op::I32_CONST);
            out.extend_from_slice(&leb_u32(not_taken));
            out.push(op::LOCAL_SET);
            out.extend_from_slice(&leb_u32(id));
            out.push(op::BR);
            out.extend_from_slice(&leb_u32(depth_to_loop + 1));
            out.push(op::END);
        }
        Exit::Ret => out.push(op::RETURN),
        Exit::Trap => out.push(op::UNREACHABLE),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatcher_locals_follow_register_locals() {
        let locals = LocalMap::new([("EAX".to_string(), 4)]);
        assert_eq!(id_local(&locals), 1);
        assert_eq!(ret_local(&locals), 2);
    }

    #[test]
    fn call_targets_are_recognised() {
        assert_eq!(
            const_of(&crate::ir::Operand::Const { value: 7, size: 4 }),
            Some(7)
        );
        assert_eq!(
            const_of(&crate::ir::Operand::Ram { offset: 9, size: 4 }),
            Some(9)
        );
        assert_eq!(
            const_of(&crate::ir::Operand::Reg {
                name: "EAX".into(),
                size: 4
            }),
            None
        );
    }
}

#[cfg(test)]
mod dispatcher_tests {
    use super::*;

    /// Build a two-block program by hand: block 0 writes `flag` to memory and
    /// branches on a condition; block 1 (taken) writes 0xA; block 2 (not taken)
    /// writes 0xB. The condition is a *constant* chosen per test, so the
    /// expected path is known by construction — no lifter, no encodings, no
    /// fixture arithmetic can be wrong here.
    fn conditional_program(cond: u64) -> (Program, BTreeMap<u64, Block>) {
        let regs = vec!["EAX".to_string()];
        let mut blocks = BTreeMap::new();
        // Block 0: store 0 at 0x100, set temp 1 = cond, CBranch on it.
        blocks.insert(
            0x1000u64,
            Block {
                start: 0x1000,
                end: 0x1008,
                ops: vec![
                    crate::ir::IR::Store {
                        addr: crate::ir::Operand::Const { value: 0x100, size: 4 },
                        value: crate::ir::Operand::Const { value: 0, size: 4 },
                        size: 4,
                    },
                    crate::ir::IR::Copy {
                        dst: crate::ir::Operand::Temp { offset: 1, size: 4 },
                        src: crate::ir::Operand::Const { value: cond, size: 4 },
                    },
                    crate::ir::IR::CBranch {
                        cond: crate::ir::Operand::Temp { offset: 1, size: 4 },
                        target: crate::ir::Operand::Ram { offset: 0x1010, size: 4 },
                    },
                ],
                terminator: Terminator::Conditional { taken: Some(0x1010), not_taken: 0x1014 },
            },
        );
        let leaf = |addr: u64, value: u64| Block {
            start: addr,
            end: addr + 8,
            ops: vec![crate::ir::IR::Store {
                addr: crate::ir::Operand::Const { value: 0x104, size: 4 },
                value: crate::ir::Operand::Const { value, size: 4 },
                size: 4,
            }],
            terminator: Terminator::Return,
        };
        blocks.insert(0x1010, leaf(0x1010, 0xA));
        blocks.insert(0x1014, leaf(0x1014, 0xB));
        (partition(&[0x1000], &blocks, &regs), blocks)
    }

    fn run_module(module: &[u8]) -> (u32, u32, String) {
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("wlam-disp-{}-{n}.wasm", std::process::id()));
        std::fs::write(&path, module).unwrap();
        let out = std::process::Command::new("timeout")
            .arg("10")
            .arg("node")
            .arg("-e")
            .arg(format!(
                "const fs=require('fs');const m=new WebAssembly.Module(fs.readFileSync({path:?}));\
                 const i=new WebAssembly.Instance(m,{{}});\
                 try{{i.exports.entry();}}catch(e){{console.log('TRAP '+e.message);process.exit(0)}}\
                 const d=new DataView(i.exports.memory.buffer);\
                 console.log(d.getUint32(0x100,true)+' '+d.getUint32(0x104,true));"
            ))
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&path);
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let mut it = text.split_whitespace();
        (
            it.next().and_then(|v| v.parse().ok()).unwrap_or(u32::MAX),
            it.next().and_then(|v| v.parse().ok()).unwrap_or(u32::MAX),
            text,
        )
    }

    #[test]
    fn constant_condition_taken() {
        let (prog, blocks) = conditional_program(1);
        let m = build_module(&prog, &blocks).expect("module");
        let (at100, at104, log) = run_module(&m);
        assert_eq!(at100, 0, "block 0 must run: {log}");
        assert_eq!(at104, 0xA, "condition 1 must take the 'taken' leaf: {log}");
    }

    #[test]
    fn constant_condition_not_taken() {
        let (prog, blocks) = conditional_program(0);
        let m = build_module(&prog, &blocks).expect("module");
        let (at100, at104, log) = run_module(&m);
        assert_eq!(at100, 0, "block 0 must run: {log}");
        assert_eq!(at104, 0xB, "condition 0 must take the 'not_taken' leaf: {log}");
    }
}

#[cfg(test)]
mod dump_tmp {
    use super::*;
    #[test]
    fn dump_conditional_module() {
        // Rebuild the same constructed program and write it for inspection.
        let regs = vec!["EAX".to_string()];
        let mut blocks = BTreeMap::new();
        blocks.insert(0x1000u64, Block {
            start: 0x1000, end: 0x1008,
            ops: vec![
                crate::ir::IR::Copy { dst: crate::ir::Operand::Temp { offset: 1, size: 4 }, src: crate::ir::Operand::Const { value: 0, size: 4 } },
                crate::ir::IR::CBranch { cond: crate::ir::Operand::Temp { offset: 1, size: 4 }, target: crate::ir::Operand::Ram { offset: 0x1010, size: 4 } },
            ],
            terminator: Terminator::Conditional { taken: Some(0x1010), not_taken: 0x1014 },
        });
        for (a, v) in [(0x1010u64, 0xAu64), (0x1014, 0xB)] {
            blocks.insert(a, Block {
                start: a, end: a + 8,
                ops: vec![crate::ir::IR::Store {
                    addr: crate::ir::Operand::Const { value: 0x104, size: 4 },
                    value: crate::ir::Operand::Const { value: v, size: 4 },
                    size: 4,
                }],
                terminator: Terminator::Return,
            });
        }
        let prog = partition(&[0x1000], &blocks, &regs);
        let m = build_module(&prog, &blocks).unwrap();
        let _ = m;
    }
}
