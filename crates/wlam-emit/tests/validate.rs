//! Integration tests for emitted modules.
//!
//! Every module this crate produces must be *valid* WebAssembly, not merely
//! well-formed bytes. These tests build modules with [`wlam_emit::Encoder`] and
//! validate them with the `wasm-tools` binary from `toolchain/tools/bin`, the
//! same tool used during development. If the tool is absent the test skips, so
//! the suite still runs on a bare checkout.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use wlam_emit::{module_header, section_id, Encoder};

fn wasm_tools() -> Option<String> {
    // Look relative to the crate first (works from `cargo test`), then PATH.
    let rel = "../../toolchain/tools/bin/wasm-tools";
    if Path::new(rel).exists() {
        return Some(rel.to_string());
    }
    let out = Command::new("which").arg("wasm-tools").output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

fn validate(bytes: &[u8]) -> Result<(), String> {
    let Some(tool) = wasm_tools() else {
        eprintln!("skipping: wasm-tools not found");
        return Ok(());
    };
    // Tests run concurrently and all write quickly; a pid-only filename lets
    // one test validate another test's bytes. Use a monotonic counter too.
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("wlam-validate-{}-{n}.wasm", std::process::id()));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    let out = Command::new(tool)
        .arg("validate")
        .arg(&path)
        .output()
        .map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&path);
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

fn leb(mut v: u32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

fn vec_of(items: &[Vec<u8>]) -> Vec<u8> {
    let mut out = leb(items.len() as u32);
    for i in items {
        out.extend_from_slice(i);
    }
    out
}

/// Minimal module: one memory, one exported function returning 42.
fn const_module() -> Vec<u8> {
    let mut e = Encoder::new();
    module_header(&mut e);
    // type section: () -> i32
    let mut ty = Vec::new();
    ty.push(0x60);
    ty.extend(vec_of(&[])); // params
    ty.extend(vec_of(&[vec![0x7f]])); // results
    e.section(section_id::TYPE, |b| b.raw(&vec_of(&[ty])));
    // function section: one function of type 0
    e.section(section_id::FUNCTION, |b| b.raw(&vec_of(&[vec![0x00]])));
    // memory section: min 1 page
    e.section(section_id::MEMORY, |b| b.raw(&vec_of(&[vec![0x00, 0x01]])));
    // export "f"
    let mut export = Vec::new();
    export.extend(leb(1));
    export.push(b'f');
    export.push(0x00);
    export.extend(leb(0));
    e.section(section_id::EXPORT, |b| b.raw(&vec_of(&[export])));
    // code: locals 0 ; i32.const 42 ; end
    let mut body = Vec::new();
    body.extend(vec_of(&[])); // no locals
    body.push(0x41);
    body.extend(leb(42));
    body.push(0x0b);
    let mut entry = leb(body.len() as u32);
    entry.extend(body);
    e.section(section_id::CODE, |b| b.raw(&vec_of(&[entry])));
    e.into_bytes()
}

#[test]
fn encoder_output_passes_the_reference_validator() {
    let bytes = const_module();
    validate(&bytes).expect("module must be valid wasm");
}

#[test]
fn empty_module_is_valid() {
    let mut e = Encoder::new();
    module_header(&mut e);
    validate(e.bytes()).expect("bare header must be valid");
}

#[test]
fn validator_rejects_corruption() {
    // Sanity check that `validate` really does reject nonsense, so passing the
    // tests above means something.
    let mut bytes = const_module();
    bytes.truncate(bytes.len() - 3);
    assert!(
        validate(&bytes).is_err(),
        "truncated module must not validate"
    );
}

#[test]
fn emitted_module_runs_in_v8_with_the_expected_result() {
    // Node is the second consumer used by the browser path, so check the
    // module executes and returns what it claims, not just that it parses.
    let node = Command::new("node").arg("--version").output();
    if node.map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("skipping: node not available");
        return;
    }
    let bytes = const_module();
    let path = std::env::temp_dir().join(format!("wlam-exec-{}.wasm", std::process::id()));
    // Distinct from the validator files above.
    std::fs::write(&path, &bytes).unwrap();
    let script = format!(
        "const fs=require('fs');const m=new WebAssembly.Module(fs.readFileSync({path:?}));\
         const i=new WebAssembly.Instance(m,{{}});console.log(i.exports.f());"
    );
    let out = Command::new("node").arg("-e").arg(script).output().unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "42");
}
