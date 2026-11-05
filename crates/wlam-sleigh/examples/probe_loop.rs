//! Find out *where* a starved guest is spinning.
//!
//! The Uxn32 run stops on `all fuel consumed by WebAssembly` after only
//! `GetVersion` and `HeapCreate`. Fuel exhaustion says nothing about the cause:
//! it means the guest executed a billion instructions, which is either an
//! infinite loop or a program that is genuinely busy.
//!
//! This probe runs the guest *one block at a time*. Every discovered block gets
//! its own tiny wasm module (the same emitter path the real module uses,
//! `module_with_snapshot`), the snapshot area is the guest's only state, and the
//! probe decides which block to run next from the block's terminator. That is
//! slow -- a few hundred thousand wasmi calls -- but it makes the loop visible:
//! it prints the path as it goes and, when a block repeats, the window it
//! repeats with.
//!
//! Why not just instrument the real module: the dispatcher is native wasm with
//! no host call per block, so there is nothing to hook. Running blocks
//! individually is the instrumentation that needs no change to the emitter.
//!
//! Run with: `cargo run -q --example probe_loop [image]`.

use std::collections::{BTreeMap, BTreeSet};

use wlam_emit::ir as eir;
use wlam_emit::{module_with_snapshot, translate_ops, LocalMap, REG_SNAPSHOT_BASE};
use wlam_harness::wasmi;
use wlam_pe::Pe;
use wlam_sleigh::discover::discover;
use wlam_sleigh::ir as sir;
use wlam_sleigh::{with_big_stack, Block as SBlock, Terminator as STerm};

use wlam_sleigh::emit as shared;

/// Registers seeded the way a loader would: a stack, a TIB, and no SEH chain.
const ESP_SLOT: u32 = 0x70000;
const TIB: u32 = 0x60000;

fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let image = std::env::args()
        .nth(1)
        .unwrap_or_else(|| root.join("north-star/bin/Uxn32.exe").display().to_string());
    let max_steps: usize = std::env::var("STEPS").ok().and_then(|s| s.parse().ok()).unwrap_or(4000);
    let bytes = std::fs::read(&image).expect("read image");

    let pe = Pe::parse(&bytes).expect("parse");
    let base = pe.image_base as u64;
    let prog = {
        let b = bytes.clone();
        with_big_stack(move || discover(&Pe::parse(&b).unwrap(), 64))
    };
    let entry = base + pe.entry_point_rva as u64;
    let iat: BTreeMap<u64, String> = pe
        .imports
        .iter()
        .filter(|i| i.iat_slot_verified)
        .map(|i| {
            (
                base + i.iat_rva as u64,
                format!(
                    "{}!{}",
                    i.library,
                    i.name.clone().unwrap_or_else(|| format!("#{}", i.ordinal.unwrap_or(0)))
                ),
            )
        })
        .collect();

    let mut sr_blocks: BTreeMap<u64, SBlock> = BTreeMap::new();
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
        let lifted = with_big_stack(move || wlam_sleigh::lift_block(&slice, start));
        blocks.insert(b.start, shared::convert(&lifted));
        sr_blocks.insert(b.start, lifted);
    }

    // Use the same eight-byte register slots as the loader and chained emitter.
    let mut regs: Vec<String> = wlam_emit::seeded_registers();
    let mut used = BTreeSet::new();
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
    let slot_of: BTreeMap<String, u64> = regs
        .iter()
        .enumerate()
        .map(|(i, r)| (r.clone(), wlam_emit::snapshot_address(i)))
        .collect();

    // Compile every block once. A block that cannot be translated is reported
    // by name; it is a gap in the emitter, not a gap in the run.
    let mut compiled: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
    let mut indirect: BTreeMap<u64, Option<IndirectCall>> = BTreeMap::new();
    let mut failed: BTreeMap<u64, String> = BTreeMap::new();
    for (start, block) in &blocks {
        let locals = LocalMap::new(regs.iter().map(|r| (r.clone(), 4)));
        indirect.insert(*start, indirect_target(&sr_blocks[start], &iat));
        let ops = shared::strip_control_flow(block.ops.clone());
        match translate_ops(&ops, locals) {
            Ok(t) => {
                let m = module_with_snapshot(&t, &t.map, &regs, &[]);
                compiled.insert(*start, m);
            }
            Err(e) => {
                failed.insert(*start, e.to_string());
            }
        }
    }
    println!(
        "image {image}: {} discovered blocks, {} compiled, {} failed",
        blocks.len(),
        compiled.len(),
        failed.len()
    );
    for (a, why) in failed.iter().take(5) {
        println!("  cannot compile {a:#x}: {why}");
    }

    let engine = {
        let mut c = wasmi::Config::default();
        c.consume_fuel(true);
        c.wasm_tail_call(true);
        wasmi::Engine::new(&c)
    };
    let mut instances: BTreeMap<u64, wasmi::Instance> = BTreeMap::new();
    let mut store = wasmi::Store::new(&engine, ());
    store.set_fuel(u64::MAX).ok();
    for (start, m) in &compiled {
        let module = match wasmi::Module::new(&engine, &m[..]) {
            Ok(module) => module,
            Err(e) => {
                failed.insert(*start, format!("module: {e}"));
                continue;
            }
        };
        let linker = wasmi::Linker::<()>::new(&engine);
        let inst = linker
            .instantiate(&mut store, &module)
            .expect("instantiate")
            .start(&mut store)
            .expect("start");
        instances.insert(*start, inst);
    }

    // Snapshot: seeded registers, all integers.
    //
    // The snapshot lives at `REG_SNAPSHOT_BASE`, so it is indexed from there,
    // not from address zero.
    let snap_len = (slot_of.values().copied().max().unwrap() - REG_SNAPSHOT_BASE) as usize + 8;
    let mut snap = vec![0u8; snap_len];
    let mut put = |name: &str, v: u32| {
        let s = (slot_of[name] - REG_SNAPSHOT_BASE) as usize;
        snap[s..s + 4].copy_from_slice(&v.to_le_bytes());
    };
    put("ESP", ESP_SLOT);
    put("FS_OFFSET", TIB);
    // TIB[0] is the SEH head: address 0xFFFFFFFF, i.e. an empty chain that
    // still looks non-NULL, which is what the CRT checks for.
    let _ = &mut put;
    let seh = [0xFFu8; 4];
    // Jump tables need no seeding: each block runs against the real image, so
    // `jmp [ecx*4 + table]` reads the image's own entries.

    let mut path: Vec<u64> = Vec::new();
    let mut stack: Vec<u64> = Vec::new();
    let mut host_calls: BTreeMap<String, u32> = BTreeMap::new();
    let last_host = String::new();
    let mut visits: BTreeMap<u64, u32> = BTreeMap::new();
    let mut cur = entry;
    println!("entry {entry:#x}");
    for step in 0..max_steps {
        if step == 0 {
            let mut why: Vec<_> = failed.iter().filter(|(_, w)| w.as_str().starts_with("module:")).collect();
            for (a, w) in why.iter().take(6) {
                println!("  module build failed {a:#x}: {w}");
            }
            if why.len() == 1 {
                for (a, _) in &why {
                    if let Some(src) = sr_blocks.get(a) {
                        println!("next block -> {:#x} end {:#x}", src.start, src.end);
                    }
                }
            }
            println!("  {} module-build failures", why.len());
            why.clear();
        }
        let Some(inst) = instances.get(&cur) else {
            match failed.get(&cur) {
                Some(why) => println!("step {step}: {cur:#x} could not be run: {why}"),
                None => println!("step {step}: {cur:#x} is not a discovered block; stopping"),
            }
            break;
        };
        // Load the snapshot into the module's memory, run, and take it back.
        {
            let mem = inst.get_memory(&mut store, "memory").expect("memory");
            mem.write(&mut store, REG_SNAPSHOT_BASE as usize, &snap)
                .expect("snapshot write");
            if cur == entry {
                let _ = mem.write(&mut store, TIB as usize, &seh);
            }
        }
        if std::env::var("PROBE_FUEL").is_ok() {
            store.set_fuel(50_000_000).ok();
        }
        let f = inst.get_func(&mut store, "run").expect("run export");
        let f = f.typed::<(), ()>(&store).expect("() -> ()");
        let outcome = f.call(&mut store, ());
        // A block that traps has still done part of its work, and what it left
        // in guest memory is the evidence: read the snapshot back regardless of
        // the outcome rather than reporting the registers from before the run.
        {
            let data = inst
                .get_memory(&mut store, "memory")
                .expect("memory")
                .data(&store)
                .to_vec();
            let from = REG_SNAPSHOT_BASE as usize;
            let n = snap.len();
            snap.copy_from_slice(&data[from..from + n]);
        }
        let count = visits.entry(cur).or_insert(0);
        *count += 1;
        path.push(cur);
        if path.len() > 40 {
            path.remove(0);
        }
        let get = |name: &str| {
            let s = slot_of[name] as usize - REG_SNAPSHOT_BASE as usize;
            u32::from_le_bytes(snap[s..s + 4].try_into().unwrap())
        };
        if let Err(e) = outcome {
            let blk = &sr_blocks[&cur];
            let fuel = match store.get_fuel() {
                Ok(f) => f.to_string(),
                Err(e) => format!("{e}"),
            };
            println!("step {step}: {cur:#x} trapped: {e:?}; block {:#x}..{:#x}; fuel left {fuel}", blk.start, blk.end);
            if std::env::var("PROBE_WASM").is_ok() {
                std::fs::write("../../.scratch/probe_block.wasm", &compiled[&cur]).unwrap();
                println!("    wrote .scratch/probe_block.wasm ({} bytes)", compiled[&cur].len());
            }
            if let Some(w) = env_steps() {
                let _ = w;
            }
            for n in ["EAX", "ECX", "EDX", "ESP", "EIP"] {
                println!("    after: {n}={:#x}", get(n));
            }
            // A trap out of bounds is uninformative on its own: the address
            // that was out of range is the finding. Every memory op in the
            // block module is `offset=0` plus an `i32.const` address, so the
            // last constant before the failing op is a strong hint -- print it
            // alongside the block's own displacement constants so a wrong
            // address can be recognised as wrong rather than as "memory".
            let blk = &sr_blocks[&cur];
            println!("    block instructions:");
            for ins in &blk.instructions {
                let mem_addrs: Vec<u64> = ins
                    .ops
                    .iter()
                    .filter_map(|op| match op {
                        sir::IR::Store { addr: sir::Operand::Const { value, .. }, .. }
                        | sir::IR::Load { addr: sir::Operand::Const { value, .. }, .. } => Some(*value),
                        _ => None,
                    })
                    .collect();
                if !mem_addrs.is_empty() {
                    println!(
                        "      {:#x} {:<40} fixed addr {}",
                        ins.address,
                        ins.disassembly,
                        mem_addrs.iter().map(|a| format!("{a:#x}")).collect::<Vec<_>>().join(", ")
                    );
                }
            }
            print_window(&path, &visits, &sr_blocks, &snap, &slot_of);
            return;
        }
        let mut next = next_between(&sr_blocks[&cur], get("ZF"));
        match &sr_blocks[&cur].terminator {
            STerm::Call { resume } => {
                next = call_target(&sr_blocks[&cur]);
                stack.push(*resume);
            }
            STerm::CallInd { resume } => {
                match indirect.get(&cur).cloned().flatten() {
                    Some(IndirectCall::Host(api)) => {
                        // The probe has no host. Report the call and model the
                        // one thing a refusing host would not do: return.
                        println!(
                            "step {step}: {cur:#x} calls host {} (first time: {})",
                            api,
                            visits[&cur] == 1
                        );
                        host_calls.entry(api).or_insert(0);
                        if let Some(n) = host_calls.get_mut(&last_host) {
                            let _ = n;
                        }
                        next = Some(*resume);
                    }
                    Some(IndirectCall::Unknown(why)) => {
                        println!("step {step}: {cur:#x} indirect call to {why}; stopping");
                        print_window(&path, &visits, &sr_blocks, &snap, &slot_of);
                        return;
                    }
                    None => {
                        println!("step {step}: {cur:#x} is an indirect call the probe could not classify; stopping");
                        return;
                    }
                }
            }
            STerm::Return => {
                next = stack.pop();
                if next.is_none() {
                    println!("step {step}: {cur:#x} returns with an empty probe stack");
                    print_window(&path, &visits, &sr_blocks, &snap, &slot_of);
                    return;
                }
            }
            STerm::IndirectJump => {
                next = Some(get("EIP") as u64);
            }
            _ => {}
        }
        let Some(next) = next else {
            println!(
                "step {step}: {cur:#x} has no computable successor (EIP {:#x}); stopping",
                get("EIP")
            );
            print_window(&path, &visits, &sr_blocks, &snap, &slot_of);
            return;
        };
        if *visits.get(&next).unwrap_or(&0) >= 200 {
            println!(
                "step {step}: block {next:#x} reached {} times -- this is the hot block",
                visits[&next]
            );
            print_window(&path, &visits, &sr_blocks, &snap, &slot_of);
            return;
        }
        cur = next;
    }
    println!("stopped after {max_steps} steps at {cur:#x}");
    print_window(&path, &visits, &sr_blocks, &snap, &slot_of);
    report_host_calls(&host_calls);
}

fn print_window(
    path: &[u64],
    visits: &BTreeMap<u64, u32>,
    blocks: &BTreeMap<u64, SBlock>,
    snap: &[u8],
    slot_of: &BTreeMap<String, u64>,
) {
    println!("\nlast {} blocks:", path.len());
    for a in path {
        let blk = &blocks[a];
        println!(
            "  {a:#x} {:?} end={:#x} [visited {}x]",
            blk.terminator,
            blk.end,
            visits.get(a).copied().unwrap_or(0)
        );
    }
    for name in ["EAX", "EBX", "ECX", "EDX", "ESI", "EDI", "EBP", "ESP", "EIP"] {
        if let Some(&slot) = slot_of.get(name) {
            let s = slot as usize - REG_SNAPSHOT_BASE as usize;
            let v = u32::from_le_bytes(snap[s..s + 4].try_into().unwrap());
            println!("  {name}={v:#010x}");
        }
    }
}

/// The callee of a direct `call` in this block.
fn call_target(block: &SBlock) -> Option<u64> {
    block.instructions.iter().flat_map(|i| &i.ops).find_map(|op| match op {
        sir::IR::Call { target: sir::Operand::Ram { offset, .. } | sir::Operand::Const { value: offset, .. } } => Some(*offset),
        _ => None,
    })
}

/// Where an indirect call goes, recovered from the block it ended.
///
/// `call [slot]` is a load of `[slot]` followed by `CallInd` on the loaded
/// value. The probe resolves the load's address against the import table: a
/// verified IAT slot means the guest is calling a host API, which this probe
/// reports and then treats as "returned 0". A register target is not
/// resolvable statically, and is reported as such rather than guessed.
fn indirect_target(block: &SBlock, iat: &BTreeMap<u64, String>) -> Option<IndirectCall> {
    let mut loaded_from: Option<u64> = None;
    for ins in &block.instructions {
        for op in &ins.ops {
            match op {
                sir::IR::Load {
                    addr: sir::Operand::Const { value, .. },
                    ..
                } => loaded_from = Some(*value),
                sir::IR::Load { .. } => loaded_from = None,
                sir::IR::CallInd { target } => {
                    return Some(match target {
                        // `call [slot]`: the slot the load came from decides.
                        sir::Operand::Ram { offset, .. } => match iat.get(offset) {
                            Some(name) => IndirectCall::Host(name.clone()),
                            None => IndirectCall::Unknown(format!("ram {offset:#x} (not an import slot)")),
                        },
                        sir::Operand::Temp { .. } => match loaded_from {
                            Some(slot) => match iat.get(&slot) {
                                Some(name) => IndirectCall::Host(name.clone()),
                                None => IndirectCall::Unknown(format!("load [{slot:#x}], not an import slot")),
                            },
                            None => IndirectCall::Unknown("temp (value computed, not loaded from a slot)".into()),
                        },
                        sir::Operand::Reg { name, .. } => IndirectCall::Unknown(format!("register {name}")),
                        sir::Operand::Const { value, .. } => IndirectCall::Unknown(format!("const {value:#x}")),
                    });
                }
                _ => {}
            }
        }
    }
    None
}

#[derive(Clone)]
enum IndirectCall {
    Host(String),
    Unknown(String),
}

fn next_between(block: &SBlock, zf: u32) -> Option<u64> {
    use STerm as T;
    match &block.terminator {
        T::FallsThrough => Some(block.end),
        T::Jump(a) => Some(*a),
        T::Conditional { taken, not_taken } => Some(match taken {
            Some(t) if zf == 0 => *t,
            _ => *not_taken,
        }),
        T::IndirectJump | T::Return | T::Trap(_) | T::Call { .. } | T::CallInd { .. } | T::Undecodable(_) => None,
    }
}

fn env_steps() -> Option<usize> {
    None
}

fn report_host_calls(host_calls: &BTreeMap<String, u32>) {
    println!("\nhost calls the run reached:");
    for (api, n) in host_calls {
        println!("  {api} x{n}");
    }
}
