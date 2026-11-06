//! Shared PE-to-module compilation for CLI bundles and execution harnesses.
use std::collections::{BTreeMap, BTreeSet};
use wlam_emit::chain::{build_module_with_memory, partition_with_indirect_targets, HostImport};
use wlam_pe::Pe;
#[derive(Debug)]
pub struct Segment {
    pub address: u32,
    pub bytes: Vec<u8>,
    pub mapped_size: u32,
    pub executable: bool,
}
/// A supplied DLL linked into the same guest module.
#[derive(Debug, Clone)]
pub struct DllInfo {
    pub name: String,
    pub base: u32,
    pub entry: u32,
}
#[derive(Debug)]
pub struct Compilation {
    pub module: Vec<u8>,
    pub segments: Vec<Segment>,
    pub registers: Vec<String>,
    pub callbacks: Vec<u32>,
    pub imports: Vec<HostImport>,
    pub image_base: u32,
    pub iat: Vec<(u32, HostImport)>,
    pub resources: Vec<wlam_pe::ResourceEntry>,
    /// DLLs that contribute code to this module, in initialization order.
    pub dlls: Vec<DllInfo>,
}

/// One parsed image in a multi-image guest. Addresses are absolute.
struct ModuleImage {
    name: String,
    pe: Pe,
    base: u64,
    entry: u64,
}

impl ModuleImage {
    fn end(&self) -> u64 {
        self.base + self.pe.size_of_image as u64
    }
}

#[derive(Debug, Clone, Copy)]
struct ExportSite {
    address: u64,
    executable: bool,
}

/// Name and ordinal views of one DLL's export table. MFC30 and other
/// 16-bit-era DLLs export by ordinal only, so name lookup alone is not enough.
#[derive(Debug, Default)]
struct ExportMap {
    by_name: BTreeMap<String, ExportSite>,
    by_ordinal: BTreeMap<u32, ExportSite>,
}

impl ExportMap {
    fn new(pe: &Pe) -> Self {
        let mut map = Self::default();
        for export in &pe.exports {
            let Some(section) = export.section.and_then(|index| pe.sections.get(index)) else {
                continue;
            };
            let site = ExportSite {
                address: pe.image_base + export.rva as u64,
                executable: section.is_executable(),
            };
            map.by_ordinal.insert(export.ordinal, site);
            if let Some(name) = &export.name {
                map.by_name.insert(name.clone(), site);
            }
        }
        map
    }
    fn resolve(&self, name: Option<&str>, ordinal: Option<u16>) -> Option<ExportSite> {
        if let Some(name) = name {
            self.by_name.get(name).copied()
        } else {
            ordinal.and_then(|ordinal| self.by_ordinal.get(&(ordinal as u32)).copied())
        }
    }
}

fn library_stem(name: &str) -> String {
    let base = name
        .rsplit(|c| c == '\\' || c == '/')
        .next()
        .unwrap_or(name);
    let lower = base.to_ascii_lowercase();
    lower.strip_suffix(".dll").unwrap_or(&lower).to_string()
}

/// Libraries the host provides. Even if the user supplies a copy, it is not
/// linked as guest code: the host layer is the implementation.
fn is_system_library(stem: &str) -> bool {
    matches!(
        stem,
        "kernel32"
            | "kernelbase"
            | "user32"
            | "gdi32"
            | "advapi32"
            | "shell32"
            | "comctl32"
            | "comdlg32"
            | "winmm"
            | "shlwapi"
            | "ole32"
            | "oleaut32"
            | "version"
            | "ws2_32"
            | "winspool"
            | "setupapi"
            | "rpcrt4"
            | "uxtheme"
            | "msimg32"
            | "imm32"
            | "dbghelp"
            | "psapi"
            | "crypt32"
            | "secur32"
            | "iphlpapi"
            | "mpr"
            | "netapi32"
            | "wininet"
            | "urlmon"
            | "opengl32"
            | "glu32"
            | "msvcrt"
            | "ucrtbase"
    ) || stem.starts_with("msvcr")
        || stem.starts_with("vcruntime")
        || stem.starts_with("api-ms-win-")
}

/// Patch a 32-bit IAT slot in the importing image.
fn patch_iat(pe: &mut Pe, rva: u32, value: u64) -> Result<(), String> {
    let value =
        u32::try_from(value).map_err(|_| format!("import address {value:#x} exceeds wasm32"))?;
    let index = pe
        .section_of(rva)
        .ok_or_else(|| format!("IAT slot {rva:#x} is not in a section"))?;
    let section = &mut pe.sections[index];
    let offset = (rva - section.virtual_address) as usize;
    let slot = section
        .data
        .get_mut(offset..offset + 4)
        .ok_or_else(|| format!("IAT slot {rva:#x} is outside section data"))?;
    slot.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn initialized_code_pointers(pe: &Pe) -> Vec<u64> {
    let base = pe.image_base;
    let mut pointers = BTreeSet::new();
    for section in &pe.sections {
        // Read-only vtables in .rdata and writable callback tables both hold
        // legitimate indirect entry points. Exclude executable bytes (which
        // contain many coincidental integer values) and the IAT (whose
        // addresses are handled as imports separately).
        if section.is_executable() || section.name == ".idata" {
            continue;
        }
        for word in section.data.chunks_exact(4) {
            let address = u32::from_le_bytes(word.try_into().unwrap()) as u64;
            let Some(rva) = address
                .checked_sub(base)
                .and_then(|v| u32::try_from(v).ok())
            else {
                continue;
            };
            if pe
                .section_of(rva)
                .is_some_and(|index| pe.sections[index].is_executable())
            {
                pointers.insert(address);
            }
        }
    }
    pointers.into_iter().collect()
}
pub fn compile(bytes: &[u8]) -> Result<Compilation, String> {
    compile_with_seeds(bytes, &[])
}
/// Compile a PE while treating the supplied virtual addresses as additional
/// code roots. This is useful for targets whose callback or exception paths
/// expose an entry point only at runtime.
pub fn compile_with_seeds(bytes: &[u8], extra_seeds: &[u64]) -> Result<Compilation, String> {
    compile_with_dlls(bytes, &[], extra_seeds)
}
/// Compile a main image plus user-supplied DLLs into one guest module.
///
/// Imports satisfied by a supplied DLL are patched to the DLL export's guest
/// address and the DLL's code is discovered and emitted into the same wasm
/// module. Everything else remains a host import.
pub fn compile_with_dlls(
    bytes: &[u8],
    dlls: &[(String, Vec<u8>)],
    extra_seeds: &[u64],
) -> Result<Compilation, String> {
    let main = Pe::parse(bytes).map_err(|e| e.to_string())?;
    let main_base = u32::try_from(main.image_base).map_err(|_| "image base exceeds wasm32")?;
    let main_entry = main_base as u64 + main.entry_point_rva as u64;
    let mut dll_images = Vec::new();
    let mut supplied = BTreeMap::new();
    for (name, raw) in dlls {
        let stem = library_stem(name);
        if is_system_library(&stem) {
            continue;
        }
        let pe = Pe::parse(raw).map_err(|e| format!("{name}: {e}"))?;
        if pe.image_base == 0 {
            return Err(format!(
                "{name}: preferred image base is zero; rebasing is not implemented"
            ));
        }
        if supplied.contains_key(&stem) {
            return Err(format!("duplicate supplied DLL '{stem}'"));
        }
        let base = u32::try_from(pe.image_base)
            .map_err(|_| format!("{name}: image base exceeds wasm32"))?;
        let entry = base as u64 + pe.entry_point_rva as u64;
        supplied.insert(stem, dll_images.len());
        dll_images.push(ModuleImage {
            name: name.clone(),
            pe,
            base: base as u64,
            entry,
        });
    }
    let mut images = vec![ModuleImage {
        name: "<main>".into(),
        pe: main,
        base: main_base as u64,
        entry: main_entry,
    }];
    images.extend(dll_images);

    // Include only supplied DLLs reachable from the main image or another
    // included DLL. A supplied file nobody imports must not enlarge the module.
    let mut needed = BTreeSet::new();
    let mut changed = true;
    while changed {
        changed = false;
        for index in 0..images.len() {
            if index != 0 && !needed.contains(&index) {
                continue;
            }
            for import in &images[index].pe.imports {
                if let Some(&target) = supplied.get(&library_stem(&import.library)) {
                    // `supplied` indexes `dll_images`; images are offset by one.
                    if needed.insert(target + 1) {
                        changed = true;
                    }
                }
            }
        }
    }
    let mut modules = vec![images.remove(0)];
    let mut module_by_stem = BTreeMap::new();
    for (index, image) in images.into_iter().enumerate() {
        if !needed.contains(&(index + 1)) {
            continue;
        }
        module_by_stem.insert(library_stem(&image.name), modules.len());
        modules.push(image);
    }
    for index in 0..modules.len() {
        for other in index + 1..modules.len() {
            let (a, b) = (&modules[index], &modules[other]);
            if a.base < b.end() && b.base < a.end() {
                return Err(format!("image ranges overlap: {:#x}..{:#x} and {:#x}..{:#x}; rebasing is not implemented",a.base,a.end(),b.base,b.end()));
            }
        }
    }

    let exports: Vec<Option<ExportMap>> = modules
        .iter()
        .map(|module| (module.name != "<main>").then(|| ExportMap::new(&module.pe)))
        .collect();
    let module_names: Vec<String> = modules.iter().map(|module| module.name.clone()).collect();
    let mut host_imports = BTreeMap::new();
    let mut cross_seeds = BTreeSet::new();
    for module in &mut modules {
        for import_index in 0..module.pe.imports.len() {
            let import = module.pe.imports[import_index].clone();
            if !import.iat_slot_verified {
                continue;
            }
            let Some(&target) = module_by_stem.get(&library_stem(&import.library)) else {
                host_imports.insert(
                    module.base + import.iat_rva as u64,
                    HostImport {
                        library: import.library.clone(),
                        name: import
                            .name
                            .clone()
                            .unwrap_or_else(|| format!("#{}", import.ordinal.unwrap_or(0))),
                    },
                );
                continue;
            };
            let Some(site) = exports[target]
                .as_ref()
                .and_then(|map| map.resolve(import.name.as_deref(), import.ordinal))
            else {
                return Err(format!(
                    "{}: {} is not exported by supplied DLL {}",
                    import.library,
                    import
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("#{}", import.ordinal.unwrap_or(0))),
                    module_names[target]
                ));
            };
            patch_iat(&mut module.pe, import.iat_rva, site.address)?;
            if site.executable {
                cross_seeds.insert(site.address);
            }
        }
    }

    let mut blocks = BTreeMap::new();
    let mut indirect_targets = BTreeMap::new();
    let mut entry_set = BTreeSet::new();
    entry_set.insert(main_entry);
    for (index, module) in modules.iter().enumerate() {
        let mut seeds = initialized_code_pointers(&module.pe);
        seeds.extend(
            extra_seeds
                .iter()
                .copied()
                .filter(|address| *address >= module.base && *address < module.end()),
        );
        if let Some(map) = &exports[index] {
            seeds.extend(
                map.by_name
                    .values()
                    .chain(map.by_ordinal.values())
                    .filter(|site| site.executable)
                    .map(|site| site.address),
            );
        }
        if module.name != "<main>" && module.entry != 0 {
            seeds.push(module.entry);
        }
        seeds.sort_unstable();
        seeds.dedup();
        for seed in &seeds {
            if *seed >= module.base && *seed < module.end() {
                entry_set.insert(*seed);
            }
        }
        let discovery =
            crate::discover::discover_observing_with_seeds(&module.pe, 512, &seeds, |block| {
                blocks.insert(block.start, crate::emit::convert(block));
            });
        if discovery.refused() {
            return Err(format!(
                "{}: discovery refused image: {:?}",
                module.name,
                discovery.problems()
            ));
        }
        if discovery.incomplete() {
            let pending = discovery.pending().iter().take(16).collect::<Vec<_>>();
            return Err(format!(
                "{}: discovery incomplete after {} passes ({} pending work items; first 16: {:?})",
                module.name,
                discovery.passes(),
                discovery.pending().len(),
                pending
            ));
        }
        for address in discovery.functions() {
            if blocks.contains_key(&address) {
                entry_set.insert(address);
            }
        }
        indirect_targets.extend(
            discovery
                .indirect_targets()
                .iter()
                .map(|(site, targets)| (*site, targets.clone())),
        );
    }
    for seed in cross_seeds {
        if blocks.contains_key(&seed) {
            entry_set.insert(seed);
        }
    }

    let mut registers = wlam_emit::seeded_registers();
    let mut used = BTreeSet::new();
    for block in blocks.values() {
        for op in &block.ops {
            crate::emit::collect_regs(op, &mut used);
        }
    }
    for name in used {
        if !registers.contains(&name) {
            registers.push(name);
        }
    }
    let mut entries = vec![main_entry];
    for address in entry_set {
        if address != main_entry && blocks.contains_key(&address) && !entries.contains(&address) {
            entries.push(address);
        }
    }
    let mut program =
        partition_with_indirect_targets(&entries, &blocks, &registers, &indirect_targets);
    program.image_base = main_base;
    program.host_imports = host_imports.clone();
    program.register_host_imports(&blocks, |slot| host_imports.get(&slot).cloned());
    let image_end = modules
        .iter()
        .map(ModuleImage::end)
        .max()
        .unwrap_or(main_base as u64 + 1);
    let module = build_module_with_memory(
        &program,
        &blocks,
        wlam_emit::chain::pages_for_image(image_end),
    )
    .map_err(|e| e.to_string())?;
    if let Err(error) = wasmparser::Validator::new().validate_all(&module) {
        if let Ok(path) = std::env::var("WLAM_INVALID_WASM") {
            std::fs::write(path, &module).map_err(|e| e.to_string())?;
        }
        return Err(format!("emitted invalid Wasm: {error}"));
    }

    let mut segments = Vec::new();
    let mut resources = Vec::new();
    let mut dlls_out = Vec::new();
    for module in &modules {
        let base = module.base;
        segments.push(Segment {
            address: u32::try_from(base).map_err(|_| "segment address exceeds wasm32")?,
            bytes: module.pe.raw[..(module.pe.size_of_headers as usize).min(module.pe.raw.len())]
                .to_vec(),
            mapped_size: module.pe.size_of_headers,
            executable: false,
        });
        for section in &module.pe.sections {
            segments.push(Segment {
                address: u32::try_from(base + section.virtual_address as u64)
                    .map_err(|_| "section address exceeds wasm32")?,
                bytes: section.data.clone(),
                mapped_size: section.virtual_size.max(section.file_size),
                executable: section.is_executable(),
            });
        }
        for resource in &module.pe.resources.entries {
            let mut resource = resource.clone();
            resource.data_rva = u32::try_from(base + resource.data_rva as u64)
                .map_err(|_| "resource address exceeds wasm32")?;
            resources.push(resource);
        }
        if module.name != "<main>" {
            dlls_out.push(DllInfo {
                name: module.name.clone(),
                base: u32::try_from(base).map_err(|_| "DLL base exceeds wasm32")?,
                entry: u32::try_from(module.entry).unwrap_or(0),
            });
        }
    }
    Ok(Compilation {
        resources,
        iat: program
            .iat_imports
            .iter()
            .map(|(a, i)| (*a as u32, i.clone()))
            .collect(),
        module,
        segments,
        registers,
        callbacks: program.function_index.keys().map(|a| *a as u32).collect(),
        imports: program.import_index.keys().cloned().collect(),
        image_base: main_base,
        dlls: dlls_out,
    })
}
/// JSON string encoding, including untrusted PE import names.
fn quoted(text: &str) -> String {
    let mut result = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            c if c < ' ' => result.push_str(&format!("\\u{:04x}", c as u32)),
            c => result.push(c),
        }
    }
    result.push('"');
    result
}
impl Compilation {
    /// A manifest and packed segment payload. Offsets refer to image.bin.
    pub fn bundle_image(&self) -> (String, Vec<u8>) {
        let mut image = Vec::new();
        let mut segments = Vec::new();
        for segment in &self.segments {
            segments.push(format!("{{\"address\":{},\"offset\":{},\"length\":{},\"mappedSize\":{},\"executable\":{}}}",segment.address,image.len(),segment.bytes.len(),segment.mapped_size,segment.executable));
            image.extend_from_slice(&segment.bytes);
        }
        let registers = self
            .registers
            .iter()
            .enumerate()
            .map(|(i, n)| {
                format!(
                    "{{\"name\":{},\"address\":{}}}",
                    quoted(n),
                    wlam_emit::snapshot_address(i)
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let callbacks = self
            .callbacks
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let resources = self
            .resources
            .iter()
            .map(|r| {
                let name = match &r.name {
                    wlam_pe::ResourceName::Id(id) => id.to_string(),
                    wlam_pe::ResourceName::Name(name) => quoted(name),
                };
                format!(
                    "{{\"type\":{},\"name\":{},\"language\":{},\"address\":{},\"size\":{}}}",
                    r.type_id, name, r.lang, r.data_rva, r.size
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let dlls = self
            .dlls
            .iter()
            .map(|dll| {
                format!(
                    "{{\"name\":{},\"base\":{},\"entry\":{}}}",
                    quoted(&dll.name),
                    dll.base,
                    dll.entry
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let imports=self.imports.iter().map(|i| {
            let (pop,argument_bytes)=match wlam_pcode::abi::signature(&i.library,&i.name) {
                Some(signature)=>(signature.return_pop().to_string(),signature.argument_bytes.to_string()),
                // Unknown conventions stay explicit; the host refuses them.
                None=>("null".into(),"0".into()),
            };
            let slots=self.iat.iter().filter(|(_,import)|import==i).map(|(slot,_)|slot.to_string()).collect::<Vec<_>>().join(",");
            format!("{{\"library\":{},\"name\":{},\"returnPop\":{},\"argumentBytes\":{},\"slots\":[{}]}}",quoted(&i.library),quoted(&i.name),pop,argument_bytes,slots)
        }).collect::<Vec<_>>().join(",");
        (format!("{{\"version\":1,\"module\":\"guest.wasm\",\"image\":\"image.bin\",\"imageBase\":{},\"stack\":458752,\"tib\":393216,\"segments\":[{}],\"registers\":[{}],\"callbacks\":[{}],\"imports\":[{}],\"resources\":[{}],\"dlls\":[{}]}}",self.image_base,segments.join(","),registers,callbacks,imports,resources,dlls),image)
    }
}
