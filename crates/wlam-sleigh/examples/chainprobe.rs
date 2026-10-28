//! Chain a real lifted loop and run it in V8.
use std::collections::BTreeMap;
use wlam_emit::chain::{chain, module_for_function};
use wlam_emit::ir as eir;
use wlam_sleigh::{lift_block, with_big_stack};

/// Adapt a lifted block to the emitter's block contract.
fn convert(b: &wlam_sleigh::Block) -> eir::Block {
    let ops = b.instructions.iter().flat_map(|i| i.ops.iter()).map(to_eir).collect();
    let term = match &b.terminator {
        wlam_sleigh::Terminator::FallsThrough => eir::Terminator::FallsThrough,
        wlam_sleigh::Terminator::Jump(t) => eir::Terminator::Jump(*t),
        wlam_sleigh::Terminator::Conditional { taken, not_taken } => {
            eir::Terminator::Conditional { taken: *taken, not_taken: *not_taken }
        }
        wlam_sleigh::Terminator::IndirectJump => eir::Terminator::IndirectJump,
        wlam_sleigh::Terminator::Return => eir::Terminator::Return,
        wlam_sleigh::Terminator::Trap(a) => eir::Terminator::Trap(*a),
        wlam_sleigh::Terminator::Undecodable(a) => eir::Terminator::Undecodable(*a),
    };
    eir::Block { start: b.start, end: b.end, ops, terminator: term }
}

fn to_eir(ir: &wlam_sleigh::ir::IR) -> eir::IR {
    use eir::Operand as EO;
    use wlam_sleigh::ir::{IR as SI, Operand as SO};
    fn op(o: &SO) -> EO {
        match o {
            SO::Const { value, size } => EO::Const { value: *value, size: *size },
            SO::Reg { name, size } => EO::Reg { name: name.clone(), size: *size },
            SO::Temp { offset, size } => EO::Temp { offset: *offset, size: *size },
            SO::Ram { offset, size } => EO::Ram { offset: *offset, size: *size },
        }
    }
    match ir {
        SI::Copy { dst, src } => eir::IR::Copy { dst: op(dst), src: op(src) },
        SI::Load { dst, addr, size } => eir::IR::Load { dst: op(dst), addr: op(addr), size: *size },
        SI::Store { addr, value, size } => eir::IR::Store { addr: op(addr), value: op(value), size: *size },
        SI::Bin { op: b, dst, operands } => eir::IR::Bin {
            op: match b {
                wlam_sleigh::ir::BinOp::Add => eir::BinOp::Add,
                wlam_sleigh::ir::BinOp::Sub => eir::BinOp::Sub,
                wlam_sleigh::ir::BinOp::Mul => eir::BinOp::Mul,
                wlam_sleigh::ir::BinOp::And => eir::BinOp::And,
                wlam_sleigh::ir::BinOp::Or => eir::BinOp::Or,
                wlam_sleigh::ir::BinOp::Xor => eir::BinOp::Xor,
                wlam_sleigh::ir::BinOp::Eq => eir::BinOp::Eq,
                wlam_sleigh::ir::BinOp::NotEq => eir::BinOp::NotEq,
                wlam_sleigh::ir::BinOp::LessU => eir::BinOp::LessU,
                wlam_sleigh::ir::BinOp::LessS => eir::BinOp::LessS,
                wlam_sleigh::ir::BinOp::LessEqU => eir::BinOp::LessEqU,
                wlam_sleigh::ir::BinOp::LessEqS => eir::BinOp::LessEqS,
                wlam_sleigh::ir::BinOp::Carry => eir::BinOp::Carry,
                wlam_sleigh::ir::BinOp::SCarry => eir::BinOp::SCarry,
                wlam_sleigh::ir::BinOp::SBorrow => eir::BinOp::SBorrow,
                wlam_sleigh::ir::BinOp::Lsh => eir::BinOp::Lsh,
                wlam_sleigh::ir::BinOp::Rsh => eir::BinOp::Rsh,
                wlam_sleigh::ir::BinOp::Asr => eir::BinOp::Asr,
                wlam_sleigh::ir::BinOp::DivS => eir::BinOp::DivS,
                wlam_sleigh::ir::BinOp::DivU => eir::BinOp::DivU,
                wlam_sleigh::ir::BinOp::RemS => eir::BinOp::RemS,
                wlam_sleigh::ir::BinOp::RemU => eir::BinOp::RemU,
                wlam_sleigh::ir::BinOp::MulFull => eir::BinOp::MulFull,
            },
            dst: op(dst),
            operands: Box::new((op(&operands.0), op(&operands.1))),
        },
        SI::Un { op: u, dst, src } => eir::IR::Un {
            op: match u {
                wlam_sleigh::ir::UnOp::Neg => eir::UnOp::Neg,
                wlam_sleigh::ir::UnOp::Not => eir::UnOp::Not,
                wlam_sleigh::ir::UnOp::Zext => eir::UnOp::Zext,
                wlam_sleigh::ir::UnOp::Sext => eir::UnOp::Sext,
                wlam_sleigh::ir::UnOp::Popcount => eir::UnOp::Popcount,
                wlam_sleigh::ir::UnOp::Lzcount => eir::UnOp::Lzcount,
            },
            dst: op(dst),
            src: op(src),
        },
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

fn collect(ir: &eir::IR, out: &mut std::collections::BTreeSet<String>) {
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
        I::Un { dst, src, .. } | I::Subpiece { dst, src, .. } => { v(dst, out); v(src, out); }
        I::CBranch { cond, target } => { v(cond, out); v(target, out); }
        I::Branch { target } | I::IndirectJump { target } | I::Call { target } | I::CallInd { target } => v(target, out),
        I::Ret { target } => { if let Some(t) = target { v(t, out); } }
        I::Userop { args, dst, .. } => { for a in args { v(a, out); } if let Some(d) = dst { v(d, out); } }
        I::Nop => {}
    }
}

fn main() {
    // A counting loop compiled by clang:  int f(int n){int s=0;for(int i=0;i<n;i++)s+=i;return s;}
    // Hand-assembled equivalent:
    //   33 C0      xor eax,eax      ; sum = 0
    //   31 C9      xor ecx,ecx      ; i = 0
    //  loop:
    //   3B 4C 24 04 cmp ecx,[esp+4]
    //   7D 05      jge done          ; if i >= n, done
    //   03 C1      add eax,ecx
    //   41         inc ecx
    //   EB F7      jmp loop
    //  done:
    //   C3         ret
    let code = [
        0x33, 0xC0, // xor eax,eax
        0x31, 0xC9, // xor ecx,ecx
        0x3B, 0x4C, 0x24, 0x04, // cmp ecx,[esp+4]
        0x7D, 0x05, // jge +5 -> 0x100f
        0x03, 0xC1, // add eax,ecx
        0x41, // inc ecx
        0xEB, 0xF5, // jmp -11 -> 0x1004
        0xC3, // ret
    ];
    let base = 0x1000u64;
    let mut blocks = BTreeMap::new();
    // Lift from each jump target onward; discovery normally provides this set.
    for start in [0x1000u64, 0x1004, 0x100a, 0x100f] {
        let off = (start - base) as usize;
        let b = with_big_stack({
            let code = code.to_vec();
            move || lift_block(&code[off..], start)
        });
        blocks.insert(start, convert(&b));
    }
    for (a, b) in &blocks {
        println!("block {a:#x}: {:?} nops={}", b.terminator, b.ops.len());
    }
    // Collect every register the blocks mention, exactly as a whole-program
    // build would.
    let mut regset = std::collections::BTreeSet::new();
    for b in blocks.values() {
        for op in &b.ops {
            collect(op, &mut regset);
        }
    }
    println!("registers used: {:?}", regset);
    let regs: Vec<String> = regset.into_iter().collect();
    let f = chain(0x1000, &blocks, &regs).expect("chain");
    println!("order: {:X?} problems={:?}", f.order, f.problems);
    let module = module_for_function(&f, &blocks, 0x1000).expect("module");
    std::fs::write("chain.wasm", &module).unwrap();
    println!("wrote {} bytes", module.len());

    // Execute: f(n) sums 0..n-1 into EAX. Seed the guest stack argument and
    // read EAX from the snapshot the module writes at REG_SNAPSHOT_BASE.
    // The block functions do not snapshot here, so instead run with a small
    // wrapper: place n at [ESP+4] and read memory.
    let script = format!(
        "const fs=require('fs');\
         const m=new WebAssembly.Module(fs.readFileSync('chain.wasm'));\
         const i=new WebAssembly.Instance(m,{{}});\
         const mem=new DataView(i.exports.memory.buffer);\
         mem.setUint32(0x8000+4, 10, true);   // [ESP+4] = 10 \
         mem.setUint32(0x8000, 0x9000, true); // ESP (unused by this snippet) \
         i.exports.entry();\
         console.log('ran');",
    );
    let _ = script;
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg("const fs=require('fs');const m=new WebAssembly.Module(fs.readFileSync('chain.wasm'));const i=new WebAssembly.Instance(m,{});const d=new DataView(i.exports.memory.buffer);d.setUint32(4,10,true);try{i.exports.entry();console.log('RAN');}catch(e){console.log('TRAP: '+e.message)}")
        .output()
        .unwrap();
    println!("{}", String::from_utf8_lossy(&out.stdout));
    println!("{}", String::from_utf8_lossy(&out.stderr));
}
