//! Shared conversion from the lifter's IR to the emitter's contract.
#![allow(dead_code)]
use wlam_emit::ir as eir;
pub fn convert(b: &wlam_sleigh::Block) -> eir::Block {
    let ops = b.instructions.iter().flat_map(|i| i.ops.iter()).map(to_eir).collect();
    let term = match &b.terminator {
        wlam_sleigh::Terminator::FallsThrough => eir::Terminator::FallsThrough,
        wlam_sleigh::Terminator::Jump(t) => eir::Terminator::Jump(*t),
        wlam_sleigh::Terminator::Conditional { taken, not_taken } => eir::Terminator::Conditional { taken: *taken, not_taken: *not_taken },
        wlam_sleigh::Terminator::IndirectJump => eir::Terminator::IndirectJump,
        wlam_sleigh::Terminator::Return => eir::Terminator::Return,
        wlam_sleigh::Terminator::Trap(a) => eir::Terminator::Trap(*a),
        wlam_sleigh::Terminator::Undecodable(a) => eir::Terminator::Undecodable(*a),
    };
    eir::Block { start: b.start, end: b.end, ops, terminator: term }
}
pub fn to_eir(ir: &wlam_sleigh::ir::IR) -> eir::IR {
    use eir::Operand as EO;
    use wlam_sleigh::ir::{BinOp as SB, IR as SI, Operand as SO, UnOp as SU};
    fn op(o: &SO) -> EO {
        match o {
            SO::Const { value, size } => EO::Const { value: *value, size: *size },
            SO::Reg { name, size } => EO::Reg { name: name.clone(), size: *size },
            SO::Temp { offset, size } => EO::Temp { offset: *offset, size: *size },
            SO::Ram { offset, size } => EO::Ram { offset: *offset, size: *size },
        }
    }
    let bin = |b: &SB| match b {
        SB::Add => eir::BinOp::Add, SB::Sub => eir::BinOp::Sub, SB::Mul => eir::BinOp::Mul,
        SB::MulFull => eir::BinOp::MulFull, SB::DivS => eir::BinOp::DivS, SB::DivU => eir::BinOp::DivU,
        SB::RemS => eir::BinOp::RemS, SB::RemU => eir::BinOp::RemU, SB::And => eir::BinOp::And,
        SB::Or => eir::BinOp::Or, SB::Xor => eir::BinOp::Xor, SB::Lsh => eir::BinOp::Lsh,
        SB::Rsh => eir::BinOp::Rsh, SB::Asr => eir::BinOp::Asr, SB::Eq => eir::BinOp::Eq,
        SB::NotEq => eir::BinOp::NotEq, SB::LessU => eir::BinOp::LessU, SB::LessEqU => eir::BinOp::LessEqU,
        SB::LessS => eir::BinOp::LessS, SB::LessEqS => eir::BinOp::LessEqS, SB::Carry => eir::BinOp::Carry,
        SB::SCarry => eir::BinOp::SCarry, SB::SBorrow => eir::BinOp::SBorrow,
    };
    match ir {
        SI::Copy { dst, src } => eir::IR::Copy { dst: op(dst), src: op(src) },
        SI::Load { dst, addr, size } => eir::IR::Load { dst: op(dst), addr: op(addr), size: *size },
        SI::Store { addr, value, size } => eir::IR::Store { addr: op(addr), value: op(value), size: *size },
        SI::Bin { op: b, dst, operands } => eir::IR::Bin { op: bin(b), dst: op(dst), operands: Box::new((op(&operands.0), op(&operands.1))) },
        SI::Un { op: u, dst, src } => eir::IR::Un {
            op: match u { SU::Neg => eir::UnOp::Neg, SU::Not => eir::UnOp::Not, SU::Zext => eir::UnOp::Zext, SU::Sext => eir::UnOp::Sext, SU::Popcount => eir::UnOp::Popcount, SU::Lzcount => eir::UnOp::Lzcount },
            dst: op(dst), src: op(src) },
        SI::Subpiece { dst, src, lsb } => eir::IR::Subpiece { dst: op(dst), src: op(src), lsb: *lsb },
        SI::Branch { target } => eir::IR::Branch { target: op(target) },
        SI::IndirectJump { target } => eir::IR::IndirectJump { target: op(target) },
        SI::CBranch { cond, target } => eir::IR::CBranch { cond: op(cond), target: op(target) },
        SI::Call { target } => eir::IR::Call { target: op(target) },
        SI::CallInd { target } => eir::IR::CallInd { target: op(target) },
        SI::Ret { target } => eir::IR::Ret { target: target.as_ref().map(op) },
        SI::Userop { id, args, dst } => eir::IR::Userop { id: *id, args: args.iter().map(op).collect(), dst: dst.as_ref().map(op) },
        SI::Nop => eir::IR::Nop,
    }
}
