//! Dispatch ids must survive encoding.
//!
//! ## The bug this pins
//!
//! `i32.const` takes a **signed** LEB128 immediate. The dispatcher sets the id
//! local with `i32.const <block id>`, and every id used to be written with
//! *unsigned* LEB128. For ids below 64 the two encodings coincide, so a
//! 21-block fixture or a 63-block fixture passes either way. From id 64 the
//! unsigned byte has bit 6 set, which the decoder reads as a sign bit:
//!
//! ```text
//!   id 64  -> unsigned bytes [0x40] -> decoded as -64
//!   id 65  -> unsigned bytes [0x41] -> decoded as -63
//!   ...
//!   id 70  -> unsigned bytes [0x46] -> decoded as -58
//! ```
//!
//! The dispatcher then sees an id it never assigned, `br_table` takes its
//! default arm, and the function branches straight back to the dispatcher with
//! the same out-of-range id: an infinite loop that leaves every guest register
//! untouched. Uxn32 spun in `0x40d221` for exactly this reason, and it was
//! invisible to `wasm-validate` because the module is perfectly well-formed --
//! just semantically wrong.
//!
//! ## Why the fixtures here are large
//!
//! The smallest id that can be mis-encoded is 64, so a meaningful fixture needs
//! more than 64 blocks in one function. The test builds one by hand and walks
//! a chain of gotos whose ids cross that boundary.

mod shared;

use std::collections::BTreeMap;

use wlam_emit::chain::{build_module, partition};
use wlam_emit::ir as eir;
use wlam_sleigh::{lift_block, with_big_stack};

/// Build a block that jumps unconditionally to `target` by encoding
/// `jmp rel32` at `addr`.
fn jmp_to(addr: u64, target: u64) -> (u64, Vec<u8>, eir::Block) {
    let rel = (target as i64 - (addr as i64 + 5)) as i32;
    let mut code = vec![0xE9u8];
    code.extend_from_slice(&rel.to_le_bytes());
    let slice = code.clone();
    let lifted = with_big_stack(move || lift_block(&slice, addr));
    (addr, code, shared::convert(&lifted))
}

/// A function of `n` blocks where block `i` jumps to block `i + 1` and the last
/// returns. The ids are assigned in discovery order, so a chain of jumps gives
/// predictable ids well past 64.
fn chain(n: usize) -> (BTreeMap<u64, eir::Block>, Vec<String>) {
    let base = 0x1000u64;
    let stride = 5u64; // each jmp rel32 is 5 bytes
    let mut blocks = BTreeMap::new();
    // Emit into one contiguous buffer so addresses are real.
    let mut code = Vec::new();
    for i in 0..n {
        let addr = base + i as u64 * stride;
        let target = if i + 1 < n {
            base + (i as u64 + 1) * stride
        } else {
            addr // last block falls through to a ret placed after the buffer
        };
        let (a, bytes, blk) = jmp_to(addr, target);
        let _ = a;
        code.extend_from_slice(&bytes);
        blocks.insert(addr, blk);
    }
    // A `ret` for the final block.
    let ret_addr = base + n as u64 * stride;
    let ret_block = {
        let slice = vec![0xC3u8];
        let lifted = with_big_stack(move || lift_block(&slice, ret_addr));
        shared::convert(&lifted)
    };
    blocks.insert(ret_addr, ret_block);
    // The last jmp must actually target the ret, not itself.
    let last = base + (n as u64 - 1) * stride;
    let (_, _, fixed) = jmp_to(last, ret_addr);
    blocks.insert(last, fixed);

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
    (blocks, regs)
}

/// Run the module and report EAX, which the final block sets to a sentinel by
/// writing to memory. A spin never gets there, so the runner has a timeout.
fn runs_to_completion(module: &[u8], regs: &[String]) -> bool {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("wlam-leb-{}-{n}.wasm", std::process::id()));
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
             d.setUint32({base}+{esp}*8,0x4000,true);\
             try{{i.exports.entry();console.log('done');}}catch(e){{console.log('TRAP');}}",
            base = wlam_emit::REG_SNAPSHOT_BASE,
            esp = idx("ESP"),
        ))
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    let text = String::from_utf8_lossy(&out.stdout);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.trim().is_empty(), "node failed: {err}");
    text.trim() == "done"
}

/// Ids below 64 encode identically either way -- the case that hid the bug.
#[test]
fn a_short_chain_dispatches() {
    let (blocks, regs) = chain(8);
    let prog = partition(&[0x1000], &blocks, &regs);
    let module = build_module(&prog, &blocks).expect("module");
    assert!(runs_to_completion(&module, &regs), "an 8-block chain must run");
}

/// 71 blocks puts the ids well past 64, where unsigned LEB128 is wrong.
#[test]
fn a_chain_whose_ids_exceed_64_dispatches() {
    let (blocks, regs) = chain(71);
    assert!(blocks.len() > 64, "the fixture must cross the 64 boundary");
    let prog = partition(&[0x1000], &blocks, &regs);
    let module = build_module(&prog, &blocks).expect("module");
    assert!(
        runs_to_completion(&module, &regs),
        "a 71-block chain must run: ids >= 64 were being sign-extended by \
         unsigned LEB128, sending the dispatcher to its default arm forever"
    );
}

/// The encoded bytes for an id of 64 must be a *signed* LEB128, i.e. two bytes
/// (`0xC0 0x00`), not the single byte `0x40` that decodes to -64.
#[test]
fn an_id_of_64_is_encoded_as_signed_leb128() {
    let (blocks, regs) = chain(71);
    let prog = partition(&[0x1000], &blocks, &regs);
    let module = build_module(&prog, &blocks).expect("module");
    // Ask a real decoder what the module means rather than grepping bytes.
    let path = std::env::temp_dir().join(format!("wlam-leb-check-{}.wasm", std::process::id()));
    std::fs::write(&path, &module).unwrap();
    let out = std::process::Command::new("../../toolchain/wabt-src/bin/wasm2wat")
        .arg(&path)
        .output()
        .expect("wasm2wat");
    let _ = std::fs::remove_file(&path);
    assert!(out.status.success(), "module must validate");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !text.contains("i32.const -58"),
        "id 70 must not appear as -58: that is unsigned LEB128 read as signed"
    );
    for bad in ["i32.const -60", "i32.const -61", "i32.const -62"] {
        assert!(!text.contains(bad), "found a sign-extended dispatch id: {bad}");
    }
}
