//! Progress oracle: how far does emission get on a real image, and what stops it?
//!
//! Prints a single scoreboard line plus a histogram of the reason emission
//! stopped. This is the loop's exit condition: it must reach `RESULT: VALIDATED` with
//! zero blocking reasons.
//!
//! Usage: cargo run --example progress [path-to.exe]
use wlam_sleigh::emit as shared;

use std::collections::BTreeMap;
use wlam_emit::chain::{build_module_with_memory, partition, HostImport};
use wlam_pe::Pe;
use wlam_sleigh::discover::discover;
use wlam_sleigh::with_big_stack;

/// Every register the program mentions, ABI locals first.
///
/// Derived from the IR rather than from a fixed list: real images use
/// sub-registers (AL, AX), individual flags (CF..ZF), x87 (ST0) and Sleigh's
/// own synthetic registers (reg_10a0_2), and a hand-maintained list went stale
/// the moment a second binary was tried.
fn registers_for(blocks: &BTreeMap<u64, wlam_emit::ir::Block>) -> Vec<String> {
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

/// The repository root, found by walking up from this source file.
///
/// The oracle used to resolve `../../north-star/...` relative to the *current
/// directory*, so running it from anywhere but `crates/wlam-sleigh` reported a
/// missing image or a missing validator -- and the validator path was the same
/// `../../`, which meant a wrong cwd produced `RESULT: INVALID` for a module
/// that was never checked. Both are now anchored to the source location.
fn repo_root() -> std::path::PathBuf {
    let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    here.parent()
        .and_then(|p| p.parent())
        .map_or_else(|| here.to_path_buf(), |p| p.to_path_buf())
}

/// Run `wasm-validate` over the emitted module.
///
/// The validators live in `toolchain/wabt-src/bin`, which is the same build the
/// rest of the project uses. If the binary is missing that is a distinct
/// outcome from "the module is invalid", and must not be reported as one.
fn validate(module: &[u8]) -> Result<(), Vec<String>> {
    let bin = repo_root().join("toolchain/wabt-src/bin/wasm-validate");
    if !bin.exists() {
        return Err(vec![format!("MISSING VALIDATOR: {}", bin.display())]);
    }
    // Keep the module in the workspace when DIAG is set, so it can be inspected
    // with wasm-objdump/wasm2wat after a failure. The system temp dir is not
    // always readable from the tool sandbox.
    let path = if std::env::var("KEEP_WASM").is_ok() {
        repo_root().join(".scratch-out.wasm")
    } else {
        std::env::temp_dir().join(format!("wlam-progress-{}.wasm", std::process::id()))
    };
    std::fs::write(&path, module).map_err(|e| vec![format!("write failed: {e}")])?;
    let out = std::process::Command::new(&bin)
        .arg(&path)
        .output()
        .map_err(|e| vec![format!("validator failed to run: {e}")])?;
    if std::env::var("KEEP_WASM").is_err() {
        let _ = std::fs::remove_file(&path);
    }
    if out.status.success() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&out.stderr);
    let errs: Vec<String> = text
        .lines()
        .filter(|l| l.contains("error"))
        .map(|l| l.trim_start_matches(".scratch/").to_string())
        .collect();
    if errs.is_empty() {
        Err(vec![text.trim().to_string()])
    } else {
        Err(errs)
    }
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| repo_root().join("north-star/bin/Uxn32.exe"))
        .display()
        .to_string();
    let Ok(bytes) = std::fs::read(&path) else {
        println!("RESULT: SKIP (no image at {path})");
        return;
    };
    let prog = {
        let b2 = bytes.clone();
        with_big_stack(move || {
            let p = Pe::parse(&b2).expect("parse");
            discover(&p, 64)
        })
    };
    let pe = Pe::parse(&bytes).expect("parse");
    let base = pe.image_base as u64;

    let by_slot: BTreeMap<u64, HostImport> = pe
        .imports
        .iter()
        .filter(|i| i.iat_slot_verified)
        .map(|i| {
            (
                base + i.iat_rva as u64,
                HostImport {
                    library: i.library.clone(),
                    name: i
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("#{}", i.ordinal.unwrap_or(0))),
                },
            )
        })
        .collect();

    let mut blocks = BTreeMap::new();
    for b in prog.blocks() {
        let rva = (b.start - base) as u32;
        let Some(idx) = pe.section_of(rva) else {
            continue;
        };
        let sec = &pe.sections[idx];
        let off = (rva - sec.virtual_address) as usize;
        if off >= sec.data.len() {
            continue;
        }
        let slice = sec.data[off..].to_vec();
        let start = b.start;
        let lb = with_big_stack(move || wlam_sleigh::lift_block(&slice, start));
        blocks.insert(b.start, shared::convert(&lb));
    }

    // The memory must reach the guest's image base and the whole image, or
    // every absolute access traps. Rounded up to a whole page.
    let image_end = base + pe.size_of_image as u64;
    let pages = wlam_emit::chain::pages_for_image(image_end);
    let regs = registers_for(&blocks);
    let entries = vec![base + pe.entry_point_rva as u64];
    let mut p = partition(&entries, &blocks, &regs);
    p.register_host_imports(&blocks, |slot| by_slot.get(&slot).cloned());

    let gaps = p
        .host_imports
        .values()
        .filter(|i| i.library == "wlam.unsupported")
        .map(|i| i.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    println!(
        "SCOPE: module validation only; {} unsupported-operation diagnostics trap if reached",
        gaps.len()
    );
    for gap in gaps.iter().take(5) {
        println!("GAP: {gap}");
    }
    let cover = prog.block_starts().len();
    match build_module_with_memory(&p, &blocks, pages) {
        Ok(m) => {
            // Emitting is not the same as emitting something VALID. An earlier
            // version of this oracle printed RESULT: OK for a module that
            // wasm-validate rejected outright, which made the scoreboard lie.
            // Validate before claiming success.
            match validate(&m) {
                Ok(()) => {
                    println!(
                        "RESULT: VALIDATED  blocks={cover} functions={} imports={} bytes={}",
                        p.functions.len(),
                        p.host_imports.len(),
                        m.len()
                    );
                }
                Err(errs) => {
                    let missing = errs.iter().any(|e| e.starts_with("MISSING VALIDATOR"));
                    println!(
                        "RESULT: {}  blocks={cover} functions={} imports={} bytes={} errors={}",
                        if missing { "UNCHECKED" } else { "INVALID" },
                        p.functions.len(),
                        p.host_imports.len(),
                        m.len(),
                        errs.len()
                    );
                    for e in errs.iter().take(5) {
                        println!("REASON: {e}");
                    }
                }
            }
        }
        Err(e) => {
            println!(
                "RESULT: BLOCKED  blocks={cover} functions={} imports={}",
                p.functions.len(),
                p.host_imports.len()
            );
            println!("REASON: {e}");
        }
    }
    if prog.incomplete() {
        println!(
            "WARN: discovery did not converge ({} pending)",
            prog.pending().len()
        );
    }
}
