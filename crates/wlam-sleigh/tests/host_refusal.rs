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
use wlam_harness::{
    instantiate, load_image, run_entry_diagnosing, seed_register, Host, IatPolicy, OnRefuse,
    RunError,
};
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
                    name: i
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("#{}", i.ordinal.unwrap_or(0))),
                },
            )
        })
        .collect();

    let mut blocks: BTreeMap<u64, eir::Block> = BTreeMap::new();
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
    store.set_fuel(1_000_000).expect("fuel enabled");
    let image = std::fs::read("../../north-star/bin/Uxn32.exe").unwrap();
    let pe = Pe::parse(&image).unwrap();
    let memory = instance.get_memory(&store, "memory").unwrap();
    load_image(memory.data_mut(&mut store), &pe, IatPolicy::Poison).unwrap();
    let result = run_entry_diagnosing(&mut store, &instance, &loader_seed(), &host);

    // The guest must stop, and the reason must be a named refusal rather than
    // an anonymous trap somewhere downstream.
    match result {
        Err(RunError::Refused(d)) => {
            assert!(!d.api.name.is_empty(), "diagnostic must name the API: {d}");
            assert!(
                !d.api.library.is_empty(),
                "diagnostic must name the DLL: {d}"
            );
        }
        Err(RunError::Trap(msg)) => {
            // A trap before any host call is a different bug (loader seeding).
            let diags = host.lock().unwrap().diagnostics().len();
            panic!("expected a refusal diagnostic, got a trap with {diags} diagnostics: {msg}");
        }
        Err(RunError::Load(e)) => panic!("link failed: {e}"),
        Ok(()) => {
            let h = host.lock().unwrap_or_else(|error| error.into_inner());
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
    store.set_fuel(1_000_000).expect("fuel enabled");
    let image = std::fs::read("../../north-star/bin/Uxn32.exe").unwrap();
    let pe = Pe::parse(&image).unwrap();
    let memory = instance.get_memory(&store, "memory").unwrap();
    load_image(memory.data_mut(&mut store), &pe, IatPolicy::Poison).unwrap();
    let _ = run_entry_diagnosing(&mut store, &instance, &loader_seed(), &host);

    let h = host.lock().unwrap_or_else(|error| error.into_inner());
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

/// The real loaded image must make a measurable startup milestone within a
/// bounded run. Merely printing a trap or exhausting twenty billion fuel units
/// cannot establish progress.
#[test]
fn real_image_reaches_version_and_heap_within_budget() {
    let Some(module) = build_uxn32() else { return };
    let mut h = Host::new();
    kernel32(&mut h);
    let host = Arc::new(Mutex::new(h));
    let (mut store, instance) = instantiate(&module, Arc::clone(&host)).expect("link");
    store.set_fuel(1_000_000).expect("fuel enabled");
    let image = std::fs::read("../../north-star/bin/Uxn32.exe").unwrap();
    let pe = Pe::parse(&image).unwrap();
    let memory = instance.get_memory(&store, "memory").unwrap();
    load_image(memory.data_mut(&mut store), &pe, IatPolicy::Poison).unwrap();
    let outcome = run_entry_diagnosing(&mut store, &instance, &loader_seed(), &host);
    let h = host.lock().unwrap_or_else(|error| error.into_inner());
    for name in ["GetVersion", "HeapCreate"] {
        let api = wlam_harness::Api::new("KERNEL32.dll", name);
        assert!(
            h.call_counts().get(&api).copied().unwrap_or(0) > 0,
            "real startup never reached {name}: {outcome:?}"
        );
    }
}
