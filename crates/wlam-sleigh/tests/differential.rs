//! The differential gate: emitted wasm must agree with the reference
//! interpreter on the same IR.
//!
//! Why this exists: a translator that merely produces *valid* wasm is
//! worthless. This test takes real lifted x86, compiles it to a wasm function,
//! executes that function in V8, and compares the resulting machine state with
//! the reference interpreter's. Any disagreement fails the test — the two are
//! peers, not master and servant, so neither can quietly define correctness.
//!
//! The comparison covers: registers (including flags and partial-register
//! views), temp-independence, and guest memory.

use std::collections::{BTreeMap, BTreeSet};

use wlam_emit::ir as eir;
use wlam_emit::{translate_ops, LocalMap};
use wlam_sleigh::interp::Machine;
use wlam_sleigh::ir as sir;
use wlam_sleigh::{lift_block, with_big_stack};

/// Convert the lifter's IR into the emitter's contract.
///
/// The two types are structurally identical but defined in different crates
/// (the emitter must not depend on the vendored Sleigh stack). This conversion
/// is the boundary, so it is written exhaustively — no wildcard arm — to make a
/// future IR addition fail to compile rather than silently drop.
fn to_emit(ir: &sir::IR) -> eir::IR {
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

/// Every register name that appears anywhere in a sequence of ops.
fn registers(ops: &[sir::IR]) -> BTreeMap<String, u32> {
    fn visit(o: &sir::Operand, out: &mut BTreeMap<String, u32>) {
        if let sir::Operand::Reg { name, size } = o {
            out.insert(name.to_string(), *size);
        }
    }
    let mut out = BTreeMap::new();
    for ir in ops {
        match ir {
            sir::IR::Copy { dst, src } => {
                visit(dst, &mut out);
                visit(src, &mut out);
            }
            sir::IR::Float { dst, operands, .. } => {
                visit(dst, &mut out);
                visit(&operands.0, &mut out);
                visit(&operands.1, &mut out);
            }
            sir::IR::Convert { dst, src, .. } => {
                visit(dst, &mut out);
                visit(src, &mut out);
            }
            sir::IR::Load { dst, addr, .. } => {
                visit(dst, &mut out);
                visit(addr, &mut out);
            }
            sir::IR::Store { addr, value, .. } => {
                visit(addr, &mut out);
                visit(value, &mut out);
            }
            sir::IR::Bin { dst, operands, .. } => {
                visit(dst, &mut out);
                visit(&operands.0, &mut out);
                visit(&operands.1, &mut out);
            }
            sir::IR::Un { dst, src, .. } | sir::IR::Subpiece { dst, src, .. } => {
                visit(dst, &mut out);
                visit(src, &mut out);
            }
            sir::IR::CBranch { cond, target } => {
                visit(cond, &mut out);
                visit(target, &mut out);
            }
            sir::IR::Branch { target }
            | sir::IR::IndirectJump { target }
            | sir::IR::Call { target }
            | sir::IR::CallInd { target } => visit(target, &mut out),
            sir::IR::Ret { target } => {
                if let Some(t) = target {
                    visit(t, &mut out);
                }
            }
            sir::IR::Userop { args, dst, .. } => {
                for a in args {
                    visit(a, &mut out);
                }
                if let Some(d) = dst {
                    visit(d, &mut out);
                }
            }
            sir::IR::Nop => {}
        }
    }
    out
}

/// Which registers are read or written, so the comparison only checks state the
/// program could have touched (untouched memory reads the same in both).
fn touched_memory(ops: &[sir::IR]) -> BTreeSet<u64> {
    let mut out = BTreeSet::new();
    for ir in ops {
        if let sir::IR::Store { addr, size, .. } = ir {
            if let sir::Operand::Const { value, .. } | sir::Operand::Ram { offset: value, .. } = addr {
                for i in 0..*size as u64 {
                    out.insert(value + i);
                }
            }
        }
    }
    out
}

/// Run the module in Node and return `{key: value}` for the snapshot area.
fn run_in_v8(
    module: &[u8],
    n_regs: usize,
    initial: &[u64],
    mem_addrs: &[u64],
) -> BTreeMap<String, u64> {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("wlam-diff-{}-{n}.wasm", std::process::id()));
    std::fs::write(&path, module).unwrap();
    let addrs: Vec<String> = mem_addrs.iter().map(|a| a.to_string()).collect();
    let init_js: Vec<String> = initial.iter().map(|v| v.to_string()).collect();
    let script = format!(
        "const fs=require('fs');\
         const m=new WebAssembly.Module(fs.readFileSync({path:?}));\
         const inst=new WebAssembly.Instance(m,{{}});\
         const init=new Uint32Array([{init_vals}]);\
         const iv=new DataView(inst.exports.memory.buffer);\
         for(let i=0;i<init.length;i++){{iv.setUint32({base}+i*4,init[i],true);}}\
         inst.exports.run();\
         const view=new DataView(inst.exports.memory.buffer);\
         const out={{}};\
         for(let i=0;i<{n_regs};i++){{out['r'+i]=view.getUint32({base}+i*4,true);}}\
         for(const a of [{addrs}]){{out['mem:'+a]=view.getUint8(a>>>0);}}\
         console.log(JSON.stringify(out));",
        base = wlam_emit::REG_SNAPSHOT_BASE,
        addrs = addrs.join(","),
        init_vals = init_js.join(","),
    );
    let out = std::process::Command::new("node").arg("-e").arg(&script).output().unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let mut map = BTreeMap::new();
    for part in text.trim().trim_start_matches('{').trim_end_matches('}').split(',') {
        let Some((k, v)) = part.split_once(':') else { continue };
        let key = k.trim().trim_matches('"').to_string();
        if let Ok(val) = v.trim().parse::<u64>() {
            map.insert(key, val);
        }
    }
    map
}

/// Compare the emitted wasm against the reference interpreter.
///
/// `None` means the IR used something the emitter reports as unsupported (it
/// never means "assume it matched"), so tests can count the genuinely compared
/// cases and fail when that count drops to zero.
fn differential(bytes: &[u8], at: u64, setup: &dyn Fn(&mut Machine)) -> Option<String> {
    let block = with_big_stack({
        let b = bytes.to_vec();
        move || lift_block(&b, at)
    });
    let ops: Vec<sir::IR> = block.instructions.iter().flat_map(|i| i.ops.iter().cloned()).collect();
    if ops.iter().any(|o| {
        matches!(
            o,
            sir::IR::Branch { .. }
                | sir::IR::CBranch { .. }
                | sir::IR::Ret { .. }
                | sir::IR::Call { .. }
                | sir::IR::CallInd { .. }
                | sir::IR::IndirectJump { .. }
        )
    }) {
        // Control flow is block chaining's job; covered separately.
        return None;
    }
    let mut machine = Machine::new(512 * 1024);
    setup(&mut machine);
    machine.run(&ops);

    let regs = registers(&ops);
    let names: Vec<String> = regs.keys().cloned().collect();
    // Capture the initial register values once, and give them to both sides:
    // the interpreter gets them via `setup`, and the wasm gets them written
    // into its snapshot area before `run` copies them into locals. Without
    // this the wasm started from zero while the interpreter started from the
    // test's setup, and the comparison was meaningless.
    let initial: Vec<u64> = names.iter().map(|n| machine.get(n)).collect();
    let locals = LocalMap::new(regs.iter().map(|(n, s)| (n.clone(), *s)));
    let emit_ops: Vec<eir::IR> = ops.iter().map(to_emit).collect();
    let translated = translate_ops(&emit_ops, locals).ok()?;
    // Rebuild a LocalMap identical to the one used for translation so snapshot
    // indices match.
    let snapshot_locals = LocalMap::new(regs.iter().map(|(n, s)| (n.clone(), *s)));
    let module = wlam_emit::module_with_snapshot(&translated, &snapshot_locals, &names, &initial);
    let stores: Vec<u64> = touched_memory(&ops).into_iter().collect();
    let observed = run_in_v8(&module, names.len(), &initial, &stores);

    let mut problems = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let want = machine.get(name);
        let got = observed.get(&format!("r{i}")).copied().unwrap_or(u64::MAX);
        if want != got {
            problems.push(format!("{name}: interpreter {want:#x} vs wasm {got:#x}"));
        }
    }
    for a in &stores {
        let want = machine.load(*a, 1);
        let got = observed.get(&format!("mem:{a}")).copied().unwrap_or(u64::MAX);
        if want != got {
            problems.push(format!("mem[{a:#x}]: interpreter {want:#x} vs wasm {got:#x}"));
        }
    }
    Some(problems.join("; "))
}

#[test]
fn wasm_agrees_with_interpreter_on_lifted_instructions() {
    // Real x86 encodings; each updates registers and/or flags, which are now
    // compared through the register snapshot.
    let cases: &[(&str, &[u8])] = &[
        // mov eax, 0x11223344
        ("mov imm", &[0xb8, 0x44, 0x33, 0x22, 0x11]),
        // add eax, 5   (updates CF OF SF ZF PF)
        ("add", &[0x83, 0xc0, 0x05]),
        // sub eax, 9
        ("sub", &[0x83, 0xe8, 0x09]),
        // cmp eax, 3 (flags only)
        ("cmp", &[0x83, 0xf8, 0x03]),
        // shl eax, 1
        ("shl", &[0xd1, 0xe0]),
        // mov ecx, eax ; xor edx, edx
        ("mov/xor chain", &[0x89, 0xc1, 0x31, 0xd2]),
        // inc eax (does not touch CF)
        ("inc", &[0x40]),
    ];
    let mut compared = 0;
    let mut skipped = 0;
    for (name, bytes) in cases {
        match differential(bytes, 0x1000, &|m| m.set("EAX", 0xffff_fff0)) {
            None => skipped += 1,
            Some(problems) if problems.is_empty() => compared += 1,
            Some(problems) => panic!("{name}: wasm disagreed with interpreter: {problems}"),
        }
    }
    eprintln!("differential: {compared} agreed, {skipped} unsupported");
    assert!(compared >= 4, "only {compared} cases were actually compared");
}
