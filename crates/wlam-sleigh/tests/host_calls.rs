//! A call through the IAT becomes a wasm import, not a guest function call.
//!
//! `CALL dword ptr [slot]` lifts as `Load T <- [slot]; CallInd T` where `slot`
//! is a constant `.idata` address. That is a Win32 import, so the emitter must
//! route it to a host function in the importing namespace (docs/abi.md) rather
//! than trying to dispatch into guest code.

mod shared;

use std::collections::BTreeMap;

use wlam_emit::chain::{build_module, partition, HostImport};
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
    let mut set = std::collections::BTreeSet::new();
    for b in blocks.values() {
        for op in &b.ops {
            shared::collect_regs(op, &mut set);
        }
    }
    set.into_iter().collect()
}

/// ```text
/// 0x1000: FF 15 0C 11 40 00   call dword ptr [0x40110c]   (return addr 0x1006)
/// 0x1006: C7 05 04 00 01 00 02 00 00 00   mov dword [0x10004], 2
/// 0x1010: C3                  ret
/// ```
#[test]
fn iat_call_becomes_a_host_import() {
    let base = 0x1000u64;
    let slot = 0x40110cu32;
    let mut code: Vec<u8> = vec![0xFF, 0x15];
    code.extend_from_slice(&slot.to_le_bytes());
    // 0x1006: post-call store
    code.extend_from_slice(&[0xC7, 0x05, 0x04, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00]);
    code.push(0xC3);

    let blocks = blocks_from(&code, base, &[base, base + 6, base + 0x10]);
    let regs = registers(&blocks);
    let mut prog = partition(&[base], &blocks, &regs);

    // The load feeds the call, so the block must be recognised as an IAT call.
    assert_eq!(
        blocks[&base].terminator,
        eir::Terminator::CallInd { resume: base + 6 },
        "call [slot] must end the block at its return address"
    );

    prog.host_imports.insert(
        slot as u64,
        HostImport { library: "KERNEL32.dll".into(), name: "GetTickCount".into() },
    );
    prog.import_index.insert(
        HostImport { library: "KERNEL32.dll".into(), name: "GetTickCount".into() },
        0,
    );

    let module = build_module(&prog, &blocks).expect("module");
    let wat = std::process::Command::new("../../toolchain/wabt-src/bin/wasm2wat")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            c.stdin.as_mut().unwrap().write_all(&module)?;
            c.wait_with_output()
        })
        .expect("wasm2wat");
    let text = String::from_utf8_lossy(&wat.stdout).to_string();
    assert!(
        text.contains("import \"KERNEL32.dll\" \"GetTickCount\""),
        "module must import the Win32 API under its DLL namespace:\n{text}"
    );
    assert!(
        text.contains("call 0"),
        "the call site must call the import index:\n{text}"
    );
}

#[test]
fn register_target_call_is_not_confused_with_an_import() {
    // `call eax` has no statically known target, so it must NOT be routed to a
    // host import the way `call [slot]` is. It goes through the function table
    // instead, and with no host imports registered the module declares none.
    // The trap-behaviour of that path is covered in tests/indirect_calls.rs.
    let base = 0x1000u64;
    let code: Vec<u8> = vec![0xFF, 0xD0, 0xC3];
    let blocks = blocks_from(&code, base, &[base]);
    let regs = registers(&blocks);
    let prog = partition(&[base], &blocks, &regs);
    let module = build_module(&prog, &blocks).expect("call eax now emits via the table");
    assert!(
        prog.host_imports.is_empty(),
        "a register-target call must not invent a host import: {:?}",
        prog.host_imports
    );
    assert!(
        !module.is_empty(),
        "module must still be produced"
    );
}

/// Execute the module with a real host function and check the guest observes
/// the return value in EAX and then continues past the call.
#[test]
fn iat_call_executes_and_returns_into_the_guest() {
    let base = 0x1000u64;
    let slot = 0x40110cu32;
    let mut code: Vec<u8> = vec![0xFF, 0x15];
    code.extend_from_slice(&slot.to_le_bytes());
    // 0x1006: mov [0x10004], EAX   (89 05 04 00 01 00) -- prove EAX survived
    code.extend_from_slice(&[0x89, 0x05, 0x04, 0x00, 0x01, 0x00]);
    // 0x100c: ret
    code.push(0xC3);

    let blocks = blocks_from(&code, base, &[base, base + 6, base + 0xc]);
    let regs = registers(&blocks);
    let mut prog = partition(&[base], &blocks, &regs);
    let import = HostImport { library: "KERNEL32.dll".into(), name: "GetTickCount".into() };
    prog.host_imports.insert(slot as u64, import.clone());
    prog.import_index.insert(import, 0);

    let module = build_module(&prog, &blocks).expect("module");

    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("wlam-hc-{}-{n}.wasm", std::process::id()));
    std::fs::write(&path, &module).unwrap();

    let out = std::process::Command::new("timeout")
        .arg("10")
        .arg("node")
        .arg("-e")
        .arg(format!(
            "const fs=require('fs');const m=new WebAssembly.Module(fs.readFileSync({path:?}));\
             const imports={{'KERNEL32.dll':{{GetTickCount:(esp,slot)=>0x1234}} }};\
             const i=new WebAssembly.Instance(m,imports);\
             const d=new DataView(i.exports.memory.buffer);\
             d.setUint32({base}+{esp_idx}*4,0x4000,true);\
             try{{i.exports.entry();}}catch(e){{console.log('TRAP '+e.message);process.exit(0)}}\
             console.log('EAX_STORED='+d.getUint32(0x10004,true));",
            base = wlam_emit::REG_SNAPSHOT_BASE,
            esp_idx = regs.iter().position(|r| r == "ESP").expect("ESP is a guest local"),
        ))
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(
        text.trim(),
        "EAX_STORED=4660",
        "host return value must reach EAX and the guest must resume: {text} {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
