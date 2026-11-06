//! Execute AOT-compiled NE instructions. Optional differential verification
//! compares every instruction's full registers and segmented memory with the
//! checked CPU bridge. Execution never falls back to interpreting missing code.
use std::{env, fs, path::Path};
use wasmi::{Caller, Engine, Linker, Module, Store};
use wlam_ne::{NeAccess, NeTask, NeTaskEvent};
enum Input {
    Key(u16),
    Command(u16),
    Text(u16, Vec<u8>),
    Answer(u16),
}
fn capture_windows(
    task: &mut NeTask,
    args: &[String],
    compiled: usize,
) -> Result<Vec<(u16, u32)>, Box<dyn std::error::Error + Send + Sync>> {
    let windows = task.window_handles();
    for (_, window) in windows.iter().rev() {
        if task.host.gdi.surface(*window).is_some() {
            task.host.compose_children(*window)?;
        }
    }
    for (guest, host) in &windows {
        let window = task
            .host
            .windows
            .get(*host)
            .map_err(|e| format!("window: {e:?}"))?;
        println!(
            "  HWND {guest:04x}: {}x{} visible={} parent={:x}",
            window.width, window.height, window.visible, window.parent
        );
        if let Some(directory) = args.iter().find_map(|s| s.strip_prefix("--capture=")) {
            if let Some(surface) = task.host.gdi.surface(*host) {
                fs::create_dir_all(directory)?;
                let rgba = task
                    .host
                    .presented_frame(*host)
                    .unwrap_or_else(|| surface.rgba.clone());
                let mut ppm =
                    format!("P6\n{} {}\n255\n", surface.width, surface.height).into_bytes();
                for pixel in rgba.chunks_exact(4) {
                    ppm.extend_from_slice(&pixel[..3]);
                }
                fs::write(
                    Path::new(directory).join(format!("{compiled}-{guest:04x}.ppm")),
                    ppm,
                )?;
            }
        }
    }
    Ok(windows)
}
fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args: Vec<_> = env::args().collect();
    if args.len() < 3 {
        return Err(
            "usage: run_compiled_ne <image.exe> <guest.wasm> [step-limit] [--differential]".into(),
        );
    }
    let limit: usize = args
        .get(3)
        .filter(|s| !s.starts_with("--"))
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(1000000);
    let differential = args.iter().any(|s| s == "--differential");
    let mut inputs = std::collections::VecDeque::new();
    for arg in &args {
        if let Some(value) = arg.strip_prefix("--key=") {
            inputs.push_back(Input::Key(value.parse()?));
        } else if let Some(value) = arg.strip_prefix("--answer=") {
            inputs.push_back(Input::Answer(value.parse()?));
        } else if let Some(value) = arg.strip_prefix("--command=") {
            inputs.push_back(Input::Command(value.parse()?));
        } else if let Some(value) = arg.strip_prefix("--text=") {
            let (id, text) = value
                .split_once(':')
                .ok_or("--text requires control-id:text")?;
            inputs.push_back(Input::Text(
                id.parse()?,
                text.encode_utf16()
                    .map(wlam_host::locale::ansi_encode)
                    .collect(),
            ));
        }
    }
    let mut task = NeTask::load(fs::read(&args[1])?, Vec::new())?;
    task.set_executable_path(
        format!(
            "C:\\CASTLE\\{}",
            Path::new(&args[1])
                .file_name()
                .ok_or("missing filename")?
                .to_string_lossy()
        )
        .as_bytes(),
    )?;
    let directory = Path::new(&args[1])
        .parent()
        .unwrap_or_else(|| Path::new("."));
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            task.register_file(
                format!("C:\\CASTLE\\{}", entry.file_name().to_string_lossy()).as_bytes(),
                fs::read(entry.path())?,
            );
        }
    }
    let engine = Engine::default();
    let module = Module::new(&engine, &fs::read(&args[2])?[..])?;
    let mut linker = Linker::<NeTask>::new(&engine);
    linker.func_wrap(
        "ne",
        "read",
        |caller: Caller<'_, NeTask>, ptr: i32, size: i32| -> Result<i64, wasmi::Error> {
            if !(1..=8).contains(&size) {
                return Err(wasmi::Error::new("invalid compiled read width"));
            }
            let pointer = ptr as u32;
            let bytes = caller
                .data()
                .loaded
                .read(
                    (pointer >> 16) as u16,
                    pointer & 0xffff,
                    size as u32,
                    NeAccess::Read,
                )
                .map_err(|e| wasmi::Error::new(e.to_string()))?;
            Ok(bytes
                .iter()
                .enumerate()
                .fold(0u64, |v, (i, b)| v | (u64::from(*b) << (i * 8))) as i64)
        },
    )?;
    linker.func_wrap(
        "ne",
        "write",
        |mut caller: Caller<'_, NeTask>,
         ptr: i32,
         size: i32,
         value: i64|
         -> Result<(), wasmi::Error> {
            if !(1..=8).contains(&size) {
                return Err(wasmi::Error::new("invalid compiled write width"));
            }
            let pointer = ptr as u32;
            caller
                .data_mut()
                .loaded
                .write(
                    (pointer >> 16) as u16,
                    pointer & 0xffff,
                    &value.to_le_bytes()[..size as usize],
                )
                .map_err(|e| wasmi::Error::new(e.to_string()))
        },
    )?;
    linker.func_wrap(
        "ne",
        "fault",
        |pc: i32, index: i32| -> Result<(), wasmi::Error> {
            Err(wasmi::Error::new(format!(
                "uncompiled NE operation at {:04x}:{:04x}, op {}",
                (pc as u32) >> 16,
                pc as u16,
                index as u32
            )))
        },
    )?;
    let mut store = Store::new(&engine, task);
    let instance = linker.instantiate(&mut store, &module)?.start(&mut store)?;
    let memory = instance
        .get_memory(&store, "memory")
        .ok_or("no compiled memory")?;
    let registers = instance
        .get_typed_func::<(), i32>(&store, "ne_registers")?
        .call(&mut store, ())? as u32 as usize;
    let execute = instance.get_typed_func::<i32, i32>(&store, "ne_step")?;
    let mut decoder = rsleigh_api::Decoder::new(rsleigh_api::Architecture::X86_16Protected);
    let mut compiled = 0;
    for step in 0..limit {
        let task = store.data();
        let cs = task.machine.cs();
        let ip = task.machine.ip();
        if task.is_api_address(cs) || task.is_waiting() {
            let arguments = if env::var_os("WLAM_NE_TRACE").is_some() {
                let ss = task.machine.register(0x104, 2)? as u16;
                let sp = task.machine.register(0x10, 2)? as u32;
                task.loaded.read(ss, sp, 16, NeAccess::Read).ok()
            } else {
                None
            };
            let event = match store.data_mut().step() {
                Ok(event) => event,
                Err(error) => {
                    if let Err(capture) = capture_windows(store.data_mut(), &args, compiled) {
                        eprintln!("frame capture failed: {capture}");
                    }
                    return Err(error.into());
                }
            };
            match event {
                NeTaskEvent::Wait => {
                    println!("waiting for input after {compiled} compiled instructions");
                    let windows = capture_windows(store.data_mut(), &args, compiled)?;
                    let task = store.data();
                    if let Some(input) = inputs.pop_front() {
                        let target = windows
                            .iter()
                            .rev()
                            .find(|(_, host)| {
                                task.host
                                    .windows
                                    .get(*host)
                                    .is_ok_and(|w| w.visible && w.style & 0x40000000 == 0)
                            })
                            .ok_or("no visible top-level window for key")?
                            .0;
                        match input {
                            Input::Command(value) => store.data_mut().post_menu_command(target, value)?,
                            Input::Key(value) => {
                                store.data_mut().post_key(target, value, true)?;
                                store.data_mut().post_key(target, value, false)?;
                            }
                            Input::Text(control, text) => store.data_mut().set_dialog_text(target, control, &text)?,
                            Input::Answer(_) => return Err("script expected a message box, but task is waiting for window input".into()),
                        }
                        continue;
                    }
                    return Ok(());
                }
                NeTaskEvent::Api { module, symbol } => {
                    println!("{step:08} {module}!{symbol:?} => DX:AX={:04x}:{:04x} ({compiled} compiled instructions)",store.data().machine.register(8,2)?,store.data().machine.register(0,2)?);
                    if let Some(arguments) = arguments {
                        println!("  far return and arguments: {arguments:02x?}");
                    }
                }
                NeTaskEvent::Exit(code) => {
                    println!("exit {code}");
                    return Ok(());
                }
                NeTaskEvent::MessageBox { text, title, flags } => {
                    if matches!(inputs.front(), Some(Input::Answer(_))) {
                        let Some(Input::Answer(button)) = inputs.pop_front() else {
                            unreachable!()
                        };
                        println!(
                            "message box {}: {}; selected button {button}",
                            String::from_utf8_lossy(&title),
                            String::from_utf8_lossy(&text)
                        );
                        store.data_mut().answer_message_box(button)?;
                        continue;
                    }
                    return Err(format!("guest message box (flags {flags:#x}), after {compiled} compiled instructions: {}: {}",String::from_utf8_lossy(&title),String::from_utf8_lossy(&text)).into());
                }
                _ => return Err("API dispatch executed an interpreted instruction".into()),
            }
            continue;
        }
        let pc = (u32::from(cs) << 16) | u32::from(ip);
        let expected = if differential {
            let mut cpu = task.machine.clone();
            let mut loaded = task.loaded.deep_clone();
            let execution = cpu.step(&mut loaded, &mut decoder).map_err(|error| {
                let decoded = task.loaded.decode_next(&mut decoder, cs, ip);
                format!(
                    "reference CPU failed at step {step}, {cs:04x}:{ip:04x}: {error}; {decoded:#?}"
                )
            })?;
            if env::var_os("WLAM_NE_INSTRUCTIONS").is_some() {
                println!("{step:08} {cs:04x}:{ip:04x} {} -> {:04x}:{:04x}; AX={:04x} BX={:04x} CX={:04x} DX={:04x} SP={:04x} BP={:04x}",execution.disassembly,cpu.cs(),cpu.ip(),cpu.register(0,2)?,cpu.register(12,2)?,cpu.register(4,2)?,cpu.register(8,2)?,cpu.register(16,2)?,cpu.register(20,2)?);
            }
            if cpu.cs() == 0 {
                return Err(format!("reference CPU transferred to a null selector at step {step}, {cs:04x}:{ip:04x}: {:#?}",task.loaded.decode_next(&mut decoder,cs,ip)).into());
            }
            Some((cpu, loaded))
        } else {
            None
        };
        let snapshot = task.machine.register_bytes().to_vec();
        memory.write(&mut store, registers, &snapshot)?;
        execute
            .call(&mut store, pc as i32)
            .map_err(|e| format!("step {step}: {e}"))?;
        let mut snapshot = vec![0; 8192];
        memory.read(&store, registers, &mut snapshot)?;
        store.data_mut().machine.restore_registers(&snapshot)?;
        if let Some((cpu, loaded)) = expected {
            if cpu.register_bytes() != store.data().machine.register_bytes() {
                let differences = cpu
                    .register_bytes()
                    .iter()
                    .zip(store.data().machine.register_bytes())
                    .enumerate()
                    .filter(|(_, (a, b))| a != b)
                    .take(12)
                    .collect::<Vec<_>>();
                return Err(
                    format!("register mismatch at {cs:04x}:{ip:04x}: {differences:?}").into(),
                );
            }
            if loaded != store.data().loaded {
                return Err(format!("segmented memory mismatch at {cs:04x}:{ip:04x}").into());
            }
        }
        compiled += 1;
    }
    Err(format!("instruction budget exhausted after {compiled} compiled instructions").into())
}
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(run)?
        .join()
        .expect("compiled NE runner panicked")
}
