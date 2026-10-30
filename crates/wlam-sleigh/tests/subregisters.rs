//! Sub-register writes: `mov dl, ah` and friends.
//!
//! Sleigh models x86 sub-registers as independent varnodes, so `mov dl, ah`
//! lifts to `Copy { dst: Reg("DL",1), src: Reg("AH",1) }` and `mov [slot], edx`
//! reads `Reg("EDX",4)` -- three different names for two bytes of one register.
//!
//! Wiring each name to its own wasm local makes the copy write a local nothing
//! reads, so the store sees a stale EDX. This is the exact sequence Uxn32 runs
//! after `GetVersion` (0x40cce0 `8A D4`), which is why its recorded major
//! version was 0 even though the host returned 5.
//!
//! These tests pin the aliasing down at the instruction level, where the bytes
//! are small enough to reason about without a whole image.

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

/// Seed EAX/EDX/EBX, run the module, and report the three registers back
/// (plus the word stored at `0x10004` if the fixture writes one).
fn run(module: &[u8], regs: &[String], eax: u32, edx: u32) -> String {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("wlam-subreg-{}-{n}.wasm", std::process::id()));
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
             d.setUint32({base}+{eax_i}*4,{eax},true);\
             d.setUint32({base}+{edx_i}*4,{edx},true);\
             d.setUint32({base}+{esp_i}*4,0x4000,true);\
             try{{i.exports.entry();\
               console.log('eax='+d.getUint32({base}+{eax_i}*4,true)+\
                 ' edx='+d.getUint32({base}+{edx_i}*4,true)+\
                 ' mem='+d.getUint32(0x10004,true));}}\
             catch(e){{console.log('TRAP '+e.message)}}",
            base = wlam_emit::REG_SNAPSHOT_BASE,
            eax_i = idx("EAX"),
            edx_i = idx("EDX"),
            esp_i = idx("ESP"),
        ))
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    // A node failure (bad module, missing file) otherwise shows up as an empty
    // string that gets compared against an expected value, which hides the real
    // cause. Surface stderr instead.
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !err.is_empty() {
        panic!("node failed: {err}");
    }
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn build(code: &[u8]) -> Option<(Vec<u8>, Vec<String>)> {
    let base = 0x1000u64;
    let blocks = blocks_from(code, base, &[base]);
    let regs = registers(&blocks);
    let prog = partition(&[base], &blocks, &regs);
    let module = build_module(&prog, &blocks).ok()?;
    Some((module, regs))
}

/// The literal sequence from Uxn32 at 0x40cce0:
///
/// ```text
/// 8A D4                  mov dl, ah            ; DL = bits 8..16 of EAX
/// 89 15 04 00 01 00      mov [0x10004], edx    ; store the dword
/// C3                     ret
/// ```
///
/// With EAX = 0x0000050A, AH is 0x05, so DL becomes 0x05 and the stored dword
/// must be 0x00000005 -- assuming EDX started at 0.
#[test]
fn mov_dl_ah_then_store_edx_keeps_the_high_byte() {
    let code = vec![
        0x8A, 0xD4, // mov dl, ah
        0x89, 0x15, 0x04, 0x00, 0x01, 0x00, // mov [0x10004], edx
        0xC3, // ret
    ];
    let Some((module, regs)) = build(&code) else {
        panic!("mov dl, ah must emit");
    };
    // EDX starts at 0, so the only value the store can see is what DL wrote.
    let out = run(&module, &regs, 0x0000_050A, 0);
    assert_eq!(
        out, "eax=1290 edx=5 mem=5",
        "DL must hold bits 8..16 of EAX (0x05) and reach memory through EDX"
    );
}

/// The write must not disturb the rest of EDX: `mov dl, ah` with EDX already
/// holding high bytes leaves them alone.
#[test]
fn writing_dl_preserves_the_upper_bytes_of_edx() {
    let code = vec![
        0x8A, 0xD4, // mov dl, ah
        0x89, 0x15, 0x04, 0x00, 0x01, 0x00, // mov [0x10004], edx
        0xC3, // ret
    ];
    let Some((module, regs)) = build(&code) else {
        panic!("mov dl, ah must emit");
    };
    // EDX = 0xAABBCCDD; only the low byte may change, from DD to 05.
    let out = run(&module, &regs, 0x0000_050A, 0xAABB_CCDD);
    assert_eq!(
        out, "eax=1290 edx=2864434181 mem=2864434181",
        "DL is byte 0 of EDX (2864434181 == 0xAABBCC05)"
    );
}

/// `mov dh, al` writes byte 1 of EDX, so the value must land shifted by 8.
#[test]
fn writing_dh_targets_the_second_byte_of_edx() {
    // 8A F0 = mov dh, al ; 89 15 04 00 01 00 = mov [0x10004], edx ; C3
    let code = vec![
        0x8A, 0xF0, // mov dh, al
        0x89, 0x15, 0x04, 0x00, 0x01, 0x00, // mov [0x10004], edx
        0xC3,
    ];
    let Some((module, regs)) = build(&code) else {
        panic!("mov dh, al must emit");
    };
    // EAX = 0x...FF -> AL = 0xFF -> DH = 0xFF -> EDX = 0x0000FF00
    let out = run(&module, &regs, 0x0000_00FF, 0);
    assert_eq!(out, "eax=255 edx=65280 mem=65280", "0x0000FF00 == 65280");
}

/// Reading AX must be bits 0..16 of EAX, and writing AL must only touch byte 0.
#[test]
fn ax_reads_the_low_word_and_al_writes_only_a_byte() {
    // 66 89 C2      mov dx, ax      -> DX = low word of EAX
    // 89 15 04 00 01 00   mov [0x10004], edx ; C3
    let code = vec![
        0x66, 0x89, 0xC2, // mov dx, ax
        0x89, 0x15, 0x04, 0x00, 0x01, 0x00, // mov [0x10004], edx
        0xC3,
    ];
    let Some((module, regs)) = build(&code) else {
        panic!("mov dx, ax must emit");
    };
    // EAX low word is 0xBEEF, so DX must be 0xBEEF and EDX = 0x0000BEEF.
    let out = run(&module, &regs, 0x1234_BEEF, 0);
    // EAX is unchanged (0x1234BEEF == 305446639); DX took its low word, so
    // EDX = 0x0000BEEF == 48879.
    assert_eq!(out, "eax=305446639 edx=48879 mem=48879", "0x0000BEEF == 48879");
}

/// A sub-register that is *written but never aliased back* would leave the
/// parent untouched. Writing AL then reading EAX proves the parent updated.
#[test]
fn writing_al_is_visible_through_eax() {
    // B0 7F   mov al, 0x7f
    // C3      ret
    let code = vec![0xB0, 0x7F, 0xC3];
    let Some((module, regs)) = build(&code) else {
        panic!("mov al, imm must emit");
    };
    let out = run(&module, &regs, 0x1234_5600, 0);
    // Only the low byte changes: 0x12345600 -> 0x1234567F.
    assert_eq!(
        out, "eax=305419903 edx=0 mem=0",
        "0x1234567F == 305419903: AL updated EAX's low byte and nothing else"
    );
}
