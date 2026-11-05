//! Emit the guest module with the PE image injected as active data segments and
//! the loader state seeded by the entry itself, so a plain wasm runtime
//! (`wasm-interp --trace`) reproduces exactly what the Rust harness runs.
//!
//! Written because tracing the *harness* path needs the image in memory: without
//! it the guest reads zeroed `.text` and diverges immediately, which is why an
//! unseeded wasm-interp run appeared to "succeed" while the harness spun.

use wlam_sleigh::emit as shared;

use std::collections::BTreeMap;
use wlam_emit::chain::{build_module_with_memory, pages_for_image, partition, HostImport};
use wlam_pe::Pe;
use wlam_sleigh::discover::discover;
use wlam_sleigh::with_big_stack;

fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().to_path_buf();
    let bytes = std::fs::read(root.join("north-star/bin/Uxn32.exe")).unwrap();
    let prog = { let b = bytes.clone(); with_big_stack(move || discover(&Pe::parse(&b).unwrap(), 64)) };
    let pe = Pe::parse(&bytes).unwrap();
    let base = pe.image_base as u64;
    let by_slot: BTreeMap<u64, HostImport> = pe.imports.iter().filter(|i| i.iat_slot_verified)
        .map(|i| (base + i.iat_rva as u64, HostImport {
            library: i.library.clone(),
            name: i.name.clone().unwrap_or_else(|| format!("#{}", i.ordinal.unwrap_or(0))),
        })).collect();
    let mut blocks = BTreeMap::new();
    for b in prog.blocks() {
        let rva = (b.start - base) as u32;
        let Some(idx) = pe.section_of(rva) else { continue };
        let sec = &pe.sections[idx];
        let off = (rva - sec.virtual_address) as usize;
        if off >= sec.data.len() { continue; }
        let slice = sec.data[off..].to_vec();
        let start = b.start;
        blocks.insert(b.start, shared::convert(&with_big_stack(move || wlam_sleigh::lift_block(&slice, start))));
    }
    let mut regs: Vec<String> = wlam_emit::seeded_registers();
    let mut used = std::collections::BTreeSet::new();
    for b in blocks.values() { for op in &b.ops { shared::collect_regs(op, &mut used); } }
    for r in used { if !regs.contains(&r) { regs.push(r); } }
    let pages = pages_for_image(base + pe.size_of_image as u64);
    let mut p = partition(&[base + pe.entry_point_rva as u64], &blocks, &regs);
    p.register_host_imports(&blocks, |slot| by_slot.get(&slot).cloned());
    let mut m = build_module_with_memory(&p, &blocks, pages).unwrap();

    // Graft a data section (id 11) holding the image, plus the loader seed, so
    // the module is self-contained. Sections must stay in ascending id order, so
    // the data section goes after CODE.
    let mut segments: Vec<(u32, Vec<u8>)> = Vec::new();
    let hdr_len = pe.size_of_headers.min(pe.raw.len() as u32) as usize;
    segments.push((base as u32, pe.raw[..hdr_len].to_vec()));
    for sec in &pe.sections {
        let n = (sec.file_size as usize).min(sec.data.len());
        if n > 0 {
            segments.push((base as u32 + sec.virtual_address, sec.data[..n].to_vec()));
        }
    }
    // Loader state: ESP, FS_OFFSET, empty SEH chain.
    let mut seed = Vec::new();
    for (idx, val) in [(4u32, 0x70000u32), (10, 0x60000)] {
        seed.extend_from_slice(&(wlam_emit::snapshot_address(idx as usize) as u32).to_le_bytes());
        seed.extend_from_slice(&val.to_le_bytes());
    }
    seed.extend_from_slice(&0x60000u32.to_le_bytes());
    seed.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    segments.push((0, seed));

    let mut data = Vec::new();
    data.extend_from_slice(&wlam_emit::leb_u32(segments.len() as u32));
    for (off, bytes) in &segments {
        data.push(0x00); // active, memory 0
        data.push(0x41); // i32.const
        data.extend_from_slice(&wlam_emit::leb_s32(*off as i32));
        data.push(0x0b); // end
        data.extend_from_slice(&wlam_emit::leb_u32(bytes.len() as u32));
        data.extend_from_slice(bytes);
    }
    m.push(11);
    m.extend_from_slice(&wlam_emit::leb_u32(data.len() as u32));
    m.extend_from_slice(&data);

    // The FIRST argument is the output module; the image is fixed (Uxn32).
    // Guard against pointing this at a guest binary: an earlier version took the
    // same argument and overwrote `benchmarks/minesweeper/bin/minesweeper-x86.exe`
    // with a wasm module. Refuse to write anything that looks like a PE.
    let out = std::env::args().nth(1).unwrap_or_else(|| "seeded.wasm".into());
    if out.ends_with(".exe") || out.ends_with(".dll") {
        eprintln!("refusing to write a wasm module over {out}: looks like a guest binary");
        std::process::exit(2);
    }
    std::fs::write(&out, &m).unwrap();
    // Map wasm function index -> guest entry, so a hot loop found in a trace can
    // be named.
    let import_count = p.import_index.len() as u32;
    for (i, func) in p.functions.iter().enumerate() {
        eprintln!("func {} entry={:#x} blocks={}", import_count + i as u32, func.entry, func.blocks.len());
    }
    eprintln!("wrote {} bytes to {out} with {} data segments", m.len(), segments.len());
}
