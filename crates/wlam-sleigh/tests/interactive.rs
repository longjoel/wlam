//! Execute the real CRT-free PE, including synchronous nested USER32 callbacks.
use std::sync::{Arc, Mutex};
use wlam_harness::wasmi::{self, Engine, Linker, Module, Store};
use wlam_host::{
    runtime::{Effect, Runtime},
    window::Message,
    GuestMemory,
};
#[derive(Default)]
struct State {
    runtime: Runtime,
    frames: Vec<Vec<u8>>,
    phase: u32,
    window: u32,
    exit: Option<u32>,
    depth: u32,
}
#[test]
fn pe_draws_responds_to_input_and_exits() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let build = std::process::Command::new("bash")
        .arg(root.join("scripts/build-interactive-fixture.sh"))
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let bytes = std::fs::read(root.join("target/interactive/window.exe")).unwrap();
    let compiled =
        wlam_sleigh::with_big_stack(move || wlam_sleigh::compile::compile(&bytes)).unwrap();
    let mut config = wasmi::Config::default();
    config.consume_fuel(true);
    let engine = Engine::new(&config);
    let module = Module::new(&engine, &compiled.module[..]).unwrap();
    let state = Arc::new(Mutex::new(State::default()));
    let mut store = Store::new(&engine, ());
    store.set_fuel(10_000_000).unwrap();
    let mut linker = Linker::new(&engine);
    for import in &compiled.imports {
        let shared = state.clone();
        let library = import.library.clone();
        let name = import.name.clone();
        linker
            .func_wrap(
                &import.library,
                &import.name,
                move |mut caller: wasmi::Caller<'_, ()>,
                      argument_pointer: i32,
                      _slot: i32|
                      -> Result<i32, wasmi::Error> {
                    let memory = caller.get_export("memory").unwrap().into_memory().unwrap();
                    let signature =
                        wlam_pcode::abi::signature(&library, &name).ok_or_else(|| {
                            wasmi::Error::new(format!("unsupported {library}!{name}"))
                        })?;
                    let mut arguments = Vec::new();
                    for i in 0..signature.argument_bytes / 4 {
                        let mut raw = [0; 4];
                        memory
                            .read(
                                &caller,
                                (argument_pointer as u32 + i * 4) as usize,
                                &mut raw,
                            )
                            .map_err(|e| wasmi::Error::new(e.to_string()))?;
                        arguments.push(u32::from_le_bytes(raw));
                    }
                    eprintln!("IMPORT {library}!{name} {arguments:x?}");
                    let mut effect = {
                        let mut state = shared.lock().unwrap();
                        let mut mem = GuestMemory::new(memory.data_mut(&mut caller));
                        state
                            .runtime
                            .call(&library, &name, &arguments, &mut mem)
                            .map_err(wasmi::Error::new)?
                    };
                    loop {
                        match effect {
                            Effect::Return(value) => return Ok(value as i32),
                            Effect::MessageBox { .. } => {
                                return Err(wasmi::Error::new("unexpected fixture dialog"))
                            }
                            Effect::Exit(code) => {
                                shared.lock().unwrap().exit = Some(code);
                                return Err(wasmi::Error::new("process exited"));
                            }
                            Effect::Invoke {
                                token,
                                procedure,
                                args,
                            } => {
                                let argument_vector = {
                                    let mut state = shared.lock().unwrap();
                                    if args.len() == 4 {
                                        state.window = args[0];
                                    }
                                    state.depth += 1;
                                    0x20000 + state.depth * 256
                                };
                                for (i, value) in args.iter().enumerate() {
                                    memory
                                        .write(
                                            &mut caller,
                                            argument_vector as usize + i * 4,
                                            &value.to_le_bytes(),
                                        )
                                        .unwrap();
                                }
                                let callback = caller
                                    .get_export("invoke_guest")
                                    .unwrap()
                                    .into_func()
                                    .unwrap()
                                    .typed::<(i32, i32, i32), i32>(&caller)
                                    .unwrap();
                                eprintln!("CALLBACK {procedure:x} {args:x?}");
                                let value = callback.call(
                                    &mut caller,
                                    (procedure as i32, args.len() as i32, argument_vector as i32),
                                )?;
                                let mut state = shared.lock().unwrap();
                                state.depth -= 1;
                                effect = state
                                    .runtime
                                    .resume(token, value as u32)
                                    .map_err(wasmi::Error::new)?;
                            }
                            Effect::Retry => {
                                let mut state = shared.lock().unwrap();
                                let mut mem = GuestMemory::new(memory.data_mut(&mut caller));
                                effect = state
                                    .runtime
                                    .call(&library, &name, &arguments, &mut mem)
                                    .map_err(wasmi::Error::new)?;
                            }
                            Effect::Wait => {
                                let mut state = shared.lock().unwrap();
                                let window = state.window;
                                let surface = state
                                    .runtime
                                    .gdi
                                    .surface(window)
                                    .ok_or_else(|| wasmi::Error::new("no window surface"))?;
                                let frame = surface.rgba.clone();
                                state.frames.push(frame);
                                let (message, wparam) = match state.phase {
                                    0 => (0x201, 1),
                                    1 => (0x100, 65),
                                    2 => (0x100, 27),
                                    _ => {
                                        return Err(wasmi::Error::new("fixture waited after close"))
                                    }
                                };
                                state.phase += 1;
                                state
                                    .runtime
                                    .post(Message {
                                        window,
                                        message,
                                        wparam,
                                        lparam: 0,
                                    })
                                    .map_err(wasmi::Error::new)?;
                                let mut mem = GuestMemory::new(memory.data_mut(&mut caller));
                                effect = state
                                    .runtime
                                    .call(&library, &name, &arguments, &mut mem)
                                    .map_err(wasmi::Error::new)?;
                            }
                        }
                    }
                },
            )
            .unwrap();
    }
    let instance = linker
        .instantiate(&mut store, &module)
        .unwrap()
        .start(&mut store)
        .unwrap();
    let memory = instance.get_memory(&store, "memory").unwrap();
    for segment in &compiled.segments {
        memory
            .write(&mut store, segment.address as usize, &segment.bytes)
            .unwrap();
    }
    for (name, value) in [("ESP", 0x70000u32), ("FS_OFFSET", 0x60000)] {
        let index = compiled.registers.iter().position(|r| r == name).unwrap();
        memory
            .write(
                &mut store,
                wlam_emit::snapshot_address(index) as usize,
                &value.to_le_bytes(),
            )
            .unwrap();
    }
    memory
        .write(&mut store, 0x60000, &u32::MAX.to_le_bytes())
        .unwrap();
    let result = instance
        .get_typed_func::<(), ()>(&store, "entry")
        .unwrap()
        .call(&mut store, ());
    let state = state.lock().unwrap();
    assert_eq!(state.exit, Some(0), "guest outcome: {result:?}");
    assert_eq!(state.frames.len(), 3);
    for (frame, pixel) in state.frames.iter().zip([
        [0x12, 0x34, 0x56, 255],
        [0xe0, 0x40, 0x20, 255],
        [0x20, 0xc0, 0x60, 255],
    ]) {
        assert_eq!(frame.len(), 32 * 24 * 4);
        for actual in frame.chunks_exact(4) {
            assert_eq!(actual, pixel);
        }
    }
}
