//! 64-bit integer temporaries.
//!
//! `MUL` in x86-32 produces EDX:EAX, so Sleigh lifts a dword x dword product
//! through an 8-byte temp and slices it with `Subpiece`. These are x86-32
//! constructs, not x64 -- see docs/roadmap.md for the 32-bit-only scope.

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
    for r in used {
        if !regs.contains(&r) {
            regs.push(r);
        }
    }
    regs
}

fn run(module: &[u8], regs: &[String], eax: u32, ebx: u32) -> String {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("wlam-wide-{}-{n}.wasm", std::process::id()));
    std::fs::write(&path, module).unwrap();
    let idx = |name: &str| regs.iter().position(|r| r == name).unwrap_or(0);
    let out = std::process::Command::new("timeout")
        .arg("10")
        .arg("node")
        .arg("-e")
        .arg(format!(
            "const fs=require('fs');const m=new WebAssembly.Module(fs.readFileSync({path:?}));\
             const i=new WebAssembly.Instance(m,{{}});\
             const d=new DataView(i.exports.memory.buffer);\
             d.setUint32({base}+{eax_i}*8,{eax},true);\
             d.setUint32({base}+{ebx_i}*8,{ebx},true);\
             d.setUint32({base}+{esp_i}*8,0x4000,true);\
             try{{i.exports.entry();\
               console.log('eax='+d.getUint32({base}+{eax_i}*8,true)+' edx='+d.getUint32({base}+{edx_i}*8,true));}}\
             catch(e){{console.log('TRAP '+e.message)}}",
            base = wlam_emit::REG_SNAPSHOT_BASE,
            eax_i = idx("EAX"),
            ebx_i = idx("EBX"),
            edx_i = idx("EDX"),
            esp_i = idx("ESP"),
        ))
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// `F7 E3` = `MUL EBX` -> EDX:EAX = EAX * EBX, then `C3` = `ret`.
fn mul_fixture() -> (Vec<u8>, u64) {
    (vec![0xF7, 0xE3, 0xC3], 0x1000)
}

#[test]
fn mul_small_result_has_zero_high_half() {
    let (code, base) = mul_fixture();
    let blocks = blocks_from(&code, base, &[base]);
    let regs = registers(&blocks);
    let prog = partition(&[base], &blocks, &regs);
    let module = build_module(&prog, &blocks).expect("64-bit mul must emit");
    let out = run(&module, &regs, 7, 9);
    assert_eq!(out, "eax=63 edx=0", "7*9 fits a dword: no high half");
}

#[test]
fn mul_large_result_carries_into_edx() {
    let (code, base) = mul_fixture();
    let blocks = blocks_from(&code, base, &[base]);
    let regs = registers(&blocks);
    let prog = partition(&[base], &blocks, &regs);
    let module = build_module(&prog, &blocks).expect("64-bit mul must emit");
    // 0x10000 * 0x10000 = 0x1_0000_0000 -> EAX = 0, EDX = 1
    let out = run(&module, &regs, 0x10000, 0x10000);
    assert_eq!(out, "eax=0 edx=1", "the high half must reach EDX");
}

#[test]
fn mul_max_dword_carries_correctly() {
    let (code, base) = mul_fixture();
    let blocks = blocks_from(&code, base, &[base]);
    let regs = registers(&blocks);
    let prog = partition(&[base], &blocks, &regs);
    let module = build_module(&prog, &blocks).expect("64-bit mul must emit");
    // 0xFFFFFFFF * 0xFFFFFFFF = 0xFFFFFFFE_00000001
    let out = run(&module, &regs, 0xFFFF_FFFF, 0xFFFF_FFFF);
    assert_eq!(
        out, "eax=1 edx=4294967294",
        "unsigned dword multiply must not be treated as signed"
    );
}
