//! Run a compiled image and log every host call, so the sequence leading up to
//! a trap is visible.
//!
//! cargo run -p wlam-sleigh --example trace_run -- image.exe bundle-directory
//!
//! Each line shows the API, its argument pointer, and the EIP/ESP/EIP+esp read
//! from the register snapshot at the moment of the call. The snapshot holds
//! the most recently *spilled* state, not a live instruction pointer, so treat
//! the pair as "where execution last handed control to the host" rather than
//! as the exact call site. `ORG <addr>` markers name the function that most
//! recently handed a host import a guest address in `.text` range; they help
//! place a call inside the guest's own control flow.
use std::sync::{Arc, Mutex};

use wlam_harness::{load_image, run_entry_diagnosing, seed_register, Api, Host, IatPolicy};

fn register(data: &[u8], name: &str) -> u32 {
    let index = wlam_emit::GUEST_LOCALS
        .iter()
        .position(|n| *n == name)
        .expect("register exists");
    let at = wlam_emit::snapshot_address(index) as usize;
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}

/// `words` guest dwords starting at `address`, skipping anything unreadable.
fn read_u32(data: &[u8], address: u32, words: u32) -> Vec<u32> {
    (0..words)
        .filter_map(|i| {
            let at = (address as usize).checked_add(i as usize * 4)?;
            Some(u32::from_le_bytes(
                data.get(at..at + 4)?.try_into().unwrap(),
            ))
        })
        .collect()
}

/// Parse `WLAM_BP=0xADDR[,0xADDR...]`.
fn breakpoints() -> Vec<u32> {
    std::env::var("WLAM_BP")
        .unwrap_or_default()
        .split(',')
        .filter_map(|t| u32::from_str_radix(t.trim().trim_start_matches("0x"), 16).ok())
        .collect()
}

fn main() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() < 2 {
        return Err("usage: trace_run image.exe bundle-directory [--call FUNCTION]".into());
    }
    let image = std::fs::read(&args[0]).map_err(|e| e.to_string())?;
    let pe = wlam_pe::Pe::parse(&image).map_err(|e| e.to_string())?;
    let module_bytes = std::fs::read(std::path::Path::new(&args[1]).join("guest.wasm"))
        .map_err(|e| e.to_string())?;
    let mut host = Host::new();
    wlam_host::kernel32(&mut host);
    let host = Arc::new(Mutex::new(host));

    // Link with a tracing shim rather than `instantiate`, so every call is
    // logged before it is served.
    let mut config = wlam_harness::wasmi::Config::default();
    config.consume_fuel(true);
    let engine = wlam_harness::wasmi::Engine::new(&config);
    let module =
        wlam_harness::wasmi::Module::new(&engine, &module_bytes).map_err(|e| e.to_string())?;
    let mut store = wlam_harness::wasmi::Store::new(&engine, ());
    store.set_fuel(2_000_000).map_err(|e| e.to_string())?;
    let mut linker = wlam_harness::wasmi::Linker::<()>::new(&engine);
    let breakpoints = Arc::new(breakpoints());
    for imp in module.imports() {
        if !matches!(imp.ty(), wlam_harness::wasmi::ExternType::Func(_)) {
            continue;
        }
        let module_name = imp.module().to_string();
        let name = imp.name().to_string();
        let api = Api::new(module_name.clone(), name.clone());
        let host = Arc::clone(&host);
        let label = name.clone();
        let breakpoints = Arc::clone(&breakpoints);
        let func = wlam_harness::wasmi::Func::wrap(
            &mut store,
            move |mut caller: wlam_harness::wasmi::Caller<'_, ()>,
                  args_ptr: i32,
                  _ret: i32|
                  -> i32 {
                let memory = caller.get_export("memory").and_then(|e| e.into_memory());
                let mut h = host.lock().unwrap_or_else(|e| e.into_inner());
                match memory {
                    Some(mem) => {
                        let data = mem.data_mut(&mut caller);
                        let eip = register(data, "EIP");
                        let esp = register(data, "ESP");
                        // Read-only breakpoints: `WLAM_BP=0x43e2da[,0x...]` stops
                        // (and reports the guest stack) whenever a host call is
                        // made while EIP already holds a breakpoint address. The
                        // image is left untouched, so this cannot perturb a run.
                        if breakpoints.contains(&eip) {
                            let stack = read_u32(data, esp, 16);
                            let back = read_u32(data, esp.saturating_add(0x30), 16);
                            eprintln!("HIT {eip:#010x} {label} esp={esp:#010x}");
                            eprintln!("  from esp : {stack:08x?}");
                            eprintln!("  from +0x30: {back:08x?}");
                        }
                        eprintln!("CALL {label:<24} args={args_ptr:#010x} eip={eip:#010x} esp={esp:#010x}");
                        let mut guest = wlam_harness::GuestMemory::new(data);
                        h.resolve_with_memory(&api, args_ptr, &mut guest)
                    }
                    None => {
                        let mut guest = wlam_harness::GuestMemory::new(&mut []);
                        h.resolve_with_memory(&api, args_ptr, &mut guest)
                    }
                }
            },
        );
        linker
            .define(&module_name, &name, func)
            .map_err(|e| e.to_string())?;
    }
    let instance = linker
        .instantiate(&mut store, &module)
        .map_err(|e| e.to_string())?
        .start(&mut store)
        .map_err(|e| e.to_string())?;
    let memory = instance
        .get_memory(&store, "memory")
        .ok_or("module has no guest memory")?;
    load_image(memory.data_mut(&mut store), &pe, IatPolicy::Poison).map_err(|e| e.to_string())?;
    let mut seed = Vec::new();
    seed_register(&mut seed, 4, 0x70000);
    seed_register(&mut seed, 10, 0x60000);
    seed.push((0x60000, u32::MAX));
    // `--call FUNCTION` runs one exported guest function instead of the entry
    // point, which isolates a function from the rest of the program.
    let call = std::env::args().collect::<Vec<_>>();
    let named = call
        .iter()
        .position(|a| a == "--call")
        .and_then(|i| call.get(i + 1));
    let outcome = match named {
        Some(function) => {
            for (addr, value) in &seed {
                memory
                    .write(&mut store, *addr as usize, &value.to_le_bytes())
                    .map_err(|e| e.to_string())?;
            }
            let func = instance
                .get_func(&store, function)
                .ok_or("no such export")?;
            match func.call(&mut store, &[], &mut []) {
                Ok(()) => Ok(()),
                Err(e) => Err(wlam_harness::RunError::Trap(format!("{e:?}"))),
            }
        }
        None => run_entry_diagnosing(&mut store, &instance, &seed, &host),
    };
    println!("OUTCOME: {outcome:?}");
    if std::env::var("WLAM_TABLE").is_ok() {
        // Diagnostic: which function does each table slot really hold?
        if let Some(table) = instance.get_table(&store, "table") {
            println!("TABLE size {:?}", table.size(&store));
            for slot in [0u32, 1, 2, 10, 20, 30, 40, 50, 60, 64] {
                println!("TABLE[{slot}] = {:?}", table.get(&store, slot));
            }
        }
    }
    let host = host.lock().unwrap_or_else(|error| error.into_inner());
    for (api, count) in host.call_counts().iter().filter(|(_, count)| **count > 0) {
        println!("COUNT: {api} {count}");
    }
    let data = memory.data(&store);
    for (index, name) in wlam_emit::GUEST_LOCALS.iter().enumerate() {
        let at = wlam_emit::snapshot_address(index) as usize;
        if let Some(bytes) = data.get(at..at + 4) {
            println!(
                "REGISTER: {name}={:#010x}",
                u32::from_le_bytes(bytes.try_into().unwrap())
            );
        }
    }
    Ok(())
}
