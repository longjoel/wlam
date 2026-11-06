//! Trace actual NE startup up to the first imported API or unsupported operation.
//! Import gates deliberately stop execution; they do not fake API results.
use rsleigh_api::{Architecture, Decoder};
use std::{collections::BTreeMap, env, fs};
use wlam_ne::{NeFarAddress, NeImage, NeMachine, NeTaskStartContext};

fn trace() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let path = env::args()
        .nth(1)
        .ok_or("usage: trace_ne <image.exe> [step-limit]")?;
    let limit: usize = env::args()
        .nth(2)
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(10000);
    let raw = fs::read(path)?;
    let image = NeImage::parse(&raw)?;
    let mut loaded = image.load_segments(&raw, 0x100)?;
    let gate_selector = loaded.add_region_auto(65536, false, true)?;
    let mut gates = BTreeMap::new();
    for segment in &image.segments {
        for r in &segment.relocations {
            if matches!(r.flags & 3, 1 | 2) {
                let next = u16::try_from(gates.len())?;
                gates
                    .entry((r.flags & 3, r.target1, r.target2))
                    .or_insert(next);
            }
        }
    }
    loaded.apply_relocations(&image, |r| {
        gates
            .get(&(r.flags & 3, r.target1, r.target2))
            .map(|&offset| NeFarAddress {
                selector: gate_selector,
                offset,
            })
    })?;
    let psp = loaded.add_region_auto(256, true, false)?;
    let context = NeTaskStartContext {
        psp_selector: psp,
        instance: loaded
            .selector_for_segment(image.auto_data_segment)
            .ok_or("no automatic data segment")?,
        previous_instance: 0,
        parent_psp: 0,
        environment_selector: 0,
        dos_dispatcher: NeFarAddress {
            selector: gate_selector,
            offset: 65535,
        },
        command_line: Vec::new(),
    };
    NeMachine::initialize_task_psp(&mut loaded, &context)?;
    let mut machine = NeMachine::new(&image, &loaded)?;
    machine.initialize_task_start(&image, &loaded, &context)?;
    let mut decoder = Decoder::new(Architecture::X86_16Protected);
    println!(
        "{} segments, {} resources, {} imported exports",
        image.segments.len(),
        image.resources.len(),
        gates.len()
    );
    for step in 0..limit {
        if machine.cs() == gate_selector {
            if let Some((&(kind, module, symbol), _)) =
                gates.iter().find(|(_, offset)| **offset == machine.ip())
            {
                let module = image.module_reference(module).ok_or("invalid module")?;
                let symbol = if kind == 1 {
                    format!("#{symbol}")
                } else {
                    String::from_utf8_lossy(
                        image.imported_name(symbol).ok_or("invalid import name")?,
                    )
                    .into_owned()
                };
                return Err(format!(
                    "API boundary after {step} instructions: {}!{symbol}",
                    String::from_utf8_lossy(&module.name)
                )
                .into());
            }
            return Err("DOS dispatcher boundary".into());
        }
        let decoded = loaded.decode_next(&mut decoder, machine.cs(), machine.ip())?;
        println!(
            "{step:06} {:04x}:{:04x} {}",
            machine.cs(),
            machine.ip(),
            decoded.instruction.disassembly
        );
        machine.step(&mut loaded, &mut decoder)?;
    }
    Err(format!("instruction budget exhausted ({limit})").into())
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(trace)?
        .join()
        .expect("trace worker panicked")
}
