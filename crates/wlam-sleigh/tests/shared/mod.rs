//! Shared helpers for integration tests: adapt the lifter's IR to the
//! emitter's contract, collect register names, and convert blocks.
#![allow(dead_code)]

use wlam_emit::ir as eir;
use wlam_sleigh::ir as sir;

pub fn convert(b: &wlam_sleigh::Block) -> eir::Block {
    let ops = b.instructions.iter().flat_map(|i| i.ops.iter()).map(to_eir).collect();
    let term = match &b.terminator {
        wlam_sleigh::Terminator::FallsThrough => eir::Terminator::FallsThrough,
        wlam_sleigh::Terminator::Jump(t) => eir::Terminator::Jump(*t),
        wlam_sleigh::Terminator::Conditional { taken, not_taken } => {
            eir::Terminator::Conditional { taken: *taken, not_taken: *not_taken }
        }
        wlam_sleigh::Terminator::IndirectJump => eir::Terminator::IndirectJump,
        wlam_sleigh::Terminator::Call { resume } => eir::Terminator::Call { resume: *resume },
        wlam_sleigh::Terminator::CallInd { resume } => eir::Terminator::CallInd { resume: *resume },
        wlam_sleigh::Terminator::Return => eir::Terminator::Return,
        wlam_sleigh::Terminator::Trap(a) => eir::Terminator::Trap(*a),
        wlam_sleigh::Terminator::Undecodable(a) => eir::Terminator::Undecodable(*a),
    };
    eir::Block { start: b.start, end: b.end, ops, terminator: term }
}

pub fn to_eir(ir: &sir::IR) -> eir::IR {
    use eir::Operand as EO;
    use sir::Operand as SO;
    fn op(o: &SO) -> EO {
        match o {
            SO::Const { value, size } => EO::Const { value: *value, size: *size },
            SO::Reg { name, size } => EO::Reg { name: name.clone(), size: *size },
            SO::Temp { offset, size } => EO::Temp { offset: *offset, size: *size },
            SO::Ram { offset, size } => EO::Ram { offset: *offset, size: *size },
        }
    }
    match ir {
        sir::IR::Copy { dst, src } => eir::IR::Copy { dst: op(dst), src: op(src) },
        sir::IR::Load { dst, addr, size } => {
            eir::IR::Load { dst: op(dst), addr: op(addr), size: *size }
        }
        sir::IR::Store { addr, value, size } => {
            eir::IR::Store { addr: op(addr), value: op(value), size: *size }
        }
        sir::IR::Bin { op: b, dst, operands } => eir::IR::Bin {
            op: match b {
                sir::BinOp::Add => eir::BinOp::Add,
                sir::BinOp::Sub => eir::BinOp::Sub,
                sir::BinOp::Mul => eir::BinOp::Mul,
                sir::BinOp::MulFull => eir::BinOp::MulFull,
                sir::BinOp::DivS => eir::BinOp::DivS,
                sir::BinOp::DivU => eir::BinOp::DivU,
                sir::BinOp::RemS => eir::BinOp::RemS,
                sir::BinOp::RemU => eir::BinOp::RemU,
                sir::BinOp::And => eir::BinOp::And,
                sir::BinOp::Or => eir::BinOp::Or,
                sir::BinOp::Xor => eir::BinOp::Xor,
                sir::BinOp::Lsh => eir::BinOp::Lsh,
                sir::BinOp::Rsh => eir::BinOp::Rsh,
                sir::BinOp::Asr => eir::BinOp::Asr,
                sir::BinOp::Eq => eir::BinOp::Eq,
                sir::BinOp::NotEq => eir::BinOp::NotEq,
                sir::BinOp::LessU => eir::BinOp::LessU,
                sir::BinOp::LessEqU => eir::BinOp::LessEqU,
                sir::BinOp::LessS => eir::BinOp::LessS,
                sir::BinOp::LessEqS => eir::BinOp::LessEqS,
                sir::BinOp::Carry => eir::BinOp::Carry,
                sir::BinOp::SCarry => eir::BinOp::SCarry,
                sir::BinOp::SBorrow => eir::BinOp::SBorrow,
            },
            dst: op(dst),
            operands: Box::new((op(&operands.0), op(&operands.1))),
        },
        sir::IR::Float { op: f, dst, operands } => eir::IR::Float {
            op: match f {
                sir::FloatOp::Add => eir::FloatOp::Add,
                sir::FloatOp::Sub => eir::FloatOp::Sub,
                sir::FloatOp::Mul => eir::FloatOp::Mul,
                sir::FloatOp::Div => eir::FloatOp::Div,
                sir::FloatOp::Eq => eir::FloatOp::Eq,
                sir::FloatOp::NotEq => eir::FloatOp::NotEq,
                sir::FloatOp::Less => eir::FloatOp::Less,
                sir::FloatOp::LessEq => eir::FloatOp::LessEq,
            },
            dst: op(dst),
            operands: Box::new((op(&operands.0), op(&operands.1))),
        },
        sir::IR::Un { op: u, dst, src } => eir::IR::Un {
            op: match u {
                sir::UnOp::Neg => eir::UnOp::Neg,
                sir::UnOp::Not => eir::UnOp::Not,
                sir::UnOp::Zext => eir::UnOp::Zext,
                sir::UnOp::Sext => eir::UnOp::Sext,
                sir::UnOp::Popcount => eir::UnOp::Popcount,
                sir::UnOp::Lzcount => eir::UnOp::Lzcount,
            },
            dst: op(dst),
            src: op(src),
        },
        sir::IR::Convert { op: c, dst, src } => eir::IR::Convert {
            op: match c {
                sir::ConvertOp::IntToFloat => eir::ConvertOp::IntToFloat,
                sir::ConvertOp::FloatToFloat => eir::ConvertOp::FloatToFloat,
                sir::ConvertOp::FloatToInt => eir::ConvertOp::FloatToInt,
            },
            dst: op(dst),
            src: op(src),
        },
        sir::IR::Subpiece { dst, src, lsb } => {
            eir::IR::Subpiece { dst: op(dst), src: op(src), lsb: *lsb }
        }
        sir::IR::Branch { target } => eir::IR::Branch { target: op(target) },
        sir::IR::IndirectJump { target } => eir::IR::IndirectJump { target: op(target) },
        sir::IR::CBranch { cond, target } => {
            eir::IR::CBranch { cond: op(cond), target: op(target) }
        }
        sir::IR::Call { target } => eir::IR::Call { target: op(target) },
        sir::IR::CallInd { target } => eir::IR::CallInd { target: op(target) },
        sir::IR::Ret { target } => eir::IR::Ret { target: target.as_ref().map(op) },
        sir::IR::Userop { id, args, dst } => eir::IR::Userop {
            id: *id,
            args: args.iter().map(op).collect(),
            dst: dst.as_ref().map(op),
        },
        sir::IR::Nop => eir::IR::Nop,
    }
}


pub fn collect_regs(ir: &eir::IR, out: &mut std::collections::BTreeSet<String>) {
    use eir::IR as I;
    fn v(o: &eir::Operand, out: &mut std::collections::BTreeSet<String>) {
        if let eir::Operand::Reg { name, .. } = o {
            out.insert(name.to_string());
        }
    }
    match ir {
        I::Copy { dst, src } => { v(dst, out); v(src, out); }
        I::Load { dst, addr, .. } => { v(dst, out); v(addr, out); }
        I::Store { addr, value, .. } => { v(addr, out); v(value, out); }
        I::Bin { dst, operands, .. } => { v(dst, out); v(&operands.0, out); v(&operands.1, out); }
        I::Float { dst, operands, .. } => { v(dst, out); v(&operands.0, out); v(&operands.1, out); }
        I::Un { dst, src, .. } | I::Subpiece { dst, src, .. } | I::Convert { dst, src, .. } => { v(dst, out); v(src, out); }
        I::CBranch { cond, target } => { v(cond, out); v(target, out); }
        I::Branch { target } | I::IndirectJump { target } | I::Call { target } | I::CallInd { target } => v(target, out),
        I::Ret { target } => { if let Some(t) = target { v(t, out); } }
        I::Userop { args, dst, .. } => { for a in args { v(a, out); } if let Some(d) = dst { v(d, out); } }
        I::Nop => {}
    }
}

/// Drop the `Branch`/`CBranch`/`Call`/`Ret` ops from a block, leaving the data
/// ops, so the block can be emitted as a straight-line sequence.
///
/// The terminator (not the ops) decides where the block goes next, so these ops
/// duplicate a fact the caller already has. Indirect transfers are dropped too:
/// the caller is the one that knows where they went (it resolved the target
/// itself), and a stripped `call_ind` leaves nothing that needs a table.
pub fn strip_control_flow(ops: Vec<eir::IR>) -> Vec<eir::IR> {
    ops.into_iter()
        .filter(|ir| {
            !matches!(
                ir,
                eir::IR::Branch { .. }
                    | eir::IR::CBranch { .. }
                    | eir::IR::Call { .. }
                    | eir::IR::Ret { .. }
                    | eir::IR::IndirectJump { .. }
                    | eir::IR::CallInd { .. }
            )
        })
        .collect()
}
