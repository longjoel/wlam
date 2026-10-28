//! Executable tests for block chaining.
//!
//! The previous round produced a chained loop that validated as wasm but never
//! terminated. These tests pin *behaviour*: a branch must be taken when the
//! condition says so and not otherwise. A module that merely validates is not
//! evidence of anything.

mod shared;

use std::collections::BTreeMap;

use wlam_emit::ir as eir;
use wlam_emit::chain::{build_module, partition};
use wlam_sleigh::{lift_block, with_big_stack};

/// Lift `code` starting at each address in `starts` and adapt to the emitter's
/// block contract. Discovery normally supplies the block set; here the test
/// states it explicitly so the fixture is readable.
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

/// Registers mentioned by any block.
fn registers(blocks: &BTreeMap<u64, eir::Block>) -> Vec<String> {
    let mut set = std::collections::BTreeSet::new();
    for b in blocks.values() {
        for op in &b.ops {
            shared::collect_regs(op, &mut set);
        }
    }
    set.into_iter().collect()
}

/// Run the module in Node, returning the value of EAX (read from the register
/// snapshot) and whether execution completed.
fn run(module: &[u8], regs: &[String], eax: u32, ecx: u32, mem: &[(u32, u32)]) -> (Option<u32>, String) {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("wlam-chain-{}-{n}.wasm", std::process::id()));
    std::fs::write(&path, module).unwrap();
    let eax_idx = regs.iter().position(|r| r == "EAX").unwrap_or(usize::MAX);
    let ecx_idx = regs.iter().position(|r| r == "ECX").unwrap_or(usize::MAX);
    // Only seed registers the program actually uses: an absent register has no
    // snapshot slot, and indexing with `usize::MAX` silently produced a garbage
    // address (and a timeout) instead of a clear failure.
    let mut seeds = vec![format!("d.setUint32({base}+{eax_idx}*4,{eax},true);", base = wlam_emit::REG_SNAPSHOT_BASE)];
    if ecx_idx != usize::MAX {
        seeds.push(format!("d.setUint32({base}+{ecx_idx}*4,{ecx},true);", base = wlam_emit::REG_SNAPSHOT_BASE));
    }
    let mem_js: Vec<String> = mem.iter().map(|(a, v)| format!("d.setUint32({a},{v},true);")).collect();
    let seeds_js = seeds.join("");
    let script = format!(
        "const fs=require('fs');\
         const m=new WebAssembly.Module(fs.readFileSync({path:?}));\
         const i=new WebAssembly.Instance(m,{{}});\
         const d=new DataView(i.exports.memory.buffer);\
         {seeds}\
         {} \
         try{{ i.exports.entry(); }}catch(e){{ console.log(JSON.stringify({{err:String(e.message)}})); process.exit(0); }}\
         console.log(JSON.stringify({{eax:d.getUint32({base}+{eax_idx}*4,true)}}));",
        mem_js.join(""),
        seeds = seeds_js,
        base = wlam_emit::REG_SNAPSHOT_BASE,
    );
    let out = std::process::Command::new("timeout")
        .arg("10")
        .arg("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    let text = String::from_utf8_lossy(&out.stdout);
    if text.trim().is_empty() {
        return (None, format!("no output (timeout or crash): {}", String::from_utf8_lossy(&out.stderr)));
    }
    if let Some(rest) = text.split("\"eax\":").nth(1) {
        let v = rest.trim().trim_end_matches('}').trim();
        return (v.parse().ok(), String::new());
    }
    (None, text.trim().to_string())
}

/// The simplest possible branch: one conditional block and two leaf blocks.
///
/// The fixture builds its own displacements and asserts the resulting targets,
/// because hand-computing `rel8` is exactly the kind of arithmetic that produced
/// wrong fixtures twice in this project.
///
/// ```text
/// 0x1000: 83 F8 05          cmp eax, 5
/// 0x1003: 7C ??             jl   <taken leaf>
/// 0x1005: EB ??             jmp  <not-taken leaf>
/// 0x1007: 90 90 90          nops
/// 0x100a: B8 01 00 00 00    mov eax, 1
/// 0x100f: C3                ret
/// 0x1010: B8 02 00 00 00    mov eax, 2
/// 0x1015: C3                ret
/// ```
fn branch_fixture() -> (Vec<u8>, u64, Vec<u64>) {
    let base: u64 = 0x1000;
    let taken_leaf = base + 0x0a;
    let not_taken_leaf = base + 0x10;
    let jl_at = base + 3;
    let jmp_at = base + 5;
    let jl_rel = (taken_leaf as i64 - (jl_at + 2) as i64) as i8;
    let jmp_rel = (not_taken_leaf as i64 - (jmp_at + 2) as i64) as i8;
    #[rustfmt::skip]
    let code: Vec<u8> = vec![
        0x83, 0xF8, 0x05,                          // cmp eax, 5
        0x7C, jl_rel as u8,                        // jl taken_leaf
        0xEB, jmp_rel as u8,                       // jmp not_taken_leaf
        0x90, 0x90, 0x90,                          // padding to 0x100a
        0xB8, 0x01, 0x00, 0x00, 0x00, 0xC3,        // mov eax, 1 ; ret
        0xB8, 0x02, 0x00, 0x00, 0x00, 0xC3,        // mov eax, 2 ; ret
    ];
    assert_eq!(code.len() as u64, 0x16, "fixture length");
    // Block starts: the conditional, the fall-through `jmp` at 0x1005, and both
    // leaves. Sleigh's `not_taken` is a fall-through *address* (0x1005), which is
    // the `jmp` instruction itself, so it must be a block in the program.
    (code, base, vec![base, base + 5, taken_leaf, not_taken_leaf])
}

/// A conditional branch, observed through guest memory.
///
/// ```text
/// 0x1000: 83 F8 05   cmp eax, 5
/// 0x1003: 7C ??      jl  taken
/// 0x1005: EB ??      jmp not_taken
/// 0x1007: 90 90 90   padding
/// 0x100a: C7 05 00 00 01 00 01 00 00 00   mov dword [0x10000], 1   (taken)
/// 0x1014: C3
/// 0x1015: C7 05 04 00 01 00 02 00 00 00   mov dword [0x10004], 2   (not taken)
/// 0x101f: C3
/// ```
#[test]
fn conditional_branch_takes_the_correct_path() {
    let base: u64 = 0x1000;
    let jl_at = base + 3;
    let jmp_at = base + 5;
    let taken = base + 0x0a;
    let not_taken = base + 0x15;
    let jl_rel = (taken as i64 - (jl_at + 2) as i64) as i8;
    let jmp_rel = (not_taken as i64 - (jmp_at + 2) as i64) as i8;
    #[rustfmt::skip]
    let code: Vec<u8> = vec![
        0x83, 0xF8, 0x05,                            // cmp eax, 5
        0x7C, jl_rel as u8,                          // jl taken
        0xEB, jmp_rel as u8,                         // jmp not_taken
        0x90, 0x90, 0x90,                            // padding
        0xC7, 0x05, 0x00, 0x00, 0x01, 0x00, 0x01, 0, 0, 0,  // mov [0x10000],1
        0xC3,                                        // ret
        0xC7, 0x05, 0x04, 0x00, 0x01, 0x00, 0x02, 0, 0, 0,  // mov [0x10004],2
        0xC3,                                        // ret
    ];
    assert_eq!(code.len() as u64, not_taken + 0x0a - base + 1, "fixture length");
    let blocks = blocks_from(&code, base, &[base, jmp_at, taken, not_taken]);
    assert_eq!(
        blocks[&base].terminator,
        eir::Terminator::Conditional { taken: Some(taken), not_taken: jmp_at }
    );
    let regs = registers(&blocks);
    let prog = partition(&[base], &blocks, &regs);
    let func = &prog.functions[0];
    assert!(func.problems.is_empty(), "problems: {:?}", func.problems);
    let module = build_module(&prog, &blocks).expect("module");

    let read = |eax: u32| -> (Option<u32>, Option<u32>, String) {
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("wlam-cond-{}-{n}.wasm", std::process::id()));
        std::fs::write(&path, &module).unwrap();
        let out = std::process::Command::new("timeout")
            .arg("10")
            .arg("node")
            .arg("-e")
            .arg(format!(
                "const fs=require('fs');const m=new WebAssembly.Module(fs.readFileSync({path:?}));\
                 const i=new WebAssembly.Instance(m,{{}});\
                 const d=new DataView(i.exports.memory.buffer);\
                 d.setUint32({base}+{eax_idx}*4,{eax},true);\
                 try{{i.exports.entry();}}catch(e){{console.log('TRAP '+e.message);process.exit(0)}}\
                 console.log('A='+d.getUint32(0x10000,true)+' B='+d.getUint32(0x10004,true));",
                base = wlam_emit::REG_SNAPSHOT_BASE,
                eax_idx = regs.iter().position(|r| r == "EAX").unwrap_or(0),
            ))
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&path);
        let t = String::from_utf8_lossy(&out.stdout).to_string();
        let num = |key: &str| {
            t.split(key)
                .nth(1)
                .and_then(|r| r.split_whitespace().next().and_then(|v| v.parse().ok()))
        };
        (num("A="), num("B="), t)
    };

    // Guest memory starts zeroed, so the *untaken* slot reads 0 rather than
    // being unset; the test asserts the taken store happened and the other did
    // not.
    let (a, b, log) = read(3);
    assert_eq!((a, b), (Some(1), Some(0)), "eax=3 < 5 must take jl (log {log})");
    let (a, b, log) = read(7);
    assert_eq!((a, b), (Some(0), Some(2)), "eax=7 >= 5 must not take jl (log {log})");
}

/// Chaining across an explicit jump, observed through guest memory.
///
/// 0x1000: EB 04            jmp 0x1006
/// 0x1002: 90 90 90 90      padding
/// 0x1006: C7 05 00 00 01 00 2A 00 00 00   mov dword [0x10000], 42
/// 0x1010: C3               ret
#[test]
fn jump_chains_into_the_next_block_and_stores() {
    let base = 0x1000u64;
    let jmp_rel = 4i8; // 0x1006 - (0x1000 + 2)
    #[rustfmt::skip]
    let code: Vec<u8> = vec![
        0xEB, jmp_rel as u8,
        0x90, 0x90, 0x90, 0x90,
        0xC7, 0x05, 0x00, 0x00, 0x01, 0x00, 0x2A, 0x00, 0x00, 0x00,
        0xC3,
    ];
    let blocks = blocks_from(&code, base, &[base, base + 6]);
    assert_eq!(blocks[&base].terminator, eir::Terminator::Jump(base + 6));
    let regs = registers(&blocks);
    let prog = partition(&[base], &blocks, &regs);
    assert_eq!(prog.functions.len(), 1);
    let func = &prog.functions[0];
    assert_eq!(func.blocks, vec![base, base + 6], "both blocks in one function");
    assert!(func.problems.is_empty(), "problems: {:?}", func.problems);
    let module = build_module(&prog, &blocks).expect("module");

    // Read guest memory at 0x10000 after running.
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("wlam-jmp-{}-{n}.wasm", std::process::id()));
    std::fs::write(&path, &module).unwrap();
    let out = std::process::Command::new("timeout")
        .arg("10")
        .arg("node")
        .arg("-e")
        .arg(format!(
            "const fs=require('fs');const m=new WebAssembly.Module(fs.readFileSync({path:?}));\
             const i=new WebAssembly.Instance(m,{{}});\
             try{{i.exports.entry();}}catch(e){{console.log('TRAP '+e.message);process.exit(0)}}\
             console.log('MEM='+new DataView(i.exports.memory.buffer).getUint32(0x10000,true));"
        ))
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("MEM=42"),
        "chained jump must execute the target's store, got: {text} {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn chained_module_is_valid_wasm() {
    let (code, base, starts) = branch_fixture();
    let blocks = blocks_from(&code, base, &starts);
    let regs = registers(&blocks);
    let prog = partition(&[0x1000], &blocks, &regs);
    let module = build_module(&prog, &blocks).expect("module");
    let path = std::env::temp_dir().join(format!("wlam-valid-{}.wasm", std::process::id()));
    std::fs::write(&path, &module).unwrap();
    let out = std::process::Command::new("../../toolchain/tools/bin/wasm-tools")
        .arg("validate")
        .arg(&path)
        .output();
    let _ = std::fs::remove_file(&path);
    if let Ok(out) = out {
        assert!(
            out.status.success(),
            "module must validate: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
