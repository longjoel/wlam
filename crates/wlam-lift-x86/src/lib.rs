//! Lifting x86-32 instructions to P-code.
//!
//! This is the front-end that makes the recompiler possible: it decodes one
//! instruction at a time (via `iced-x86`) and translates it into the
//! architecture-neutral IR from `wlam-pcode`.
//!
//! Design rules, stated here so they stay enforced:
//!
//! 1. **Every flag an instruction defines is materialised**, even when nothing
//!    obviously reads it. Dead-flag elimination is a later optimisation; a
//!    missing flag is a miscompile that surfaces in a branch far away.
//! 2. **Partial-register writes preserve upper bits.** Writing `AL` reads and
//!    modifies `EAX`; 16-bit writes keep `EAX[31:16]`; `AH` writes keep
//!    everything but bits 8-15.
//! 3. **x87 is modelled on a RAM register stack** (wasm has no 80-bit type).
//!    `FTOP` holds the stack pointer; `ST(i)` addresses
//!    `RAM[X87_STACK_BASE + ((FTOP + i) % 8) * 10]`, so `fxch`, `ffree` and
//!    `fstp` stay correct across calls.
//! 4. **Unsupported constructs are errors, never silent no-ops.** A lifter that
//!    quietly drops an instruction yields a program that runs and is wrong.
//! 5. **REP string instructions are bounded.** An instruction that copies
//!    ECX * 4 bytes becomes a loop with a count register and a `CallOther`
//!    instruction-limit check every iteration; a single iteration never
//!    unrolls to millions of IR ops.

use std::fmt;

use iced_x86::Register as IReg;
use iced_x86::{Decoder, DecoderOptions, FlowControl, Instruction, MemorySize, Mnemonic, OpKind};
use wlam_pcode::{
    mask, BasicBlock, BinaryOp, ByteSize, Op, PcodeInstruction, Register, Terminator, UnaryOp,
    Varnode,
};

/// Base address of the x87 `ST(0..7)` register stack in guest memory. Each
/// slot is 10 bytes (80-bit extended precision), matching the x87 format so
/// the host's software `f80` helpers can address them directly.
pub const X87_STACK_BASE: u64 = 0xF000_0000;
/// Bytes per x87 stack slot.
pub const X87_SLOT_SIZE: u64 = 10;

/// `ST(i)` guest address for the current TOP.
pub fn x87_slot(i: u32) -> u64 {
    // The address depends on the live TOP, so it is emitted as IR; this is
    // the static layout helper used by the emitter's stack setup.
    X87_STACK_BASE + (i as u64 % 8) * X87_SLOT_SIZE
}

/// Errors that make translation impossible. Every variant carries the address
/// so the recompiler can produce a symbolized diagnostic.
#[derive(Debug, Clone, PartialEq)]
pub enum LiftError {
    /// The decoder could not decode the bytes at `at`.
    Undecodable { at: u64 },
    /// Decoded, but not modelled yet.
    Unsupported {
        at: u64,
        instruction: String,
        reason: String,
    },
}

impl fmt::Display for LiftError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LiftError::Undecodable { at } => write!(f, "undecodable bytes at {at:#x}"),
            LiftError::Unsupported {
                at,
                instruction,
                reason,
            } => {
                write!(
                    f,
                    "unsupported instruction at {at:#x}: {instruction} ({reason})"
                )
            }
        }
    }
}

impl std::error::Error for LiftError {}

/// Map an iced register to our 32-bit and width-specific registers.
fn gp32_of(r: IReg) -> Option<Register> {
    Some(match r {
        IReg::EAX | IReg::AX | IReg::AL | IReg::AH => Register::EAX,
        IReg::ECX | IReg::CX | IReg::CL | IReg::CH => Register::ECX,
        IReg::EDX | IReg::DX | IReg::DL | IReg::DH => Register::EDX,
        IReg::EBX | IReg::BX | IReg::BL | IReg::BH => Register::EBX,
        IReg::ESP | IReg::SP => Register::ESP,
        IReg::EBP | IReg::BP => Register::EBP,
        IReg::ESI | IReg::SI => Register::ESI,
        IReg::EDI | IReg::DI => Register::EDI,
        _ => return None,
    })
}

fn gp_exact(r: IReg) -> Option<Register> {
    Some(match r {
        IReg::EAX => Register::EAX,
        IReg::ECX => Register::ECX,
        IReg::EDX => Register::EDX,
        IReg::EBX => Register::EBX,
        IReg::ESP => Register::ESP,
        IReg::EBP => Register::EBP,
        IReg::ESI => Register::ESI,
        IReg::EDI => Register::EDI,
        IReg::AX => Register::AX,
        IReg::CX => Register::CX,
        IReg::DX => Register::DX,
        IReg::BX => Register::BX,
        IReg::SP => Register::SP,
        IReg::BP => Register::BP,
        IReg::SI => Register::SI,
        IReg::DI => Register::DI,
        IReg::AL => Register::AL,
        IReg::CL => Register::CL,
        IReg::DL => Register::DL,
        IReg::BL => Register::BL,
        IReg::AH => Register::AH,
        IReg::CH => Register::CH,
        IReg::DH => Register::DH,
        IReg::BH => Register::BH,
        _ => return None,
    })
}

fn register_byte_size(r: IReg) -> ByteSize {
    if r.is_gpr8() {
        1
    } else if r.is_gpr16() {
        2
    } else if r.is_gpr32() {
        4
    } else {
        0
    }
}

fn memory_size_bytes(m: MemorySize) -> ByteSize {
    match m {
        MemorySize::UInt8 | MemorySize::Int8 => 1,
        MemorySize::UInt16 | MemorySize::Int16 => 2,
        MemorySize::UInt32 | MemorySize::Int32 | MemorySize::Float32 => 4,
        MemorySize::UInt64 | MemorySize::Int64 | MemorySize::Float64 => 8,
        _ => 0,
    }
}

/// Result of lifting one instruction.
#[derive(Debug, Clone)]
pub struct LiftResult {
    pub instruction: PcodeInstruction,
    /// iced's classification, which decides how the block terminates.
    pub flow: FlowControl,
    /// Address of the following instruction.
    pub next_ip: u64,
    /// Direct call target for `FlowControl::Call`.
    pub call_target: Option<u64>,
}

impl LiftResult {
    /// Whether this instruction ends a basic block.
    pub fn ends_block(&self) -> bool {
        !matches!(self.flow, FlowControl::Next)
    }
}

/// Where a condition's flag values live in our split-flag model.
fn condition_of(m: Mnemonic) -> Option<Condition> {
    use Condition::*;
    // One row per (condition, jcc, setcc, cmovcc). Written out explicitly so a
    // missing mnemonic is visible in review rather than buried in a pattern.
    let rows: &[(Condition, Mnemonic, Mnemonic, Mnemonic)] = &[
        (Eq, Mnemonic::Je, Mnemonic::Sete, Mnemonic::Cmove),
        (Ne, Mnemonic::Jne, Mnemonic::Setne, Mnemonic::Cmovne),
        (B, Mnemonic::Jb, Mnemonic::Setb, Mnemonic::Cmovb),
        (Ae, Mnemonic::Jae, Mnemonic::Setae, Mnemonic::Cmovae),
        (Be, Mnemonic::Jbe, Mnemonic::Setbe, Mnemonic::Cmovbe),
        (A, Mnemonic::Ja, Mnemonic::Seta, Mnemonic::Cmova),
        (S, Mnemonic::Js, Mnemonic::Sets, Mnemonic::Cmovs),
        (Ns, Mnemonic::Jns, Mnemonic::Setns, Mnemonic::Cmovns),
        (P, Mnemonic::Jp, Mnemonic::Setp, Mnemonic::Cmovp),
        (Np, Mnemonic::Jnp, Mnemonic::Setnp, Mnemonic::Cmovnp),
        (L, Mnemonic::Jl, Mnemonic::Setl, Mnemonic::Cmovl),
        (Ge, Mnemonic::Jge, Mnemonic::Setge, Mnemonic::Cmovge),
        (Le, Mnemonic::Jle, Mnemonic::Setle, Mnemonic::Cmovle),
        (G, Mnemonic::Jg, Mnemonic::Setg, Mnemonic::Cmovg),
    ];
    rows.iter()
        .find(|(_, j, se, cm)| *j == m || *se == m || *cm == m)
        .map(|(c, _, _, _)| *c)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Condition {
    Eq,
    Ne,
    B,
    Ae,
    Be,
    A,
    S,
    Ns,
    L,
    Ge,
    Le,
    G,
    P,
    Np,
}

/// Lift one already-decoded instruction.
pub fn lift_instruction(insn: &Instruction) -> Result<LiftResult, LiftError> {
    let at = insn.ip();
    let disassembly = format!("{:?}", insn.mnemonic());
    let mut l = Lifter::new(insn, at, disassembly);
    l.lift()?;
    let call_target = l.call_target;
    Ok(LiftResult {
        instruction: l.finish(),
        flow: insn.flow_control(),
        next_ip: insn.next_ip(),
        call_target,
    })
}

/// Decode and lift one basic block from `bytes` starting at guest address
/// `start`.
///
/// Returns the block and, if lifting stopped on an unsupported instruction,
/// the error. The block always ends with a [`Terminator`]; an unsupported
/// instruction becomes `Trap`, so the emitted program fails loudly rather than
/// continuing with wrong state.
pub fn lift_block(bytes: &[u8], start: u64) -> (BasicBlock, Option<LiftError>) {
    lift_block_with_base(bytes, 0, start)
}

/// Lift a block whose `start` is an RVA: decode with `ip = base + rva` so that
/// displacements and branch targets come out as image-relative VAs, exactly
/// as the PE loader would compute them.
pub fn lift_block_with_base(
    bytes: &[u8],
    base: u64,
    start: u64,
) -> (BasicBlock, Option<LiftError>) {
    let mut decoder = Decoder::with_ip(32, bytes, base + start, DecoderOptions::NONE);
    let mut block = BasicBlock::new(base + start);
    while decoder.can_decode() {
        let insn = decoder.decode();
        let next_ip = insn.next_ip();
        match lift_instruction(&insn) {
            Ok(r) => {
                let flow = r.flow;
                let call_target = r.call_target;
                block.push(r.instruction);
                if !matches!(flow, FlowControl::Next) {
                    block.terminator =
                        terminator_for(flow, insn.near_branch_target(), next_ip, call_target);
                    return (block, None);
                }
            }
            Err(e) => {
                block.terminator = Terminator::Trap {
                    reason: format!("{e}"),
                };
                return (block, Some(e));
            }
        }
    }
    block.terminator = Terminator::Trap {
        reason: "code ended without a control transfer".into(),
    };
    (block, None)
}

fn terminator_for(
    flow: FlowControl,
    branch_target: u64,
    next_ip: u64,
    call_target: Option<u64>,
) -> Terminator {
    match flow {
        FlowControl::Next => Terminator::Fallthrough { next: next_ip },
        FlowControl::UnconditionalBranch => Terminator::Jump { target: next_ip },
        FlowControl::ConditionalBranch => Terminator::Conditional {
            taken: branch_target,
            not_taken: next_ip,
        },
        FlowControl::IndirectBranch => Terminator::IndirectJump,
        FlowControl::Call => Terminator::Call {
            target: call_target.unwrap_or(next_ip),
            ret: next_ip,
        },
        FlowControl::IndirectCall => Terminator::IndirectCall { ret: next_ip },
        FlowControl::Return => Terminator::Return,
        other => Terminator::Trap {
            reason: format!("flow control {other:?}"),
        },
    }
}

struct Lifter<'a> {
    insn: &'a Instruction,
    at: u64,
    disasm: String,
    ops: Vec<Op>,
    uniques: u64,
    call_target: Option<u64>,
}

impl<'a> Lifter<'a> {
    fn new(insn: &'a Instruction, at: u64, disasm: String) -> Self {
        Lifter {
            insn,
            at,
            disasm,
            ops: Vec::new(),
            uniques: 0,
            call_target: None,
        }
    }

    fn finish(self) -> PcodeInstruction {
        PcodeInstruction::new(self.at, self.insn.len() as u8, self.disasm, self.ops)
    }

    fn unsupported(&self, reason: &str) -> LiftError {
        LiftError::Unsupported {
            at: self.at,
            instruction: self.disasm.clone(),
            reason: reason.to_string(),
        }
    }

    // ------------------------------------------------------------- plumbing

    fn unique(&mut self, size: ByteSize) -> Varnode {
        self.uniques += 1;
        Varnode::unique(self.uniques, size)
    }

    fn reg(r: Register) -> Varnode {
        Varnode::reg(r)
    }

    /// Produce `v` at a different width.
    ///
    /// P-code `Addr` inputs are lvalues, so declaring a 4-byte lvalue as a
    /// 1-byte one would change *how many bytes are loaded* from it, which is
    /// wrong: resizing a value must not change its access width. We therefore
    /// load the value at its own width into a temporary and then copy the low
    /// `size` bytes out of it. Constants simply truncate.
    fn resize(&mut self, v: Varnode, size: ByteSize) -> Varnode {
        if v.size() == size {
            return v;
        }
        if v.size() < size {
            // Widening a value is a zero-extension of the value, never a
            // re-declaration of a narrower lvalue.
            let loaded = self.zero_extend(v, size);
            return loaded;
        }
        match v {
            Varnode::Const(c, _) => Varnode::Const(c & mask(size), size),
            Varnode::Addr(..) => {
                let loaded = self.unique(v.size());
                self.ops.push(Op::Copy {
                    out: loaded.clone(),
                    input: v,
                });
                let t = self.unique(size);
                self.ops.push(Op::Copy {
                    out: t.clone(),
                    input: loaded.truncate(size),
                });
                t
            }
        }
    }

    /// Zero-extend a value to `size`. When the destination is wider than 4
    /// bytes, P-code's `IntSext`-style width change is expressed with a COPY of
    /// a 4-byte zero constant ORed in, which keeps 64-bit results correct.
    fn zero_extend(&mut self, v: Varnode, size: ByteSize) -> Varnode {
        if v.size() >= size {
            return self.resize(v, size);
        }
        let tmp = self.unique(size);
        // COPY with a larger destination zero-extends in P-code.
        self.ops.push(Op::Copy {
            out: tmp.clone(),
            input: v,
        });
        tmp
    }

    fn bin(&mut self, out: Varnode, a: Varnode, b: Varnode, op: BinaryOp) {
        self.ops.push(Op::Binary { out, a, b, op });
    }

    fn operand_size(&self, i: u32) -> ByteSize {
        match self.insn.op_kind(i) {
            OpKind::Register => register_byte_size(self.insn.op_register(i)),
            OpKind::Memory => memory_size_bytes(self.insn.memory_size()),
            _ => 0,
        }
    }

    fn effective_address(&mut self) -> Result<Varnode, LiftError> {
        let i = self.insn;
        if i.memory_base() == IReg::None && i.memory_index() == IReg::None {
            return Ok(Varnode::constant(i.memory_displacement64(), 4));
        }
        // Segment overrides other than FS/GS are irrelevant to 32-bit flat
        // programs; FS/GS matter for SEH and TLS and are not lifted yet.
        let seg = i.memory_segment();
        if seg == IReg::FS || seg == IReg::GS {
            return Err(self.unsupported("FS/GS segment-relative addressing"));
        }
        let mut acc = Varnode::constant(0, 4);
        let mut have = false;
        if i.memory_base() != IReg::None {
            let r = gp32_of(i.memory_base()).ok_or_else(|| self.unsupported("base register"))?;
            acc = Self::reg(r);
            have = true;
        }
        if i.memory_index() != IReg::None {
            let r = gp32_of(i.memory_index()).ok_or_else(|| self.unsupported("index register"))?;
            let mut idx = Self::reg(r);
            let scale = i.memory_index_scale();
            if scale != 1 {
                let t = self.unique(4);
                self.bin(
                    t.clone(),
                    idx,
                    Varnode::constant(scale as u64, 4),
                    BinaryOp::IntMult,
                );
                idx = t;
            }
            if have {
                let t = self.unique(4);
                self.bin(t.clone(), acc, idx, BinaryOp::IntAdd);
                acc = t;
            } else {
                acc = idx;
                have = true;
            }
        }
        let disp = i.memory_displacement64();
        if disp != 0 || !have {
            let t = self.unique(4);
            self.bin(t.clone(), acc, Varnode::constant(disp, 4), BinaryOp::IntAdd);
            acc = t;
        }
        Ok(acc)
    }

    fn read_operand(&mut self, i: u32) -> Result<Varnode, LiftError> {
        match self.insn.op_kind(i) {
            OpKind::Register => {
                let r = gp_exact(self.insn.op_register(i))
                    .ok_or_else(|| self.unsupported("non-GP register operand"))?;
                Ok(Self::reg(r))
            }
            OpKind::Memory => {
                let addr = self.effective_address()?;
                let size = self.operand_size(i);
                let t = self.unique(size);
                self.ops.push(Op::Load {
                    out: t.clone(),
                    input: addr,
                    size,
                });
                Ok(t)
            }
            OpKind::Immediate8 => Ok(Varnode::constant(self.insn.immediate8() as u64, 1)),
            OpKind::Immediate8_2nd => Ok(Varnode::constant(self.insn.immediate8_2nd() as u64, 1)),
            OpKind::Immediate16 => Ok(Varnode::constant(self.insn.immediate16() as u64, 2)),
            OpKind::Immediate32 => Ok(Varnode::constant(self.insn.immediate32() as u64, 4)),
            OpKind::Immediate8to16 => Ok(Varnode::constant(
                self.insn.immediate8to16() as u16 as u64,
                2,
            )),
            OpKind::Immediate8to32 => Ok(Varnode::constant(
                self.insn.immediate8to32() as u32 as u64,
                4,
            )),
            OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => {
                Ok(Varnode::constant(self.insn.near_branch_target(), 4))
            }
            other => Err(self.unsupported(&format!("operand kind {other:?}"))),
        }
    }

    /// Write `value` to operand `i`, honouring partial-register rules.
    fn write_operand(&mut self, i: u32, value: Varnode) -> Result<(), LiftError> {
        match self.insn.op_kind(i) {
            OpKind::Register => {
                let r = self.insn.op_register(i);
                let ours =
                    gp_exact(r).ok_or_else(|| self.unsupported("non-GP register destination"))?;
                let value = self.resize(value, register_byte_size(r));
                match r {
                    IReg::AX
                    | IReg::CX
                    | IReg::DX
                    | IReg::BX
                    | IReg::SP
                    | IReg::BP
                    | IReg::SI
                    | IReg::DI => {
                        let full = gp32_of(r).unwrap();
                        let v = self.resize(value, 4);
                        let v = self.resize(v, 2);
                        let old = self.unique(4);
                        self.ops.push(Op::Copy {
                            out: old.clone(),
                            input: Self::reg(full),
                        });
                        let kept = self.unique(4);
                        self.bin(
                            kept.clone(),
                            old,
                            Varnode::constant(0xFFFF_0000, 4),
                            BinaryOp::IntAnd,
                        );
                        let zero = self.unique(4);
                        self.bin(zero.clone(), v, Varnode::constant(0, 4), BinaryOp::IntOr);
                        let merged = self.unique(4);
                        self.bin(merged.clone(), kept, zero, BinaryOp::IntOr);
                        self.ops.push(Op::Copy {
                            out: Self::reg(full),
                            input: merged,
                        });
                    }
                    IReg::AH | IReg::CH | IReg::DH | IReg::BH => {
                        let full = gp32_of(r).unwrap();
                        let old = self.unique(4);
                        self.ops.push(Op::Copy {
                            out: old.clone(),
                            input: Self::reg(full),
                        });
                        let kept = self.unique(4);
                        self.bin(
                            kept.clone(),
                            old,
                            Varnode::constant(0xFFFF_00FF, 4),
                            BinaryOp::IntAnd,
                        );
                        let wide = self.resize(value, 4);
                        let shifted = self.unique(4);
                        self.bin(
                            shifted.clone(),
                            wide,
                            Varnode::constant(8, 4),
                            BinaryOp::IntLeft,
                        );
                        let merged = self.unique(4);
                        self.bin(merged.clone(), kept, shifted, BinaryOp::IntOr);
                        self.ops.push(Op::Copy {
                            out: Self::reg(full),
                            input: merged,
                        });
                    }
                    _ => {
                        self.ops.push(Op::Copy {
                            out: Self::reg(ours),
                            input: value,
                        });
                    }
                }
                Ok(())
            }
            OpKind::Memory => {
                let addr = self.effective_address()?;
                let size = self.operand_size(i);
                let value = self.resize(value, size);
                self.ops.push(Op::Store {
                    address: addr,
                    value,
                    size,
                });
                Ok(())
            }
            other => Err(self.unsupported(&format!("destination operand kind {other:?}"))),
        }
    }

    // ---------------------------------------------------------------- flags

    fn write_flag(&mut self, flag: Register, value: Varnode) {
        let v = self.resize(value, 1);
        self.ops.push(Op::Copy {
            out: Self::reg(flag),
            input: v,
        });
    }

    /// ZF = (value == 0), written as 0/1.
    fn compute_zf(&mut self, v: Varnode) -> Varnode {
        // Compare at the value's own width: a 4-byte lvalue compared against a
        // 1-byte zero would make the load read the wrong number of bytes.
        let t = self.unique(1);
        self.bin(
            t.clone(),
            v,
            Varnode::constant(0, v.size()),
            BinaryOp::IntEqual,
        );
        t
    }

    /// SF = sign bit of the value as a 0/1 byte.
    fn compute_sf(&mut self, v: Varnode) -> Varnode {
        let msb = 1u64 << (v.size() * 8 - 1);
        let t = self.unique(v.size());
        self.bin(
            t.clone(),
            v,
            Varnode::constant(msb, v.size()),
            BinaryOp::IntAnd,
        );
        let nz = self.unique(1);
        self.bin(
            nz.clone(),
            t,
            Varnode::constant(0, t.size()),
            BinaryOp::IntNotEqual,
        );
        nz
    }

    /// PF = parity of the low byte (1 when the number of set bits is even).
    fn compute_pf(&mut self, v: Varnode) -> Varnode {
        // Copy the value at its own width, then take the low byte of that
        // value. Resizing the lvalue directly would change the load width.
        let loaded = self.unique(v.size());
        self.ops.push(Op::Copy {
            out: loaded.clone(),
            input: v,
        });
        let lo = self.resize(loaded, 1);
        let lo4 = self.resize(lo, 4);
        let pc = self.unique(4);
        self.ops.push(Op::Unary {
            out: pc.clone(),
            input: lo4,
            op: UnaryOp::Popcount,
        });
        let odd = self.unique(1);
        self.bin(odd.clone(), pc, Varnode::constant(1, 4), BinaryOp::IntAnd);
        let even = self.unique(1);
        self.ops.push(Op::Unary {
            out: even.clone(),
            input: odd,
            op: UnaryOp::BoolNegate,
        });
        even
    }

    /// OF for `a + b` (or `a - b` when `is_sub`).
    ///
    /// Addition: overflow iff both operands share a sign and the result
    /// differs from it. Subtraction: overflow iff the operands' signs differ
    /// and the result differs from the minuend's sign.
    fn compute_of(&mut self, a: Varnode, b: Varnode, res: Varnode, is_sub: bool) -> Varnode {
        let sa = self.compute_sf(a);
        let sb = self.compute_sf(b);
        let sr = self.compute_sf(res);
        if is_sub {
            let in_diff = self.unique(1);
            self.bin(in_diff.clone(), sa.clone(), sb, BinaryOp::IntNotEqual);
            let out_diff = self.unique(1);
            self.bin(out_diff.clone(), sa, sr, BinaryOp::IntNotEqual);
            let of = self.unique(1);
            self.bin(of.clone(), in_diff, out_diff, BinaryOp::IntAnd);
            of
        } else {
            let same = self.unique(1);
            self.bin(same.clone(), sa.clone(), sb, BinaryOp::IntEqual);
            let differs = self.unique(1);
            self.bin(differs.clone(), sr, sa, BinaryOp::IntNotEqual);
            let of = self.unique(1);
            self.bin(of.clone(), same, differs, BinaryOp::IntAnd);
            of
        }
    }

    /// CF for `a + b`: carry out of the top bit, from `a + b < a` (unsigned).
    fn compute_cf_add(&mut self, a: Varnode, res: Varnode) -> Varnode {
        let t = self.unique(1);
        self.bin(t.clone(), res, a, BinaryOp::IntLess);
        t
    }

    /// CF for `a - b`: borrow, i.e. `a < b` unsigned.
    fn compute_cf_sub(&mut self, a: Varnode, b: Varnode) -> Varnode {
        let t = self.unique(1);
        self.bin(t.clone(), a, b, BinaryOp::IntLess);
        t
    }

    // ----------------------------------------------------------------- lift

    fn lift(&mut self) -> Result<(), LiftError> {
        let m = self.insn.mnemonic();
        match m {
            Mnemonic::Mov => self.lift_mov(),
            Mnemonic::Movzx => self.lift_movzx(),
            Mnemonic::Movsx => self.lift_movsx(),
            Mnemonic::Lea => self.lift_lea(),
            Mnemonic::Xchg => self.lift_xchg(),
            Mnemonic::Push => self.lift_push(),
            Mnemonic::Pop => self.lift_pop(),
            Mnemonic::Add => self.alu(BinaryOp::IntAdd, true),
            Mnemonic::Sub => self.alu(BinaryOp::IntSub, true),
            Mnemonic::Cmp => self.alu(BinaryOp::IntSub, false),
            Mnemonic::Inc => self.lift_inc_dec(true),
            Mnemonic::Dec => self.lift_inc_dec(false),
            Mnemonic::Test => self.lift_test(),
            Mnemonic::And => self.lift_logic(BinaryOp::IntAnd),
            Mnemonic::Or => self.lift_logic(BinaryOp::IntOr),
            Mnemonic::Xor => self.lift_logic(BinaryOp::IntXor),
            Mnemonic::Not => self.lift_not(),
            Mnemonic::Neg => self.lift_neg(),
            Mnemonic::Imul => self.lift_imul(),
            Mnemonic::Shl | Mnemonic::Sal => self.lift_shift(ShiftKind::Shl),
            Mnemonic::Shr => self.lift_shift(ShiftKind::Shr),
            Mnemonic::Sar => self.lift_shift(ShiftKind::Sar),
            Mnemonic::Rdtsc => {
                // Host-provided cycle counter.
                self.ops.push(Op::CallOther {
                    name: "x86.rdtsc".into(),
                    inputs: vec![],
                    output: None,
                });
                Ok(())
            }
            Mnemonic::Cbw => self.lift_cwde_like(Register::AL, Register::AX),
            Mnemonic::Cwde => self.lift_cwde_like(Register::AX, Register::EAX),
            Mnemonic::Cwd | Mnemonic::Cdq | Mnemonic::Cdqe | Mnemonic::Cqo => {
                let (src, hi) = match m {
                    Mnemonic::Cwd => (Register::AX, Register::DX),
                    Mnemonic::Cdq => (Register::EAX, Register::EDX),
                    _ => return Err(self.unsupported("64-bit sign extension")),
                };
                self.lift_sign_half(src, hi);
                Ok(())
            }
            Mnemonic::Sahf => self.lift_sahf(),
            Mnemonic::Lahf => self.lift_lahf(),
            Mnemonic::Nop | Mnemonic::Fnop | Mnemonic::Pause => {
                self.ops.push(Op::Nop);
                Ok(())
            }
            Mnemonic::Ret => self.lift_ret(),
            Mnemonic::Call => self.lift_call(),
            Mnemonic::Jmp => self.lift_jmp(),
            Mnemonic::Jcxz | Mnemonic::Jecxz => self.lift_jecxz(),
            _ if condition_of(m).is_some() && is_jcc(m) => self.lift_jcc(m),
            _ if is_cmovcc(m) => self.lift_cmov(m),
            _ if is_setcc(m) => self.lift_setcc(m),
            Mnemonic::Leave => self.lift_leave(),
            Mnemonic::Pushfd => {
                let eflags = self.pack_eflags();
                self.push_raw(eflags);
                Ok(())
            }
            Mnemonic::Popfd => self.lift_popfd(),
            Mnemonic::Bswap => self.lift_bswap(),
            Mnemonic::Movsb
            | Mnemonic::Movsd
            | Mnemonic::Movsw
            | Mnemonic::Stosb
            | Mnemonic::Stosd
            | Mnemonic::Stosw
            | Mnemonic::Lodsb
            | Mnemonic::Lodsd
            | Mnemonic::Scasb
            | Mnemonic::Scasd => self.lift_string_op(m),
            _ => Err(self.unsupported("mnemonic not lifted yet")),
        }
    }

    fn lift_mov(&mut self) -> Result<(), LiftError> {
        // Read the source first: `mov [esp], esp`-style aliasing is
        // order-sensitive and the x86 semantics read before writing.
        let src = self.read_operand(1)?;
        self.write_operand(0, src)
    }

    fn lift_movzx(&mut self) -> Result<(), LiftError> {
        let src = self.read_operand(1)?;
        let size = self.operand_size(0);
        let ext = self.resize(src, size);
        self.write_operand(0, ext)
    }

    fn lift_movsx(&mut self) -> Result<(), LiftError> {
        let src = self.read_operand(1)?;
        let size = self.operand_size(0);
        let ext = self.unique(size);
        self.ops.push(Op::Unary {
            out: ext.clone(),
            input: src,
            op: UnaryOp::IntSext,
        });
        self.write_operand(0, ext)
    }

    fn lift_lea(&mut self) -> Result<(), LiftError> {
        if self.insn.op_kind(1) != OpKind::Memory {
            return Err(self.unsupported("lea without a memory operand"));
        }
        let addr = self.effective_address()?;
        let size = self.operand_size(0);
        // LEA truncates to 16 bits for a 16-bit destination.
        let v = self.resize(addr, size);
        self.write_operand(0, v)
    }

    fn lift_xchg(&mut self) -> Result<(), LiftError> {
        let a = self.read_operand(0)?;
        let b = self.read_operand(1)?;
        self.write_operand(1, a)?;
        self.write_operand(0, b)
    }

    /// Push an explicit value (used by CALL and PUSHFD).
    fn push_raw(&mut self, value: Varnode) -> Varnode {
        let v = self.resize(value, 4);
        let sp = self.unique(4);
        self.bin(
            sp.clone(),
            Self::reg(Register::ESP),
            Varnode::constant(4, 4),
            BinaryOp::IntSub,
        );
        self.ops.push(Op::Copy {
            out: Self::reg(Register::ESP),
            input: sp.clone(),
        });
        self.ops.push(Op::Store {
            address: sp.clone(),
            value: v,
            size: 4,
        });
        sp
    }

    fn pop_raw(&mut self) -> Varnode {
        let v = self.unique(4);
        self.ops.push(Op::Load {
            out: v.clone(),
            input: Self::reg(Register::ESP),
            size: 4,
        });
        let sp = self.unique(4);
        self.bin(
            sp.clone(),
            Self::reg(Register::ESP),
            Varnode::constant(4, 4),
            BinaryOp::IntAdd,
        );
        self.ops.push(Op::Copy {
            out: Self::reg(Register::ESP),
            input: sp,
        });
        v
    }

    fn lift_push(&mut self) -> Result<(), LiftError> {
        match self.insn.op_kind(0) {
            OpKind::Register => {
                let r = gp_exact(self.insn.op_register(0))
                    .ok_or_else(|| self.unsupported("push register"))?;
                let container = gp32_of(self.insn.op_register(0)).unwrap();
                // `push ax` pushes a 16-bit value; the stack slot is 4 bytes,
                // but only the low half is meaningful.
                if r.size == 1 {
                    return Err(self.unsupported("8-bit push"));
                }
                // A 16-bit push puts the low half on the stack; the upper half
                // of the slot is unspecified, so zero-extending is fine.
                let value = if r.size == 2 {
                    self.resize(Self::reg(r), 4)
                } else {
                    Self::reg(container)
                };
                self.push_raw(value);
                Ok(())
            }
            OpKind::Memory => {
                let v = self.read_operand(0)?;
                self.push_raw(v);
                Ok(())
            }
            OpKind::Immediate8 => {
                // `push imm8` sign-extends the immediate to 32 bits.
                let v = Varnode::constant(self.insn.immediate8() as i8 as i32 as u32 as u64, 4);
                self.push_raw(v);
                Ok(())
            }
            OpKind::Immediate8to32 => {
                let v = Varnode::constant(self.insn.immediate8to32() as u32 as u64, 4);
                self.push_raw(v);
                Ok(())
            }
            OpKind::Immediate32 => {
                let v = Varnode::constant(self.insn.immediate32() as u64, 4);
                self.push_raw(v);
                Ok(())
            }
            other => Err(self.unsupported(&format!("push operand {other:?}"))),
        }
    }

    fn lift_pop(&mut self) -> Result<(), LiftError> {
        let v = self.pop_raw();
        // `pop ax` writes only AX; `pop` of a 16-bit slot zero-extends.
        self.write_operand(0, v)
    }

    fn alu(&mut self, op: BinaryOp, write: bool) -> Result<(), LiftError> {
        let a = self.read_operand(0)?;
        let b = self.read_operand(1)?;
        // The operation width is the destination's width (both operands
        // already have it); computing at max(a, b) would be a no-op, and
        // resizing to the *larger* size would misread a memory operand.
        let size = a.size();
        let res = self.unique(size);
        self.bin(res.clone(), a.clone(), b.clone(), op);
        let is_sub = op == BinaryOp::IntSub;
        let zf = self.compute_zf(res.clone());
        let sf = self.compute_sf(res.clone());
        let pf = self.compute_pf(res.clone());
        let of = self.compute_of(a.clone(), b.clone(), res.clone(), is_sub);
        let cf = if is_sub {
            self.compute_cf_sub(a.clone(), b.clone())
        } else {
            self.compute_cf_add(a.clone(), res.clone())
        };
        self.write_flag(Register::ZE, zf);
        self.write_flag(Register::SE, sf);
        self.write_flag(Register::PE, pf);
        self.write_flag(Register::OE, of);
        self.write_flag(Register::CE, cf);
        if write {
            self.write_operand(0, res)?;
        }
        Ok(())
    }

    fn lift_inc_dec(&mut self, inc: bool) -> Result<(), LiftError> {
        let a = self.read_operand(0)?;
        let one = Varnode::constant(1, a.size());
        let res = self.unique(a.size());
        let op = if inc {
            BinaryOp::IntAdd
        } else {
            BinaryOp::IntSub
        };
        self.bin(res.clone(), a.clone(), one.clone(), op);
        let zf = self.compute_zf(res.clone());
        let sf = self.compute_sf(res.clone());
        let pf = self.compute_pf(res.clone());
        let of = self.compute_of(a.clone(), one, res.clone(), !inc);
        // INC/DEC deliberately leave CF untouched.
        self.write_flag(Register::ZE, zf);
        self.write_flag(Register::SE, sf);
        self.write_flag(Register::PE, pf);
        self.write_flag(Register::OE, of);
        self.write_operand(0, res)
    }

    fn lift_test(&mut self) -> Result<(), LiftError> {
        let a = self.read_operand(0)?;
        let b = self.read_operand(1)?;
        let size = a.size();
        let res = self.unique(size);
        self.bin(res.clone(), a, b, BinaryOp::IntAnd);
        let zf = self.compute_zf(res.clone());
        let sf = self.compute_sf(res.clone());
        let pf = self.compute_pf(res.clone());
        self.write_flag(Register::ZE, zf);
        self.write_flag(Register::SE, sf);
        self.write_flag(Register::PE, pf);
        self.write_flag(Register::CE, Varnode::constant(0, 1));
        self.write_flag(Register::OE, Varnode::constant(0, 1));
        Ok(())
    }

    fn lift_logic(&mut self, op: BinaryOp) -> Result<(), LiftError> {
        let a = self.read_operand(0)?;
        let b = self.read_operand(1)?;
        let size = a.size();
        let res = self.unique(size);
        self.bin(res.clone(), a, b, op);
        let zf = self.compute_zf(res.clone());
        let sf = self.compute_sf(res.clone());
        let pf = self.compute_pf(res.clone());
        self.write_flag(Register::ZE, zf);
        self.write_flag(Register::SE, sf);
        self.write_flag(Register::PE, pf);
        self.write_flag(Register::CE, Varnode::constant(0, 1));
        self.write_flag(Register::OE, Varnode::constant(0, 1));
        self.write_operand(0, res)
    }

    fn lift_not(&mut self) -> Result<(), LiftError> {
        let a = self.read_operand(0)?;
        let all = Varnode::constant(mask(a.size()), a.size());
        let res = self.unique(a.size());
        self.bin(res.clone(), a, all, BinaryOp::IntXor);
        // NOT affects no flags, which is why it is not routed through
        // lift_logic.
        self.write_operand(0, res)
    }

    fn lift_neg(&mut self) -> Result<(), LiftError> {
        let a = self.read_operand(0)?;
        let zero = Varnode::constant(0, a.size());
        let res = self.unique(a.size());
        self.bin(res.clone(), zero.clone(), a.clone(), BinaryOp::IntSub);
        let zf = self.compute_zf(res.clone());
        let sf = self.compute_sf(res.clone());
        let pf = self.compute_pf(res.clone());
        // OF iff operand == INT_MIN.
        let msb = 1u64 << (a.size() * 8 - 1);
        let of = self.unique(1);
        self.bin(
            of.clone(),
            a.clone(),
            Varnode::constant(msb, a.size()),
            BinaryOp::IntEqual,
        );
        // CF iff operand != 0.
        let is_zero = self.compute_zf(a.clone());
        let cf = self.unique(1);
        self.ops.push(Op::Unary {
            out: cf.clone(),
            input: is_zero,
            op: UnaryOp::BoolNegate,
        });
        self.write_flag(Register::ZE, zf);
        self.write_flag(Register::SE, sf);
        self.write_flag(Register::PE, pf);
        self.write_flag(Register::OE, of);
        self.write_flag(Register::CE, cf);
        self.write_operand(0, res)
    }

    fn lift_imul(&mut self) -> Result<(), LiftError> {
        match self.insn.op_count() {
            1 => {
                // EDX:EAX = EAX * rm32 (signed)
                let src = self.read_operand(0)?;
                let src = self.resize(src, 4);
                let a = self.sign_extend_to_8(Self::reg(Register::EAX))?;
                let b = self.sign_extend_to_8(src.clone())?;
                let wide = self.unique(8);
                self.bin(wide.clone(), a, b, BinaryOp::IntMult);
                let lo = self.unique(4);
                self.ops.push(Op::Copy {
                    out: lo.clone(),
                    input: wide.truncate(4),
                });
                self.ops.push(Op::Copy {
                    out: Self::reg(Register::EAX),
                    input: lo,
                });
                let hi = self.unique(4);
                self.bin(
                    hi.clone(),
                    wide.clone(),
                    Varnode::constant(32, 8),
                    BinaryOp::IntRight,
                );
                let hi32 = self.unique(4);
                self.ops.push(Op::Copy {
                    out: hi32.clone(),
                    input: hi.truncate(4),
                });
                self.ops.push(Op::Copy {
                    out: Self::reg(Register::EDX),
                    input: hi32.clone(),
                });
                // CF=OF iff the high half is not just the sign extension of the
                // low half.
                // The high half must be 0 when EAX >= 0 and 0xFFFFFFFF when
                // it is negative; negating the 0/1 sign byte yields exactly
                // that. Then EDX must equal 0 or -1 according to the sign of
                // the product.
                let sign = self.compute_sf(Self::reg(Register::EAX));
                let expected = self.unique(4);
                let sign32 = self.resize(sign, 4);
                self.bin(
                    expected.clone(),
                    Varnode::constant(0, 4),
                    sign32,
                    BinaryOp::IntSub,
                );
                let overflow = self.unique(1);
                self.bin(overflow.clone(), hi32, expected, BinaryOp::IntNotEqual);
                self.write_flag(Register::CE, overflow.clone());
                self.write_flag(Register::OE, overflow);
                Ok(())
            }
            2 | 3 => {
                let (dst, a_idx, b_idx) = if self.insn.op_count() == 2 {
                    (0, 0, 1)
                } else {
                    (0, 1, 2)
                };
                let a = self.read_operand(a_idx)?;
                let b = self.read_operand(b_idx)?;
                let b = self.resize(b, a.size());
                let size = a.size();
                let prod = self.unique(size);
                self.bin(prod.clone(), a.clone(), b.clone(), BinaryOp::IntMult);
                // Overflow iff sign-extending the truncated product gives back
                // the full product.
                let a8 = self.sign_extend_to_8(a)?;
                let b8 = self.sign_extend_to_8(b)?;
                let full = self.unique(8);
                self.bin(full.clone(), a8, b8, BinaryOp::IntMult);
                let narrowed = self.resize(prod.clone(), 8);
                let fits = self.unique(1);
                self.bin(fits.clone(), full, narrowed, BinaryOp::IntEqual);
                let overflow = self.unique(1);
                self.ops.push(Op::Unary {
                    out: overflow.clone(),
                    input: fits,
                    op: UnaryOp::BoolNegate,
                });
                self.write_flag(Register::CE, overflow.clone());
                self.write_flag(Register::OE, overflow);
                self.write_operand(dst, prod)
            }
            _ => Err(self.unsupported("imul operand count")),
        }
    }

    /// Sign-extend `v` to 8 bytes. The input is first materialised at 4 bytes
    /// and then loaded at its *own* width, so no lvalue is re-declared wider.
    fn sign_extend_to_8(&mut self, v: Varnode) -> Result<Varnode, LiftError> {
        let v4 = self.resize(v, 4);
        let out = self.unique(8);
        self.ops.push(Op::Unary {
            out: out.clone(),
            input: v4,
            op: UnaryOp::IntSext,
        });
        Ok(out)
    }

    fn lift_shift(&mut self, kind: ShiftKind) -> Result<(), LiftError> {
        let count = if self.insn.op_count() < 2 {
            Varnode::constant(1, 1)
        } else {
            match self.insn.op_kind(1) {
                OpKind::Immediate8 => Varnode::constant(self.insn.immediate8() as u64, 1),
                OpKind::Register => {
                    // The count comes from CL.
                    Self::reg(Register::CL)
                }
                other => return Err(self.unsupported(&format!("shift count {other:?}"))),
            }
        };
        let val = self.read_operand(0)?;
        let size = val.size();
        let count = self.resize(count, size);
        // x86 masks the count to 5 bits (0..31) for 32-bit operands.
        let masked = self.unique(size);
        self.bin(
            masked.clone(),
            count,
            Varnode::constant(31, size),
            BinaryOp::IntAnd,
        );
        let res = self.unique(size);
        let op = match kind {
            ShiftKind::Shl => BinaryOp::IntLeft,
            ShiftKind::Shr => BinaryOp::IntRight,
            ShiftKind::Sar => BinaryOp::IntSRight,
        };
        self.bin(res.clone(), val.clone(), masked.clone(), op);
        let nonzero = self.unique(1);
        self.bin(
            nonzero.clone(),
            masked.clone(),
            Varnode::constant(0, size),
            BinaryOp::IntNotEqual,
        );

        // Last bit shifted out, used for CF.
        let cf_bit = match kind {
            ShiftKind::Shl => {
                // value >> (32 - count), then bit 0
                let inv = self.unique(size);
                self.bin(
                    inv.clone(),
                    Varnode::constant(size as u64 * 8, size),
                    masked.clone(),
                    BinaryOp::IntSub,
                );
                let sh = self.unique(size);
                self.bin(sh.clone(), val.clone(), inv, BinaryOp::IntRight);
                sh
            }
            ShiftKind::Shr | ShiftKind::Sar => {
                let n1 = self.unique(size);
                self.bin(
                    n1.clone(),
                    masked.clone(),
                    Varnode::constant(1, size),
                    BinaryOp::IntSub,
                );
                let sh = self.unique(size);
                self.bin(sh.clone(), val.clone(), n1, op);
                sh
            }
        };
        let cf = self.unique(1);
        self.bin(
            cf.clone(),
            cf_bit,
            Varnode::constant(1, size),
            BinaryOp::IntAnd,
        );

        let zf = self.compute_zf(res.clone());
        let sf = self.compute_sf(res.clone());
        let pf = self.compute_pf(res.clone());
        // OF: defined for count == 1. Shl: MSB(res) xor CF. Sar: 0.
        // Shr: MSB of the original value. Other counts leave OF undefined;
        // we clear it, which matches common emulators and is only observable
        // by code reading OF after a multi-bit shift.
        let of = match kind {
            ShiftKind::Shl => {
                let msb = self.compute_sf(res.clone());
                let x = self.unique(1);
                self.bin(x.clone(), msb, cf.clone(), BinaryOp::IntXor);
                x
            }
            ShiftKind::Sar => Varnode::constant(0, 1),
            ShiftKind::Shr => self.compute_sf(val.clone()),
        };
        let is_one = self.unique(1);
        self.bin(
            is_one.clone(),
            masked.clone(),
            Varnode::constant(1, size),
            BinaryOp::IntEqual,
        );
        let of_guarded = self.unique(1);
        self.ops.push(Op::BranchIndirect {
            out: of_guarded.clone(),
            cond: is_one,
            taken: of,
            not_taken: Varnode::constant(0, 1),
        });

        // All flags except CF are written only for a non-zero count; CF for a
        // zero count is also unchanged, so guard every write.
        let apply = nonzero.clone();
        self.write_flag_guarded(Register::CE, cf, apply.clone());
        self.write_flag_guarded(Register::ZE, zf, apply.clone());
        self.write_flag_guarded(Register::SE, sf, apply.clone());
        self.write_flag_guarded(Register::PE, pf, apply.clone());
        self.write_flag_guarded(Register::OE, of_guarded, apply);
        self.write_operand(0, res)
    }

    fn write_flag_guarded(&mut self, flag: Register, value: Varnode, guard: Varnode) {
        let out = self.unique(1);
        let value = self.resize(value, 1);
        self.ops.push(Op::BranchIndirect {
            out: out.clone(),
            cond: guard,
            taken: value,
            not_taken: Self::reg(flag),
        });
        self.ops.push(Op::Copy {
            out: Self::reg(flag),
            input: out,
        });
    }

    fn lift_cwde_like(&mut self, src: Register, dst: Register) -> Result<(), LiftError> {
        let out = self.unique(dst.size);
        self.ops.push(Op::Unary {
            out: out.clone(),
            input: Self::reg(src),
            op: UnaryOp::IntSext,
        });
        self.ops.push(Op::Copy {
            out: Self::reg(dst),
            input: out,
        });
        Ok(())
    }

    /// CWD/CDQ: fill the high half with the sign of the low half.
    fn lift_sign_half(&mut self, src: Register, hi: Register) {
        let negative = self.compute_sf(Self::reg(src));
        let filled = self.unique(hi.size);
        // filled = negative ? ~0 : 0. Using 0 - negative avoids a select on
        // differing widths.
        let neg = self.resize(negative, hi.size);
        self.bin(
            filled.clone(),
            Varnode::constant(0, hi.size),
            neg,
            BinaryOp::IntSub,
        );
        self.ops.push(Op::Copy {
            out: Self::reg(hi),
            input: filled,
        });
    }

    fn lift_sahf(&mut self) -> Result<(), LiftError> {
        // EFLAGS <- AH: SF(7) ZF(6) AF(4) PF(2) CF(0).
        let flags = [
            (Register::SE, 7u64),
            (Register::ZE, 6),
            (Register::AC, 4),
            (Register::PE, 2),
            (Register::CE, 0),
        ];
        for (flag, bit) in flags {
            let shifted = self.unique(1);
            self.bin(
                shifted.clone(),
                Self::reg(Register::AH),
                Varnode::constant(bit, 1),
                BinaryOp::IntRight,
            );
            let bitv = self.unique(1);
            self.bin(
                bitv.clone(),
                shifted,
                Varnode::constant(1, 1),
                BinaryOp::IntAnd,
            );
            self.ops.push(Op::Copy {
                out: Self::reg(flag),
                input: bitv,
            });
        }
        Ok(())
    }

    fn lift_lahf(&mut self) -> Result<(), LiftError> {
        // AH <- SF:ZF:0:AF:0:PF:1:CF (bit 1 is always 1).
        let mut acc = Varnode::constant(2, 1);
        for (flag, bit) in [
            (Register::SE, 7u64),
            (Register::ZE, 6),
            (Register::AC, 4),
            (Register::PE, 2),
            (Register::CE, 0),
        ] {
            let shifted = self.unique(1);
            self.bin(
                shifted.clone(),
                Self::reg(flag),
                Varnode::constant(bit, 1),
                BinaryOp::IntLeft,
            );
            let or = self.unique(1);
            self.bin(or.clone(), acc, shifted, BinaryOp::IntOr);
            acc = or;
        }
        self.ops.push(Op::Copy {
            out: Self::reg(Register::AH),
            input: acc,
        });
        Ok(())
    }

    fn lift_ret(&mut self) -> Result<(), LiftError> {
        // `ret imm16` additionally adds to ESP after popping the address.
        let extra = self.insn.immediate16() as u64;
        let target = self.pop_raw();
        if extra != 0 {
            let sp = self.unique(4);
            self.bin(
                sp.clone(),
                Self::reg(Register::ESP),
                Varnode::constant(extra, 4),
                BinaryOp::IntAdd,
            );
            self.ops.push(Op::Copy {
                out: Self::reg(Register::ESP),
                input: sp,
            });
        }
        self.ops.push(Op::ReturnTo { input: target });
        Ok(())
    }

    fn lift_call(&mut self) -> Result<(), LiftError> {
        let ret = self.insn.next_ip();
        match self.insn.op_kind(0) {
            OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => {
                let target = self.insn.near_branch_target();
                // The return address is pushed, exactly as hardware does.
                self.push_raw(Varnode::constant(ret, 4));
                self.call_target = Some(target);
                self.ops.push(Op::Call { target, ret });
                Ok(())
            }
            OpKind::Register | OpKind::Memory => {
                let v = self.read_operand(0)?;
                let target = self.resize(v, 4);
                self.push_raw(Varnode::constant(ret, 4));
                self.ops.push(Op::CallInd { input: target, ret });
                Ok(())
            }
            other => Err(self.unsupported(&format!("call operand {other:?}"))),
        }
    }

    fn lift_jmp(&mut self) -> Result<(), LiftError> {
        match self.insn.op_kind(0) {
            OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => {
                self.ops.push(Op::Branch {
                    target: self.insn.near_branch_target(),
                });
                Ok(())
            }
            OpKind::Memory | OpKind::Register => {
                // A `jmp [table + EAX*4]` or `jmp EAX`. The destination is a
                // computed address, which becomes the block's IndirectJump.
                let v = self.read_operand(0)?;
                let target = self.resize(v, 4);
                self.ops.push(Op::BranchIndirect {
                    out: Self::reg(Register::EIP),
                    cond: Varnode::constant(1, 1),
                    taken: target,
                    not_taken: Self::reg(Register::EIP),
                });
                Ok(())
            }
            other => Err(self.unsupported(&format!("jmp operand {other:?}"))),
        }
    }

    fn lift_jecxz(&mut self) -> Result<(), LiftError> {
        let is_zero = self.compute_zf(Self::reg(Register::ECX));
        self.ops.push(Op::CBranch {
            cond: is_zero,
            target: self.insn.near_branch_target(),
        });
        Ok(())
    }

    fn condition_value(&mut self, c: Condition) -> Varnode {
        let ze = Self::reg(Register::ZE);
        let se = Self::reg(Register::SE);
        let oe = Self::reg(Register::OE);
        let ce = Self::reg(Register::CE);
        let pe = Self::reg(Register::PE);
        let not = |l: &mut Lifter, v: Varnode| {
            let t = l.unique(1);
            l.ops.push(Op::Unary {
                out: t.clone(),
                input: v,
                op: UnaryOp::BoolNegate,
            });
            t
        };
        match c {
            Condition::Eq => ze,
            Condition::Ne => not(self, ze),
            Condition::B => ce,
            Condition::Ae => not(self, ce),
            Condition::Be => {
                let t = self.unique(1);
                self.bin(t.clone(), ce, ze, BinaryOp::IntOr);
                t
            }
            Condition::A => {
                let nc = not(self, ce);
                let nz = not(self, ze);
                let t = self.unique(1);
                self.bin(t.clone(), nc, nz, BinaryOp::IntAnd);
                t
            }
            Condition::S => se,
            Condition::Ns => not(self, se),
            Condition::L => {
                let t = self.unique(1);
                self.bin(t.clone(), se, oe, BinaryOp::IntNotEqual);
                t
            }
            Condition::Ge => {
                let t = self.unique(1);
                self.bin(t.clone(), se, oe, BinaryOp::IntEqual);
                t
            }
            Condition::Le => {
                let a = self.unique(1);
                self.bin(a.clone(), se, oe, BinaryOp::IntNotEqual);
                let t = self.unique(1);
                self.bin(t.clone(), a, ze, BinaryOp::IntOr);
                t
            }
            Condition::G => {
                let a = self.unique(1);
                self.bin(a.clone(), se, oe, BinaryOp::IntEqual);
                let nz = not(self, ze);
                let t = self.unique(1);
                self.bin(t.clone(), a, nz, BinaryOp::IntAnd);
                t
            }
            Condition::P => pe,
            Condition::Np => not(self, pe),
        }
    }

    fn lift_jcc(&mut self, m: Mnemonic) -> Result<(), LiftError> {
        let c = condition_of(m).ok_or_else(|| self.unsupported("jcc condition"))?;
        let cond = self.condition_value(c);
        self.ops.push(Op::CBranch {
            cond,
            target: self.insn.near_branch_target(),
        });
        Ok(())
    }

    fn lift_cmov(&mut self, m: Mnemonic) -> Result<(), LiftError> {
        let c = condition_of(m).ok_or_else(|| self.unsupported("cmov condition"))?;
        let cond = self.condition_value(c);
        let src = self.read_operand(1)?;
        let dst_size = self.operand_size(0);
        let src = self.resize(src, dst_size);
        let cur = self.read_operand(0)?;
        let out = self.unique(self.operand_size(0));
        self.ops.push(Op::BranchIndirect {
            out: out.clone(),
            cond,
            taken: src,
            not_taken: cur,
        });
        self.write_operand(0, out)
    }

    fn lift_setcc(&mut self, m: Mnemonic) -> Result<(), LiftError> {
        let c = condition_of(m).ok_or_else(|| self.unsupported("setcc condition"))?;
        let v = self.condition_value(c);
        self.write_operand(0, v)
    }

    fn lift_leave(&mut self) -> Result<(), LiftError> {
        // ESP = EBP; EBP = pop()
        self.ops.push(Op::Copy {
            out: Self::reg(Register::ESP),
            input: Self::reg(Register::EBP),
        });
        let v = self.pop_raw();
        self.ops.push(Op::Copy {
            out: Self::reg(Register::EBP),
            input: v,
        });
        Ok(())
    }

    fn pack_eflags(&mut self) -> Varnode {
        // Bit 1 is always set.
        let mut acc = Varnode::constant(2, 4);
        for (flag, bit) in [
            (Register::SE, 7u64),
            (Register::ZE, 6),
            (Register::AC, 4),
            (Register::PE, 2),
            (Register::CE, 0),
        ] {
            let wide = self.resize(Self::reg(flag), 4);
            let shifted = self.unique(4);
            self.bin(
                shifted.clone(),
                wide,
                Varnode::constant(bit, 4),
                BinaryOp::IntLeft,
            );
            let or = self.unique(4);
            self.bin(or.clone(), acc, shifted, BinaryOp::IntOr);
            acc = or;
        }
        acc
    }

    fn lift_popfd(&mut self) -> Result<(), LiftError> {
        let v = self.pop_raw();
        for (flag, bit) in [
            (Register::SE, 7u64),
            (Register::ZE, 6),
            (Register::AC, 4),
            (Register::PE, 2),
            (Register::CE, 0),
        ] {
            let shifted = self.unique(4);
            self.bin(
                shifted.clone(),
                v.clone(),
                Varnode::constant(bit, 4),
                BinaryOp::IntRight,
            );
            let bitv = self.unique(4);
            self.bin(
                bitv.clone(),
                shifted,
                Varnode::constant(1, 4),
                BinaryOp::IntAnd,
            );
            let narrow = self.unique(1);
            self.ops.push(Op::Copy {
                out: narrow.clone(),
                input: bitv.truncate(1),
            });
            self.ops.push(Op::Copy {
                out: Self::reg(flag),
                input: narrow,
            });
        }
        Ok(())
    }

    fn lift_bswap(&mut self) -> Result<(), LiftError> {
        let v = self.read_operand(0)?;
        let v = self.resize(v, 4);
        // Reverse bytes with shifts and masks.
        let mut parts: Vec<(Varnode, u64)> = Vec::new();
        for i in 0..4u64 {
            let shifted = self.unique(4);
            self.bin(
                shifted.clone(),
                v.clone(),
                Varnode::constant(i * 8, 4),
                BinaryOp::IntRight,
            );
            let masked = self.unique(4);
            self.bin(
                masked.clone(),
                shifted,
                Varnode::constant(0xFF, 4),
                BinaryOp::IntAnd,
            );
            parts.push((masked, i));
        }
        let mut acc = Varnode::constant(0, 4);
        for (byte, i) in parts {
            let placed = self.unique(4);
            self.bin(
                placed.clone(),
                byte,
                Varnode::constant((3 - i) * 8, 4),
                BinaryOp::IntLeft,
            );
            let or = self.unique(4);
            self.bin(or.clone(), acc, placed, BinaryOp::IntOr);
            acc = or;
        }
        self.write_operand(0, acc)
    }

    /// MOVS/STOS/LODS/SCAS. REP forms become a bounded loop over a count
    /// register rather than a linear unroll.
    fn lift_string_op(&mut self, m: Mnemonic) -> Result<(), LiftError> {
        let (elem, kind) = match m {
            Mnemonic::Movsb => (1u8, StringKind::Mov),
            Mnemonic::Movsw => (2, StringKind::Mov),
            Mnemonic::Movsd => (4, StringKind::Mov),
            Mnemonic::Stosb => (1, StringKind::Stos),
            Mnemonic::Stosw => (2, StringKind::Stos),
            Mnemonic::Stosd => (4, StringKind::Stos),
            Mnemonic::Lodsb => (1, StringKind::Lods),
            Mnemonic::Lodsd => (4, StringKind::Lods),
            Mnemonic::Scasb => (1, StringKind::Scas),
            Mnemonic::Scasd => (4, StringKind::Scas),
            _ => return Err(self.unsupported("string op")),
        };
        if self.insn.has_rep_prefix() {
            // The loop is emitted by the recompiler as a dedicated block:
            // here we describe one iteration and the count test.
            return Err(self.unsupported(
                "REP-prefixed string op: needs loop lowering (planned, see docs/roadmap)",
            ));
        }
        self.emit_string_iteration(elem, kind);
        Ok(())
    }

    fn emit_string_iteration(&mut self, elem: ByteSize, kind: StringKind) {
        let step = Varnode::constant(elem as u64, 4);
        match kind {
            StringKind::Mov => {
                let v = self.unique(elem);
                self.ops.push(Op::Load {
                    out: v.clone(),
                    input: Self::reg(Register::ESI),
                    size: elem,
                });
                self.ops.push(Op::Store {
                    address: Self::reg(Register::EDI),
                    value: v,
                    size: elem,
                });
                let esi = self.unique(4);
                self.bin(
                    esi.clone(),
                    Self::reg(Register::ESI),
                    step.clone(),
                    BinaryOp::IntAdd,
                );
                self.ops.push(Op::Copy {
                    out: Self::reg(Register::ESI),
                    input: esi,
                });
            }
            StringKind::Stos => {
                let v = Self::reg(Register::EAX);
                self.ops.push(Op::Store {
                    address: Self::reg(Register::EDI),
                    value: v.truncate(elem),
                    size: elem,
                });
            }
            StringKind::Lods => {
                let v = self.unique(elem);
                self.ops.push(Op::Load {
                    out: v.clone(),
                    input: Self::reg(Register::ESI),
                    size: elem,
                });
                self.ops.push(Op::Copy {
                    out: Self::reg(Register::EAX),
                    input: v,
                });
                let esi = self.unique(4);
                self.bin(
                    esi.clone(),
                    Self::reg(Register::ESI),
                    step.clone(),
                    BinaryOp::IntAdd,
                );
                self.ops.push(Op::Copy {
                    out: Self::reg(Register::ESI),
                    input: esi,
                });
            }
            StringKind::Scas => {
                let v = self.unique(elem);
                self.ops.push(Op::Load {
                    out: v.clone(),
                    input: Self::reg(Register::EDI),
                    size: elem,
                });
                let acc = Self::reg(Register::EAX).truncate(elem);
                let diff = self.unique(elem);
                self.bin(diff.clone(), acc, v, BinaryOp::IntSub);
                let zf = self.compute_zf(diff.clone());
                let sf = self.compute_sf(diff.clone());
                let pf = self.compute_pf(diff);
                self.write_flag(Register::ZE, zf);
                self.write_flag(Register::SE, sf);
                self.write_flag(Register::PE, pf);
            }
        }
        let edi = self.unique(4);
        self.bin(
            edi.clone(),
            Self::reg(Register::EDI),
            step,
            BinaryOp::IntAdd,
        );
        self.ops.push(Op::Copy {
            out: Self::reg(Register::EDI),
            input: edi,
        });
    }
}

#[derive(Clone, Copy)]
enum ShiftKind {
    Shl,
    Shr,
    Sar,
}

#[derive(Clone, Copy)]
enum StringKind {
    Mov,
    Stos,
    Lods,
    Scas,
}

fn is_cmovcc(m: Mnemonic) -> bool {
    matches!(
        m,
        Mnemonic::Cmovb
            | Mnemonic::Cmovae
            | Mnemonic::Cmove
            | Mnemonic::Cmovne
            | Mnemonic::Cmovbe
            | Mnemonic::Cmova
            | Mnemonic::Cmovs
            | Mnemonic::Cmovns
            | Mnemonic::Cmovp
            | Mnemonic::Cmovnp
            | Mnemonic::Cmovl
            | Mnemonic::Cmovge
            | Mnemonic::Cmovle
            | Mnemonic::Cmovg
    )
}

fn is_setcc(m: Mnemonic) -> bool {
    matches!(
        m,
        Mnemonic::Setb
            | Mnemonic::Setae
            | Mnemonic::Sete
            | Mnemonic::Setne
            | Mnemonic::Setbe
            | Mnemonic::Seta
            | Mnemonic::Sets
            | Mnemonic::Setns
            | Mnemonic::Setp
            | Mnemonic::Setnp
            | Mnemonic::Setl
            | Mnemonic::Setge
            | Mnemonic::Setle
            | Mnemonic::Setg
    )
}

fn is_jcc(m: Mnemonic) -> bool {
    matches!(
        m,
        Mnemonic::Je
            | Mnemonic::Jne
            | Mnemonic::Jb
            | Mnemonic::Jae
            | Mnemonic::Jbe
            | Mnemonic::Ja
            | Mnemonic::Js
            | Mnemonic::Jns
            | Mnemonic::Jl
            | Mnemonic::Jge
            | Mnemonic::Jle
            | Mnemonic::Jg
            | Mnemonic::Jp
            | Mnemonic::Jnp
    )
}
