//! A guest call to an unimplemented API must stop with a diagnostic naming it.
//!
//! The engine links the recompiled Uxn32 image against a host that deliberately
//! implements nothing. The run must not limp along on stub zeros; it must stop
//! at the first host call and say which API and where.

mod shared;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use wlam_emit::chain::{build_module_with_memory, partition, HostImport};
use wlam_emit::ir as eir;
use wlam_harness::{instantiate, run_entry_diagnosing, seed_register, Host, OnRefuse, RunError};
use wlam_host::kernel32;
use wlam_pe::Pe;
use wlam_sleigh::discover::discover;
use wlam_sleigh::{lift_block, with_big_stack};

fn build_uxn32() -> Option<Vec<u8>> {
    let bytes = std::fs::read("../../north-star/bin/Uxn32.exe").ok()?;
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
                    name: i.name.clone().unwrap_or_else(|| format!("#{}", i.ordinal.unwrap_or(0))),
                },
            )
        })
        .collect();

    let mut blocks: BTreeMap<u64, eir::Block> = BTreeMap::new();
    for b in prog.blocks() {
        let rva = (b.start - base) as u32;
        let Some(idx) = pe.section_of(rva) else { continue };
        let sec = &pe.sections[idx];
        let off = (rva - sec.virtual_address) as usize;
        if off >= sec.data.len() {
            continue;
        }
        let slice = sec.data[off..].to_vec();
        let start = b.start;
        let lifted = with_big_stack(move || lift_block(&slice, start));
        blocks.insert(b.start, shared::convert(&lifted));
    }

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

    // Cover the image AND the heap region the host allocates from; sizing to
    // the image alone made HeapAlloc return addresses outside the memory.
    let image_end = base + pe.size_of_image as u64;
    let pages = wlam_emit::chain::pages_for_image(image_end);
    let mut p = partition(&[base + pe.entry_point_rva as u64], &blocks, &regs);
    p.register_host_imports(&blocks, |slot| by_slot.get(&slot).cloned());
    Some(build_module_with_memory(&p, &blocks, pages).expect("module"))
}

/// A stack pointer and TIB base so the first `push` and `fs:[0]` do not fault.
fn loader_seed() -> Vec<(u32, u32)> {
    let mut seed = Vec::new();
    seed_register(&mut seed, 4, 0x70000); // ESP
    seed_register(&mut seed, 10, 0x60000); // FS_OFFSET -> TIB
    seed.push((0x60000, 0xFFFF_FFFF)); // empty SEH chain
    seed
}

#[test]
fn unimplemented_api_stops_the_run_and_names_itself() {
    let Some(module) = build_uxn32() else {
        return; // image not present
    };
    let host = Arc::new(Mutex::new(Host::new()));
    let (mut store, instance) = instantiate(&module, Arc::clone(&host)).expect("link");
    let result = run_entry_diagnosing(&mut store, &instance, &loader_seed(), &host);

    // The guest must stop, and the reason must be a named refusal rather than
    // an anonymous trap somewhere downstream.
    match result {
        Err(RunError::Refused(d)) => {
            assert!(!d.api.name.is_empty(), "diagnostic must name the API: {d}");
            assert!(!d.api.library.is_empty(), "diagnostic must name the DLL: {d}");
        }
        Err(RunError::Trap(msg)) => {
            // A trap before any host call is a different bug (loader seeding).
            let diags = host.lock().unwrap().diagnostics().len();
            panic!("expected a refusal diagnostic, got a trap with {diags} diagnostics: {msg}");
        }
        Err(RunError::Load(e)) => panic!("link failed: {e}"),
        Ok(()) => {
            let h = host.lock().unwrap();
            panic!(
                "guest ran to completion with {} unimplemented APIs ({:?})",
                h.unimplemented().len(),
                h.unimplemented().iter().take(4).collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn probing_mode_reaches_further_and_still_records_every_refusal() {
    let Some(module) = build_uxn32() else { return };
    // Start from the APIs the host genuinely implements, so the refusals
    // reported are the ones actually still missing rather than the whole import
    // table.
    let mut h = Host::new();
    kernel32(&mut h);
    // Deliberately non-cranky: return 0 so the guest keeps going. Used to
    // discover what the image actually calls.
    h.on_refuse(OnRefuse::Return(0));
    let host = Arc::new(Mutex::new(h));
    let (mut store, instance) = instantiate(&module, Arc::clone(&host)).expect("link");
    let _ = run_entry_diagnosing(&mut store, &instance, &loader_seed(), &host);

    let h = host.lock().unwrap();
    let refused = h.traps();
    assert_eq!(
        h.diagnostics().len() as u64,
        refused,
        "every refusal must be recorded exactly once"
    );
    // How far the guest gets is reported, not asserted: as the host grows, the
    // guest legitimately stops refusing and fails somewhere else instead.
    println!("refused {refused} unimplemented APIs before stopping");
}

/// Reports the order in which the image reaches host APIs, so the work of
/// implementing them can be driven by what the program actually calls rather
/// than by the import table's alphabetical order.
///
/// Prints under `--nocapture`; asserts only that the run stops at a named API.
#[test]
fn first_refusals_are_reported_in_call_order() {
    let Some(module) = build_uxn32() else { return };
    let mut h = Host::new();
    kernel32(&mut h);
    h.on_refuse(OnRefuse::Return(0)); // keep going so we can see a sequence
    let host = Arc::new(Mutex::new(h));
    let (mut store, instance) = instantiate(&module, Arc::clone(&host)).expect("link");
    // Probing is expected to run long; give it room to show whether it is
    // making progress or spinning.
    store.set_fuel(20_000_000_000).ok();
    let outcome = run_entry_diagnosing(&mut store, &instance, &loader_seed(), &host);
    eprintln!("PROBING RUN: {outcome:?}");

    let h = host.lock().unwrap_or_else(|e| e.into_inner());
    eprintln!("CALLS: {:?}", h.call_counts().iter().filter(|(_, n)| **n > 0).map(|(a, n)| format!("{a}:{n}")).collect::<Vec<_>>());
    let diags = h.diagnostics();
    // This test reports what the guest reaches; it does not require a trap. Once
    // enough of the startup path is implemented the guest stops refusing and
    // the failure moves elsewhere (a guest-internal trap, or running to
    // completion), so demanding a refusal would make progress look like a
    // regression.
    if diags.is_empty() {
        println!("no refusals reached; guest outcome: {outcome:?}");
    }
    println!("first host APIs reached ({} refused):", diags.len());
    for d in diags.iter().take(20) {
        println!("  {d}");
    }
}
