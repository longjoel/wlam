//! Reference interpreter for the IR.
//!
//! This is the oracle the wasm translator is checked against. It implements the
//! same semantics in the simplest correct way (a match over ops with a local
//! map and a flat memory), with no optimisation. If the emitter and this
//! interpreter disagree on any test, one of them is wrong — and the test fails
//! rather than preferring either side.

use std::collections::BTreeMap;

use crate::ir::{BinOp, IR, Operand, UnOp};

/// A machine state for interpreting IR: named registers, temps, memory, flags.
pub struct Machine {
    /// Register values, keyed by name.
    pub regs: BTreeMap<String, u64>,
    /// Temp values, keyed by offset.
    pub temps: BTreeMap<u64, u64>,
    /// Guest memory (flat; tests use the first 64 KiB).
    pub mem: Vec<u8>,
}

impl Machine {
    pub fn new(mem_size: usize) -> Self {
        Machine { regs: BTreeMap::new(), temps: BTreeMap::new(), mem: vec![0; mem_size] }
    }

    pub fn set(&mut self, reg: &str, value: u64) {
        self.regs.insert(reg.to_string(), value & 0xffff_ffff);
    }

    pub fn get(&self, reg: &str) -> u64 {
        self.regs.get(reg).copied().unwrap_or(0) & 0xffff_ffff
    }

    pub fn load(&self, addr: u64, size: u32) -> u64 {
        let mut v = 0u64;
        for i in (0..size as u64).rev() {
            let a = ((addr + i) as usize) & (self.mem.len() - 1);
            v = (v << 8) | self.mem[a] as u64;
        }
        v
    }

    pub fn store(&mut self, addr: u64, size: u32, value: u64) {
        for i in 0..size as u64 {
            let a = ((addr + i) as usize) & (self.mem.len() - 1);
            self.mem[a] = ((value >> (8 * i)) & 0xff) as u8;
        }
    }

    fn read(&self, op: &Operand) -> u64 {
        match op {
            Operand::Const { value, .. } => *value,
            Operand::Reg { name, .. } => self.get(name),
            Operand::Temp { offset, .. } => self.temps.get(offset).copied().unwrap_or(0),
            Operand::Ram { offset, .. } => *offset,
        }
    }

    fn write(&mut self, op: &Operand, value: u64) {
        let masked = value & mask(op.size());
        match op {
            Operand::Reg { name, .. } => {
                self.regs.insert(name.to_string(), masked);
            }
            Operand::Temp { offset, .. } => {
                self.temps.insert(*offset, masked);
            }
            Operand::Const { .. } | Operand::Ram { .. } => {
                panic!("IR writes to a non-storage operand: {op:?}")
            }
        }
    }

    /// Execute a straight-line sequence of ops (no control flow).
    pub fn run(&mut self, ops: &[IR]) {
        for ir in ops {
            match ir {
                IR::Copy { dst, src } => {
                    let v = self.read(src);
                    self.write(dst, v);
                }
                IR::Load { dst, addr, size } => {
                    let a = self.read(addr);
                    let v = self.load(a, *size);
                    self.write(dst, v);
                }
                IR::Store { addr, value, size } => {
                    let a = self.read(addr);
                    let v = self.read(value);
                    self.store(a, *size, v);
                }
                IR::Bin { op, dst, operands } => {
                    let a = self.read(&operands.0);
                    let b = self.read(&operands.1);
                    let v = eval_bin(*op, a, b);
                    self.write(dst, v);
                }
                IR::Un { op, dst, src } => {
                    let a = self.read(src);
                    let size = src.size();
                    let v = eval_un(*op, a, size);
                    self.write(dst, v);
                }
                IR::Subpiece { dst, src, lsb } => {
                    let v = self.read(src) >> (lsb * 8);
                    self.write(dst, v);
                }
                IR::Userop { dst, .. } => {
                    if let Some(d) = dst {
                        self.write(d, 0);
                    }
                }
                IR::Nop => {}
                other => panic!("reference interpreter cannot execute control flow: {other:?}"),
            }
        }
    }
}

fn mask(size: u32) -> u64 {
    if size >= 8 {
        u64::MAX
    } else {
        (1u64 << (size * 8)) - 1
    }
}

fn sign_extend(v: u64, size: u32) -> i64 {
    let bits = size * 8;
    if bits >= 64 {
        v as i64
    } else {
        let shift = 64 - bits;
        ((v << shift) as i64) >> shift
    }
}

fn eval_bin(op: BinOp, a: u64, b: u64) -> u64 {
    match op {
        BinOp::Add => a.wrapping_add(b),
        BinOp::Sub => a.wrapping_sub(b),
        BinOp::Mul => a.wrapping_mul(b),
        BinOp::MulFull => a.wrapping_mul(b) & 0xffff_ffff,
        BinOp::DivS => {
            if b == 0 {
                0
            } else {
                (sign_extend(a, 4).wrapping_div(sign_extend(b, 4)) as u64) & 0xffff_ffff
            }
        }
        BinOp::DivU => {
            if b == 0 {
                0
            } else {
                (a & 0xffff_ffff) / (b & 0xffff_ffff)
            }
        }
        BinOp::RemS => {
            if b == 0 {
                0
            } else {
                (sign_extend(a, 4).wrapping_rem(sign_extend(b, 4)) as u64) & 0xffff_ffff
            }
        }
        BinOp::RemU => {
            if b == 0 {
                0
            } else {
                (a & 0xffff_ffff) % (b & 0xffff_ffff)
            }
        }
        BinOp::And => a & b,
        BinOp::Or => a | b,
        BinOp::Xor => a ^ b,
        BinOp::Lsh => a.wrapping_shl(b as u32) & 0xffff_ffff,
        BinOp::Rsh => (a & 0xffff_ffff) >> (b as u32 & 31),
        BinOp::Asr => ((sign_extend(a, 4) >> (b as u32 & 31)) as u64) & 0xffff_ffff,
        BinOp::Eq => (a == b) as u64,
        BinOp::NotEq => (a != b) as u64,
        BinOp::LessU => ((a & 0xffff_ffff) < (b & 0xffff_ffff)) as u64,
        BinOp::LessEqU => ((a & 0xffff_ffff) <= (b & 0xffff_ffff)) as u64,
        BinOp::LessS => (sign_extend(a, 4) < sign_extend(b, 4)) as u64,
        BinOp::LessEqS => (sign_extend(a, 4) <= sign_extend(b, 4)) as u64,
        // Unsigned carry out of a 32-bit addition. The operands must be masked
        // to 32 bits *before* widening: `a` arrives as u64, so 0xffffffff is
        // already 4294967295 and the sum cannot overflow u64.
        BinOp::Carry => {
            let sum = (a & 0xffff_ffff) + (b & 0xffff_ffff);
            (sum > 0xffff_ffff) as u64
        }
        // Signed overflow of a 32-bit addition: the operands share a sign that
        // the 32-bit result does not. Everything must be done in 32-bit space —
        // sign-extending to i64 first makes 0x80000000 positive, so the classic
        // `(sa^r)&(sb^r) < 0` test never fires.
        BinOp::SCarry => {
            let sa = a as u32;
            let sb = b as u32;
            let r = sa.wrapping_add(sb);
            ((((sa ^ r) & (sb ^ r)) as i32) < 0) as u64
        }
        // Signed borrow out of a subtraction, i.e. the sign of
        // (a ^ ((a ^ b) & (a ^ r))).
        BinOp::SBorrow => {
            let sa = a as u32;
            let sb = b as u32;
            let r = sa.wrapping_sub(sb);
            ((((sa ^ ((sa ^ sb) & (sa ^ r))) as i32) < 0) as u64) & 1
        }
    }
}

fn eval_un(op: UnOp, a: u64, size: u32) -> u64 {
    match op {
        UnOp::Neg => (a.wrapping_neg()) & mask(size),
        UnOp::Not => (!a) & mask(size),
        UnOp::Zext => a & mask(size),
        UnOp::Sext => {
            // Sign-extend from the source width into 32 bits, which is what the
            // model stores (2-byte sext -> 4-byte result).
            (sign_extend(a, size) as u64) & 0xffff_ffff
        }
        UnOp::Popcount => a.count_ones() as u64,
        UnOp::Lzcount => ((a & mask(size)) << (32 - size * 8)).leading_zeros() as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_wraps_at_32_bits() {
        let ops = vec![IR::Bin {
            op: BinOp::Add,
            dst: Operand::Reg { name: "EAX".into(), size: 4 },
            operands: Box::new((
                Operand::Const { value: 0xffff_ffff, size: 4 },
                Operand::Const { value: 2, size: 4 },
            )),
        }];
        let mut m = Machine::new(64 * 1024);
        m.run(&ops);
        assert_eq!(m.get("EAX"), 1);
    }

    #[test]
    fn carry_and_overflow_match_hardware_semantics() {
        // 0xFFFFFFFF + 1: carry out, no signed overflow.
        assert_eq!(eval_bin(BinOp::Carry, 0xffff_ffff, 1), 1);
        assert_eq!(eval_bin(BinOp::SCarry, 0xffff_ffff, 1), 0);
        // 0x7FFFFFFF + 1: signed overflow, no carry.
        assert_eq!(eval_bin(BinOp::SCarry, 0x7fff_ffff, 1), 1);
        assert_eq!(eval_bin(BinOp::Carry, 0x7fff_ffff, 1), 0);
        // 5 - 3: no borrow.
        assert_eq!(eval_bin(BinOp::SBorrow, 5, 3), 0);
    }

    #[test]
    fn store_then_load_round_trips() {
        let ops = vec![
            IR::Store {
                addr: Operand::Const { value: 0x100, size: 4 },
                value: Operand::Const { value: 0x1122_3344, size: 4 },
                size: 4,
            },
            IR::Load {
                dst: Operand::Reg { name: "EAX".into(), size: 4 },
                addr: Operand::Const { value: 0x100, size: 4 },
                size: 4,
            },
        ];
        let mut m = Machine::new(64 * 1024);
        m.run(&ops);
        assert_eq!(m.get("EAX"), 0x1122_3344);
    }
}
