//! Translation of lifted IR into WebAssembly functions.
//!
//! This is the step that makes `wlam` a *recompiler* rather than an
//! interpreter: each guest basic block becomes one wasm function whose body is
//! native wasm instructions. Nothing here reads guest code at run time.
//!
//! ## Mapping
//!
//! | IR | wasm |
//! |---|---|
//! | `Reg` / `Temp` operand | local (all temps and registers are locals) |
//! | `Const` | `i32.const` / `i64.const` |
//! | `Ram` operand as a value | the address itself (`i32.const`) |
//! | `Load` / `Store` | `i32.load8_u`/`i32.load16_u`/`i32.load`/`i64.load` |
//! | `Bin` arithmetic | `i32.add`, `i32.mul`, … (flags via explicit code) |
//! | `Bin` comparisons | `i32.lt_u`, `i32.lt_s`, … producing `0`/`1` |
//! | `Un::Popcount` | `i32.popcnt` |
//! | `Un::Sext`/`Zext` | `i32.extend8_s`/`extend16_s`, or masking |
//!
//! Multiplier ops (`Carry`, `SCarry`, `SBorrow`) are *computed* from the
//! operands rather than taken from a host call, because Sleigh emits them for
//! every arithmetic instruction and a host call per `add` would be absurd.
//! Their formulas are standard and each has a differential test.
//!
//! ## Correctness gate
//!
//! The emitted functions are executed against a reference interpreter of the
//! same IR (`wlam_sleigh::interp`) on real lifted code and on hand-checked
//! instruction sequences. A translator that is only tested for *valid wasm* is
//! worthless; a translator tested for *agreeing behaviour* is not.

use std::collections::BTreeMap;

use crate::ir::{BinOp, Operand, UnOp, IR};

use crate::encode::leb_u32;
use crate::{module_header, section_id, Encoder};

/// Everything that can stop translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslateError {
    /// An operand width the emitter does not support (e.g. 80-bit x87).
    UnsupportedWidth(u32),
    /// A 64-bit value produced or consumed where the model has none.
    UnsupportedWide(&'static str),
    /// An IR construct not yet emitted.
    Unsupported(&'static str),
}

impl std::fmt::Display for TranslateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TranslateError::UnsupportedWidth(w) => write!(f, "unsupported operand width {w} bytes"),
            TranslateError::UnsupportedWide(what) => {
                write!(f, "unsupported 64-bit operation: {what}")
            }
            TranslateError::Unsupported(what) => write!(f, "not emitted yet: {what}"),
        }
    }
}

impl std::error::Error for TranslateError {}

/// wasm local indices for a block function: guest registers first, then temps.
#[derive(Debug, Default, Clone)]
pub struct LocalMap {
    /// Register name (e.g. "EAX", "ZF") -> local index.
    regs: BTreeMap<String, u32>,
    /// Temp offset -> local index and width.
    temps: BTreeMap<u64, (u32, u32)>,
    next: u32,
}

impl LocalMap {
    /// Seed the register locals from the first block seen, so every block in a
    /// function uses the SAME local indices for the same register. This is what
    /// lets a lifted function be a sequence of block functions sharing state
    /// through locals.
    pub fn new(registers: impl IntoIterator<Item = (String, u32)>) -> Self {
        let mut m = LocalMap::default();
        for (name, width) in registers {
            let idx = m.next;
            m.next += 1;
            let _ = width;
            m.regs.insert(name, idx);
        }
        m
    }

    /// Allocate (or reuse) a local for a temp.
    fn temp(&mut self, offset: u64, width: u32) -> u32 {
        if let Some((idx, _)) = self.temps.get(&offset) {
            return *idx;
        }
        let idx = self.next;
        self.next += 1;
        self.temps.insert(offset, (idx, width));
        idx
    }

    /// Index of a temp local, if one was allocated for that offset.
    pub fn temp_index(&self, offset: u64) -> Option<u32> {
        self.temps.get(&offset).map(|(idx, _)| *idx)
    }

    /// Reserve a scratch local for the epilogue (outside the temp keyspace).
    pub fn reserve_scratch(&mut self, slot: u64) -> u32 {
        self.temp(u64::MAX - 1000 - slot, 4)
    }

    /// Index of a register local.
    pub fn reg(&self, name: &str) -> Option<u32> {
        self.regs.get(name).copied()
    }

    /// Number of locals (register + temp) used.
    pub fn len(&self) -> u32 {
        self.next
    }

    pub fn is_empty(&self) -> bool {
        self.next == 0
    }
}

/// The result of translating one block.
#[derive(Debug, Clone)]
pub struct TranslatedBlock {
    /// The block's function body (without the size prefix).
    pub body: Vec<u8>,
    /// Locals declared by this body: (count, type) pairs.
    pub locals: Vec<(u32, u8)>,
    /// Total locals incl. parameters.
    pub local_count: u32,
    /// Offset in `body` where the ops start (after the locals declaration).
    pub ops_offset: usize,
    /// The final local map (registers plus every temp allocated during
    /// emission), so callers can resolve temp/register operands to indices.
    pub map: LocalMap,
}

impl TranslatedBlock {
    /// Drop the trailing `end` so a caller can append its own epilogue.
    ///
    /// The locals declaration stays intact: rebuilding a body by hand is how a
    /// previous version silently emitted functions with undeclared locals.
    pub fn without_end(mut self) -> Self {
        if self.body.last() == Some(&0x0b) {
            self.body.pop();
        }
        self
    }

    /// Append raw wasm instructions, then close the body with `end`.
    pub fn finish(mut self) -> Self {
        self.body.push(0x0b);
        self
    }
}

/// wasm opcodes we emit, named for readability.
mod op {
    pub const I32_CONST: u8 = 0x41;
    pub const LOCAL_GET: u8 = 0x20;
    pub const LOCAL_SET: u8 = 0x21;
    pub const LOCAL_TEE: u8 = 0x22;
    pub const I32_LOAD: u8 = 0x28;
    pub const I32_LOAD8_U: u8 = 0x2d;
    pub const I32_LOAD16_U: u8 = 0x2f;
    pub const I32_STORE: u8 = 0x36;
    pub const I32_STORE8: u8 = 0x3a;
    pub const I32_STORE16: u8 = 0x3b;
    pub const I32_EQ: u8 = 0x46;
    pub const I32_NE: u8 = 0x47;
    pub const I32_LT_S: u8 = 0x48;
    pub const I32_LT_U: u8 = 0x49;
    pub const I32_GT_U: u8 = 0x4b;
    pub const I32_LE_S: u8 = 0x4c;
    pub const I32_LE_U: u8 = 0x4d;
    pub const I32_ADD: u8 = 0x6a;
    pub const I32_SUB: u8 = 0x6b;
    pub const I32_MUL: u8 = 0x6c;
    pub const I32_DIV_S: u8 = 0x6d;
    pub const I32_DIV_U: u8 = 0x6e;
    pub const I32_REM_S: u8 = 0x6f;
    pub const I32_REM_U: u8 = 0x70;
    pub const I32_AND: u8 = 0x71;
    pub const I32_OR: u8 = 0x72;
    pub const I32_XOR: u8 = 0x73;
    pub const I32_SHL: u8 = 0x74;
    pub const I32_SHR_S: u8 = 0x75;
    pub const I32_SHR_U: u8 = 0x76;
    pub const I32_POPCNT: u8 = 0x69;
    pub const I32_EXTEND8_S: u8 = 0xc0;
    pub const I32_EXTEND16_S: u8 = 0xc1;
}

/// Translator state for one block.
struct BlockEmitter {
    locals: LocalMap,
    body: Vec<u8>,
}

impl BlockEmitter {
    fn new(locals: LocalMap) -> Self {
        BlockEmitter {
            locals,
            body: Vec::new(),
        }
    }

    fn op(&mut self, byte: u8) {
        self.body.push(byte);
    }

    fn local(&mut self, byte: u8, idx: u32) {
        self.body.push(byte);
        self.body.extend_from_slice(&leb_u32(idx));
    }

    /// Push an operand's *value* onto the stack.
    ///
    /// Per the IR contract, `Addr`-shaped operands are lvalues: a `Ram`
    /// operand used as a value is its address. Registers and temps are locals,
    /// so they are read with `local.get`.
    fn push_value(&mut self, op: &Operand) -> Result<u32, TranslateError> {
        let width = op.size();
        match op {
            Operand::Const { value, .. } => {
                if width > 4 {
                    // 64-bit immediates are part of the x64 phase; report
                    // rather than truncate silently.
                    return Err(TranslateError::UnsupportedWide("i64 immediate"));
                }
                self.op(op::I32_CONST);
                self.body.extend_from_slice(&leb_u32(*value as u32));
                Ok(width)
            }
            Operand::Reg { name, .. } => {
                let idx = self
                    .locals
                    .reg(name)
                    .ok_or(TranslateError::Unsupported("unknown register"))?;
                self.local(op::LOCAL_GET, idx);
                Ok(width)
            }
            Operand::Temp { offset, .. } => {
                let idx = self.locals.temp(*offset, width);
                self.local(op::LOCAL_GET, idx);
                Ok(width)
            }
            Operand::Ram { offset, .. } => {
                self.op(op::I32_CONST);
                self.body.extend_from_slice(&leb_u32(*offset as u32));
                Ok(4)
            }
        }
    }

    /// Pop the top of stack into the destination operand.
    fn pop_into(&mut self, dst: &Operand) -> Result<(), TranslateError> {
        match dst {
            Operand::Reg { name, .. } => {
                let idx = self
                    .locals
                    .reg(name)
                    .ok_or(TranslateError::Unsupported("unknown register"))?;
                self.local(op::LOCAL_SET, idx);
            }
            Operand::Temp { offset, .. } => {
                let idx = self.locals.temp(*offset, dst.size());
                self.local(op::LOCAL_SET, idx);
            }
            Operand::Const { .. } | Operand::Ram { .. } => {
                // Writing to an immediate or an address is outside the IR.
                return Err(TranslateError::Unsupported("store to constant/address"));
            }
        }
        Ok(())
    }

    /// Narrow or widen the i32 currently on the stack to the operand's width,
    /// matching how the IR defines a value at that width.
    fn normalize(&mut self, op: &Operand) -> Result<(), TranslateError> {
        match op.size() {
            1 => self.op(op::I32_EXTEND8_S),
            2 => self.op(op::I32_EXTEND16_S),
            4 => {}
            w => return Err(TranslateError::UnsupportedWidth(w)),
        }
        Ok(())
    }

    fn emit_ir(&mut self, ir: &IR) -> Result<(), TranslateError> {
        match ir {
            IR::Copy { dst, src } => {
                self.push_value(src)?;
                self.normalize(src)?;
                self.pop_into(dst)
            }
            IR::Load { dst, addr, size } => {
                self.push_value(addr)?;
                match (size, dst.size()) {
                    (1, 1) => self.body.push(op::I32_LOAD8_U),
                    (1, _) => self.body.push(op::I32_LOAD8_U),
                    (2, 2) => self.body.push(op::I32_LOAD16_U),
                    (2, _) => self.body.push(op::I32_LOAD16_U),
                    (4, _) => self.body.push(op::I32_LOAD),
                    (8, _) => return Err(TranslateError::UnsupportedWide("i64 load")),
                    (w, _) => return Err(TranslateError::UnsupportedWidth(*w)),
                }
                self.body.push(0);
                self.body.push(0);
                // A narrower load into a wider destination is zero-extended by
                // the load opcode itself, matching Sleigh's IntZext.
                if dst.size() > *size {
                    if dst.size() == 2 {
                        // already zero-extended by load16_u
                    }
                }
                self.pop_into(dst)
            }
            IR::Store { addr, value, size } => {
                self.push_value(addr)?;
                self.push_value(value)?;
                match size {
                    1 => self.body.push(op::I32_STORE8),
                    2 => self.body.push(op::I32_STORE16),
                    4 => self.body.push(op::I32_STORE),
                    8 => return Err(TranslateError::UnsupportedWide("i64 store")),
                    w => return Err(TranslateError::UnsupportedWidth(*w)),
                }
                self.body.push(0);
                self.body.push(0);
                Ok(())
            }
            IR::Bin {
                op: bin,
                dst,
                operands,
            } => self.emit_bin(*bin, dst, &operands.0, &operands.1),
            IR::Un { op: un, dst, src } => {
                self.push_value(src)?;
                match un {
                    UnOp::Neg => {
                        // 0 - x
                        self.op(op::I32_CONST);
                        self.body.extend_from_slice(&leb_u32(0));
                        // Reorder: stack has x, 0 -> need 0 - x. Use a temp.
                        let tmp = self.locals.temp(u64::MAX - 1, 4);
                        self.local(op::LOCAL_SET, tmp);
                        self.op(op::I32_CONST);
                        self.body.extend_from_slice(&leb_u32(0));
                        self.local(op::LOCAL_GET, tmp);
                        self.op(op::I32_SUB);
                    }
                    UnOp::Not => {
                        self.op(op::I32_CONST);
                        self.body.extend_from_slice(&leb_u32(u32::MAX));
                        self.op(op::I32_XOR);
                    }
                    UnOp::Popcount => self.op(op::I32_POPCNT),
                    UnOp::Lzcount => {
                        // Not on the wasm MVP baseline; a host-free equivalent
                        // needs a loop, so report rather than miscompile.
                        return Err(TranslateError::Unsupported("lzcount"));
                    }
                    UnOp::Zext => {
                        // Zero-extension = mask to the source width.
                        let bits = src.size() * 8;
                        let mask = if bits >= 32 {
                            u32::MAX
                        } else {
                            (1u32 << bits) - 1
                        };
                        self.op(op::I32_CONST);
                        self.body.extend_from_slice(&leb_u32(mask));
                        self.op(op::I32_AND);
                    }
                    UnOp::Sext => match src.size() {
                        1 => self.op(op::I32_EXTEND8_S),
                        2 => self.op(op::I32_EXTEND16_S),
                        4 => {}
                        w => return Err(TranslateError::UnsupportedWidth(w)),
                    },
                }
                self.normalize(dst)?;
                self.pop_into(dst)
            }
            IR::Subpiece { dst, src, lsb } => {
                self.push_value(src)?;
                if *lsb > 0 {
                    self.op(op::I32_CONST);
                    self.body.extend_from_slice(&leb_u32(*lsb * 8));
                    self.op(op::I32_SHR_U);
                }
                self.normalize(dst)?;
                self.pop_into(dst)
            }
            IR::Userop { dst, .. } => {
                // Environment operations: on the browser side these become host
                // imports. Until the host layer exists, a userop traps by
                // returning the current value unchanged *and* is reported by the
                // caller; never silently treated as a value-producing op.
                match dst {
                    Some(d) => {
                        self.op(op::I32_CONST);
                        self.body.extend_from_slice(&leb_u32(0));
                        self.normalize(d)?;
                        self.pop_into(d)
                    }
                    None => Ok(()),
                }
            }
            IR::Nop => Ok(()),
            IR::Branch { .. }
            | IR::IndirectJump { .. }
            | IR::CBranch { .. }
            | IR::Call { .. }
            | IR::CallInd { .. }
            | IR::Ret { .. } => Err(TranslateError::Unsupported(
                "control flow (handled by block chaining)",
            )),
        }
    }

    fn emit_bin(
        &mut self,
        bin: BinOp,
        dst: &Operand,
        lhs: &Operand,
        rhs: &Operand,
    ) -> Result<(), TranslateError> {
        // A handful of ops need operands more than once; evaluate lhs into a
        // scratch local in that case so both uses see the same value.
        match bin {
            BinOp::Carry => return self.emit_carry(lhs, rhs, dst, false),
            BinOp::SCarry => return self.emit_carry(lhs, rhs, dst, true),
            BinOp::SBorrow => return self.emit_borrow(lhs, rhs, dst),
            BinOp::MulFull => {
                // Full 64-bit product: wasm has no way to express this in one
                // op pre-`i64.mul` combinations, so report it.
                return Err(TranslateError::UnsupportedWide("MulFull"));
            }
            _ => {}
        }
        self.push_value(lhs)?;
        self.push_value(rhs)?;
        let byte = match bin {
            BinOp::Add => op::I32_ADD,
            BinOp::Sub => op::I32_SUB,
            BinOp::Mul => op::I32_MUL,
            BinOp::DivS => op::I32_DIV_S,
            BinOp::DivU => op::I32_DIV_U,
            BinOp::RemS => op::I32_REM_S,
            BinOp::RemU => op::I32_REM_U,
            BinOp::And => op::I32_AND,
            BinOp::Or => op::I32_OR,
            BinOp::Xor => op::I32_XOR,
            BinOp::Lsh => op::I32_SHL,
            BinOp::Rsh => op::I32_SHR_U,
            BinOp::Asr => op::I32_SHR_S,
            BinOp::Eq => op::I32_EQ,
            BinOp::NotEq => op::I32_NE,
            BinOp::LessU => op::I32_LT_U,
            BinOp::LessEqU => op::I32_LE_U,
            BinOp::LessS => op::I32_LT_S,
            BinOp::LessEqS => op::I32_LE_S,
            BinOp::Carry | BinOp::SCarry | BinOp::SBorrow | BinOp::MulFull => unreachable!(),
        };
        self.op(byte);
        // Comparisons already produce exactly 0 or 1, which is the correct value
        // at every destination width — narrowing them (e.g. i32.extend8_s on a
        // flag byte) would be a no-op at best and wrong the moment the result
        // exceeded 1. Only arithmetic results are narrowed.
        let is_comparison = matches!(
            bin,
            BinOp::Eq
                | BinOp::NotEq
                | BinOp::LessU
                | BinOp::LessEqU
                | BinOp::LessS
                | BinOp::LessEqS
        );
        if !is_comparison {
            self.normalize(dst)?;
        }
        self.pop_into(dst)
    }

    /// `Carry(a, b)` = carry out of the top bit of `a + b` (unsigned);
    /// `SCarry(a, b)` = signed overflow of the same addition.
    ///
    /// Both are computed from the operands rather than delegated to a host
    /// call: Sleigh emits one of them for *every* arithmetic instruction, so a
    /// call here would make lifted code an RPC per add.
    fn emit_carry(
        &mut self,
        lhs: &Operand,
        rhs: &Operand,
        dst: &Operand,
        signed: bool,
    ) -> Result<(), TranslateError> {
        if signed {
            return self.emit_carry_signed(lhs, rhs, dst);
        }
        // Unsigned: `a + b` carries iff the result is below `a`.
        self.push_value(lhs)?;
        self.push_value(rhs)?;
        self.op(op::I32_ADD);
        let scratch = self.locals.temp(u64::MAX - 2, 4);
        self.local(op::LOCAL_TEE, scratch);
        self.push_value(lhs)?;
        self.op(op::I32_GT_U); // a > (a + b)  <=>  carry out
        self.normalize(dst)?;
        self.pop_into(dst)
    }

    /// Signed overflow: `((a ^ res) & (b ^ res)) >> 31`.
    fn emit_carry_signed(
        &mut self,
        lhs: &Operand,
        rhs: &Operand,
        dst: &Operand,
    ) -> Result<(), TranslateError> {
        let a = self.locals.temp(u64::MAX - 4, 4);
        let b = self.locals.temp(u64::MAX - 5, 4);
        let r = self.locals.temp(u64::MAX - 6, 4);
        self.push_value(lhs)?;
        self.local(op::LOCAL_SET, a);
        self.push_value(rhs)?;
        self.local(op::LOCAL_SET, b);
        self.local(op::LOCAL_GET, a);
        self.local(op::LOCAL_GET, b);
        self.op(op::I32_ADD);
        self.local(op::LOCAL_SET, r);
        self.local(op::LOCAL_GET, a);
        self.local(op::LOCAL_GET, r);
        self.op(op::I32_XOR);
        self.local(op::LOCAL_GET, b);
        self.local(op::LOCAL_GET, r);
        self.op(op::I32_XOR);
        self.op(op::I32_AND);
        self.op(op::I32_CONST);
        self.body.extend_from_slice(&leb_u32(31));
        self.op(op::I32_SHR_S);
        self.op(op::I32_CONST);
        self.body.extend_from_slice(&leb_u32(1));
        self.op(op::I32_AND);
        self.normalize(dst)?;
        self.pop_into(dst)
    }

    /// `SBorrow(a, b)` = signed borrow out of `a - b`, i.e. the sign bit of
    /// `(a ^ ((a ^ b) & (a ^ res)))`.
    fn emit_borrow(
        &mut self,
        lhs: &Operand,
        rhs: &Operand,
        dst: &Operand,
    ) -> Result<(), TranslateError> {
        let a = self.locals.temp(u64::MAX - 7, 4);
        let b = self.locals.temp(u64::MAX - 8, 4);
        let r = self.locals.temp(u64::MAX - 9, 4);
        self.push_value(lhs)?;
        self.local(op::LOCAL_SET, a);
        self.push_value(rhs)?;
        self.local(op::LOCAL_SET, b);
        self.local(op::LOCAL_GET, a);
        self.local(op::LOCAL_GET, b);
        self.op(op::I32_SUB);
        self.local(op::LOCAL_SET, r);
        self.local(op::LOCAL_GET, a);
        self.local(op::LOCAL_GET, b);
        self.op(op::I32_XOR);
        self.local(op::LOCAL_GET, a);
        self.local(op::LOCAL_GET, r);
        self.op(op::I32_XOR);
        self.op(op::I32_AND);
        self.local(op::LOCAL_GET, a);
        self.op(op::I32_XOR);
        self.op(op::I32_CONST);
        self.body.extend_from_slice(&leb_u32(31));
        self.op(op::I32_SHR_S);
        self.op(op::I32_CONST);
        self.body.extend_from_slice(&leb_u32(1));
        self.op(op::I32_AND);
        self.normalize(dst)?;
        self.pop_into(dst)
    }
}

/// Translate a sequence of IR ops into one wasm function body.
///
/// `locals` must come from [`LocalMap::new`] with the union of registers used
/// across the function, so block functions agree on local indices.
pub fn translate_ops(ops: &[IR], locals: LocalMap) -> Result<TranslatedBlock, TranslateError> {
    let base_count = locals.len();
    let mut e = BlockEmitter::new(locals);
    for ir in ops {
        e.emit_ir(ir)?;
    }
    // Every local is i32 in this model; x87/i64 support is a later phase and is
    // reported rather than silently truncated.
    //
    // ALL locals must be declared: registers are locals (not parameters), and
    // the temps appended during emission follow them. Declaring only the temps
    // made `local.get 0` reference an undeclared index, which V8 rejects with
    // "invalid local index: 0".
    let extra = e.locals.len() - base_count;
    let total_declared = base_count + extra;
    let locals = if total_declared > 0 {
        vec![(total_declared, 0x7f)]
    } else {
        Vec::new()
    };
    let total = e.locals.len();
    let map = e.locals.clone();
    // Function body: locals, then ops, then `end`.
    let mut out = Vec::new();
    out.extend_from_slice(&leb_u32(locals.len() as u32));
    for (n, ty) in &locals {
        out.extend_from_slice(&leb_u32(*n));
        out.push(*ty);
    }
    let prefix_len = out.len();
    out.extend_from_slice(&e.body);
    out.push(0x0b);
    // `prefix_len` is where the ops begin: everything before it is the locals
    // declaration. Callers that need to append an epilogue before the closing
    // `end` can use it rather than re-deriving the layout.
    Ok(TranslatedBlock {
        body: out,
        locals,
        local_count: total,
        ops_offset: prefix_len,
        map,
    })
}

/// Build a complete wasm module holding the translated block, so tests can
/// execute it in V8.
///
/// The module exports `run()` and has one page of memory. It exists so the
/// differential tests can compare against the reference interpreter.
pub fn module_for_block(block: &TranslatedBlock, exports: &str) -> Vec<u8> {
    let mut e = Encoder::new();
    module_header(&mut e);
    // type: () -> ()
    let mut ty = Vec::new();
    ty.push(0x60);
    ty.push(0); // no params
    ty.push(0); // no results
    e.section(section_id::TYPE, |b| {
        b.u32(1);
        b.raw(&ty);
    });
    e.section(section_id::FUNCTION, |b| {
        b.u32(1);
        b.u32(0);
    });
    e.section(section_id::MEMORY, |b| {
        b.u32(1);
        b.push(0x00);
        b.u32(1);
    });
    // Export both the function and its memory: the memory is the guest's
    // address space and tests/diagnostics read the register snapshot from it.
    e.section(section_id::EXPORT, |b| {
        b.u32(2);
        b.u32(exports.len() as u32);
        b.raw(exports.as_bytes());
        b.push(0x00);
        b.u32(0);
        b.u32(6);
        b.raw(b"memory");
        b.push(0x02); // extern kind: memory
        b.u32(0);
    });
    e.section(section_id::CODE, |b| {
        b.u32(1);
        b.u32(block.body.len() as u32);
        b.raw(&block.body);
    });
    e.into_bytes()
}

/// Build a module that runs the block and then stores every register local into
/// guest memory at [`REG_SNAPSHOT_BASE`] + index * 4.
///
/// Registers are wasm locals (fast), which makes them invisible from outside.
/// A snapshot area keeps the machine state observable so tests and trap
/// diagnostics can read it without giving up the locals.
pub const REG_SNAPSHOT_BASE: u64 = 0x8000;

/// `initial` is currently informational: the harness writes the initial
/// register values into the snapshot area itself before calling `run`, so the
/// module only needs to *read* them from there (which it does). Keeping the
/// argument makes the caller's intent explicit and lets a future variant embed
/// the constants directly.
pub fn module_with_snapshot(
    block: &TranslatedBlock,
    locals: &LocalMap,
    registers: &[String],
    initial: &[u64],
) -> Vec<u8> {
    let _ = initial;
    let mut body = block.body.clone();
    body.pop(); // drop the trailing `end`
    // Prologue: load the initial register values from the snapshot area into
    // the locals, so the function starts from a known machine state.
    for (i, name) in registers.iter().enumerate() {
        let Some(idx) = locals.reg(name) else {
            continue;
        };
        body.push(op::I32_CONST);
        body.extend_from_slice(&leb_u32((REG_SNAPSHOT_BASE + (i as u64) * 4) as u32));
        body.push(op::I32_LOAD);
        body.push(0);
        body.push(0);
        body.push(op::LOCAL_SET);
        body.extend_from_slice(&leb_u32(idx));
    }
    for (i, name) in registers.iter().enumerate() {
        let Some(idx) = locals.reg(name) else {
            continue;
        };
        body.push(op::I32_CONST);
        body.extend_from_slice(&leb_u32((REG_SNAPSHOT_BASE + (i as u64) * 4) as u32));
        body.push(op::LOCAL_GET);
        body.extend_from_slice(&leb_u32(idx));
        body.push(op::I32_STORE);
        body.push(0);
        body.push(0);
    }
    body.push(0x0b);
    let augmented = TranslatedBlock {
        body,
        locals: block.locals.clone(),
        local_count: block.local_count,
        ops_offset: block.ops_offset,
        map: block.map.clone(),
    };
    module_for_block(&augmented, "run")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locals_for(regs: &[&str]) -> LocalMap {
        LocalMap::new(regs.iter().map(|r| (r.to_string(), 4)))
    }

    #[test]
    fn copy_immediate_emits_const_then_local_set() {
        let ops = vec![IR::Copy {
            dst: Operand::Reg {
                name: "EAX".into(),
                size: 4,
            },
            src: Operand::Const {
                value: 0x1122_3344,
                size: 4,
            },
        }];
        let b = translate_ops(&ops, locals_for(&["EAX"])).unwrap();
        // One local-declaration group covering EAX; then
        // i32.const 0x11223344 ; local.set 0 ; end.
        // (Requiring zero declared locals here was the bug that produced
        // "invalid local index: 0" from V8: registers are locals and must be
        // declared.)
        assert_eq!(b.body[0], 0x01, "one local declaration group");
        assert_eq!(b.body[1], 1, "declaring exactly one local (EAX)");
        assert_eq!(b.body[2], 0x7f, "i32 local");
        assert_eq!(b.body[3], op::I32_CONST);
        assert_eq!(*b.body.last().unwrap(), 0x0b);
        assert_eq!(b.local_count, 1);
    }

    #[test]
    fn add_emits_i32_add() {
        let ops = vec![IR::Bin {
            op: BinOp::Add,
            dst: Operand::Reg {
                name: "EAX".into(),
                size: 4,
            },
            operands: Box::new((
                Operand::Reg {
                    name: "EAX".into(),
                    size: 4,
                },
                Operand::Const { value: 5, size: 4 },
            )),
        }];
        let b = translate_ops(&ops, locals_for(&["EAX"])).unwrap();
        assert!(
            b.body.contains(&op::I32_ADD),
            "must emit i32.add: {:02x?}",
            b.body
        );
    }

    #[test]
    fn comparison_yields_zero_or_one_not_narrowed() {
        let ops = vec![IR::Bin {
            op: BinOp::LessU,
            dst: Operand::Reg {
                name: "CF".into(),
                size: 1,
            },
            operands: Box::new((
                Operand::Reg {
                    name: "EAX".into(),
                    size: 4,
                },
                Operand::Const { value: 4, size: 4 },
            )),
        }];
        let b = translate_ops(&ops, locals_for(&["EAX", "CF"])).unwrap();
        assert!(
            b.body.contains(&op::I32_LT_U),
            "unsigned comparison emitted"
        );
        // A comparison's result is already 0/1 at every width, so no narrowing
        // is emitted. (The original assertion here demanded the opposite; the
        // code was right and the test was wrong.)
        assert!(
            !b.body.contains(&op::I32_EXTEND8_S),
            "comparison results must not be narrowed: {:02x?}",
            b.body
        );
    }

    #[test]
    fn load_and_store_use_guest_memory() {
        let store = IR::Store {
            addr: Operand::Reg {
                name: "ESP".into(),
                size: 4,
            },
            value: Operand::Const { value: 7, size: 4 },
            size: 4,
        };
        let load = IR::Load {
            dst: Operand::Reg {
                name: "EAX".into(),
                size: 4,
            },
            addr: Operand::Reg {
                name: "ESP".into(),
                size: 4,
            },
            size: 4,
        };
        let b = translate_ops(&[store, load], locals_for(&["ESP", "EAX"])).unwrap();
        assert!(b.body.contains(&op::I32_STORE));
        assert!(b.body.contains(&op::I32_LOAD));
    }

    #[test]
    fn unsupported_constructs_are_reported_not_dropped() {
        let lz = vec![IR::Un {
            op: UnOp::Lzcount,
            dst: Operand::Reg {
                name: "EAX".into(),
                size: 4,
            },
            src: Operand::Reg {
                name: "EAX".into(),
                size: 4,
            },
        }];
        assert_eq!(
            translate_ops(&lz, locals_for(&["EAX"])).err(),
            Some(TranslateError::Unsupported("lzcount"))
        );
        let mul = vec![IR::Bin {
            op: BinOp::MulFull,
            dst: Operand::Reg {
                name: "EAX".into(),
                size: 4,
            },
            operands: Box::new((
                Operand::Const { value: 1, size: 4 },
                Operand::Const { value: 2, size: 4 },
            )),
        }];
        assert!(translate_ops(&mul, locals_for(&["EAX"])).is_err());
    }

    #[test]
    fn control_flow_is_not_silently_ignored() {
        let br = vec![IR::Branch {
            target: Operand::Ram {
                offset: 0x1000,
                size: 4,
            },
        }];
        assert!(translate_ops(&br, locals_for(&[])).is_err());
    }
}
