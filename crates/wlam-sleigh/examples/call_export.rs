//! Execute one exported guest function under the host, with the register
//! snapshot seeded from a bundle's entry state.
//!
//! cargo run -p wlam-sleigh --example call_export -- image.exe bundle FUNCTION [fuel]
//!
//! This is a probe for "does this single function run?", which is far cheaper
//! to read than a whole-program trace when a specific lifted function is in
//! question. The bundle module exports every guest function as `g_<address>`.
use std::sync::{Arc, Mutex};
use wlam_harness::{instantiate, load_image, seed_register, Host, IatPolicy};

fn main() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() < 3 {
        return Err("usage: call_export image.exe bundle FUNCTION [fuel]".into());
    }
    let image = std::fs::read(&args[0]).map_err(|e| e.to_string())?;
    let pe = wlam_pe::Pe::parse(&image).map_err(|e| e.to_string())?;
    let module_bytes =
        std::fs::read(std::path::Path::new(&args[1]).join("guest.wasm")).map_err(|e| e.to_string())?;
    let fuel: u64 = args.get(3).and_then(|f| f.parse().ok()).unwrap_or(100_000);
    let mut host = Host::new();
    wlam_host::kernel32(&mut host);
    let host = Arc::new(Mutex::new(host));
    let (mut store, instance) = instantiate(&module_bytes, host.clone()).map_err(|e| e.to_string())?;
    store.set_fuel(fuel).map_err(|e| e.to_string())?;
    let memory = instance.get_memory(&store, "memory").ok_or("module has no guest memory")?;
    load_image(memory.data_mut(&mut store), &pe, IatPolicy::Poison).map_err(|e| e.to_string())?;

    // Entry state identical to `probe_image`, except that ESP points at a
    // parked sentinel so a `push ebp; mov ebp, esp` prologue has a return
    // address to pop from.
    let mut seed = Vec::new();
    seed_register(&mut seed, 4, 0x70000);
    seed_register(&mut seed, 10, 0x60000);
    seed.push((0x60000, u32::MAX));
    seed.push((0x70000 - 4, 0));
    for (addr, value) in &seed {
        memory
            .write(&mut store, *addr as usize, &value.to_le_bytes())
            .map_err(|e| e.to_string())?;
    }
    let func = instance.get_func(&store, &args[2]).ok_or_else(|| format!("no export {}", args[2]))?;
    let result = func.call(&mut store, &[], &mut []);
    println!("RESULT: {result:?}");
    let data = memory.data(&store);
    for (index, name) in wlam_emit::GUEST_LOCALS.iter().enumerate() {
        let at = wlam_emit::snapshot_address(index) as usize;
        println!("REGISTER: {name}={:#010x}", u32::from_le_bytes(data[at..at + 4].try_into().unwrap()));
    }
    let host = host.lock().unwrap_or_else(|e| e.into_inner());
    for (api, count) in host.call_counts().iter().filter(|(_, c)| **c > 0) {
        println!("REACHED: {api} {count}");
    }
    if let Some(d) = host.diagnostics().last() {
        println!("REFUSED: {d}");
    }
    Ok(())
}
