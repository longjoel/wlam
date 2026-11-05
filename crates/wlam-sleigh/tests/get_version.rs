//! `GetVersion` end-to-end: the value the host reports must reach the guest's
//! data segment, extracted the way the guest itself extracts it.
//!
//! A host-layer unit test can prove the dword is packed correctly. Only running
//! the real image proves the value survives the call, the return, and the
//! guest's own unpacking. Uxn32 does all three in the eight instructions after
//! its `GetVersion` call (0x40ccd8), storing the results at fixed addresses:
//!
//! ```text
//! 0x40cce2  mov [0x415b80], edx   ; major
//! 0x40ccf0  mov [0x415b7c], ecx   ; minor
//! 0x40ccfb  mov [0x415b78], ecx   ; derived platform tag
//! ```
//!
//! Those addresses are the observable interface: if the host returns the right
//! dword and the guest stores the right fields, the whole path is correct.

mod shared;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use wlam_emit::chain::{build_module_with_memory, partition, HostImport};
use wlam_emit::ir as eir;
use wlam_harness::{
    instantiate, load_image, run_entry_diagnosing, seed_register, Api, Host, IatPolicy, RunError,
};
use wlam_host::{kernel32, HOST_VERSION};
use wlam_pe::Pe;
use wlam_sleigh::discover::discover;
use wlam_sleigh::{lift_block, with_big_stack};

const MAJOR_SLOT: usize = 0x415b7c;
const MINOR_SLOT: usize = 0x415b80;
const TAG_SLOT: usize = 0x415b78;

/// Build Uxn32 with a memory covering its image, plus the page count.
fn build_uxn32() -> Option<(Vec<u8>, &'static str)> {
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
    let module = build_module_with_memory(&p, &blocks, pages).expect("module");
    Some((module, "north-star/bin/Uxn32.exe"))
}

fn loader_seed() -> Vec<(u32, u32)> {
    let mut seed = Vec::new();
    seed_register(&mut seed, 4, 0x70000); // ESP
    seed_register(&mut seed, 10, 0x60000); // FS_OFFSET -> TIB
    seed.push((0x60000, 0xFFFF_FFFF)); // empty SEH chain
    seed
}

/// Read a u32 out of the guest memory after the run.
fn peek(mem: &[u8], addr: usize) -> u32 {
    u32::from_le_bytes(mem[addr..addr + 4].try_into().unwrap())
}

/// Run the guest with the PE image actually mapped into its memory.
type Linked = (
    wlam_harness::wasmi::Store<()>,
    wlam_harness::wasmi::Instance,
);

fn run_with_image(module: &[u8], host: Arc<Mutex<Host>>) -> Result<Linked, String> {
    let (mut store, instance) = instantiate(module, Arc::clone(&host)).map_err(|e| e.to_string())?;
    let pe = Pe::parse(&std::fs::read("../../north-star/bin/Uxn32.exe").unwrap()).unwrap();
    let mem = instance.get_memory(&mut store, "memory").unwrap();
    let data = mem.data_mut(&mut store);
    load_image(data, &pe, IatPolicy::Poison).map_err(|e| e.to_string())?;
    Ok((store, instance))
}

#[test]
fn the_guest_stores_the_version_the_host_reported() {
    let Some((module, image)) = build_uxn32() else {
        return; // image not present
    };
    let mut h = Host::new();
    kernel32(&mut h);
    let host = Arc::new(Mutex::new(h));
    // The image must be mapped before the guest runs: its stores are absolute,
    // and on an unmapped image every observation reads zeros.
    let (mut store, instance) = run_with_image(&module, Arc::clone(&host)).expect("link + load");
    store.set_fuel(1_000_000).expect("fuel enabled");
    let result = run_entry_diagnosing(&mut store, &instance, &loader_seed(), &host);

    // The run must not be a GetVersion refusal any more.
    assert!(
        !matches!(&result, Err(RunError::Refused(d)) if d.api == Api::new("KERNEL32.dll", "GetVersion")),
        "GetVersion must be served ({image}); got {result:?}"
    );

    let mem = instance
        .get_memory(&mut store, "memory")
        .expect("guest memory")
        .data(&store);
    // Sanity: the image really is mapped. If this fails, every check below is
    // meaningless, so report it as itself rather than as a version mismatch.
    let text_head = peek(&mem, 0x401000);
    assert_ne!(text_head, 0, "the image's .text was not mapped into guest memory");

    let major = peek(&mem, MAJOR_SLOT);
    let minor = peek(&mem, MINOR_SLOT);
    let tag = peek(&mem, TAG_SLOT);
    assert_eq!(
        major, HOST_VERSION.major as u32,
        "guest stored the wrong major version (run: {result:?})"
    );
    assert_eq!(minor, HOST_VERSION.minor as u32, "wrong minor version");

    // The guest derives its platform tag as `(major << 8) + minor` (SHL ECX,8;
    // ADD ECX,EDX at 0x40ccf6). Assert the guest's own arithmetic rather than a
    // magic constant: with a 5.0 report the tag is 5, and any "NT" string
    // encoding would be a different computation entirely.
    let expected_tag = (major << 8).wrapping_add(minor);
    assert_eq!(
        tag, expected_tag,
        "the derived platform tag disagrees with the version fields it came from"
    );
}

#[test]
fn memory_covers_the_image_so_absolute_addresses_work() {
    // Without this the guest's first store to 0x415b80 traps out of bounds,
    // which is indistinguishable from a GetVersion that returned nothing.
    let Some((module, _)) = build_uxn32() else { return };
    use wlam_harness::instantiate as link;
    let host = Arc::new(Mutex::new(Host::new()));
    let (mut store, instance) = link(&module, host).expect("link");
    let mem = instance.get_memory(&mut store, "memory").expect("memory");
    let len = mem.data(&store).len();
    assert!(
        len > 0x415b80 + 4,
        "memory is {len:#x} bytes; the guest stores to 0x415b80"
    );
}
