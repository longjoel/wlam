//! Shared PE-to-module compilation for CLI bundles and execution harnesses.
use std::collections::{BTreeMap,BTreeSet};
use wlam_emit::chain::{partition_with_indirect_targets,build_module_with_memory,HostImport};
use wlam_pe::Pe;
#[derive(Debug)]
pub struct Segment { pub address:u32, pub bytes:Vec<u8>, pub mapped_size:u32, pub executable:bool }
#[derive(Debug)]
pub struct Compilation {
    pub module:Vec<u8>, pub segments:Vec<Segment>, pub registers:Vec<String>,
    pub callbacks:Vec<u32>, pub imports:Vec<HostImport>, pub image_base:u32,
    pub iat:Vec<(u32,HostImport)>,
    pub resources:Vec<wlam_pe::ResourceEntry>,
}
fn initialized_code_pointers(pe:&Pe)->Vec<u64> {
    let base=pe.image_base;
    let mut pointers=BTreeSet::new();
    for section in &pe.sections {
        // Read-only vtables in .rdata and writable callback tables both hold
        // legitimate indirect entry points. Exclude executable bytes (which
        // contain many coincidental integer values) and the IAT (whose
        // addresses are handled as imports separately).
        if section.is_executable()||section.name==".idata" {continue;}
        for word in section.data.chunks_exact(4) {
            let address=u32::from_le_bytes(word.try_into().unwrap()) as u64;
            let Some(rva)=address.checked_sub(base).and_then(|v|u32::try_from(v).ok()) else {continue;};
            if pe.section_of(rva).is_some_and(|index|pe.sections[index].is_executable()) {pointers.insert(address);}
        }
    }
    pointers.into_iter().collect()
}
pub fn compile(bytes:&[u8])->Result<Compilation,String> {
    compile_with_seeds(bytes,&[])
}
/// Compile a PE while treating the supplied virtual addresses as additional
/// code roots. This is useful for targets whose callback or exception paths
/// expose an entry point only at runtime.
pub fn compile_with_seeds(bytes:&[u8],extra_seeds:&[u64])->Result<Compilation,String> {
    let pe=Pe::parse(bytes).map_err(|e|e.to_string())?;
    let mut pointer_seeds=initialized_code_pointers(&pe);
    pointer_seeds.extend(extra_seeds.iter().copied().filter(|address| {
        address.checked_sub(pe.image_base).and_then(|rva|u32::try_from(rva).ok())
            .and_then(|rva|pe.section_of(rva).map(|index|(rva,index)))
            .is_some_and(|(rva,index)|pe.sections[index].is_executable()&&
                rva-pe.sections[index].virtual_address < pe.sections[index].data.len() as u32)
    }));
    pointer_seeds.sort_unstable();
    pointer_seeds.dedup();
    let mut blocks=BTreeMap::new();
    // Import-heavy Win32 programs can have long chains of indirect callback
    // continuations. Each pass discovers a frontier; IDEAS needs 272 passes.
    // Keep a bounded margin while reporting the exact frontier if another
    // image does not converge.
    let discovery=crate::discover::discover_observing_with_seeds(&pe,512,&pointer_seeds,|block| {
        blocks.insert(block.start,crate::emit::convert(block));
    });
    if discovery.refused() {
        return Err(format!("discovery refused image: {:?}", discovery.problems()));
    }
    if discovery.incomplete() {
        let pending = discovery.pending().iter().take(16).collect::<Vec<_>>();
        return Err(format!(
            "discovery incomplete after {} passes ({} pending work items; first 16: {:?})",
            discovery.passes(),
            discovery.pending().len(),
            pending
        ));
    }
    let base=u32::try_from(pe.image_base).map_err(|_|"image base exceeds wasm32")?;
    let mut registers=wlam_emit::seeded_registers(); let mut used=BTreeSet::new();
    for block in blocks.values() {for op in &block.ops {crate::emit::collect_regs(op,&mut used);}}
    for name in used {if !registers.contains(&name) {registers.push(name);}}
    let entry=base as u64+pe.entry_point_rva as u64;
    let mut entries=vec![entry];
    for address in discovery.functions() {
        if address != entry && blocks.contains_key(&address) { entries.push(address); }
    }
    for address in &pointer_seeds {if *address!=entry&&blocks.contains_key(address)&&!entries.contains(address){entries.push(*address);}}
    for export in &pe.exports {
        if export.section.and_then(|i|pe.sections.get(i)).is_some_and(|s|s.is_executable()) {
            let address=base as u64+export.rva as u64;
            if blocks.contains_key(&address) && !entries.contains(&address) {entries.push(address);}
        }
    }
    let imports:BTreeMap<_,_>=pe.imports.iter().filter(|i|i.iat_slot_verified).map(|i| (base as u64+i.iat_rva as u64,HostImport {
        library:i.library.clone(),name:i.name.clone().unwrap_or_else(||format!("#{}",i.ordinal.unwrap_or(0)))
    })).collect();
    let mut program=partition_with_indirect_targets(&entries,&blocks,&registers,discovery.indirect_targets());
    program.image_base=base;
    program.host_imports=imports.clone();
    program.register_host_imports(&blocks,|slot|imports.get(&slot).cloned());
    let module=build_module_with_memory(&program,&blocks,wlam_emit::chain::pages_for_image(base as u64+pe.size_of_image as u64)).map_err(|e|e.to_string())?;
    if let Err(error) = wasmparser::Validator::new().validate_all(&module) {
        if let Ok(path) = std::env::var("WLAM_INVALID_WASM") {
            std::fs::write(path, &module).map_err(|e|e.to_string())?;
        }
        return Err(format!("emitted invalid Wasm: {error}"));
    }
    let mut segments=vec![Segment {address:base,bytes:bytes[..(pe.size_of_headers as usize).min(bytes.len())].to_vec(),mapped_size:pe.size_of_headers,executable:false}];
    for section in &pe.sections {
        segments.push(Segment {address:base.checked_add(section.virtual_address).ok_or("section address overflow")?,bytes:section.data.clone(),mapped_size:section.virtual_size.max(section.file_size),executable:section.is_executable()});
    }
    Ok(Compilation {resources:pe.resources.entries.clone(),iat:program.iat_imports.iter().map(|(a,i)|(*a as u32,i.clone())).collect(),module,segments,registers,callbacks:program.function_index.keys().map(|a|*a as u32).collect(),imports:program.import_index.keys().cloned().collect(),image_base:base})
}
/// JSON string encoding, including untrusted PE import names.
fn quoted(text:&str)->String {
    let mut result=String::from("\"");
    for c in text.chars() {match c {'"'=>result.push_str("\\\""),'\\'=>result.push_str("\\\\"),c if c<' '=>result.push_str(&format!("\\u{:04x}",c as u32)),c=>result.push(c)}}
    result.push('"');result
}
impl Compilation {
    /// A manifest and packed segment payload. Offsets refer to image.bin.
    pub fn bundle_image(&self)->(String,Vec<u8>) {
        let mut image=Vec::new(); let mut segments=Vec::new();
        for segment in &self.segments {segments.push(format!("{{\"address\":{},\"offset\":{},\"length\":{},\"mappedSize\":{},\"executable\":{}}}",segment.address,image.len(),segment.bytes.len(),segment.mapped_size,segment.executable));image.extend_from_slice(&segment.bytes);}
        let registers=self.registers.iter().enumerate().map(|(i,n)|format!("{{\"name\":{},\"address\":{}}}",quoted(n),wlam_emit::snapshot_address(i))).collect::<Vec<_>>().join(",");
        let callbacks=self.callbacks.iter().map(|v|v.to_string()).collect::<Vec<_>>().join(",");
        let resources=self.resources.iter().map(|r| {
            let name=match &r.name {wlam_pe::ResourceName::Id(id)=>id.to_string(),wlam_pe::ResourceName::Name(name)=>quoted(name)};
            format!("{{\"type\":{},\"name\":{},\"language\":{},\"address\":{},\"size\":{}}}",r.type_id,name,r.lang,self.image_base+r.data_rva,r.size)
        }).collect::<Vec<_>>().join(",");
        let imports=self.imports.iter().map(|i| {
            let pop=wlam_pcode_signature(&i.library,&i.name);
            let slots=self.iat.iter().filter(|(_,import)|import==i).map(|(slot,_)|slot.to_string()).collect::<Vec<_>>().join(",");
            format!("{{\"library\":{},\"name\":{},\"returnPop\":{},\"slots\":[{}]}}",quoted(&i.library),quoted(&i.name),pop,slots)
        }).collect::<Vec<_>>().join(",");
        (format!("{{\"version\":1,\"module\":\"guest.wasm\",\"image\":\"image.bin\",\"imageBase\":{},\"stack\":458752,\"tib\":393216,\"segments\":[{}],\"registers\":[{}],\"callbacks\":[{}],\"imports\":[{}],\"resources\":[{}]}}",self.image_base,segments.join(","),registers,callbacks,imports,resources),image)
    }
}
fn wlam_pcode_signature(dll:&str,name:&str)->String {
    wlam_pcode::abi::signature(dll,name).map(|s|s.return_pop().to_string()).unwrap_or_else(||"null".into())
}
