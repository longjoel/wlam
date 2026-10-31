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
    /// A Sleigh userop the emitter has no implementation for.
    UnimplementedUserop(u64),
    /// An IR construct not yet emitted.
    Unsupported(&'static str),
    /// A register the program's local map does not declare.
    ///
    /// Carries the name: "unknown register" with no name cost a debugging round
    /// on two different images.
    UnknownRegister(String),
}

impl std::fmt::Display for TranslateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TranslateError::UnsupportedWidth(w) => write!(f, "unsupported operand width {w} bytes"),
            TranslateError::UnknownRegister(n) => write!(f, "register {n} is not in the local map"),
            TranslateError::UnsupportedWide(what) => {
                write!(f, "unsupported 64-bit operation: {what}")
            }
            TranslateError::Unsupported(what) => write!(f, "not emitted yet: {what}"),
            TranslateError::UnimplementedUserop(id) => {
                write!(f, "unimplemented userop {id} (x87 microcode)")
            }
        }
    }
}

impl std::error::Error for TranslateError {}

/// Temp-key bases for the emitter's own scratch locals.
///
/// Guest temp offsets are *addresses* (large but far below these), so the
/// scratch keyspace sits at the very top of `u64` and cannot collide with a
/// block's own temporaries. Keeping the two ranges explicit and distinct is
/// what stops a scratch local from being handed out twice as a guest temp --
/// which would clobber the dispatch id and spin the dispatcher.
const SCRATCH_KEY_BASE: u64 = u64::MAX - 999;
const SUBREG_KEY_BASE: u64 = u64::MAX - 2999;

/// Whether a register name is part of the x87 stack.
///
/// This is THE single decision point for "is this value an f64?". It is answered
/// by the register *name*, not by width or by tracking, because the x87 stack is
/// a fixed set of eight known registers: their type is known at declaration time
/// and never changes. An earlier attempt spread this across a width heuristic, a
/// per-block pre-pass and a per-temp flag; they disagreed and produced thousands
/// of "expected [f64] but got [i32]" errors.
pub fn is_x87_register(name: &str) -> bool {
    crate::X87_LOCALS.iter().any(|r| *r == name)
}

/// wasm local indices for a block function: guest registers first, then temps.
#[derive(Debug, Default, Clone)]
pub struct LocalMap {
    /// Register name (e.g. "EAX", "ZF") -> local index.
    regs: BTreeMap<String, u32>,
    /// Temp offset -> local index and width.
    temps: BTreeMap<u64, (u32, u32)>,
    /// Local index -> declared width in bytes (4 -> i32, 8 -> i64/f64).
    ///
    /// wasm locals are typed at declaration, so the width has to be carried
    /// alongside the index: an 8-byte temp cannot live in an i32 local.
    widths: BTreeMap<u32, u32>,
    /// Local indices belonging to the x87 stack (`ST0`-`ST7`), declared `f64`.
    ///
    /// Populated once, in [`LocalMap::new`], from the register name. Nothing else
    /// may add to it: keeping a single source is what makes the type coherent.
    x87: std::collections::BTreeSet<u32>,
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
            m.widths.insert(idx, width);
            if is_x87_register(&name) {
                m.x87.insert(idx);
            }
            m.regs.insert(name, idx);
        }
        m
    }

    /// Allocate (or reuse) a local for a temp.
    fn temp(&mut self, offset: u64, width: u32) -> u32 {
        if let Some((idx, _)) = self.temps.get(&offset) {
            return *idx;
        }
        // A 10-byte temp is an x87 EXTENDED intermediate, held as an f64. It
        // joins the x87 set so every consumer types it consistently.
        let is_x87 = width == 10;
        let stored = if is_x87 { 8 } else { width };
        let idx = self.next;
        self.next += 1;
        self.widths.insert(idx, stored);
        if is_x87 {
            self.x87.insert(idx);
        }
        self.temps.insert(offset, (idx, stored));
        idx
    }

    /// Whether a local is an x87 stack register (an `f64`).
    pub fn is_x87(&self, idx: u32) -> bool {
        self.x87.contains(&idx)
    }

    /// Mark an allocated local as holding a real value.
    pub fn mark_x87(&mut self, idx: u32) {
        self.x87.insert(idx);
    }

    /// Declared width of a local, in bytes.
    pub fn width(&self, idx: u32) -> u32 {
        self.widths.get(&idx).copied().unwrap_or(4)
    }

    /// Consecutive runs of same-typed locals, in local-index order.
    ///
    /// wasm assigns local indices in declaration order, so the declaration MUST
    /// list locals in index order. Grouping by type instead would renumber every
    /// local and silently point `local.get` at the wrong slot. Runs are simply
    /// maximal spans of equal type: `(0x7f, count)` for i32, `(0x7e, count)`
    /// for i64.
    pub fn declaration_runs(&self) -> Vec<(u8, u32)> {
        let mut runs: Vec<(u8, u32)> = Vec::new();
        for idx in 0..self.next {
            let ty = if self.x87.contains(&idx) {
                0x7c // f64: an x87 stack register
            } else if self.width(idx) == 8 {
                0x7e // i64
            } else {
                0x7f // i32
            };
            match runs.last_mut() {
                Some((last_ty, count)) if *last_ty == ty => *count += 1,
                _ => runs.push((ty, 1)),
            }
        }
        runs
    }

    /// Index of a temp local, if one was allocated for that offset.
    pub fn temp_index(&self, offset: u64) -> Option<u32> {
        self.temps.get(&offset).map(|(idx, _)| *idx)
    }

    /// Reserve a scratch local for the epilogue (outside the temp keyspace).
    pub fn reserve_scratch(&mut self, slot: u64) -> u32 {
        self.temp(SCRATCH_KEY_BASE - slot, 4)
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
    pub const CALL: u8 = 0x10;
    pub const I32_WRAP_I64: u8 = 0xa7;
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

    // ---- i64 (0x7e locals) ----
    // 64-bit integer temporaries come from x86-32 itself: `MUL` yields
    // EDX:EAX, i.e. a dword x dword -> qword product.
    pub const I64_EXTEND_I32_S: u8 = 0xac;
    pub const I64_EXTEND_I32_U: u8 = 0xad;
    pub const I64_ADD: u8 = 0x7c;
    pub const I64_SUB: u8 = 0x7d;
    pub const I64_MUL: u8 = 0x7e;
    pub const I64_DIV_S: u8 = 0x7f;
    pub const I64_DIV_U: u8 = 0x80;
    pub const I64_REM_S: u8 = 0x81;
    pub const I64_REM_U: u8 = 0x82;
    pub const I64_AND: u8 = 0x83;
    pub const I64_OR: u8 = 0x84;
    pub const I64_XOR: u8 = 0x85;
    pub const I64_SHL: u8 = 0x86;
    pub const I64_SHR_S: u8 = 0x87;
    pub const I64_SHR_U: u8 = 0x88;
    pub const I64_EQ: u8 = 0x51;
    pub const I64_NE: u8 = 0x52;
    pub const I64_LT_S: u8 = 0x53;
    pub const I64_LT_U: u8 = 0x54;
    pub const I64_LE_S: u8 = 0x57;
    pub const I64_LE_U: u8 = 0x58;
    pub const I64_GT_U: u8 = 0x56;
    pub const I64_WRAP_I32: u8 = 0xa7;
    pub const I64_CONST: u8 = 0x42;

    // ---- f64 (0x7c locals) ----
    // x87 `ST0`-`ST7`, modelled as doubles (see `X87_LOCALS`). These are the
    // real-number instructions; they must never be emitted for integer IR.
    pub const I64_LOAD: u8 = 0x29;
    pub const I64_STORE: u8 = 0x37;
    pub const F32_LOAD: u8 = 0x2a;
    pub const F64_LOAD: u8 = 0x2b;
    pub const F64_STORE: u8 = 0x39;
    pub const F64_CONST: u8 = 0x44;
    pub const F64_ADD: u8 = 0xa0;
    pub const F64_SUB: u8 = 0xa1;
    pub const F64_MUL: u8 = 0xa2;
    pub const F64_DIV: u8 = 0xa3;
    pub const F64_EQ: u8 = 0x61;
    pub const F64_NE: u8 = 0x62;
    pub const F64_LT: u8 = 0x63;
    pub const F64_LE: u8 = 0x65;
    pub const F64_GT: u8 = 0x64;
    pub const F64_GE: u8 = 0x66;
    /// `f64.convert_i32_s` / `f64.convert_i64_s`: integer source to double.
    pub const F64_CONVERT_I32_S: u8 = 0xb7;
    pub const F64_CONVERT_I64_S: u8 = 0xb9;
    /// `f64.promote_f32`.
    pub const F64_PROMOTE_F32: u8 = 0xbb;
    /// `i32.trunc_f64_s`: double to integer, trapping on overflow/NaN.
    pub const I32_TRUNC_F64_S: u8 = 0xaa;
    pub const I64_TRUNC_F64_S: u8 = 0xb0;
}

/// Translator state for one block.
struct BlockEmitter {
    locals: LocalMap,
    body: Vec<u8>,
    /// Host-import name -> function index, for calls the emitter makes itself
    /// (the x87 `sin`/`cos` helpers).
    import_index: std::collections::BTreeMap<crate::chain::HostImport, u32>,
}

impl BlockEmitter {
    fn new(locals: LocalMap) -> Self {
        BlockEmitter {
            import_index: std::collections::BTreeMap::new(),
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

    /// Emit an `i32.const` immediate.
    ///
    /// `i32.const` takes a SIGNED LEB128, so anything with the high bit set
    /// (a clear-mask like `!0xFF`) must go through the signed encoding: written
    /// unsigned it is the right number of bits in the wrong shape and the
    /// validator rejects it as "extra bits in varint".
    fn push_i32_const(&mut self, v: u32) {
        self.body
            .extend_from_slice(&crate::encode::leb_s32(v as i32));
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
                    self.op(op::I64_CONST);
                    // Signed LEB128, via the shared encoder: an 8-byte constant
                    // must be written as the signed value it denotes. The old
                    // hand-rolled loop shifted a u64 *logically*, so a negative
                    // constant never hit the terminator and the validator
                    // rejected the varint.
                    self.body
                        .extend_from_slice(&crate::encode::leb_s64(*value as i64));
                    return Ok(width);
                }
                self.op(op::I32_CONST);
                // i32.const is SIGNED leb128; a negative constant written
                // unsigned is the right bits at the wrong length and desyncs
                // the whole instruction stream.
                self.body.extend_from_slice(&crate::encode::leb_s32(*value as u32 as i32));
                Ok(width)
            }
            Operand::Reg { name, .. } => {
                // A sub-register is a window into its 32-bit parent: read the
                // parent, shift the window down, mask off the rest. Without
                // this, `mov dl, ah` reads whatever stale local `AH` happened
                // to hold instead of bits 8..16 of EAX.
                if let Some(slot) = crate::regfile_slot(name) {
                    if slot.width < 4 {
                        let parent = slot.parent.name();
                        let idx = self.locals.reg(parent).ok_or_else(|| {
                            TranslateError::UnknownRegister(parent.to_string())
                        })?;
                        self.local(op::LOCAL_GET, idx);
                        if slot.shift() > 0 {
                            self.op(op::I32_CONST);
                            self.push_i32_const(slot.shift());
                            self.op(op::I32_SHR_U);
                        }
                        // Masking is not strictly required for 1-byte reads of
                        // AL/DL/BL (the callers normalize), but it keeps the
                        // value's high bits defined for 2-byte windows and for
                        // AH, where the shift alone leaves garbage above bit 7.
                        self.op(op::I32_CONST);
                        self.push_i32_const((1u32 << (slot.width * 8)) - 1);
                        self.op(op::I32_AND);
                        return Ok(width);
                    }
                }
                let idx = self
                    .locals
                    .reg(name)
                    .ok_or_else(|| TranslateError::UnknownRegister(name.to_string()))?;
                self.local(op::LOCAL_GET, idx);
                // An x87 stack register is an f64 local. Report 8, not the
                // P-code's 10-byte EXTENDED size: callers type-check against the
                // wasm value they actually get.
                if self.locals.is_x87(idx) {
                    return Ok(8);
                }
                Ok(width)
            }
            Operand::Temp { offset, .. } => {
                let idx = self.locals.temp(*offset, width);
                self.local(op::LOCAL_GET, idx);
                if self.locals.is_x87(idx) {
                    return Ok(8);
                }
                // Report the local's DECLARED width, not the width this use
                // happens to mention. A temp first seen at 8 bytes is an i64
                // local; a later 4-byte use still pushes an i64, and telling the
                // caller otherwise made it emit `i32.and` on an i64 value.
                        Ok(self.locals.width(idx))
            }
            Operand::Ram { offset, .. } => {
                self.op(op::I32_CONST);
                self.body.extend_from_slice(&crate::encode::leb_s32(*offset as u32 as i32));
                Ok(4)
            }
        }
    }

    /// Pop the top of stack into the destination operand.
    ///
    /// A write to a sub-register (`AL`, `AX`, `AH`, ...) is a read-modify-write
    /// of its 32-bit parent. Sleigh models `mov dl, ah` as
    /// `Copy { dst: Reg("DL",1), src: Reg("AH",1) }`, i.e. `DL` and `AH` as
    /// independent 1-byte registers. Wiring each name to its own wasm local
    /// makes that copy write a local nothing reads, so the following
    /// `mov [slot], edx` stores the *stale* EDX. That is exactly the Uxn32 bug
    /// where `GetVersion` succeeds but the guest records major version 0.
    fn pop_into(&mut self, dst: &Operand) -> Result<(), TranslateError> {
        self.pop_into_width(dst, dst.size())
    }

    /// `pop_into`, told the width of the value actually on the stack.
    ///
    /// A `Copy` from a 32-bit register into a qword temp pushes an i32 while the
    /// destination claims 8 bytes; comparing the destination's nominal size to
    /// its declared size finds them equal and emits no conversion, leaving an i64
    /// value where an i32 is expected. Only the producer knows the real width.
    fn pop_into_width(&mut self, dst: &Operand, pushed: u32) -> Result<(), TranslateError> {
        match dst {
            Operand::Reg { name, size } => {
                // A wider-than-4-byte register (x87, 8-byte) has no parent.
                if let Some(slot) = crate::regfile_slot(name) {
                    if slot.width < 4 {
                        return self.pop_into_sub_register(slot);
                    }
                }
                let _ = size;
                let idx = self
                    .locals
                    .reg(name)
                    .ok_or_else(|| TranslateError::UnknownRegister(name.to_string()))?;
                self.local(op::LOCAL_SET, idx);
            }
            Operand::Temp { offset, size } => {
                // A temp's wasm type is fixed when it is first allocated, but
                // Sleigh reuses one temp at different widths (a narrow flag
                // written into a wide scratch, or vice versa). Convert to the
                // declared type here instead of assuming they match: storing an
                // i32 into an i64 local is a validation error, and that was the
                // source of the remaining "expected [i64] but got [i32]"
                // failures.
                let idx = self.locals.temp(*offset, dst.size());
                let declared = self.locals.width(idx);
                // An f64 destination takes the value as-is: `IntToFloat` already
                // produced a double, and widening it as if it were an integer is
                // the "i64.extend_i32_u on an f64" error.
                if self.locals.is_x87(idx) {
                    self.local(op::LOCAL_SET, idx);
                    return Ok(());
                }
                // Use the width of the value actually on the stack. `dst.size()`
                // is what the P-code nominally says, and a `Copy` from a 32-bit
                // register into a qword temp pushes an i32 while claiming 8 --
                // comparing nominal to declared then finds them equal and emits
                // no conversion, leaving an i64 value in an i32 slot.
                let _ = size;
                        if declared == 8 && pushed != 8 {
                    self.op(op::I64_EXTEND_I32_U);
                } else if declared != 8 && pushed == 8 {
                    self.op(op::I32_WRAP_I64);
                }
                self.local(op::LOCAL_SET, idx);
            }
            Operand::Const { .. } | Operand::Ram { .. } => {
                // Writing to an immediate or an address is outside the IR.
                return Err(TranslateError::Unsupported("store to constant/address"));
            }
        }
        Ok(())
    }

    /// Store the i32 on the stack into a sub-register, leaving the parent's
    /// other bytes untouched: `parent = (parent & !mask) | ((value << shift) & mask)`.
    ///
    /// The value being written is already in the stack slot (a local), so the
    /// sequence needs no scratch beyond the parent's own local.
    fn pop_into_sub_register(&mut self, slot: crate::RegSlot) -> Result<(), TranslateError> {
        let parent = slot.parent.name();
        let parent_idx = self
            .locals
            .reg(parent)
            .ok_or_else(|| TranslateError::UnknownRegister(parent.to_string()))?;

        // Mask the incoming value and move it into position. Keeping it in a
        // temp means the read-modify-write cannot have its operands reordered.
        let value_tmp = self.locals.temp(SUBREG_KEY_BASE - parent_idx as u64, 4);

        // Mask to the WINDOW width first, then shift into position. Masking
        // with the already-shifted mask (`slot.mask()`, e.g. 0xFF00 for DH)
        // before the shift would clear every bit the window can hold -- DH
        // ended up writing byte 2 instead of byte 1.
        self.op(op::I32_CONST);
        self.push_i32_const((1u32 << (slot.width * 8)) - 1);
        self.op(op::I32_AND);
        if slot.shift() > 0 {
            self.op(op::I32_CONST);
            self.push_i32_const(slot.shift());
            self.op(op::I32_SHL);
        }
        self.local(op::LOCAL_SET, value_tmp);

        // Clear the destination bytes of the parent, then OR the value in.
        self.local(op::LOCAL_GET, parent_idx);
        self.op(op::I32_CONST);
        // `!mask` has the high bits set, so as an i32 it is NEGATIVE and must
        // be signed LEB128. Writing it unsigned produced five bytes that the
        // validator rejects as "extra bits in varint".
        self.body
            .extend_from_slice(&crate::encode::leb_s32(!slot.mask() as i32));
        self.op(op::I32_AND);
        self.local(op::LOCAL_GET, value_tmp);
        self.op(op::I32_OR);
        self.local(op::LOCAL_SET, parent_idx);
        Ok(())
    }

    /// Narrow or widen the i32 currently on the stack to the operand's width,
    /// matching how the IR defines a value at that width.
    fn normalize(&mut self, op: &Operand) -> Result<(), TranslateError> {
        match op.size() {
            1 => self.op(op::I32_EXTEND8_S),
            2 => self.op(op::I32_EXTEND16_S),
            // A 4-byte value is already i32, and an 8-byte value is already i64:
            // neither needs adjusting for its own width.
            4 | 8 => {}
            // An x87 `ST` register declares 10 bytes in the P-code but is an
            // f64 local, and copying between two f64 locals needs no extension.
            // Every x87 instruction emits a rotation of these copies.
            10 if self.operand_is_x87(op) => {}
            // A 10-byte temp is an x87 EXTENDED intermediate (f64).
            10 if matches!(op, Operand::Temp { .. }) => {}
            w => {
                return Err(TranslateError::UnsupportedWidth(w))
            }
        }
        Ok(())
    }

    /// Mark every temp the block treats as a real value, before emission.
    ///
    /// A temp's type must be known when it is *written*, and x87 writes it with
    /// a `Load` that precedes the `Convert` revealing the type: the `Load` fills
    /// `tmp{8}` from memory, then `Convert ST0 = tmp{8}` reveals it is real.
    ///
    /// Marking during the forward pass is therefore one step too late.
    fn pre_mark_x87_temps(&mut self, ops: &[IR]) {
        for ir in ops {
            let (dst, a, b): (&Operand, Option<&Operand>, Option<&Operand>) = match ir {
                // Only a FloatToFloat marks its source: that source is the
                // value `FLD m64` read from memory. `IntToFloat` is `FILD`, whose
                // source is a genuine integer, and marking it made integer
                // temporaries f64.
                IR::Convert { op: crate::ir::ConvertOp::FloatToFloat, dst, src } => {
                    (dst, None, Some(src))
                }
                IR::Convert { dst, .. } => (dst, None, None),
                // `FSINCOS` writes its two results into small temps that are
                // then copied into ST0/ST1. Those temps hold doubles.
                IR::Userop { id, dst: Some(d), .. }
                    if matches!(*id, X87_USEROP_SIN | X87_USEROP_COS) =>
                {
                    (d, None, None)
                }
                // A copy whose source is a sin/cos result, or whose destination
                // is an x87 register, is real-valued.
                IR::Copy { dst, src }
                    if dst.size() == 10
                        || src.size() == 10 =>
                {
                    (dst, None, Some(src))
                }
                IR::Float { dst, operands, .. } => {
                    (dst, Some(&operands.0), Some(&operands.1))
                }
                // A copy involving a 10-byte temp (`FLDZ` pushes through one)
                // is real-valued. Restricted to size 10 on purpose: marking
                // every Copy source made ordinary integer copies f64.
                IR::Copy { dst, src } if dst.size() == 10 || src.size() == 10 => {
                    (dst, None, Some(src))
                }
                _ => continue,
            };
            for o in [Some(dst), a, b].into_iter().flatten() {
                if let Operand::Temp { offset, .. } = o {
                    let idx = self.locals.temp(*offset, 8);
                    self.locals.mark_x87(idx);
                }
            }
        }
    }

    fn emit_ir(&mut self, ir: &IR) -> Result<(), TranslateError> {
        match ir {
            IR::Copy { dst, src } => {
                        let pushed = self.push_value(src)?;
                self.normalize(src)?;
                self.pop_into_width(dst, pushed)
            }
            IR::Load { dst, addr, size } => {
                self.push_value(addr)?;
                // A load into an x87 destination is `FLD`: `m64` is `f64.load`,
                // `m32` is `f32.load` widened by `f64.promote_f32`. This branch
                // was lost in an edit, so the `(8, _)` arm below caught every
                // x87 load and emitted `i64.load` into an f64 local.
                if self.operand_is_x87(dst) {
                    match size {
                        4 => {
                            self.body.push(op::F32_LOAD);
                            self.body.push(0);
                            self.body.push(0);
                            self.op(op::F64_PROMOTE_F32);
                        }
                        8 => {
                            self.body.push(op::F64_LOAD);
                            self.body.push(0);
                            self.body.push(0);
                        }
                        w => return Err(TranslateError::UnsupportedWidth(*w)),
                    }
                    return self.pop_into(dst);
                }
                match (size, dst.size()) {
                    (1, 1) => self.body.push(op::I32_LOAD8_U),
                    (1, _) => self.body.push(op::I32_LOAD8_U),
                    (2, 2) => self.body.push(op::I32_LOAD16_U),
                    (2, _) => self.body.push(op::I32_LOAD16_U),
                    (4, _) => self.body.push(op::I32_LOAD),
                    // A genuine 64-bit integer load: `CMPXCHG8B` compares a
                    // qword from memory against EDX:EAX, and `MUL` reads 8-byte
                    // operands. This is not a float operation and must not be
                    // routed through f64.
                    (8, _) => self.body.push(op::I64_LOAD),
                    (w, _) => {
                        return Err(TranslateError::UnsupportedWidth(*w))
                    }
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
                // `FSTP m64` of the stack top, or of a temp produced by the
                // `Float2Float` that FSTP emits. Both are f64 locals.
                if self.operand_is_x87(value) && *size == 8 {
                    self.body.push(op::F64_STORE);
                    self.body.push(0);
                    self.body.push(0);
                    return Ok(());
                }
                match size {
                    1 => self.body.push(op::I32_STORE8),
                    2 => self.body.push(op::I32_STORE16),
                    4 => self.body.push(op::I32_STORE),
                    // Genuine 64-bit integer store (the write half of
                    // `CMPXCHG8B`). Floats took the f64 branch above.
                    8 => self.body.push(op::I64_STORE),
                    w => {
                        return Err(TranslateError::UnsupportedWidth(*w))
                    }
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
            IR::Float { op: fop, dst, operands } => {
                self.emit_float(*fop, dst, &operands.0, &operands.1)
            }
            IR::Convert { op: cop, dst, src } => {
                // A conversion into the real domain makes its destination an
                // f64. Marking only `FloatToFloat` left `IntToFloat` (FILD)
                // destinations declared i64, so the emitted f64 was then widened
                // as if it were an integer.
                if matches!(
                    cop,
                    crate::ir::ConvertOp::IntToFloat | crate::ir::ConvertOp::FloatToFloat
                ) {
                    self.mark_float_dest(dst);
                }
                self.emit_convert(*cop, dst, src)
            }
            // x87 transcendentals. wasm has no sin/cos, so these become host
            // calls carrying the double through guest memory.
            IR::Userop { id, args, dst: Some(d) }
                if matches!(*id, X87_USEROP_SIN | X87_USEROP_COS) =>
            {
                self.emit_x87_sin_cos(*id, args, d)
            }
            IR::Un { op: un, dst, src } => {
                self.push_value(src)?;
                match un {
                    UnOp::Neg => {
                        // The source has already been pushed, so the stack is
                        // [x]; we need [0, x] for `i32.sub`. Park x in a
                        // scratch local, push the zero, then restore x. The
                        // previous version stored into the temp *before*
                        // pushing the zero, so the sub computed `0 - 0` and
                        // left the original x orphaned on the stack (a net +1
                        // leak that made the enclosing block fail to validate).
                        let tmp = self.locals.temp(u64::MAX - 1, 4);
                        self.local(op::LOCAL_SET, tmp);
                        self.op(op::I32_CONST);
                        self.body.extend_from_slice(&crate::encode::leb_s32(0));
                        self.local(op::LOCAL_GET, tmp);
                        self.op(op::I32_SUB);
                    }
                    UnOp::Not => {
                        self.op(op::I32_CONST);
                        self.body.extend_from_slice(&crate::encode::leb_s32(-1));
                        self.op(op::I32_XOR);
                    }
                    UnOp::Popcount => self.op(op::I32_POPCNT),
                    UnOp::Lzcount => {
                        // Not on the wasm MVP baseline; a host-free equivalent
                        // needs a loop, so report rather than miscompile.
                        return Err(TranslateError::Unsupported("lzcount"));
                    }
                    UnOp::Zext => {
                        if dst.size() == 8 && src.size() == 4 {
                            // dword -> qword: zero-extend, not mask.
                            self.op(op::I64_EXTEND_I32_U);
                        } else {
                            // Zero-extension = mask to the source width.
                            let bits = src.size() * 8;
                            let mask = if bits >= 32 {
                                u32::MAX
                            } else {
                                (1u32 << bits) - 1
                            };
                            self.op(op::I32_CONST);
                            self.body.extend_from_slice(&crate::encode::leb_s32(mask as i32));
                            self.op(op::I32_AND);
                        }
                    }
                    UnOp::Sext => match (src.size(), dst.size()) {
                        (4, 8) => self.op(op::I64_EXTEND_I32_S),
                        (1, _) => self.op(op::I32_EXTEND8_S),
                        (2, _) => self.op(op::I32_EXTEND16_S),
                        (4, 4) => {}
                        (w, _) => return Err(TranslateError::UnsupportedWidth(w)),
                    },
                }
                self.normalize(dst)?;
                self.pop_into(dst)
            }
            IR::Subpiece { dst, src, lsb } => {
                // `lsb` is the number of least significant BYTES to discard
                // (Ghidra P-Code Reference: SUBPIECE). Slicing a qword is how
                // Sleigh splits EDX:EAX after a MUL, so the shift width has to
                // match the SOURCE width, not the destination's.
                self.push_value(src)?;
                let src_is_wide = src.size() == 8;
                if *lsb > 0 {
                    if src_is_wide {
                        // i64 shift: the shift amount is an i64 for i64.shr_u,
                        // but the count itself fits in an i32 literal.
                        self.op(op::I64_CONST);
                        self.body.extend_from_slice(&crate::encode::leb_s32((*lsb * 8) as i32));
                        self.op(op::I64_SHR_U);
                    } else {
                        self.op(op::I32_CONST);
                        self.body.extend_from_slice(&crate::encode::leb_s32((*lsb * 8) as i32));
                        self.op(op::I32_SHR_U);
                    }
                }
                // Narrowing a wide slice to a 4-byte destination is a wrap.
                if src_is_wide && dst.size() <= 4 {
                    self.op(op::I64_WRAP_I32);
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
                        self.body.extend_from_slice(&crate::encode::leb_s32(0));
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

    /// Emit `sin` or `cos` of an x87 value via a host import.
    ///
    /// The import signature is `(i32 args_ptr, i32 ret_addr) -> i32`, which
    /// cannot carry an f64. The argument double is stored at
    /// [`X87_HELPER_ARG`], the helper reads it there and writes its result to
    /// [`X87_HELPER_RET`], and the result is loaded back as this op's value.
    ///
    /// Only `FSINCOS` needs these. `FPREM1` and `FXAM` are lowered by Sleigh to
    /// ordinary float ops and need no helper.
    fn emit_x87_sin_cos(
        &mut self,
        id: u64,
        args: &[Operand],
        dst: &Operand,
    ) -> Result<(), TranslateError> {
        let src = args.first().ok_or(TranslateError::Unsupported("x87 helper with no argument"))?;
        // arg = *X87_HELPER_ARG
        self.op(op::I32_CONST);
        self.push_i32_const(X87_HELPER_ARG as u32);
        self.push_value(src)?;
        self.body.push(op::F64_STORE);
        self.body.push(0);
        self.body.push(0);
        // call helper(0, 0)
        let imp = if id == X87_USEROP_SIN { X87_USEROP_SIN } else { X87_USEROP_COS };
        let idx = *self
            .import_index
            .get(&x87_helper_import(imp))
            .ok_or(TranslateError::Unsupported("x87 helper import missing"))?;
        self.op(op::I32_CONST);
        self.push_i32_const(0);
        self.op(op::I32_CONST);
        self.push_i32_const(0);
        self.op(op::CALL);
        self.body.extend_from_slice(&crate::encode::leb_u32(idx));
        // value = *X87_HELPER_RET
        self.op(op::I32_CONST);
        self.push_i32_const(X87_HELPER_RET as u32);
        self.body.push(op::F64_LOAD);
        self.body.push(0);
        self.body.push(0);
        self.mark_dst_x87(dst);
        self.pop_into(dst)
    }

    /// Ensure `dst` is typed f64 before a value is stored into it.
    fn mark_dst_x87(&mut self, dst: &Operand) {
        match dst {
            Operand::Temp { offset, .. } => {
                let idx = self.locals.temp(*offset, 8);
                self.locals.mark_x87(idx);
            }
            Operand::Reg { name, .. } => {
                if let Some(idx) = self.locals.reg(name) {
                    self.locals.mark_x87(idx);
                }
            }
            _ => {}
        }
    }

    /// The width an operand's storage is declared at.
    ///
    /// A temp declared i64 holds a qword even when a particular use mentions 4
    /// bytes, and that is the width the wasm opcode must match.
    fn operand_declared_width(&self, op: &Operand) -> u32 {
        match op {
            Operand::Temp { offset, .. } => self
                .locals
                .temp_index(*offset)
                .map_or(op.size(), |idx| self.locals.width(idx)),
            Operand::Reg { name, .. } => self
                .locals
                .reg(name)
                .map_or(op.size(), |idx| self.locals.width(idx)),
            _ => op.size(),
        }
    }

    /// Mark a conversion destination as holding an `f64`.
    ///
    /// Allocates the temp at 8 bytes if it does not exist yet, so the type is
    /// fixed before anything writes to it.
    fn mark_float_dest(&mut self, dst: &Operand) {
        match dst {
            Operand::Temp { offset, .. } => {
                let idx = self.locals.temp(*offset, 8);
                self.locals.mark_x87(idx);
            }
            Operand::Reg { name, .. } => {
                if let Some(idx) = self.locals.reg(name) {
                    self.locals.mark_x87(idx);
                }
            }
            _ => {}
        }
    }

    /// Whether an operand is an x87 stack register.
    fn operand_is_x87(&self, op: &Operand) -> bool {
        match op {
            Operand::Reg { name, .. } => self
                .locals
                .reg(name)
                .is_some_and(|idx| self.locals.is_x87(idx)),
            // A temp holding an x87 intermediate (produced by a Float2Float) is
            // an f64 too. `FSTP m64` stores through exactly such a temp.
            Operand::Temp { offset, .. } => self
                .locals
                .temp_index(*offset)
                .is_some_and(|idx| self.locals.is_x87(idx)),
            _ => false,
        }
    }

    /// Emit a floating-point binary operation.
    ///
    /// Both operands are pushed as f64 (x87 locals are f64; a `double` temp from
    /// memory is f64). A 4-byte operand is a `float` and is widened with
    /// `f64.promote_f32` first -- wasm rejects mixing f32 and f64 on one
    /// operator. Comparisons produce an i32 boolean, which is what P-code's
    /// `FloatLess` family yields.
    /// Emit a typed integer <-> real conversion.
    ///
    /// `Float2Float` from an x87 register is a no-op: the P-code says 80 -> 64
    /// bits, but the source local is already an f64, so the narrowing already
    /// happened. Emitting a demote/promote there would be wrong.
    fn emit_convert(
        &mut self,
        cop: crate::ir::ConvertOp,
        dst: &Operand,
        src: &Operand,
    ) -> Result<(), TranslateError> {
        use crate::ir::ConvertOp::*;
        match cop {
            FloatToFloat => {
                // The destination holds a real value: mark it before allocating
                // so the following `FSTP m64` store sees an f64, not an i64.
                if let Operand::Temp { offset, .. } = dst {
                    let idx = self.locals.temp(*offset, 8);
                    self.locals.mark_x87(idx);
                }
                // `FLD m64` is `Load tmp; Convert ST0 = tmp`: the temp feeding
                // the conversion is the memory value, and it is real too. Without
                // this the load stayed an i64 and wrote it into an f64 slot.
                if let Operand::Temp { offset, .. } = src {
                    let idx = self.locals.temp(*offset, 8);
                    self.locals.mark_x87(idx);
                }
                let sw = self.push_value(src)?;
                if sw == 4 {
                    self.op(op::F64_PROMOTE_F32);
                }
                self.pop_into(dst)
            }
            IntToFloat => {
                let sw = self.push_value(src)?;
                match sw {
                    4 => self.op(op::F64_CONVERT_I32_S),
                    8 | 10 => self.op(op::F64_CONVERT_I64_S),
                    w => return Err(TranslateError::UnsupportedWidth(w)),
                }
                self.pop_into(dst)
            }
            FloatToInt => {
                self.push_value(src)?;
                match dst.size() {
                    1 | 2 | 4 => self.op(op::I32_TRUNC_F64_S),
                    8 => self.op(op::I64_TRUNC_F64_S),
                    w => return Err(TranslateError::UnsupportedWidth(w)),
                }
                self.pop_into(dst)
            }
        }
    }

    fn emit_float(
        &mut self,
        fop: crate::ir::FloatOp,
        dst: &Operand,
        lhs: &Operand,
        rhs: &Operand,
    ) -> Result<(), TranslateError> {
        let lw = self.push_value(lhs)?;
        if lw == 4 {
            self.op(op::F64_PROMOTE_F32);
        } else if lw != 8 && lw != 10 {
            return Err(TranslateError::UnsupportedWidth(lw));
        }
        let rw = self.push_value(rhs)?;
        if rw == 4 {
            self.op(op::F64_PROMOTE_F32);
        } else if rw != 8 && rw != 10 {
            return Err(TranslateError::UnsupportedWidth(rw));
        }
        use crate::ir::FloatOp::*;
        match fop {
            Add => self.op(op::F64_ADD),
            Sub => self.op(op::F64_SUB),
            Mul => self.op(op::F64_MUL),
            Div => self.op(op::F64_DIV),
            Eq => self.op(op::F64_EQ),
            NotEq => self.op(op::F64_NE),
            Less => self.op(op::F64_LT),
            LessEq => self.op(op::F64_LE),
        }
        self.pop_into(dst)
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
        let mut lw = self.push_value(lhs)?;
        // Widen the LEFT operand now, while it is still on top of the stack.
        // Doing it after rhs is pushed would require parking rhs in a scratch
        // local, and a scratch local gets one fixed width that then has to match
        // both uses. Sleigh knows the right operand's declared width statically,
        // so decide from that instead.
        // Never widen a real-number operand: an x87 value pushes an f64, and
        // `i64.extend_i32_u` on an f64 is a type error. Float mixing is handled
        // by IR::Float, which promotes to f64 instead.
        let rhs_float = self.operand_is_x87(rhs);
        if !rhs_float && self.operand_declared_width(rhs) == 8 && lw != 8 && !self.operand_is_x87(lhs)
        {
            self.op(op::I64_EXTEND_I32_U);
            lw = 8;
        }
        let rw = self.push_value(rhs)?;
        // rhs is on top, so widening it in place is always safe.
        if (lw == 8 || rw == 8) && rw != 8 && !self.operand_is_x87(rhs) {
            self.op(op::I64_EXTEND_I32_U);
        }
        let wide = lw == 8 || rw == 8;
        let byte = match (bin, wide) {
            (BinOp::Add, true) => op::I64_ADD,
            (BinOp::Sub, true) => op::I64_SUB,
            (BinOp::Mul, true) => op::I64_MUL,
            (BinOp::DivS, true) => op::I64_DIV_S,
            (BinOp::DivU, true) => op::I64_DIV_U,
            (BinOp::RemS, true) => op::I64_REM_S,
            (BinOp::RemU, true) => op::I64_REM_U,
            (BinOp::And, true) => op::I64_AND,
            (BinOp::Or, true) => op::I64_OR,
            (BinOp::Xor, true) => op::I64_XOR,
            (BinOp::Lsh, true) => op::I64_SHL,
            (BinOp::Rsh, true) => op::I64_SHR_U,
            (BinOp::Asr, true) => op::I64_SHR_S,
            (BinOp::Eq, true) => op::I64_EQ,
            (BinOp::NotEq, true) => op::I64_NE,
            (BinOp::LessU, true) => op::I64_LT_U,
            (BinOp::LessEqU, true) => op::I64_LE_U,
            (BinOp::LessS, true) => op::I64_LT_S,
            (BinOp::LessEqS, true) => op::I64_LE_S,
            (BinOp::Carry | BinOp::SCarry | BinOp::SBorrow | BinOp::MulFull, _) => unreachable!(),
            (_, false) => match bin {
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
            },
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
            // A qword result stored into a dword/byte destination must be
            // wrapped to i32 first: the destination local is i32-typed.
            if wide && dst.size() <= 4 {
                self.op(op::I64_WRAP_I32);
            }
            self.normalize(dst)?;
        } else if wide {
            // i64 comparisons yield i32 already, so nothing to narrow.
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
        self.body.extend_from_slice(&crate::encode::leb_s32(31));
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
        // Borrow is the classic `(a ^ (a - b)) & (a ^ b)` sign trick, valid at
        // either width; only the opcode family and the sign-bit position differ.
        // The scratch slots used to be fixed 4-byte i32 locals, so a qword
        // operand (CMPXCHG8B's 64-bit subtract) was stored into an i32 slot.
        //
        // The width has to come from what `push_value` actually pushes --
        // deciding from the operands' nominal sizes picked i64 for an i32 value.
        // Both slot sets are pre-allocated so the choice can be made after the
        // first push; the unused pair simply goes undeclared.
        let (a32, b32, r32) = (
            self.locals.temp(u64::MAX - 7, 4),
            self.locals.temp(u64::MAX - 8, 4),
            self.locals.temp(u64::MAX - 9, 4),
        );
        let (a64, b64, r64) = (
            self.locals.temp(u64::MAX - 10, 8),
            self.locals.temp(u64::MAX - 11, 8),
            self.locals.temp(u64::MAX - 12, 8),
        );
        let lw = self.push_value(lhs)?;
        // `wide` cannot be decided from the left operand alone: the DPX-style
        // `borrow from EAX - qword` has an i32 left and an i64 right. Push both
        // and widen whichever is narrow, so the pair always matches the slot
        // width. Parking happens in the i64 slots; a 32-bit result is narrowed
        // afterwards by `normalize`.
        let rw = self.push_value(rhs)?;
        let wide = lw == 8 || rw == 8;
        let (a, b, r) = if wide { (a64, b64, r64) } else { (a32, b32, r32) };
        // rhs is on top: widen it, park it, then widen and park lhs.
        if wide && rw != 8 {
            self.op(op::I64_EXTEND_I32_U);
        }
        self.local(op::LOCAL_SET, b);
        if wide && lw != 8 {
            self.op(op::I64_EXTEND_I32_U);
        }
        self.local(op::LOCAL_SET, a);
        self.local(op::LOCAL_GET, a);
        self.local(op::LOCAL_GET, b);
        self.op(step(wide, op::I32_SUB, op::I64_SUB));
        self.local(op::LOCAL_SET, r);
        self.local(op::LOCAL_GET, a);
        self.local(op::LOCAL_GET, b);
        self.op(step(wide, op::I32_XOR, op::I64_XOR));
        self.local(op::LOCAL_GET, a);
        self.local(op::LOCAL_GET, r);
        self.op(step(wide, op::I32_XOR, op::I64_XOR));
        self.op(step(wide, op::I32_AND, op::I64_AND));
        self.local(op::LOCAL_GET, a);
        self.op(step(wide, op::I32_XOR, op::I64_XOR));
        if wide {
            self.op(op::I64_CONST);
            self.body.extend_from_slice(&crate::encode::leb_s64(63));
            self.op(op::I64_SHR_S);
        } else {
            self.op(op::I32_CONST);
            self.push_i32_const(31);
            self.op(op::I32_SHR_S);
        }
        // Mask off everything above the sign bit. This must match the width the
        // shift produced: leaving it as `i32.and` put a 32-bit operator on the
        // i64 the wide path leaves on the stack.
        if wide {
            self.op(op::I64_CONST);
            self.body.extend_from_slice(&leb_u32(1));
            self.op(op::I64_AND);
        } else {
            self.op(op::I32_CONST);
            self.body.extend_from_slice(&leb_u32(1));
            self.op(op::I32_AND);
        }
        // The wide path leaves an i64; flags are 1-2 bytes, so narrow it before
        // `normalize` applies `i32.extend8_s`/`extend16_s`, which have no i64
        // form.
        if wide {
            self.op(op::I32_WRAP_I64);
        }
        self.normalize(dst)?;
        self.pop_into(dst)
    }
}

/// Pick the i32 or i64 form of an opcode.
fn step(wide: bool, i32_op: u8, i64_op: u8) -> u8 {
    if wide { i64_op } else { i32_op }
}

/// Translate a sequence of IR ops into one wasm function body.
///
/// `locals` must come from [`LocalMap::new`] with the union of registers used
/// across the function, so block functions agree on local indices.
pub fn translate_ops(ops: &[IR], locals: LocalMap) -> Result<TranslatedBlock, TranslateError> {
    translate_ops_with_imports(ops, locals, &std::collections::BTreeMap::new())
}

/// `translate_ops` with the program's host-import table, needed by blocks that
/// call an x87 transcendental helper.
pub fn translate_ops_with_imports(
    ops: &[IR],
    locals: LocalMap,
    imports: &std::collections::BTreeMap<crate::chain::HostImport, u32>,
) -> Result<TranslatedBlock, TranslateError> {
    let base_count = locals.len();
    let mut e = BlockEmitter::new(locals);
    e.import_index = imports.clone();
    e.pre_mark_x87_temps(ops);
    for ir in ops {
        e.emit_ir(ir)?;
    }
    // ALL locals must be declared: registers are locals (not parameters), and
    // the temps appended during emission follow them. Declaring only the temps
    // made `local.get 0` reference an undeclared index, which V8 rejects with
    // "invalid local index: 0".
    //
    // 8-byte temps need i64 locals, so the declaration is a list of typed runs
    // in local-index order (wasm assigns indices in declaration order).
    let _ = base_count;
    let locals: Vec<(u32, u8)> = e
        .locals
        .declaration_runs()
        .into_iter()
        .map(|(ty, count)| (count, ty))
        .collect();
    let total = e.locals.len();
    let map = e.locals.clone();
    // Function body: locals, then ops, then `end`.
    let mut out = Vec::new();
    out.extend_from_slice(&leb_u32(locals.len() as u32));
    for (count, ty) in &locals {
        out.extend_from_slice(&leb_u32(*count));
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

/// Scratch double passed to an x87 transcendental helper.
///
/// wasm has no `sin`/`cos`, so `FSINCOS` becomes a host call. Every host import
/// has the shape `(i32 args_ptr, i32 ret_addr) -> i32`, which cannot carry an
/// f64, so the argument is stored here as a double, the helper reads it, and the
/// result is loaded back. Sits well above the register snapshot area so the two
/// can never overlap.
pub const X87_HELPER_ARG: u64 = 0x8100;

/// Sleigh userop id for `sin` of `ST0` (one half of `FSINCOS`).
pub const X87_USEROP_SIN: u64 = 99;
/// Sleigh userop id for `cos` of `ST0` (the other half of `FSINCOS`).
pub const X87_USEROP_COS: u64 = 105;

/// The synthetic host import backing an x87 transcendental.
///
/// The library name marks these as emitter-internal, not a real DLL, so a guest
/// import table can never collide with them.
pub fn x87_helper_import(op: u64) -> crate::chain::HostImport {
    crate::chain::HostImport {
        library: "wlam.x87".to_string(),
        name: if op == X87_USEROP_SIN { "sin" } else { "cos" }.to_string(),
    }
}
/// Scratch double receiving an x87 helper's result.
pub const X87_HELPER_RET: u64 = 0x8108;

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
    //
    // Slot layout, in register order: integer registers take 4 bytes each, and
    // the x87 stack follows them at 8 bytes each. The x87 registers are f64
    // locals, so they must be loaded and stored as doubles -- emitting
    // `i32.load` into an f64 local is the type error a validator reports as
    // "expected f64, found i32", which is how the first version of this
    // function failed on real Minesweeper blocks.
    let mut next = REG_SNAPSHOT_BASE;
    let mut slot: Vec<(u32, u64)> = Vec::with_capacity(registers.len());
    for name in registers {
        let Some(idx) = locals.reg(name) else { continue };
        slot.push((idx, next));
        next += if locals.is_x87(idx) { 8 } else { 4 };
    }
    for &(idx, at) in &slot {
        let is_x87 = locals.is_x87(idx);
        body.push(op::I32_CONST);
        body.extend_from_slice(&leb_u32(at as u32));
        body.push(if is_x87 { op::F64_LOAD } else { op::I32_LOAD });
        body.push(0);
        body.push(0);
        body.push(op::LOCAL_SET);
        body.extend_from_slice(&leb_u32(idx));
    }
    for &(idx, at) in &slot {
        let is_x87 = locals.is_x87(idx);
        body.push(op::I32_CONST);
        body.extend_from_slice(&leb_u32(at as u32));
        body.push(op::LOCAL_GET);
        body.extend_from_slice(&leb_u32(idx));
        body.push(if is_x87 { op::F64_STORE } else { op::I32_STORE });
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
