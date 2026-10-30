//! Guest `call`/`ret` across functions, verified by executing the module.
//!
//! The lifted `call` pushes the return address on the guest's own stack and the
//! callee's `ret` pops it into `EIP`. These tests pin that the emitted module
//! reproduces that: the callee runs, control comes back, and execution
//! continues at the instruction after the call -- not at the top, and not
//! looping.

mod shared;

use std::collections::BTreeMap;

use wlam_emit::ir as eir;
use wlam_emit::chain::{build_module, partition};
use wlam_sleigh::{lift_block, with_big_stack};

fn blocks_from(code: &[u8], base: u64, starts: &[u64]) -> BTreeMap<u64, eir::Block> {
    let mut out = BTreeMap::new();
    for &start in starts {
        let off = (start - base) as usize;
        let slice = code[off..].to_vec();
        let lifted = with_big_stack(move || lift_block(&slice, start));
        out.insert(start, shared::convert(&lifted));
    }
    out
}

fn registers(blocks: &BTreeMap<u64, eir::Block>) -> Vec<String> {
    let mut set = std::collections::BTreeSet::new();
    for b in blocks.values() {
        for op in &b.ops {
            shared::collect_regs(op, &mut set);
        }
    }
    set.into_iter().collect()
}

fn run(module: &[u8], regs: &[String], esp: u32) -> (u32, u32, String) {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("wlam-call-{}-{n}.wasm", std::process::id()));
    std::fs::write(&path, module).unwrap();
    let out = std::process::Command::new("timeout")
        .arg("10")
        .arg("node")
        .arg("-e")
        .arg(format!(
            "const fs=require('fs');const m=new WebAssembly.Module(fs.readFileSync({path:?}));\
             const i=new WebAssembly.Instance(m,{{}});\
             const d=new DataView(i.exports.memory.buffer);\
             d.setUint32({base}+{esp_idx}*4,{esp},true);\
             try{{i.exports.entry();}}catch(e){{console.log('TRAP '+e.message);process.exit(0)}}\
             console.log(d.getUint32(0x10000,true)+' '+d.getUint32(0x10004,true));",
            base = wlam_emit::REG_SNAPSHOT_BASE,
            esp_idx = regs.iter().position(|r| r == "ESP").expect("ESP is a guest local"),
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

/// ```text
/// 0x1000: E8 0B 00 00 00   call 0x1010        (return address 0x1005)
/// 0x1005: C7 05 00 00 01 00 02 00 00 00   mov dword [0x10004], 2
/// 0x100f: C3               ret
/// 0x1010: C7 05 00 00 01 00 01 00 00 00   mov dword [0x10000], 1
/// 0x101a: C3               ret
/// ```
///
/// The callee writes 1 and the caller writes 2 after the call, so `1 2` proves
/// the callee ran *and* control returned to the instruction after the call.
#[test]
fn call_returns_to_the_instruction_after_it() {
    let base = 0x1000u64;
    let callee = 0x1010u64;
    let rel = (callee as i64 - (base + 5) as i64) as i32;
    let mut code: Vec<u8> = Vec::new();
    code.push(0xE8);
    code.extend_from_slice(&rel.to_le_bytes());
    // 0x1005: caller's post-call store
    code.extend_from_slice(&[0xC7, 0x05, 0x04, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00]);
    // 0x100f: caller ret
    code.push(0xC3);
    assert_eq!(base + code.len() as u64, callee, "callee must land at 0x1010");
    // 0x1010: callee store
    code.extend_from_slice(&[0xC7, 0x05, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00]);
    // 0x101a: callee ret
    code.push(0xC3);

    let blocks = blocks_from(&code, base, &[base, base + 5, base + 0xf, callee, callee + 0xa]);
    let regs = registers(&blocks);
    let prog = partition(&[base], &blocks, &regs);
    assert_eq!(prog.functions.len(), 2, "caller and callee are separate functions");
    assert_eq!(prog.functions[0].entry, base);
    assert_eq!(prog.functions[1].entry, callee);
    // The caller must own its post-call block; the callee must not.
    assert!(prog.functions[0].ids.contains_key(&(base + 5)), "caller owns 0x1005");
    assert!(!prog.functions[1].ids.contains_key(&(base + 5)), "callee must not own 0x1005");

    let module = build_module(&prog, &blocks).expect("module");
    // A stack above the scratch area, so pushes have somewhere to land.
    let (callee_wrote, caller_wrote, log) = run(&module, &regs, 0x4000);
    assert_eq!(callee_wrote, 1, "callee must have run: {log}");
    assert_eq!(caller_wrote, 2, "caller must resume after the call: {log}");
}

#[test]
fn callee_returns_without_looping() {
    // A `call` must terminate when the callee returns rather than re-entering
    // the caller's dispatch loop at id 0.
    let base = 0x1000u64;
    let callee = 0x1010u64;
    let rel = (callee as i64 - (base + 5) as i64) as i32;
    let mut code: Vec<u8> = Vec::new();
    code.push(0xE8);
    code.extend_from_slice(&rel.to_le_bytes());
    code.extend_from_slice(&[0xC7, 0x05, 0x04, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00]);
    code.push(0xC3);
    code.extend_from_slice(&[0xC7, 0x05, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00]);
    code.push(0xC3);

    let blocks = blocks_from(&code, base, &[base, base + 5, base + 0xf, callee, callee + 0xa]);
    let regs = registers(&blocks);
    let prog = partition(&[base], &blocks, &regs);
    let module = build_module(&prog, &blocks).expect("module");
    // The `timeout 10` in `run` turns an infinite loop into a parse failure.
    let (callee_wrote, caller_wrote, log) = run(&module, &regs, 0x4000);
    assert_eq!((callee_wrote, caller_wrote), (1, 2), "must terminate: {log}");
}
