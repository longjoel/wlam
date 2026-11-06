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
    pub const CALL: u8 = 0x10;
    // Opcode table, checked against the spec: a wrong value here is rejected by
    // the validator only when the type context happens to differ, so an error
    // that survives validation shows up as misbehaviour instead.
    //   br_table 0x0e   br 0x0c   return 0x0f
    pub const BR_TABLE: u8 = 0x0e;
    pub const RETURN: u8 = 0x0f;
    pub const UNREACHABLE: u8 = 0x00;
    pub const I32_CONST: u8 = 0x41;
    pub const LOCAL_GET: u8 = 0x20;
    pub const LOCAL_SET: u8 = 0x21;
    pub const F64_LOAD: u8 = 0x2b;
    pub const F64_STORE: u8 = 0x39;
    pub const I32_LOAD: u8 = 0x28;
    pub const I32_STORE: u8 = 0x36;
    pub const I32_ADD: u8 = 0x6a;
}

/// Local holding the dispatch block id (after every register/temp local).
#[cfg(test)]
fn id_local(locals: &LocalMap) -> u32 {
    locals.len()
}

/// Local used to stage a return value (after the id local).
#[cfg(test)]
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

/// One imported Win32 API, as called through an IAT slot.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct HostImport {
    /// Namespace: the DLL name as written in the image (`KERNEL32.dll`).
    pub library: String,
    /// `Name` or `#Ordinal`.
    pub name: String,
}

/// A program: guest functions sharing one register/local layout.
#[derive(Debug)]
pub struct Program {
    pub functions: Vec<GuestFunction>,
    pub function_index: BTreeMap<u64, u32>,
    pub locals: LocalMap,
    pub registers: Vec<String>,
    /// Preferred PE image base, used to normalize RVA-valued indirect targets.
    pub image_base: u32,
    pub requirements: Requirements,
    pub problems: Vec<String>,
    /// Guest IAT slot address -> the Win32 API it holds.
    ///
    /// A `call [slot]` is a call to the host, not to guest code, so these
    /// become wasm imports rather than guest functions.
    pub host_imports: BTreeMap<u64, HostImport>,
    /// wasm function index for each distinct host import.
    pub import_index: BTreeMap<HostImport, u32>,
    /// Guest IAT slot address -> the API it holds, for every verified slot.
    ///
    /// Distinct from [`Program::host_imports`], which holds only the slots
    /// discovered code actually calls and so drives which imports the module
    /// declares. A guest may still reach a slot this translation never saw a
    /// `call` for -- notably through an indirect `call eax` such as the
    /// exception dispatcher `acos_handler` at 0x43e1f8, which loads ESTABLISHER,
    /// ESTABLISHER_FRAME and HANDLER from `EXCEPTION_REGISTRATION_RECORD` and
    /// jumps to `[ebx+0x18]`. When that shadowed IAT address lands in the
    /// function table it would call some unrelated guest function, so
    /// `Exit::CallInd` also tests these slots at run time.
    pub iat_imports: BTreeMap<u64, HostImport>,
    /// Guest function index of the program entry point.
    ///
    /// NOT necessarily 0: `partition` pops `entries` from a stack, so the last
    /// entry given is numbered first. Exporting index 0 blindly exported
    /// whatever function happened to be allocated there (a table-only callee),
    /// which ran and returned without ever dispatching.
    pub entry_function: u32,
}

impl Program {
    /// Register the Win32 imports reachable through the given IAT slots.
    ///
    /// `lookup` maps a guest IAT address to the import it holds. Only slots
    /// that are actually called are registered, so the module imports what the
    /// guest uses and nothing more.
    pub fn register_host_imports(
        &mut self,
        blocks: &BTreeMap<u64, Block>,
        mut lookup: impl FnMut(u64) -> Option<HostImport>,
    ) {
        let mut slots = BTreeSet::new();
        for block in blocks.values() {
            for op in &block.ops {
                if let crate::ir::IR::Load {
                    addr: crate::ir::Operand::Const { value, .. },
                    ..
                } = op
                {
                    slots.insert(*value);
                }
            }
            if let Some(slot) = iat_slot_of(block) {
                slots.insert(slot);
            }
        }
        for slot in slots {
            if let Some(import) = lookup(slot) {
                self.host_imports.insert(slot, import);
            }
        }
        // Populate runtime dispatch after collecting slots. Taking this snapshot
        // before registration leaves register-indirect calls with no imports.
        self.iat_imports = self.host_imports.clone();
        // x87 transcendentals: `FSINCOS` becomes host calls, so their helper
        // imports must exist whenever a block uses one. Registered with a
        // synthetic slot key above every real IAT address so they cannot
        // collide with a guest import slot.
        let mut helper_slot = u64::MAX;
        for block in blocks.values() {
            for op in &block.ops {
                let import = match op {
                    crate::ir::IR::Userop { id, .. }
                        if matches!(
                            *id,
                            crate::translate::X87_USEROP_SIN
                                | crate::translate::X87_USEROP_COS
                                | crate::translate::X87_USEROP_TAN
                                | crate::translate::X87_USEROP_ATAN2
                        ) =>
                    {
                        Some(crate::translate::x87_helper_import(*id))
                    }
                    crate::ir::IR::Userop { id, .. } => Some(crate::translate::unsupported_import(
                        &TranslateError::UnimplementedUserop(*id),
                    )),
                    crate::ir::IR::Unsupported(op) => Some(crate::translate::unsupported_import(
                        &TranslateError::UnsupportedPcode(op.clone()),
                    )),
                    crate::ir::IR::Load { size: 10, .. }
                    | crate::ir::IR::Store { size: 10, .. } => Some(
                        crate::translate::unsupported_import(&TranslateError::UnsupportedWidth(10)),
                    ),
                    _ => None,
                };
                if let Some(import) = import {
                    self.host_imports.insert(helper_slot, import);
                    helper_slot -= 1;
                }
            }
        }
        // Deterministic import indices: sorted by (library, name). The set is
        // every import the module *declares*; an IAT slot that only an
        // indirect call can reach is already among them whenever it is called
        // directly somewhere, and that is what `iat_imports` tests.
        if let Ok(watches) = std::env::var("WLAM_TRACE_WRITES") {
            for address in watches
                .split(',')
                .filter_map(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
            {
                self.host_imports.insert(
                    helper_slot,
                    HostImport {
                        library: "wlam.trace".into(),
                        name: format!("write_{address:08x}"),
                    },
                );
                helper_slot -= 1;
            }
        }
        if let Ok(watches) = std::env::var("WLAM_TRACE_BLOCKS") {
            for address in watches
                .split(',')
                .filter_map(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
            {
                self.host_imports.insert(
                    helper_slot,
                    HostImport {
                        library: "wlam.trace".into(),
                        name: format!("write_{address:08x}"),
                    },
                );
                helper_slot -= 1;
            }
        }
        self.import_index = self
            .host_imports
            .values()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .enumerate()
            .map(|(i, imp)| (imp, i as u32))
            .collect();
    }
}

/// Partition `blocks` into guest functions rooted at `entries`.
///
/// A function owns every block reachable by jump and fall-through; a direct
/// `call` starts a *new* function, which is what makes `ret` well defined.
pub fn partition(entries: &[u64], blocks: &BTreeMap<u64, Block>, registers: &[String]) -> Program {
    partition_with_indirect_targets(entries, blocks, registers, &BTreeMap::new())
}
pub fn partition_with_indirect_targets(
    entries: &[u64],
    blocks: &BTreeMap<u64, Block>,
    registers: &[String],
    targets: &BTreeMap<u64, Vec<u64>>,
) -> Program {
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
                // A call ends the block: the callee becomes its own function,
                // and this function resumes at the return address. Putting the
                // resume block here (rather than starting a new function at it)
                // is what keeps the caller as one dispatch loop.
                Terminator::Call { resume } => {
                    queue.push(*resume);
                    match direct_call_target(block) {
                        Some(target) => {
                            func.callees.insert(target);
                            work.push(target);
                        }
                        None => func
                            .problems
                            .push((addr, "call with non-constant target".into())),
                    }
                }
                // An indirect call resumes at the return address. The callee
                // is either an IAT import (statically known via the slot) or a
                // runtime-guest target; either way the *caller* continues here.
                Terminator::CallInd { resume } => queue.push(*resume),
                Terminator::Return | Terminator::Trap(_) => {}
                Terminator::IndirectJump => {
                    if let Some(destinations) = targets.get(&addr) {
                        queue.extend(destinations.iter().copied());
                    }
                    // A constant `[slot]` may be an imported API (handled as a
                    // tail call at emission time); anything else is unresolved.
                    if iat_source(block).is_none() {
                        func.problems
                            .push((addr, "indirect jump needs target resolution".into()));
                    }
                }
                Terminator::Undecodable(at) => {
                    func.problems
                        .push((addr, format!("undecodable bytes at {at:#x}")));
                }
            }
        }
        functions.push(func);
    }
    let function_index: BTreeMap<u64, u32> = functions
        .iter()
        .enumerate()
        .map(|(i, f)| (f.entry, i as u32))
        .collect();
    // `entries[0]` is the program entry by contract; everything else is a
    // table-reachable function.
    let entry_function = entries
        .first()
        .and_then(|e| function_index.get(e).copied())
        .unwrap_or(0);
    Program {
        functions,
        function_index,
        locals,
        registers: registers.to_vec(),
        image_base: 0,
        requirements: Requirements::default(),
        problems: Vec::new(),
        host_imports: BTreeMap::new(),
        import_index: BTreeMap::new(),
        iat_imports: BTreeMap::new(),
        entry_function,
    }
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
    Temp(u64, u32),
    Reg(Box<str>),
}

impl CondRef {
    fn local(&self, map: &LocalMap) -> Result<u32, TranslateError> {
        match self {
            CondRef::Temp(offset, size) => map
                .temp_index(*offset, *size)
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
    JumpInd,
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
    /// Call an imported Win32 API through its IAT slot, then resume.
    ///
    /// `slot` is the guest *IAT* address (the `[slot]` in `call [slot]`), used
    /// to pick the wasm import; `resume` is the return address.
    HostCall {
        slot: u64,
        resume: u64,
    },
    /// Tail-jump to an imported Win32 API through its IAT slot.
    ///
    /// A `jmp [slot]` has no return address of its own: the address already on
    /// `[ESP]` belongs to the caller of the current function. The host call is
    /// made with that same stack, and after it returns the stdcall cleanup
    /// (`return_pop`) pops the caller's address and arguments. Returning from
    /// the wasm function then hands the updated machine state -- including the
    /// host's EAX -- to the caller, which dispatches at its own resume point.
    HostTailCall {
        slot: u64,
    },
    /// Call another guest function, then resume.
    ///
    /// `target` is the callee's guest entry address and `resume` the address to
    /// dispatch to once it returns. The guest pushed `resume` onto its own stack
    /// before the `call`, so the callee's `ret` pops it into `EIP`; the caller
    /// dispatches on `EIP` afterwards rather than assuming `resume`, which keeps
    /// guest-modified return addresses (and `EIP` manipulation generally) honest.
    Call {
        target: u64,
        resume: u64,
    },
    /// Call a guest function whose target is computed at run time.
    ///
    /// The value in `target` is a guest virtual address. Only discovered
    /// function entries are callable; a wild or null address traps.
    CallInd {
        target: CondRef,
        resume: u64,
    },
    /// Trap: unreachable or unsupported.
    Trap,
}

/// The IAT slot a block calls or tail-jumps through, if it is an import.
///
/// A Win32 import call lifts as `Load T <- [slot]` followed by `CallInd T`, and
/// a tail call as `Load T <- [slot]` followed by `IndirectJump T`, where `slot`
/// is a constant (`.idata`) address. Matching on that shape is what
/// distinguishes it from a genuine `call [reg]`/`jmp [reg]` through a computed
/// function pointer, which cannot be resolved statically.
fn iat_slot_of(block: &Block) -> Option<u64> {
    iat_source(block).map(|(slot, _)| slot)
}
/// Follow the actual transfer operand, never whichever load happened to be last.
fn iat_source(block: &Block) -> Option<(u64, crate::ir::Operand)> {
    use crate::ir::{Operand, IR};
    let mut values: Vec<(Operand, Option<(u64, Operand)>)> = Vec::new();
    for op in &block.ops {
        let target = match op {
            IR::CallInd { target } | IR::IndirectJump { target } => target.clone(),
            _ => {
                let (dst, source) = match op {
                    IR::Load {
                        dst,
                        addr: Operand::Const { value, .. },
                        ..
                    } => (dst, Some((*value, dst.clone()))),
                    // `jmp [slot]` lifts as a copy of the slot address into a
                    // temporary, not as a load through it. Treating that copy
                    // as a slot source is what makes the tail call statically
                    // resolvable.
                    IR::Copy {
                        dst,
                        src: Operand::Const { value, .. },
                    } => (dst, Some((*value, dst.clone()))),
                    IR::Copy { dst, src } => (
                        dst,
                        values
                            .iter()
                            .rev()
                            .find(|(d, _)| d == src)
                            .and_then(|(_, s)| s.clone()),
                    ),
                    IR::FloatUnary { dst, .. }
                    | IR::Load { dst, .. }
                    | IR::Bin { dst, .. }
                    | IR::Un { dst, .. }
                    | IR::Float { dst, .. }
                    | IR::Convert { dst, .. }
                    | IR::Subpiece { dst, .. } => (dst, None),
                    _ => continue,
                };
                values.push((dst.clone(), source));
                continue;
            }
        };
        // Sleigh encodes `jmp [slot]` as a branch through the RAM varnode that
        // names the slot: `Copy T <- Const slot; IndirectJump Ram{offset: T}`.
        // The RAM operand and the temporary carry the same offset, so match on
        // that as well as exact operand equality (the `call [slot]` shape).
        let found = values.iter().rev().find(|(dst, _)| {
            if dst == &target {
                return true;
            }
            match (dst, &target) {
                (Operand::Temp { offset: a, .. }, Operand::Ram { offset: b, .. }) => a == b,
                _ => false,
            }
        });
        return found.and_then(|(_, source)| source.clone());
    }
    None
}

/// Split a block into data ops and a dispatcher exit.
///
/// A lifted `call` arrives as a run of ops -- `ESP -= 4`, `[ESP] = return_addr`,
/// then `Call` -- and the terminator is `Undecodable(return_addr)` because the
/// instruction stream resumes there. The ESP bookkeeping and the return-address
/// store are ordinary data ops and MUST be kept: the callee's `ret` pops them.
/// Only the `Call` op itself is pulled out into the exit.
const JUMP_TARGET: u64 = u64::MAX - 5001;
fn split_ops(block: &Block, drop_slot_load: bool) -> Result<Vec<crate::ir::IR>, TranslateError> {
    // Preserve loads: emit_case converts verified IAT loads to address tokens.
    // Control flow is the dispatcher's job, except that the *destination* of an
    // indirect call has to survive it, so its operand is re-materialised first
    // (see below).
    let mut ops = Vec::new();
    for ir in &block.ops {
        match ir {
            // Control flow is the dispatcher's job.
            crate::ir::IR::Branch { .. }
            | crate::ir::IR::CBranch { .. }
            | crate::ir::IR::Ret { .. }
            | crate::ir::IR::Call { .. } => {}
            crate::ir::IR::IndirectJump { target } if !drop_slot_load => {
                use crate::ir::{Operand, IR};
                let dst = Operand::Temp {
                    offset: JUMP_TARGET,
                    size: 4,
                };
                if let Operand::Ram { offset, size } = target {
                    let pointer = block.ops.iter().any(|op| match op {
                        IR::Copy {
                            dst: Operand::Temp { offset: at, .. },
                            ..
                        }
                        | IR::Bin {
                            dst: Operand::Temp { offset: at, .. },
                            ..
                        }
                        | IR::Un {
                            dst: Operand::Temp { offset: at, .. },
                            ..
                        } => at == offset,
                        _ => false,
                    });
                    let addr = if pointer {
                        Operand::Temp {
                            offset: *offset,
                            size: *size,
                        }
                    } else {
                        Operand::Const {
                            value: *offset,
                            size: 4,
                        }
                    };
                    ops.push(IR::Load { dst, addr, size: 4 });
                } else {
                    ops.push(IR::Copy {
                        dst,
                        src: target.clone(),
                    });
                }
            }
            crate::ir::IR::IndirectJump { .. } => {}
            // An IAT call is `Load T <- [slot]; CallInd T` with a constant
            // slot, i.e. a Win32 import: statically known, so it needs no
            // guest function table. Anything else really is an indirect call
            // through a computed target and still needs one. Which of the two
            // it is gets decided in `block_exit`.
            crate::ir::IR::CallInd { target } => {
                // An indirect call's destination must survive the call: a guest
                // `call esi` leaves the callee in ESI, and later code may call
                // through it again. The exit drops the original ops, so
                // re-materialise the target into its own operand first.
                ops.push(crate::ir::IR::Copy {
                    dst: target.clone(),
                    src: target.clone(),
                });
            }
            other => ops.push(other.clone()),
        }
    }
    Ok(ops)
}

/// Spill every register local into the shared snapshot area.
///
/// Registers are wasm locals, so two guest functions do not share them. The
/// snapshot area is ordinary guest memory, which makes it the natural carrier
/// across a guest `call`: the caller spills before the call and the callee
/// reloads on entry. Without this the callee started from the initial snapshot
/// (typically ESP = 0) and its `ret` popped the return address from address 0.
fn spill_registers(out: &mut Vec<u8>, prog: &Program) {
    for (i, name) in prog.registers.iter().enumerate() {
        let Some(local) = prog.locals.reg(name) else {
            continue;
        };
        // An x87 stack register is an f64 local: it must be stored as a
        // double, over an 8-byte slot. Emitting `i32.store` for it wrote the
        // low half of a double and put an i32 where the declaration says f64,
        // which is what produced "expected [f64] but got [i32]".
        out.push(op::I32_CONST);
        out.extend_from_slice(&leb_u32(crate::snapshot_address(i) as u32));
        out.push(op::LOCAL_GET);
        out.extend_from_slice(&leb_u32(local));
        if prog.locals.is_x87(local) {
            out.push(op::F64_STORE);
        } else {
            out.push(op::I32_STORE);
        }
        out.push(0);
        out.push(0);
    }
}

/// Adopt the callee's registers, including ESP after its `ret` (and any
/// stdcall argument cleanup). EIP remains local to the caller's control flow.
fn adopt_callee_state(out: &mut Vec<u8>, prog: &Program) {
    reload_registers_except(out, prog, &["EIP"]);
}

/// Reload every register local from the shared snapshot area.
fn reload_registers(out: &mut Vec<u8>, prog: &Program) {
    reload_registers_except(out, prog, &[]);
}

/// `reload_registers`, leaving the named registers alone.
fn reload_registers_except(out: &mut Vec<u8>, prog: &Program, skip: &[&str]) {
    for (i, name) in prog.registers.iter().enumerate() {
        if skip.contains(&name.as_str()) {
            continue;
        }
        let Some(local) = prog.locals.reg(name) else {
            continue;
        };
        out.push(op::I32_CONST);
        out.extend_from_slice(&leb_u32(crate::snapshot_address(i) as u32));
        if prog.locals.is_x87(local) {
            out.push(op::F64_LOAD);
        } else {
            out.push(op::I32_LOAD);
        }
        out.push(0);
        out.push(0);
        out.push(op::LOCAL_SET);
        out.extend_from_slice(&leb_u32(local));
    }
}

/// The callee address of a direct `call` in this block.
///
/// The lifted ops contain the `Call`; the terminator carries only the resume
/// point, so the target is read back out of the op stream.
fn direct_call_target(block: &Block) -> Option<u64> {
    let mut target = None;
    for ir in &block.ops {
        if let crate::ir::IR::Call { target: t } = ir {
            target = const_of(t);
        }
    }
    target
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
    host_imports: &BTreeMap<u64, HostImport>,
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
        Terminator::Call { resume } => Exit::Call {
            target: direct_call_target(block)
                .ok_or(TranslateError::Unsupported("call with non-constant target"))?,
            resume: *resume,
        },
        // An import call if the target came from a constant slot, otherwise a
        // genuine call through a runtime value: that goes via the function
        // table with a bounds check.
        // Three shapes, in order of how much is known statically:
        //   1. `call [slot]` where the slot IS a known import  -> host call
        //   2. `call [slot]` where the slot holds a GUEST function pointer
        //      (a mingw `call [global]`, or a vtable-ish slot) -> table call
        //   3. anything else (register, computed)              -> table call
        // Case 2 must NOT be treated as an import: those slots are ordinary
        // globals and the module would import a function that does not exist.
        Terminator::CallInd { resume } => match iat_slot_of(block) {
            Some(slot) if host_imports.contains_key(&slot) => Exit::HostCall {
                slot,
                resume: *resume,
            },
            _ => Exit::CallInd {
                target: indirect_target_ref(block)?,
                resume: *resume,
            },
        },
        Terminator::Return => Exit::Ret,
        // A `jmp [slot]` through a known import is a tail call: the host runs
        // and then control returns from this wasm function to the guest's
        // caller. Any other indirect jump still needs real target resolution.
        Terminator::IndirectJump => match iat_slot_of(block) {
            Some(slot) if host_imports.contains_key(&slot) => Exit::HostTailCall { slot },
            _ => Exit::JumpInd,
        },
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

/// The operand holding the target of an indirect call.
fn indirect_target_ref(block: &Block) -> Result<CondRef, TranslateError> {
    let target = block
        .ops
        .iter()
        .find_map(|ir| match ir {
            crate::ir::IR::CallInd { target } => Some(target.clone()),
            _ => None,
        })
        .ok_or(TranslateError::Unsupported(
            "indirect call without a CallInd op",
        ))?;
    match target {
        crate::ir::Operand::Temp { offset, size } => Ok(CondRef::Temp(offset, size)),
        crate::ir::Operand::Reg { name, .. } => Ok(CondRef::Reg(name)),
        _ => Err(TranslateError::Unsupported(
            "indirect call on a non-value target",
        )),
    }
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
        crate::ir::Operand::Temp { offset, size } => Ok(CondRef::Temp(offset, size)),
        crate::ir::Operand::Reg { name, .. } => Ok(CondRef::Reg(name)),
        _ => Err(TranslateError::Unsupported("constant condition")),
    }
}

/// Wasm memories are sized in 64 KiB pages. The wasm32 address space is 4 GiB,
/// which is 65536 pages, so this is the most one memory can have.
///
/// Fixed pages were previously 16 (1 MiB of *linear* memory). Since a PE32
/// image is loaded at its base address, `0x400000` is page 64, i.e. far outside
/// a 16-page memory: every access there trapped with an out-of-bounds error
/// that looks nothing like the real cause.
pub const GUEST_MEMORY_PAGES: u32 = 65536;

/// The guest's smallest usable image base, as a page count.
///
/// A PE32 image is linked at its preferred base and the guest's absolute
/// addresses are hard-coded, so the memory has to cover the base itself.
pub const DEFAULT_IMAGE_BASE_PAGES: u32 = 1024; // 0x400000, the PE32 default

/// The guest's whole address space, reserved up front.
///
/// ## Why reserve all of it
///
/// A 32-bit Windows program is entitled to address anything below 4 GiB, and
/// its code hard-codes addresses accordingly. Reservation is **virtual**:
/// V8 commits pages on first touch, so a 4 GiB memory measured 0.6 MiB of RSS
/// for eight instances. Charging the guest nothing to have the full space is
/// therefore the honest choice -- one less way for a recompiled program to fail
/// for a reason the original never had.
///
/// ## What this does NOT solve
///
/// Reserving is not allocating. `HeapAlloc` must still return a pointer that
/// does not overlap anything live and can later be freed; a bump allocator that
/// never reclaims exhausts 4 GiB just as surely as 256 MiB, only later. So the
/// full space is reserved here and handed to the heap allocator
/// (`wlam-host/src/heap.rs`) as its arena -- the host still does real
/// allocation, just never has to guess how much room to reserve.
pub const GUEST_ADDRESS_SPACE_PAGES: u32 = GUEST_MEMORY_PAGES;

/// One past the last byte the guest can address.
pub const GUEST_ADDRESS_SPACE_END: u64 = GUEST_ADDRESS_SPACE_PAGES as u64 * 0x10000;

/// Base of the region the host's `HeapAlloc` carves allocations from.
///
/// Chosen to clear the image (`0x400000`), the loader stack (below `0x70000`)
/// and the TIB, while leaving the whole upper address space for allocations.
pub const HEAP_REGION_BASE: u32 = 0x1000_0000;

/// Size of the heap region: everything from [`HEAP_REGION_BASE`] to 4 GiB.
///
/// The guest decides how much it uses. Sizing this smaller only moves the
/// failure earlier, and a failed `HeapAlloc` returns NULL, which the guest
/// degrades from in ways that are hard to attribute.
pub const HEAP_REGION_SIZE: u32 = (GUEST_ADDRESS_SPACE_END - HEAP_REGION_BASE as u64) as u32;

/// Pages needed for a guest whose image ends at `image_end`.
///
/// The answer is always the whole address space: the guest may use any 32-bit
/// address, and reservation is virtual. The parameter is kept because callers
/// have the image size to hand and a future 2 GiB-bounded profile would use it.
pub fn pages_for_image(_image_end: u64) -> u32 {
    GUEST_ADDRESS_SPACE_PAGES
}

/// Build the whole module with a memory large enough for the guest's image.
pub fn build_module(
    prog: &Program,
    blocks: &BTreeMap<u64, Block>,
) -> Result<Vec<u8>, TranslateError> {
    build_module_with_memory(prog, blocks, DEFAULT_IMAGE_BASE_PAGES)
}

/// Build the whole module, declaring `memory_pages` pages of linear memory.
///
/// `memory_pages` must cover the guest's image base; see [`GUEST_MEMORY_PAGES`].
pub fn build_module_with_memory(
    prog: &Program,
    blocks: &BTreeMap<u64, Block>,
    memory_pages: u32,
) -> Result<Vec<u8>, TranslateError> {
    if memory_pages > GUEST_MEMORY_PAGES {
        return Err(TranslateError::Unsupported(
            "memory larger than the 4 GiB wasm32 address space",
        ));
    }
    // Imports occupy the low function indices, so guest functions are
    // numbered after them.
    let imports: Vec<&HostImport> = prog.import_index.keys().collect();
    let import_count = imports.len() as u32;

    let mut bodies = Vec::new();
    for func in &prog.functions {
        bodies.push(emit_function_body(func, prog, blocks, import_count)?);
    }
    let callback = crate::callback::body(prog);
    if let Some(body) = &callback {
        bodies.push(body.clone());
    }
    let indirect = indirect_dispatch_body(prog, import_count)?;
    if let Some(body) = &indirect {
        bodies.push(body.clone());
    }
    let mut e = Encoder::new();
    module_header(&mut e);
    // type 0: () -> ()            guest entry
    // type 1: (i32, i32) -> i32   host import: (args_ptr, ret_addr) -> EAX
    e.section(section_id::TYPE, |b| {
        b.u32(3);
        b.push(0x60);
        b.u32(0);
        b.u32(0);
        b.push(0x60);
        b.u32(2);
        b.push(0x7f);
        b.push(0x7f);
        b.u32(1);
        b.push(0x7f);
        b.push(0x60);
        b.u32(3);
        b.raw(&[0x7f, 0x7f, 0x7f]);
        b.u32(1);
        b.push(0x7f);
    });
    if import_count > 0 {
        e.section(section_id::IMPORT, |b| {
            b.u32(import_count);
            for imp in &imports {
                b.u32(imp.library.len() as u32);
                b.raw(imp.library.as_bytes());
                b.u32(imp.name.len() as u32);
                b.raw(imp.name.as_bytes());
                b.push(0x00); // func
                b.u32(1); // type 1: (i32 i32) -> i32
            }
        });
    }
    e.section(section_id::FUNCTION, |b| {
        b.u32(bodies.len() as u32);
        for _ in &prog.functions {
            b.u32(0);
        }
        if callback.is_some() {
            b.u32(2);
        }
        if indirect.is_some() {
            b.u32(0);
        }
    });
    e.section(section_id::TABLE, |b| {
        b.u32(1);
        b.push(0x70); // funcref
        b.push(0x00); // min only
        b.u32(prog.functions.len() as u32);
    });
    e.section(section_id::MEMORY, |b| {
        b.u32(1);
        b.push(0x00);
        b.u32(memory_pages);
    });
    e.section(section_id::EXPORT, |b| {
        // Every guest function is exported under a stable name so a debugger or
        // probe can call one directly (e.g. to step a starved guest block by
        // block). `entry` stays first so existing callers are unaffected.
        b.u32(2 + prog.functions.len() as u32 + u32::from(callback.is_some()));
        b.u32(5);
        b.raw(b"entry");
        b.push(0x00);
        b.u32(import_count + prog.entry_function); // the entry, past the imports
        b.u32(6);
        b.raw(b"memory");
        b.push(0x02);
        b.u32(0);
        if callback.is_some() {
            b.u32(12);
            b.raw(b"invoke_guest");
            b.push(0);
            b.u32(import_count + prog.functions.len() as u32);
        }
        for (i, func) in prog.functions.iter().enumerate() {
            let name = format!("g_{:08x}", func.entry as u32);
            b.u32(name.len() as u32);
            b.raw(name.as_bytes());
            b.push(0x00);
            b.u32(import_count + i as u32);
        }
    });
    // Table slot i holds guest function i. The element segment needs ABSOLUTE
    // function indices, so every guest index is shifted past the imports --
    // the same shift `call` needs, kept in one place.
    if !prog.functions.is_empty() {
        e.section(section_id::ELEMENT, |b| {
            b.u32(1); // one segment
            b.u32(0); // active, table 0
            b.push(op::I32_CONST);
            b.u32(0); // offset 0
            b.push(op::END);
            b.u32(prog.functions.len() as u32);
            for i in 0..prog.functions.len() as u32 {
                b.u32(i + import_count);
            }
        });
    }
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
    import_count: u32,
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
        body.extend_from_slice(&leb_u32(crate::snapshot_address(i) as u32));
        // x87 registers are f64 locals: load them as doubles. This is the
        // program-entry seed, the third of the three places that touch every
        // register slot; all three had to become type-aware together.
        body.push(if prog.locals.is_x87(local) {
            op::F64_LOAD
        } else {
            op::I32_LOAD
        });
        body.push(0);
        body.push(0);
        body.push(op::LOCAL_SET);
        body.extend_from_slice(&leb_u32(local));
    }
    body.push(op::I32_CONST);
    body.extend_from_slice(&crate::encode::leb_s32(0));
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
        emit_case(
            &mut body,
            func,
            prog,
            blocks,
            addr,
            i,
            import_count,
            &mut shared,
            id,
        )?;
    }
    body.push(op::END); // end loop
    body.push(op::UNREACHABLE); // out-of-range dispatch
    body.push(op::END); // end function

    // Header: every local allocated anywhere in the function (the id local is
    // already included via `reserve_scratch`). 8-byte temps need i64 locals, so
    // the declaration carries typed runs in local-index order.
    let runs = shared.declaration_runs();
    let mut out = Vec::new();
    out.extend_from_slice(&leb_u32(runs.len() as u32));
    for (ty, count) in &runs {
        out.extend_from_slice(&leb_u32(*count));
        out.push(*ty);
    }
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
    import_count: u32,
    shared: &mut LocalMap,
    id: u32,
) -> Result<(), TranslateError> {
    // Depth from a case body to the dispatch LOOP.
    //
    // The dispatcher is nested as:
    //
    //   loop                  ;; @1  <- "dispatch again" branches here
    //     block               ;; @2
    //       block             ;; @3
    //         ...
    //           block         ;; @(n+1)
    //             br_table 20 (;@2;) ... 0 (;@(n+1)) n (;@1;)
    //           end           ;; closes @(n+1)
    //           <case n-1 body>
    //         end             ;; closes @n
    //         <case n-2 body>
    //       ...
    //     end
    //     <case 0 body>
    //
    // Bodies are emitted in REVERSE (see the loop below): before case `i` is
    // emitted, the blocks for cases `i+1..n` have already been closed by their
    // `end`s, while the blocks for cases `1..=i` are still open, and the loop
    // encloses them all. So a case body is enclosed by `i` case blocks plus the
    // loop, and the loop is `i + 1` labels out.
    //
    // Check against `br_table`, which maps entry `id` to depth `n-1-id`: entry 0
    // targets depth `n-1`, i.e. the outermost case block. From case 0's body
    // there are `n-1` blocks outside it, so depth 0 is the innermost label and
    // depth `n-1` is that outermost block -- consistent.
    //
    // This was `i` (short by one). Being one label short meant a `Goto` landed
    // on the next enclosing *block* instead of the loop, which re-entered the
    // dispatcher's `end` chain and left the function early -- skipping the rest
    // of the block. That is why Uxn32 never stored the version after calling
    // GetVersion even though the call itself worked. Small fixtures hid it
    // because a short branch often still reached code that dispatched again.
    let depth_to_loop = index;
    // `Exit::Ret` is emitted as `return` when this block is the function's own
    // entry. Check the opcodes that branch decision rests on once, here, so a
    // mistyped constant in `mod op` cannot silently turn "this is the entry"
    // into "this is not the entry" for every block at once.
    debug_assert!(
        op::CALL == 0x10 && op::RETURN == 0x0f && op::BR == 0x0c && op::BR_TABLE == 0x0e,
        "opcode table is wrong: {:02x?}",
        (op::CALL, op::RETURN, op::BR, op::BR_TABLE)
    );
    let Some(block) = blocks.get(&addr) else {
        out.push(op::UNREACHABLE);
        return Ok(());
    };
    // Optional diagnostic breadcrumb: keep the last guest basic block address
    // in the exported register snapshot so a Wasm trap inside a guest callback
    // can be localized even when it occurs before the next host import.
    if std::env::var_os("WLAM_DEBUG_EIP").is_some() {
        if let Some(eip) = prog.locals.reg("EIP") {
            out.push(op::I32_CONST);
            out.extend_from_slice(&crate::encode::leb_s32(addr as i32));
            out.push(op::LOCAL_SET);
            out.extend(leb_u32(eip));
        }
        if let Some(index) = prog.registers.iter().position(|name| name == "EIP") {
            out.push(op::I32_CONST);
            out.extend_from_slice(&crate::encode::leb_u32(
                crate::snapshot_address(index) as u32
            ));
            out.push(op::I32_CONST);
            out.extend_from_slice(&crate::encode::leb_s32(addr as i32));
            out.push(op::I32_STORE);
            out.extend_from_slice(&[2, 0]);
        }
    }
    if let Ok(watches) = std::env::var("WLAM_TRACE_BLOCKS") {
        if watches
            .split(',')
            .filter_map(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
            .any(|address| address as u64 == block.start)
        {
            emit_debug_trace(out, prog, block.start as u32);
        }
    }
    // Decide up front whether this is an import call: only then is the slot
    // load dead code.
    let is_import_call = matches!(
        &block.terminator,
        Terminator::CallInd { .. } | Terminator::IndirectJump
    ) && iat_slot_of(block)
        .map(|s| prog.host_imports.contains_key(&s))
        .unwrap_or(false);
    let ops = split_ops(block, is_import_call)?
        .into_iter()
        .map(|op| match op {
            crate::ir::IR::Load {
                dst,
                addr: crate::ir::Operand::Const { value, .. },
                size: 4,
            } if prog.host_imports.contains_key(&value) => crate::ir::IR::Copy {
                dst,
                src: crate::ir::Operand::Const { value, size: 4 },
            },
            other => other,
        })
        .collect::<Vec<_>>();
    // Each block's temps must get their own locals inside the shared function
    // layout: translating with a fresh map would hand out indices the function
    // never declared ("unknown local 10: local index out of bounds").
    let translated = match crate::translate::translate_ops_with_imports(
        &ops,
        shared.clone(),
        &prog.import_index,
    ) {
        Ok(t) => t,
        Err(e) => {
            if matches!(e, TranslateError::UnsupportedWidth(10)) {
                let import = crate::translate::unsupported_import(&e);
                if let Some(index) = prog.import_index.get(&import) {
                    if let Some(eip) = prog.locals.reg("EIP") {
                        out.push(op::I32_CONST);
                        out.extend(crate::encode::leb_s32(block.start as i32));
                        out.push(op::LOCAL_SET);
                        out.extend(leb_u32(eip));
                    }
                    spill_registers(out, prog);
                    out.extend([op::I32_CONST, 0, op::I32_CONST, 0, op::CALL]);
                    out.extend(leb_u32(*index));
                    out.extend([0x1a, op::UNREACHABLE]);
                    return Ok(());
                }
            }
            if std::env::var("FLT_TRACE").is_ok() {
                eprintln!("BLOCK {:#x} failed: {e:?}", block.start);
            }
            return Err(e);
        }
    };
    *shared = translated.map.clone();
    out.extend_from_slice(&translated.body[translated.ops_offset..translated.body.len() - 1]);
    emit_store_watchpoint(out, prog, block.start as u32, &ops, shared);
    if std::env::var_os("WLAM_DEBUG_EIP").is_some() {
        if let Some(eip) = prog.locals.reg("EIP") {
            out.push(op::I32_CONST);
            out.extend_from_slice(&crate::encode::leb_s32(block.end as i32));
            out.push(op::LOCAL_SET);
            out.extend(leb_u32(eip));
        }
        if let Some(index) = prog.registers.iter().position(|name| name == "EIP") {
            out.push(op::I32_CONST);
            out.extend_from_slice(&crate::encode::leb_u32(
                crate::snapshot_address(index) as u32
            ));
            out.push(op::I32_CONST);
            out.extend_from_slice(&crate::encode::leb_s32(block.end as i32));
            out.push(op::I32_STORE);
            out.extend_from_slice(&[2, 0]);
        }
    }
    let exit = block_exit(block, func, &translated, &prog.host_imports)?;
    match exit {
        Exit::JumpInd => {
            let target = shared
                .temp_index(JUMP_TARGET, 4)
                .ok_or(TranslateError::Unsupported("indirect jump target missing"))?;
            if let Some(eip) = prog.locals.reg("EIP") {
                out.push(op::LOCAL_GET);
                out.extend(leb_u32(target));
                out.push(op::LOCAL_SET);
                out.extend(leb_u32(eip));
            }
            if std::env::var_os("WLAM_DEBUG_EIP").is_some() {
                for (address, value) in [(0x7ff0, Some(target)), (0x7ff4, None), (0x7ff8, None)] {
                    out.push(op::I32_CONST);
                    out.extend(leb_u32(address));
                    if let Some(local) = value {
                        out.push(op::LOCAL_GET);
                        out.extend(leb_u32(local));
                    } else {
                        out.push(op::I32_CONST);
                        out.extend(crate::encode::leb_s32(if address == 0x7ff4 {
                            block.start as i32
                        } else {
                            func.entry as i32
                        }));
                    }
                    out.push(op::I32_STORE);
                    out.extend([2, 0]);
                    emit_debug_trace(out, prog, address);
                }
            }
            for (address, destination) in &func.ids {
                out.push(op::LOCAL_GET);
                out.extend(leb_u32(target));
                out.push(op::I32_CONST);
                out.extend(crate::encode::leb_s32(*address as i32));
                out.push(0x46);
                if prog.image_base != 0 {
                    out.push(op::LOCAL_GET);
                    out.extend(leb_u32(target));
                    out.push(op::I32_CONST);
                    out.extend(leb_u32(prog.image_base));
                    out.push(op::I32_ADD);
                    out.push(op::I32_CONST);
                    out.extend(crate::encode::leb_s32(*address as i32));
                    out.extend([0x46, 0x72]); // exact VA or image-base-relative RVA
                }
                out.extend([op::IF, 0x40, op::I32_CONST]);
                out.extend(crate::encode::leb_s32(*destination as i32));
                out.push(op::LOCAL_SET);
                out.extend(leb_u32(id));
                out.push(op::BR);
                out.extend(leb_u32(depth_to_loop + 1));
                out.push(op::END);
            }
            if prog.locals.reg("EIP").is_some() {
                spill_registers(out, prog);
                if std::env::var_os("WLAM_TRACE_INDIRECT").is_some() {
                    if let Some(index) = prog.registers.iter().position(|name| name == "EIP") {
                        emit_debug_trace(out, prog, crate::snapshot_address(index) as u32);
                    }
                }
                let callback = prog.registers.iter().any(|r| r == "ESP")
                    && prog.registers.iter().any(|r| r == "EAX");
                out.push(op::CALL);
                out.extend(leb_u32(
                    import_count + prog.functions.len() as u32 + u32::from(callback),
                ));
                out.push(op::RETURN);
            } else {
                out.push(op::UNREACHABLE);
            }
        }
        Exit::Goto(target) => {
            out.push(op::I32_CONST);
            out.extend_from_slice(&crate::encode::leb_s32(target as i32));
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
            out.extend_from_slice(&crate::encode::leb_s32(taken as i32));
            out.push(op::LOCAL_SET);
            out.extend_from_slice(&leb_u32(id));
            // The `if` itself is one more enclosing label inside an arm.
            out.push(op::BR);
            out.extend_from_slice(&leb_u32(depth_to_loop + 1));
            out.push(op::ELSE);
            out.push(op::I32_CONST);
            out.extend_from_slice(&crate::encode::leb_s32(not_taken as i32));
            out.push(op::LOCAL_SET);
            out.extend_from_slice(&leb_u32(id));
            out.push(op::BR);
            out.extend_from_slice(&leb_u32(depth_to_loop + 1));
            out.push(op::END);
        }
        Exit::Ret => {
            // Hand the caller our final machine state before leaving.
            spill_registers(out, prog);
            if std::env::var("WLAM_TRACE_FUNCTION_RET")
                .ok()
                .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
                == Some(func.entry as u32)
            {
                emit_debug_trace(out, prog, 0x45e011);
            }
            out.push(op::RETURN);
        }
        Exit::Call { target, resume } => {
            let callee =
                prog.function_index
                    .get(&target)
                    .copied()
                    .ok_or(TranslateError::Unsupported(
                        "call target is not a discovered function",
                    ))?;
            // Hand the callee our machine state: ESP must be the *post-push*
            // value so the callee's `ret` pops the address we just pushed.
            spill_registers(out, prog);
            out.push(op::CALL);
            out.extend_from_slice(&leb_u32(callee + import_count));
            adopt_callee_state(out, prog);
            // The callee's `ret` popped the return address into EIP. Dispatch
            // on the resume block here; if the guest rewrote the return address
            // the id lookup simply fails and we trap rather than silently
            // resuming at the wrong place.
            let resume_id = func
                .ids
                .get(&resume)
                .copied()
                .ok_or(TranslateError::Unsupported(
                    "call resume block is outside its function",
                ))?;
            out.push(op::I32_CONST);
            out.extend_from_slice(&crate::encode::leb_s32(resume_id as i32));
            out.push(op::LOCAL_SET);
            out.extend_from_slice(&leb_u32(id));
            out.push(op::BR);
            out.extend_from_slice(&leb_u32(depth_to_loop));
        }
        Exit::CallInd { target, resume } => {
            // Same ABI as a direct call: spill so the callee starts from our
            // machine state (ESP already includes the pushed return address).
            let target_local = target.local(shared)?;
            if std::env::var_os("WLAM_TRACE_INDIRECT").is_some() {
                for (address, local) in [
                    (0x7fe0, target_local),
                    (0x7fe8, prog.locals.reg("ECX").unwrap_or(target_local)),
                ] {
                    out.push(op::I32_CONST);
                    out.extend(crate::encode::leb_u32(address));
                    out.push(op::LOCAL_GET);
                    out.extend(leb_u32(local));
                    out.push(op::I32_STORE);
                    out.extend([2, 0]);
                    emit_debug_trace(out, prog, address);
                }
                out.push(op::I32_CONST);
                out.extend(crate::encode::leb_u32(0x7fe4));
                out.push(op::I32_CONST);
                out.extend(crate::encode::leb_s32(block.start as i32));
                out.push(op::I32_STORE);
                out.extend([2, 0]);
                emit_debug_trace(out, prog, 0x7fe4);
            }
            if std::env::var_os("WLAM_DEBUG_EIP").is_some() {
                out.push(op::I32_CONST);
                out.extend(crate::encode::leb_u32(0x7ff0));
                out.push(op::LOCAL_GET);
                out.extend(leb_u32(target_local));
                out.push(op::I32_STORE);
                out.extend([2, 0]);
                emit_debug_trace(out, prog, 0x7ff0);
                out.push(op::I32_CONST);
                out.extend(crate::encode::leb_u32(0x7ff4));
                out.push(op::I32_CONST);
                out.extend(crate::encode::leb_s32(block.start as i32));
                out.push(op::I32_STORE);
                out.extend([2, 0]);
                emit_debug_trace(out, prog, 0x7ff4);
                out.push(op::I32_CONST);
                out.extend(crate::encode::leb_u32(0x7ff8));
                out.push(op::I32_CONST);
                out.extend(crate::encode::leb_s32(func.entry as i32));
                out.push(op::I32_STORE);
                out.extend([2, 0]);
                emit_debug_trace(out, prog, 0x7ff8);
            }
            // Leave the destination in the guest's `EIP`, as a real CPU does
            // after `call edx`. The register snapshot then names the callee if
            // the call traps; gated on the program modelling `EIP` at all.
            if let Some(eip) = prog.locals.reg("EIP") {
                out.push(op::LOCAL_GET);
                out.extend_from_slice(&leb_u32(target_local));
                out.push(op::LOCAL_SET);
                out.extend_from_slice(&leb_u32(eip));
            }
            spill_registers(out, prog);
            if prog.locals.reg("EIP").is_some() {
                // One shared resolver avoids duplicating every import and guest
                // address comparison at every indirect call site.
                let callback = prog.registers.iter().any(|r| r == "ESP")
                    && prog.registers.iter().any(|r| r == "EAX");
                out.push(op::CALL);
                out.extend(leb_u32(
                    import_count + prog.functions.len() as u32 + u32::from(callback),
                ));
                adopt_callee_state(out, prog);
            } else {
                out.extend([op::BLOCK, 0x40]);
                // Guest function pointers are virtual addresses. Resolve only
                // registered entries; a small integer or NULL is never a table ID.
                for (address, index) in &prog.function_index {
                    out.push(op::LOCAL_GET);
                    out.extend(leb_u32(target_local));
                    out.push(op::I32_CONST);
                    out.extend(crate::encode::leb_s32(*address as i32));
                    out.extend([0x46, op::IF, 0x40]);
                    out.push(op::CALL);
                    out.extend(leb_u32(index + import_count));
                    adopt_callee_state(out, prog);
                    out.extend([op::BR, 1, op::END]);
                }
                // No matching guest entry is callable.
                out.push(op::UNREACHABLE);
                out.push(op::END); // cached import/guest call join
            }

            let resume_id = func
                .ids
                .get(&resume)
                .copied()
                .ok_or(TranslateError::Unsupported(
                    "call resume block is outside its function",
                ))?;
            out.push(op::I32_CONST);
            out.extend_from_slice(&crate::encode::leb_s32(resume_id as i32));
            out.push(op::LOCAL_SET);
            out.extend_from_slice(&leb_u32(id));
            out.push(op::BR);
            out.extend_from_slice(&leb_u32(depth_to_loop));
        }
        Exit::HostCall { slot, resume } => {
            let import = prog.host_imports.get(&slot).ok_or_else(|| {
                TranslateError::Unsupported("call through an IAT slot that is not an import")
            })?;
            let index = *prog
                .import_index
                .get(import)
                .ok_or(TranslateError::Unsupported("host import has no wasm index"))?;
            // ABI (docs/abi.md): the host takes a guest pointer to the
            // argument block and validates it itself. The lifted `call` already
            // pushed the return address, so the arguments start at ESP + 4.
            // Slot is passed as the second argument so one host entry can serve
            // several imports if needed.
            spill_registers(out, prog);
            let esp = prog
                .locals
                .reg("ESP")
                .ok_or(TranslateError::Unsupported("host call needs an ESP local"))?;
            out.push(op::LOCAL_GET);
            out.extend_from_slice(&leb_u32(esp));
            out.push(op::I32_CONST);
            out.extend_from_slice(&leb_u32(4));
            out.push(op::I32_ADD);
            out.push(op::I32_CONST);
            out.extend_from_slice(&leb_u32((slot & 0xffff_ffff) as u32));
            out.push(op::CALL);
            out.extend_from_slice(&leb_u32(index));
            // The returned EAX is the guest's return value.
            if let Some(eax) = prog.locals.reg("EAX") {
                out.push(op::LOCAL_SET);
                out.extend_from_slice(&leb_u32(eax));
            } else {
                out.push(0x1a); // drop
            }
            // A `call [slot]` handed the import a target that the lifted code
            // had loaded through a register (`mov esi, [slot]; call esi`).
            // That register must still hold the slot address afterwards: the
            // same register is often reused for the next call, and a guest that
            // reads it back would otherwise see the stale pre-call value.
            if let Some(eip) = prog.locals.reg("EIP") {
                out.push(op::I32_CONST);
                out.extend_from_slice(&crate::encode::leb_s32((slot & 0xffff_ffff) as i32));
                out.push(op::LOCAL_SET);
                out.extend_from_slice(&leb_u32(eip));
            }
            if let Some(loaded) =
                loaded_target_register(block).filter(|name| name.as_ref() != "EAX")
            {
                if let Some(local) = shared.reg(loaded.as_ref()) {
                    out.push(op::I32_CONST);
                    out.extend_from_slice(&crate::encode::leb_s32((slot & 0xffff_ffff) as i32));
                    out.push(op::LOCAL_SET);
                    out.extend_from_slice(&leb_u32(local));
                }
            }
            // The lifted CALL pushed a guest return address. The host import
            // returns directly to this continuation, so implement RET n here.
            if let Some(signature) = wlam_pcode::abi::signature(&import.library, &import.name) {
                pop_import_frame(out, esp, signature);
            } else {
                out.push(op::UNREACHABLE);
            }
            let resume_id = func
                .ids
                .get(&resume)
                .copied()
                .ok_or(TranslateError::Unsupported(
                    "call resume block is outside its function",
                ))?;
            out.push(op::I32_CONST);
            out.extend_from_slice(&crate::encode::leb_s32(resume_id as i32));
            out.push(op::LOCAL_SET);
            out.extend_from_slice(&leb_u32(id));
            out.push(op::BR);
            out.extend_from_slice(&leb_u32(depth_to_loop));
        }
        Exit::HostTailCall { slot } => {
            // The host may reenter through invoke_guest. Publish the actual
            // wrapper stack before it builds a callback frame beneath ESP.
            spill_registers(out, prog);
            let import = prog.host_imports.get(&slot).ok_or_else(|| {
                TranslateError::Unsupported("tail jump through an IAT slot that is not an import")
            })?;
            let index = *prog
                .import_index
                .get(import)
                .ok_or(TranslateError::Unsupported("host import has no wasm index"))?;
            // The stack layout is the caller's: `[ESP]` is the return address
            // the caller pushed, and the import's arguments follow it. There is
            // no return address for this jump to push.
            let esp = prog
                .locals
                .reg("ESP")
                .ok_or(TranslateError::Unsupported("tail call needs an ESP local"))?;
            out.push(op::LOCAL_GET);
            out.extend_from_slice(&leb_u32(esp));
            out.push(op::I32_CONST);
            out.extend_from_slice(&leb_u32(4));
            out.push(op::I32_ADD);
            out.push(op::I32_CONST);
            out.extend_from_slice(&leb_u32((slot & 0xffff_ffff) as u32));
            out.push(op::CALL);
            out.extend_from_slice(&leb_u32(index));
            if let Some(eax) = prog.locals.reg("EAX") {
                out.push(op::LOCAL_SET);
                out.extend_from_slice(&leb_u32(eax));
            } else {
                out.push(0x1a); // drop
            }
            // The import's stdcall cleanup (`ret n`) pops the caller's return
            // address and its arguments. The wasm `return` below then hands
            // control to the caller's resume point, which is exactly the jump
            // the guest's `ret` would have made.
            let Some(signature) = wlam_pcode::abi::signature(&import.library, &import.name) else {
                // Keep unreachable code compilable, but never resume a guest
                // after an import whose stack convention is unknown.
                out.push(op::UNREACHABLE);
                return Ok(());
            };
            let tail_esp = esp;
            pop_import_frame(out, tail_esp, signature);
            spill_registers(out, prog);
            out.push(op::RETURN);
        }
        Exit::Trap => out.push(op::UNREACHABLE),
    }
    Ok(())
}

fn emit_import_value(
    out: &mut Vec<u8>,
    prog: &Program,
    slot: u64,
    import: &HostImport,
) -> Result<(), TranslateError> {
    spill_registers(out, prog);
    let esp = prog
        .locals
        .reg("ESP")
        .ok_or(TranslateError::Unsupported("host call needs ESP"))?;
    let index = *prog
        .import_index
        .get(import)
        .ok_or(TranslateError::Unsupported("host import index missing"))?;
    out.push(op::LOCAL_GET);
    out.extend(leb_u32(esp));
    out.push(op::I32_CONST);
    out.extend(crate::encode::leb_s32(4));
    out.push(op::I32_ADD);
    out.push(op::I32_CONST);
    out.extend(crate::encode::leb_s32(slot as i32));
    out.push(op::CALL);
    out.extend(leb_u32(index));
    if let Some(eax) = prog.locals.reg("EAX") {
        out.push(op::LOCAL_SET);
        out.extend(leb_u32(eax));
    } else {
        out.push(0x1a);
    }
    if let Some(signature) = wlam_pcode::abi::signature(&import.library, &import.name) {
        pop_import_frame(out, esp, signature);
    } else {
        out.push(op::UNREACHABLE);
    }
    // Cleanup and the return value are already in locals. Publish them rather
    // than reloading the pre-call snapshot and undoing the host return.
    spill_registers(out, prog);
    Ok(())
}

fn emit_debug_trace(out: &mut Vec<u8>, prog: &Program, address: u32) {
    let import = HostImport {
        library: "wlam.trace".into(),
        name: format!("write_{address:08x}"),
    };
    if let Some(index) = prog.import_index.get(&import) {
        // The trace host reads the canonical guest register snapshot. Block
        // locals may have changed since the last import, so publish them first
        // or traces show stale caller state and mislead control-flow debugging.
        spill_registers(out, prog);
        out.push(op::I32_CONST);
        out.extend(crate::encode::leb_u32(address));
        out.push(op::I32_CONST);
        out.push(0);
        out.push(op::CALL);
        out.extend(leb_u32(*index));
        out.push(0x1a);
    }
}

/// Optional diagnostic trap on the first block that stores into a watched
/// guest address range. Set `WLAM_WATCH_RANGE=start:end` in hex when building.
fn emit_store_watchpoint(
    out: &mut Vec<u8>,
    prog: &Program,
    block: u32,
    ops: &[crate::ir::IR],
    map: &LocalMap,
) {
    let Some(range) = std::env::var("WLAM_WATCH_RANGE").ok() else {
        return;
    };
    let Some((start, end)) = range.split_once(':').and_then(|(a, b)| {
        let parse = |s: &str| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok();
        Some((parse(a)?, parse(b)?))
    }) else {
        return;
    };
    if start >= end {
        return;
    }
    for ir in ops {
        let crate::ir::IR::Store { addr, size, .. } = ir else {
            continue;
        };
        let address = const_of(addr).map(|value| value as u32);
        if let Some(address) = address {
            if address.saturating_add((*size).max(1)) <= start.saturating_sub(32) || address >= end
            {
                continue;
            }
            emit_watch_trap(out, prog, block, None);
            continue;
        }
        let local = match addr {
            crate::ir::Operand::Reg { name, .. } => map.reg(name.as_ref()),
            crate::ir::Operand::Temp { offset, size } => map.temp_index(*offset, *size),
            _ => None,
        };
        let Some(local) = local else {
            continue;
        };
        out.push(op::LOCAL_GET);
        out.extend(leb_u32(local));
        out.push(op::I32_CONST);
        out.extend(crate::encode::leb_u32(start.saturating_sub(32)));
        out.push(0x4f); // i32.ge_u
        out.push(op::LOCAL_GET);
        out.extend(leb_u32(local));
        out.push(op::I32_CONST);
        out.extend(crate::encode::leb_u32(end));
        out.extend([0x49, 0x71, op::IF, 0x40]); // i32.lt_u; and; if
        emit_watch_trap(out, prog, block, Some(local));
        out.push(op::END);
    }
}

fn emit_watch_trap(out: &mut Vec<u8>, prog: &Program, block: u32, address: Option<u32>) {
    spill_registers(out, prog);
    out.push(op::I32_CONST);
    out.extend(crate::encode::leb_u32(0x7fb0));
    if let Some(local) = address {
        out.push(op::LOCAL_GET);
        out.extend(leb_u32(local));
    } else {
        out.push(op::I32_CONST);
        out.push(0);
    }
    out.extend([op::I32_STORE, 2, 0]);
    out.push(op::I32_CONST);
    out.extend(crate::encode::leb_u32(0x7fb4));
    out.push(op::I32_CONST);
    out.extend(crate::encode::leb_s32(block as i32));
    out.extend([op::I32_STORE, 2, 0, op::UNREACHABLE]);
}

/// Resolve raw x86 code pointers once per module, using the spilled EIP.
fn indirect_dispatch_body(
    prog: &Program,
    import_count: u32,
) -> Result<Option<Vec<u8>>, TranslateError> {
    let Some(target) = prog.locals.reg("EIP") else {
        return Ok(None);
    };
    let mut out = Vec::new();
    let runs = prog.locals.declaration_runs();
    out.extend(leb_u32(runs.len() as u32));
    for (ty, count) in runs {
        out.extend(leb_u32(count));
        out.push(ty);
    }
    reload_registers(&mut out, prog);
    if std::env::var_os("WLAM_TRACE_INDIRECT").is_some() {
        out.push(op::I32_CONST);
        out.extend(crate::encode::leb_u32(0x7fc0));
        out.push(op::LOCAL_GET);
        out.extend(leb_u32(target));
        out.push(op::I32_STORE);
        out.extend([2, 0]);
        emit_debug_trace(&mut out, prog, 0x7ff1);
        if prog.image_base != 0 {
            out.push(op::I32_CONST);
            out.extend(crate::encode::leb_u32(0x7fc4));
            out.push(op::LOCAL_GET);
            out.extend(leb_u32(target));
            out.push(op::I32_CONST);
            out.extend(crate::encode::leb_u32(prog.image_base));
            out.push(op::I32_ADD);
            out.push(op::I32_STORE);
            out.extend([2, 0]);
            emit_debug_trace(&mut out, prog, 0x7ff3);
        }
    }
    for (slot, import) in &prog.iat_imports {
        if *slot > u32::MAX as u64 || !prog.import_index.contains_key(import) {
            continue;
        }
        out.push(op::LOCAL_GET);
        out.extend(leb_u32(target));
        out.push(op::I32_CONST);
        out.extend(crate::encode::leb_s32(*slot as i32));
        out.extend([0x46, op::IF, 0x40]);
        emit_import_value(&mut out, prog, *slot, import)?;
        out.extend([op::RETURN, op::END]);
    }
    for (address, index) in &prog.function_index {
        out.push(op::LOCAL_GET);
        out.extend(leb_u32(target));
        out.push(op::I32_CONST);
        out.extend(crate::encode::leb_s32(*address as i32));
        out.push(0x46);
        if prog.image_base != 0 {
            out.push(op::LOCAL_GET);
            out.extend(leb_u32(target));
            out.push(op::I32_CONST);
            out.extend(leb_u32(prog.image_base));
            out.push(op::I32_ADD);
            out.push(op::I32_CONST);
            out.extend(crate::encode::leb_s32(*address as i32));
            out.extend([0x46, 0x72]);
        }
        out.extend([op::IF, 0x40, op::CALL]);
        out.extend(leb_u32(index + import_count));
        out.extend([op::RETURN, op::END]);
    }
    if std::env::var_os("WLAM_TRACE_INDIRECT").is_some() {
        out.push(op::I32_CONST);
        out.extend(crate::encode::leb_u32(0x7fc8));
        out.push(op::I32_CONST);
        out.push(1);
        out.push(op::I32_STORE);
        out.extend([2, 0]);
        emit_debug_trace(&mut out, prog, 0x7ff2);
    }
    out.extend([op::UNREACHABLE, op::END]);
    Ok(Some(out))
}

/// The register an IAT call target was loaded through, if it was a register.
///
/// `call [slot]` lifts as `Load T <- [slot]; CallInd T`, and the emitter turns
/// the load into an address token. Which destination that token lived in is
/// what this recovers, so the register can be restored after the call.
fn loaded_target_register(block: &Block) -> Option<Box<str>> {
    let target = block.ops.iter().find_map(|op| match op {
        crate::ir::IR::CallInd { target } => Some(target),
        _ => None,
    })?;
    block.ops.iter().find_map(|op| match op {
        crate::ir::IR::Load {
            dst: crate::ir::Operand::Reg { name, .. },
            ..
        } if target
            == &crate::ir::Operand::Reg {
                name: name.clone(),
                size: 4,
            } =>
        {
            Some(name.clone())
        }
        _ => None,
    })
}

/// Apply an import's stack cleanup at an IAT call site.
///
/// The lifted `call` already pushed the guest return address, so `RET n`
/// (stdcall) is `[ESP] + 4 + argument_bytes`. A `__cdecl` import is cleaned up
/// by its own caller (`add esp, 4` right after the call), so the call site must
/// pop only the return address: doing a stdcall-style cleanup as well would
/// discard that `add` a second time and walk ESP a whole argument list past the
/// frame on every call.
fn pop_import_frame(out: &mut Vec<u8>, esp: u32, signature: wlam_pcode::abi::Signature) {
    let pop = match signature.convention {
        wlam_pcode::abi::Convention::Stdcall => signature.return_pop(),
        wlam_pcode::abi::Convention::Cdecl => 4,
    };
    out.push(op::LOCAL_GET);
    out.extend_from_slice(&leb_u32(esp));
    out.push(op::I32_CONST);
    out.extend_from_slice(&crate::encode::leb_s32(pop as i32));
    out.push(op::I32_ADD);
    out.push(op::LOCAL_SET);
    out.extend_from_slice(&leb_u32(esp));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_iat_tail_jump_is_recognised_as_a_host_slot() {
        // The exact shape Sleigh produces for `jmp dword ptr [0x4ca7b0]`:
        //   Copy T <- Const 0x4ca7b0
        //   IndirectJump Ram{offset: T}
        let temp = 292_361_142_272u64;
        let block = Block {
            start: 0x441203,
            end: 0x441213,
            ops: vec![
                crate::ir::IR::Copy {
                    dst: crate::ir::Operand::Temp {
                        offset: temp,
                        size: 4,
                    },
                    src: crate::ir::Operand::Const {
                        value: 0x4ca7b0,
                        size: 4,
                    },
                },
                crate::ir::IR::IndirectJump {
                    target: crate::ir::Operand::Ram {
                        offset: temp,
                        size: 4,
                    },
                },
            ],
            terminator: Terminator::IndirectJump,
        };
        assert_eq!(iat_slot_of(&block), Some(0x4ca7b0));
    }

    #[test]
    fn dispatcher_locals_follow_register_locals() {
        let locals = LocalMap::new([("EAX".to_string(), 4)]);
        assert_eq!(id_local(&locals), 1);
        assert_eq!(ret_local(&locals), 2);
    }

    /// Hover calls the C-runtime `memcpy` import through `call [0x4ca8e8]`
    /// and then does `add esp, 0xc`. The call site must pop only the return
    /// address it pushed; a stdcall-style `ret 12` on top of that `add` would
    /// move ESP past the frame on every copy.
    #[test]
    fn a_cdecl_import_pops_only_the_return_address() {
        use wlam_pcode::abi::{signature, Convention, Signature};
        assert_eq!(
            signature("MSVCRT.dll", "memcpy").map(|signature| signature.convention),
            Some(Convention::Cdecl),
            "memcpy is cdecl and has no stdcall cleanup"
        );
        let esp = 0;
        let mut out = Vec::new();
        pop_import_frame(
            &mut out,
            esp,
            Signature {
                convention: wlam_pcode::abi::Convention::Cdecl,
                argument_bytes: 12,
            },
        );
        // local.get 0; i32.const 4; i32.add; local.set 0
        assert_eq!(
            out,
            vec![
                op::LOCAL_GET,
                0,
                op::I32_CONST,
                4,
                op::I32_ADD,
                op::LOCAL_SET,
                0
            ]
        );
        let mut stdcall = Vec::new();
        pop_import_frame(
            &mut stdcall,
            esp,
            Signature {
                convention: wlam_pcode::abi::Convention::Stdcall,
                argument_bytes: 12,
            },
        );
        // stdcall: 4 (return address) + 12 (arguments)
        assert_eq!(
            stdcall,
            vec![
                op::LOCAL_GET,
                0,
                op::I32_CONST,
                16,
                op::I32_ADD,
                op::LOCAL_SET,
                0
            ]
        );
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
                        addr: crate::ir::Operand::Const {
                            value: 0x100,
                            size: 4,
                        },
                        value: crate::ir::Operand::Const { value: 0, size: 4 },
                        size: 4,
                    },
                    crate::ir::IR::Copy {
                        dst: crate::ir::Operand::Temp { offset: 1, size: 4 },
                        src: crate::ir::Operand::Const {
                            value: cond,
                            size: 4,
                        },
                    },
                    crate::ir::IR::CBranch {
                        cond: crate::ir::Operand::Temp { offset: 1, size: 4 },
                        target: crate::ir::Operand::Ram {
                            offset: 0x1010,
                            size: 4,
                        },
                    },
                ],
                terminator: Terminator::Conditional {
                    taken: Some(0x1010),
                    not_taken: 0x1014,
                },
            },
        );
        let leaf = |addr: u64, value: u64| Block {
            start: addr,
            end: addr + 8,
            ops: vec![crate::ir::IR::Store {
                addr: crate::ir::Operand::Const {
                    value: 0x104,
                    size: 4,
                },
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
        assert_eq!(
            at104, 0xB,
            "condition 0 must take the 'not_taken' leaf: {log}"
        );
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
        blocks.insert(
            0x1000u64,
            Block {
                start: 0x1000,
                end: 0x1008,
                ops: vec![
                    crate::ir::IR::Copy {
                        dst: crate::ir::Operand::Temp { offset: 1, size: 4 },
                        src: crate::ir::Operand::Const { value: 0, size: 4 },
                    },
                    crate::ir::IR::CBranch {
                        cond: crate::ir::Operand::Temp { offset: 1, size: 4 },
                        target: crate::ir::Operand::Ram {
                            offset: 0x1010,
                            size: 4,
                        },
                    },
                ],
                terminator: Terminator::Conditional {
                    taken: Some(0x1010),
                    not_taken: 0x1014,
                },
            },
        );
        for (a, v) in [(0x1010u64, 0xAu64), (0x1014, 0xB)] {
            blocks.insert(
                a,
                Block {
                    start: a,
                    end: a + 8,
                    ops: vec![crate::ir::IR::Store {
                        addr: crate::ir::Operand::Const {
                            value: 0x104,
                            size: 4,
                        },
                        value: crate::ir::Operand::Const { value: v, size: 4 },
                        size: 4,
                    }],
                    terminator: Terminator::Return,
                },
            );
        }
        let prog = partition(&[0x1000], &blocks, &regs);
        let m = build_module(&prog, &blocks).unwrap();
        let _ = m;
    }
}

#[cfg(test)]
mod four_block_tests {
    use super::*;

    /// A 4-block program shaped exactly like the failing lifted fixture:
    ///
    /// ```text
    /// 0x1000  block 0 — store 0 at 0x100, set temp 1 = cond, CBranch
    ///                   taken -> 0x100a   not_taken -> 0x1005
    /// 0x1005  block 1 — unconditional jump to 0x1015   (pure Branch)
    /// 0x1015  block 2 — store 0xB at 0x104, ret
    /// 0x100a  block 3 — store 0xA at 0x104, ret
    /// ```
    ///
    /// The BFS order that `partition` produces is 0x1000, 0x1005, 0x1015,
    /// 0x100a: so the *taken* leaf gets id 3 and the *not taken* leaf id 2.
    /// That is what makes this fixture different from the 3-block one, where
    /// the conditional's targets land on ids 1 and 2 in address order.
    fn four_block_program(cond: u64) -> (Program, BTreeMap<u64, Block>) {
        let regs = vec!["EAX".to_string()];
        let mut blocks = BTreeMap::new();
        blocks.insert(
            0x1000u64,
            Block {
                start: 0x1000,
                end: 0x1005,
                ops: vec![
                    crate::ir::IR::Store {
                        addr: crate::ir::Operand::Const {
                            value: 0x100,
                            size: 4,
                        },
                        value: crate::ir::Operand::Const { value: 0, size: 4 },
                        size: 4,
                    },
                    crate::ir::IR::Copy {
                        dst: crate::ir::Operand::Temp { offset: 1, size: 4 },
                        src: crate::ir::Operand::Const {
                            value: cond,
                            size: 4,
                        },
                    },
                    crate::ir::IR::CBranch {
                        cond: crate::ir::Operand::Temp { offset: 1, size: 4 },
                        target: crate::ir::Operand::Ram {
                            offset: 0x100a,
                            size: 4,
                        },
                    },
                ],
                terminator: Terminator::Conditional {
                    taken: Some(0x100a),
                    not_taken: 0x1005,
                },
            },
        );
        // The jump block: nothing but a Branch, like a lifted `jmp`.
        blocks.insert(
            0x1005u64,
            Block {
                start: 0x1005,
                end: 0x1007,
                ops: vec![crate::ir::IR::Branch {
                    target: crate::ir::Operand::Ram {
                        offset: 0x1015,
                        size: 4,
                    },
                }],
                terminator: Terminator::Jump(0x1015),
            },
        );
        let leaf = |addr: u64, value: u64| Block {
            start: addr,
            end: addr + 8,
            ops: vec![crate::ir::IR::Store {
                addr: crate::ir::Operand::Const {
                    value: 0x104,
                    size: 4,
                },
                value: crate::ir::Operand::Const { value, size: 4 },
                size: 4,
            }],
            terminator: Terminator::Return,
        };
        blocks.insert(0x1015, leaf(0x1015, 0xB));
        blocks.insert(0x100a, leaf(0x100a, 0xA));
        (partition(&[0x1000], &blocks, &regs), blocks)
    }

    fn run(module: &[u8]) -> (u32, u32, String) {
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("wlam-4blk-{}-{n}.wasm", std::process::id()));
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
    fn four_block_dispatch_order_is_what_we_think() {
        let (prog, _blocks) = four_block_program(1);
        let f = &prog.functions[0];
        assert_eq!(f.blocks, vec![0x1000, 0x1005, 0x1015, 0x100a], "BFS order");
        assert_eq!(f.ids[&0x1000], 0);
        assert_eq!(f.ids[&0x1005], 1);
        assert_eq!(f.ids[&0x1015], 2);
        assert_eq!(f.ids[&0x100a], 3, "taken leaf gets id 3");
    }

    #[test]
    fn four_block_condition_taken() {
        let (prog, blocks) = four_block_program(1);
        let m = build_module(&prog, &blocks).expect("module");
        let (at100, at104, log) = run(&m);
        assert_eq!(at100, 0, "block 0 must run: {log}");
        assert_eq!(at104, 0xA, "cond 1 must take the 0xA leaf: {log}");
    }

    #[test]
    fn four_block_condition_not_taken() {
        let (prog, blocks) = four_block_program(0);
        let m = build_module(&prog, &blocks).expect("module");
        let (at100, at104, log) = run(&m);
        assert_eq!(at100, 0, "block 0 must run: {log}");
        assert_eq!(
            at104, 0xB,
            "cond 0 must take the jump block, then 0xB: {log}"
        );
    }
}
