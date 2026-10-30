//! Calls through a computed target go through a guarded wasm table.
//!
//! The guest hands the emitter a *runtime* value (a register that was set in an
//! earlier block, or by the host via `GetProcAddress`). It must become a
//! `call_indirect` into a table of guest functions, with a bounds check: a bad
//! target has to trap, never silently call function 0. A silent mis-dispatch is
//! a wrong-answer bug, which is strictly worse than a crash.

mod shared;

use std::collections::BTreeMap;

use wlam_emit::chain::{build_module, partition};
use wlam_emit::ir as eir;
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
    let mut regs: Vec<String> = wlam_emit::seeded_registers();
    let mut used = std::collections::BTreeSet::new();
    for b in blocks.values() {
        for op in &b.ops {
            shared::collect_regs(op, &mut used);
        }
    }
    let mut extra: Vec<String> = used.into_iter().filter(|r| !regs.contains(r)).collect();
    extra.sort();
    regs.extend(extra);
    regs
}

fn run(module: &[u8], regs: &[String], esp: u32, eax: u32, edi: u32) -> String {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("wlam-ind-{}-{n}.wasm", std::process::id()));
    std::fs::write(&path, module).unwrap();
    let idx = |name: &str| regs.iter().position(|r| r == name).expect(name);
    let out = std::process::Command::new("timeout")
        .arg("10")
        .arg("node")
        .arg("-e")
        .arg(format!(
            "const fs=require('fs');const m=new WebAssembly.Module(fs.readFileSync({path:?}));\
             const i=new WebAssembly.Instance(m,{{}});\
             const d=new DataView(i.exports.memory.buffer);\
             d.setUint32({base}+{esp_idx}*4,{esp},true);\
             d.setUint32({base}+{eax_idx}*4,{eax},true);\
             d.setUint32({base}+{edi_idx}*4,{edi},true);\
             try{{i.exports.entry();console.log('OK '+d.getUint32(0x10004,true));}}\
             catch(e){{console.log('TRAP '+e.message)}}",
            base = wlam_emit::REG_SNAPSHOT_BASE,
            esp_idx = idx("ESP"),
            eax_idx = idx("EAX"),
            edi_idx = idx("EDI"),
        ))
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    let so = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let se = String::from_utf8_lossy(&out.stderr);
    if so.is_empty() { format!("NO_OUTPUT stderr={}", se.trim()) } else { so }
}

/// ```text
/// 0x1000: FF D7               call edi          (target supplied at run time)
/// 0x1002: 89 05 04 00 01 00   mov [0x10004], EAX
/// 0x1008: C3                  ret
/// ```
fn call_edi_fixture() -> (Vec<u8>, u64) {
    let code = vec![0xFF, 0xD7, 0x89, 0x05, 0x04, 0x00, 0x01, 0x00, 0xC3];
    (code, 0x1000)
}

#[test]
fn indirect_call_target_out_of_range_traps() {
    let (code, base) = call_edi_fixture();
    let blocks = blocks_from(&code, base, &[base, base + 2, base + 8]);
    let regs = registers(&blocks);
    let prog = partition(&[base], &blocks, &regs);
    let module = build_module(&prog, &blocks).expect("indirect call must emit now");

    // A wild target must trap rather than fall through to function 0.
    let out = run(&module, &regs, 0x4000, 0, 0x7fff_ffff);
    assert!(
        out.starts_with("TRAP"),
        "out-of-range indirect target must trap, got: {out}"
    );
}

#[test]
fn indirect_call_to_zero_is_not_function_zero() {
    // Function 0 is a real guest function. A null target must NOT reach it.
    let (code, base) = call_edi_fixture();
    let blocks = blocks_from(&code, base, &[base, base + 2, base + 8]);
    let regs = registers(&blocks);
    let prog = partition(&[base], &blocks, &regs);
    let module = build_module(&prog, &blocks).expect("indirect call must emit now");
    let out = run(&module, &regs, 0x4000, 0, 0);
    assert!(
        out.starts_with("TRAP"),
        "null indirect target must trap, not call function 0: {out}"
    );
}

/// A valid target must reach the intended guest function, not merely "some"
/// function: the callee writes 0xAA where a wrong callee would write something
/// else.
///
/// ```text
/// 0x1000: FF D7               call edi          (EDI = table id of 0x1010)
/// 0x1002: 89 05 04 00 01 00   mov [0x10004], EAX
/// 0x1008: C3                  ret
/// 0x1010: B8 AA 00 00 00      mov eax, 0xAA
/// 0x1015: C3                  ret
/// ```
#[test]
fn indirect_call_reaches_the_intended_function() {
    let base = 0x1000u64;
    let callee = 0x1010u64;
    let code = vec![
        0xFF, 0xD7,
        0x89, 0x05, 0x04, 0x00, 0x01, 0x00,
        0xC3,
        0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90,
        0xB8, 0xAA, 0x00, 0x00, 0x00,
        0xC3,
    ];
    let blocks = blocks_from(&code, base, &[base, base + 2, base + 8, callee, callee + 5]);
    let regs = registers(&blocks);
    // The callee is reachable only through the table, so it must be declared an
    // entry: a function nothing calls directly would otherwise never be
    // discovered, and the table would have no slot to point at.
    let prog = partition(&[base, callee], &blocks, &regs);
    // `partition` pops entries from a stack, so the LAST entry is numbered
    // first. Read the ids rather than assuming them.
    let idx = prog.function_index[&callee];
    let entry_idx = prog.function_index[&base];

    eprintln!("callee idx={idx} entry idx={entry_idx}");
    eprintln!("entry blocks={:?}", prog.functions[entry_idx as usize].blocks.iter().map(|a| format!("{a:#x}")).collect::<Vec<_>>());
    eprintln!("callee blocks={:?}", prog.functions[idx as usize].blocks.iter().map(|a| format!("{a:#x}")).collect::<Vec<_>>());
    let module = build_module(&prog, &blocks).expect("module");
    std::fs::write("/var/home/longjoel/Projects/WasmLeanAndMean/.scratch/ind.wasm", &module).unwrap();
    let out = run(&module, &regs, 0x4000, 0, idx);
    assert_eq!(out, "OK 170", "call edi must reach the callee and return to the caller");
}
