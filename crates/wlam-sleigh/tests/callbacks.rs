use std::collections::BTreeMap;
use wlam_emit::chain::{build_module, partition};
use wlam_harness::wasmi::{Engine, Linker, Module, Store};
use wlam_sleigh::{lift_block, with_big_stack};
#[test]
fn callback_uses_guest_address_arguments_and_restores_outer_registers() {
    // mov eax,[esp+4]; add eax,[esp+8]; ret 16
    let bytes = vec![0x8b, 0x44, 0x24, 4, 0x03, 0x44, 0x24, 8, 0xc2, 16, 0];
    let lifted = with_big_stack(move || lift_block(&bytes, 0x401000));
    let block = wlam_sleigh::emit::convert(&lifted);
    let mut names = std::collections::BTreeSet::new();
    for op in &block.ops {
        wlam_sleigh::emit::collect_regs(op, &mut names);
    }
    let registers: Vec<_> = names.into_iter().collect();
    let blocks = BTreeMap::from([(block.start, block)]);
    let program = partition(&[0x401000], &blocks, &registers);
    let module_bytes = build_module(&program, &blocks).unwrap();
    let engine = Engine::default();
    let module = Module::new(&engine, &module_bytes[..]).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Linker::new(&engine)
        .instantiate(&mut store, &module)
        .unwrap()
        .start(&mut store)
        .unwrap();
    let memory = instance.get_memory(&store, "memory").unwrap();
    let esp = registers.iter().position(|r| r == "ESP").unwrap();
    let eax = registers.iter().position(|r| r == "EAX").unwrap();
    memory
        .write(
            &mut store,
            wlam_emit::snapshot_address(esp) as usize,
            &0x4000u32.to_le_bytes(),
        )
        .unwrap();
    memory
        .write(
            &mut store,
            wlam_emit::snapshot_address(eax) as usize,
            &0x12345678u32.to_le_bytes(),
        )
        .unwrap();
    memory
        .write(
            &mut store,
            0x2000,
            &[7, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        )
        .unwrap();
    let before = memory.data(&store)[0x8000..0x8000 + registers.len() * 8].to_vec();
    let callback = instance
        .get_typed_func::<(i32, i32, i32), i32>(&store, "invoke_guest")
        .unwrap();
    assert_eq!(
        callback.call(&mut store, (0x401000, 4, 0x2000)).unwrap(),
        16
    );
    assert_eq!(
        &memory.data(&store)[0x8000..0x8000 + registers.len() * 8],
        &before
    );
    assert!(callback.call(&mut store, (0x402000, 4, 0x2000)).is_err());
    assert_eq!(
        callback.call(&mut store, (0x401000, 4, 0x2000)).unwrap(),
        16
    );
}

#[test]
fn nested_guest_host_guest_callback_preserves_both_frames() {
    use wlam_emit::chain::HostImport;
    // Outer: call GetTickCount; add eax,1; ret 16.
    let outer = vec![0xff, 0x15, 0, 0x30, 0, 0, 0x83, 0xc0, 1, 0xc2, 16, 0];
    // Inner: mov eax,[esp+4]; add eax,[esp+8]; ret 16.
    let inner = vec![0x8b, 0x44, 0x24, 4, 0x03, 0x44, 0x24, 8, 0xc2, 16, 0];
    let mut blocks = BTreeMap::new();
    for (address, bytes) in [
        (0x401000, outer.clone()),
        (0x401006, outer[6..].to_vec()),
        (0x402000, inner),
    ] {
        let lifted = with_big_stack(move || lift_block(&bytes, address));
        blocks.insert(address, wlam_sleigh::emit::convert(&lifted));
    }
    let mut registers = wlam_emit::seeded_registers();
    let mut used = std::collections::BTreeSet::new();
    for block in blocks.values() {
        for op in &block.ops {
            wlam_sleigh::emit::collect_regs(op, &mut used);
        }
    }
    for name in used {
        if !registers.contains(&name) {
            registers.push(name);
        }
    }
    let mut program = partition(&[0x401000, 0x402000], &blocks, &registers);
    program.register_host_imports(&blocks, |_| {
        Some(HostImport {
            library: "KERNEL32.dll".into(),
            name: "GetTickCount".into(),
        })
    });
    let bytes = build_module(&program, &blocks).unwrap();
    let engine = Engine::default();
    let module = Module::new(&engine, &bytes[..]).unwrap();
    let mut store = Store::new(&engine, ());
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "KERNEL32.dll",
            "GetTickCount",
            |mut caller: wlam_harness::wasmi::Caller<'_, ()>, _: i32, _: i32| -> i32 {
                let callback = caller
                    .get_export("invoke_guest")
                    .unwrap()
                    .into_func()
                    .unwrap()
                    .typed::<(i32, i32, i32), i32>(&caller)
                    .unwrap();
                callback.call(&mut caller, (0x402000, 4, 0x2100)).unwrap()
            },
        )
        .unwrap();
    let instance = linker
        .instantiate(&mut store, &module)
        .unwrap()
        .start(&mut store)
        .unwrap();
    let memory = instance.get_memory(&store, "memory").unwrap();
    let esp = registers.iter().position(|r| r == "ESP").unwrap();
    memory
        .write(
            &mut store,
            wlam_emit::snapshot_address(esp) as usize,
            &0x4000u32.to_le_bytes(),
        )
        .unwrap();
    memory
        .write(
            &mut store,
            0x2100,
            &[2, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        )
        .unwrap();
    let before = memory.data(&store)[0x8000..0x8000 + registers.len() * 8].to_vec();
    let callback = instance
        .get_typed_func::<(i32, i32, i32), i32>(&store, "invoke_guest")
        .unwrap();
    assert_eq!(callback.call(&mut store, (0x401000, 4, 0x2000)).unwrap(), 6);
    assert_eq!(
        &memory.data(&store)[0x8000..0x8000 + registers.len() * 8],
        &before
    );
}

#[test]
fn import_tail_callback_publishes_wrapper_stack() {
    use wlam_emit::chain::HostImport;
    // Outer: call GetTickCount; add eax,1; ret 16.
    let outer = vec![0xe8, 0xfb, 0x1f, 0, 0, 0x83, 0xc0, 1, 0xc2, 16, 0];
    // Inner: mov eax,[esp+4]; add eax,[esp+8]; ret 16.
    let inner = vec![0x8b, 0x44, 0x24, 4, 0x03, 0x44, 0x24, 8, 0xc2, 16, 0];
    let mut blocks = BTreeMap::new();
    for (address, bytes) in [
        (0x401000, outer.clone()),
        (0x401005, outer[5..].to_vec()),
        (0x403000, vec![0xff, 0x25, 0, 0x30, 0, 0]),
        (0x402000, inner),
    ] {
        let lifted = with_big_stack(move || lift_block(&bytes, address));
        blocks.insert(address, wlam_sleigh::emit::convert(&lifted));
    }
    let mut registers = wlam_emit::seeded_registers();
    let mut used = std::collections::BTreeSet::new();
    for block in blocks.values() {
        for op in &block.ops {
            wlam_sleigh::emit::collect_regs(op, &mut used);
        }
    }
    for name in used {
        if !registers.contains(&name) {
            registers.push(name);
        }
    }
    let mut program = partition(&[0x401000, 0x402000, 0x403000], &blocks, &registers);
    program.register_host_imports(&blocks, |_| {
        Some(HostImport {
            library: "KERNEL32.dll".into(),
            name: "GetTickCount".into(),
        })
    });
    let bytes = build_module(&program, &blocks).unwrap();
    let engine = Engine::default();
    let module = Module::new(&engine, &bytes[..]).unwrap();
    let mut store = Store::new(&engine, ());
    let mut linker = Linker::new(&engine);
    let esp_address =
        wlam_emit::snapshot_address(registers.iter().position(|r| r == "ESP").unwrap()) as usize;
    linker
        .func_wrap(
            "KERNEL32.dll",
            "GetTickCount",
            move |mut caller: wlam_harness::wasmi::Caller<'_, ()>, args: i32, _: i32| -> i32 {
                let memory = caller.get_export("memory").unwrap().into_memory().unwrap();
                let mut saved = [0; 4];
                memory.read(&caller, esp_address, &mut saved).unwrap();
                assert_eq!(
                    u32::from_le_bytes(saved),
                    args as u32 - 4,
                    "callback must see the current import stack"
                );
                let callback = caller
                    .get_export("invoke_guest")
                    .unwrap()
                    .into_func()
                    .unwrap()
                    .typed::<(i32, i32, i32), i32>(&caller)
                    .unwrap();
                callback.call(&mut caller, (0x402000, 4, 0x2100)).unwrap()
            },
        )
        .unwrap();
    let instance = linker
        .instantiate(&mut store, &module)
        .unwrap()
        .start(&mut store)
        .unwrap();
    let memory = instance.get_memory(&store, "memory").unwrap();
    let esp = registers.iter().position(|r| r == "ESP").unwrap();
    memory
        .write(
            &mut store,
            wlam_emit::snapshot_address(esp) as usize,
            &0x4000u32.to_le_bytes(),
        )
        .unwrap();
    memory
        .write(
            &mut store,
            0x2100,
            &[2, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        )
        .unwrap();
    let before = memory.data(&store)[0x8000..0x8000 + registers.len() * 8].to_vec();
    let callback = instance
        .get_typed_func::<(i32, i32, i32), i32>(&store, "invoke_guest")
        .unwrap();
    assert_eq!(callback.call(&mut store, (0x401000, 4, 0x2000)).unwrap(), 6);
    assert_eq!(
        &memory.data(&store)[0x8000..0x8000 + registers.len() * 8],
        &before
    );
}

#[test]
fn set_rect_empty_cleanup_preserves_saved_edi() {
    use wlam_emit::chain::HostImport;
    // Match Hover's wrapper: a local RECT above saved ESI/EDI, followed by
    // SetRectEmpty(&rect). An extra argument pop makes the next PUSH overwrite EDI.
    let prologue = vec![
        0x55, 0x8b, 0xec, 0x83, 0xec, 0x10, 0x56, 0x57, 0x8d, 0x45, 0xf0, 0x50, 0xff, 0x15, 0,
        0x30, 0, 0,
    ];
    let epilogue = vec![
        0x6a, 0, 0x58, 0x5f, 0x5e, 0x8b, 0xc7, 0x8b, 0xe5, 0x5d, 0xc2, 16, 0,
    ];
    let mut blocks = BTreeMap::new();
    for (address, bytes) in [(0x401000, prologue), (0x401012, epilogue)] {
        let lifted = with_big_stack(move || lift_block(&bytes, address));
        blocks.insert(address, wlam_sleigh::emit::convert(&lifted));
    }
    let mut registers = wlam_emit::seeded_registers();
    let mut used = std::collections::BTreeSet::new();
    for block in blocks.values() {
        for op in &block.ops {
            wlam_sleigh::emit::collect_regs(op, &mut used);
        }
    }
    for name in used {
        if !registers.contains(&name) {
            registers.push(name);
        }
    }
    let mut program = partition(&[0x401000], &blocks, &registers);
    program.register_host_imports(&blocks, |_| {
        Some(HostImport {
            library: "USER32.dll".into(),
            name: "SetRectEmpty".into(),
        })
    });
    let bytes = build_module(&program, &blocks).unwrap();
    let engine = Engine::default();
    let module = Module::new(&engine, &bytes[..]).unwrap();
    let mut store = Store::new(&engine, ());
    let mut linker = Linker::new(&engine);
    linker
        .func_wrap(
            "USER32.dll",
            "SetRectEmpty",
            |mut caller: wlam_harness::wasmi::Caller<'_, ()>, args: i32, _: i32| -> i32 {
                let memory = caller.get_export("memory").unwrap().into_memory().unwrap();
                let mut pointer = [0; 4];
                memory.read(&caller, args as usize, &mut pointer).unwrap();
                memory
                    .write(&mut caller, u32::from_le_bytes(pointer) as usize, &[0; 16])
                    .unwrap();
                1
            },
        )
        .unwrap();
    let instance = linker
        .instantiate(&mut store, &module)
        .unwrap()
        .start(&mut store)
        .unwrap();
    let memory = instance.get_memory(&store, "memory").unwrap();
    for (name, value) in [("ESP", 0x4000u32), ("EDI", 0x08009028)] {
        let index = registers.iter().position(|r| r == name).unwrap();
        memory
            .write(
                &mut store,
                wlam_emit::snapshot_address(index) as usize,
                &value.to_le_bytes(),
            )
            .unwrap();
    }
    let callback = instance
        .get_typed_func::<(i32, i32, i32), i32>(&store, "invoke_guest")
        .unwrap();
    assert_eq!(
        callback.call(&mut store, (0x401000, 4, 0x2000)).unwrap(),
        0x08009028
    );
}
