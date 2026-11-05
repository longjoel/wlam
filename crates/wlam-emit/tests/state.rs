//! Execute state transfers; validation alone cannot detect overlapping slots.
use std::collections::BTreeMap;
use wasmi::{Engine, Linker, Module, Store};
use wlam_emit::chain::{build_module_with_memory, partition};
use wlam_emit::ir::{Block, Operand, Terminator, IR};
use wlam_emit::{module_with_snapshot, snapshot_address, translate_ops, LocalMap, TranslateError};

fn reg(name: &str, size: u32) -> Operand {
    Operand::Reg {
        name: name.into(),
        size,
    }
}
fn constant(value: u64) -> Operand {
    Operand::Const { value, size: 4 }
}

fn execute(bytes: &[u8], export: &str, registers: &[String]) -> Vec<u64> {
    let engine = Engine::default();
    let module = Module::new(&engine, bytes).expect("valid module");
    let mut store = Store::new(&engine, ());
    let instance = Linker::new(&engine)
        .instantiate(&mut store, &module)
        .unwrap()
        .start(&mut store)
        .unwrap();
    let memory = instance.get_memory(&store, "memory").unwrap();
    let values = [
        1.23456789f64.to_bits(),
        (-7.987654321f64).to_bits(),
        0x1234_5678,
    ];
    for (i, value) in values.iter().enumerate() {
        memory
            .write(
                &mut store,
                snapshot_address(i) as usize,
                &value.to_le_bytes(),
            )
            .unwrap();
    }
    instance
        .get_typed_func::<(), ()>(&store, export)
        .unwrap()
        .call(&mut store, ())
        .unwrap();
    registers
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let mut bytes = [0; 8];
            memory
                .read(&store, snapshot_address(i) as usize, &mut bytes)
                .unwrap();
            u64::from_le_bytes(bytes)
        })
        .collect()
}

#[test]
fn block_and_chained_snapshots_preserve_adjacent_doubles_and_integer() {
    // Deliberately put x87 first: slot safety must not depend on ABI ordering.
    let registers = vec!["ST0".to_string(), "ST1".to_string(), "EAX".to_string()];
    let locals = LocalMap::new(
        registers
            .iter()
            .map(|r| (r.clone(), if r.starts_with("ST") { 8 } else { 4 })),
    );
    let translated = translate_ops(&[], locals.clone()).unwrap();
    let block_module = module_with_snapshot(&translated, &locals, &registers, &[]);
    let expected = vec![
        1.23456789f64.to_bits(),
        (-7.987654321f64).to_bits(),
        0x1234_5678,
    ];
    assert_eq!(execute(&block_module, "run", &registers), expected);

    let blocks = BTreeMap::from([(
        0x1000,
        Block {
            start: 0x1000,
            end: 0x1001,
            ops: vec![],
            terminator: Terminator::Return,
        },
    )]);
    let program = partition(&[0x1000], &blocks, &registers);
    let chained = build_module_with_memory(&program, &blocks, 16).unwrap();
    assert_eq!(execute(&chained, "entry", &registers), expected);
}

#[test]
fn float_comparisons_and_integer_conversions_produce_integer_temporaries() {
    use wlam_emit::ir::{ConvertOp, FloatOp};
    let registers = vec!["ST0".to_string(), "ST1".to_string(), "EAX".to_string(), "ECX".to_string()];
    let boolean = Operand::Temp { offset: 1, size: 1 };
    let integer = Operand::Temp { offset: 2, size: 4 };
    let blocks = BTreeMap::from([(0x1000, Block {
        start: 0x1000, end: 0x1001,
        ops: vec![
            IR::Float { op: FloatOp::Less, dst: boolean.clone(), operands: Box::new((reg("ST1", 10), reg("ST0", 10))) },
            IR::Copy { dst: reg("EAX", 4), src: boolean },
            IR::Convert { op: ConvertOp::FloatToInt, dst: integer.clone(), src: reg("ST0", 10) },
            IR::Copy { dst: reg("ECX", 4), src: integer },
        ],
        terminator: Terminator::Return,
    })]);
    let program = partition(&[0x1000], &blocks, &registers);
    let module = build_module_with_memory(&program, &blocks, 16).unwrap();
    let result = execute(&module, "entry", &registers);
    assert_eq!(result[2], 1);
    assert_eq!(result[3], 1);
}

#[test]
fn floating_constants_are_reals_in_float_conversions() {
    use wlam_emit::ir::ConvertOp;
    let registers = vec!["ST0".to_string(), "ST1".to_string(), "EAX".to_string()];
    let blocks = BTreeMap::from([(0x1000, Block {
        start:0x1000,end:0x1001,
        ops:vec![IR::Convert {
            op:ConvertOp::FloatToFloat,dst:reg("ST0",10),
            src:Operand::Const {value:std::f64::consts::PI.to_bits(),size:8},
        }],
        terminator:Terminator::Return,
    })]);
    let program = partition(&[0x1000], &blocks, &registers);
    let module = build_module_with_memory(&program, &blocks, 16).unwrap();
    assert_eq!(execute(&module, "entry", &registers)[0], std::f64::consts::PI.to_bits());
}

#[test]
fn flags_use_the_logical_width_of_a_reused_wide_temporary() {
    let registers = vec!["EAX".to_string()];
    let locals = LocalMap::new([("EAX".to_string(), 4)]);
    for (kind, value) in [(wlam_emit::BinOp::Carry, 0xffff), (wlam_emit::BinOp::SBorrow, 0x8000)] {
        let temp = Operand::Temp { offset: 1, size: 2 };
        let ops = vec![
            IR::Copy { dst:Operand::Temp {offset:1,size:8},src:Operand::Const {value:0,size:8} },
            IR::Copy { dst:temp.clone(),src:Operand::Const {value,size:2} },
            IR::Bin {op:kind,dst:reg("EAX",4),operands:Box::new((temp,Operand::Const {value:1,size:2}))},
        ];
        let translated = translate_ops(&ops, locals.clone()).unwrap();
        let module = module_with_snapshot(&translated, &locals, &registers, &[]);
        assert_eq!(execute(&module,"run",&registers)[0] as u32,1);
    }
}

#[test]
fn extended_memory_loads_compile_to_a_named_refusal() {
    let registers=vec!["ST0".to_string(),"EIP".to_string()];
    let blocks=BTreeMap::from([(0x1000,Block {
        start:0x1000,end:0x1001,
        ops:vec![IR::Load {dst:reg("ST0",10),addr:constant(0x100),size:10}],
        terminator:Terminator::Return,
    })]);
    let mut program=partition(&[0x1000],&blocks,&registers);
    program.register_host_imports(&blocks,|_|None);
    let bytes=build_module_with_memory(&program,&blocks,16).unwrap();
    let engine=Engine::default();let module=Module::new(&engine,&bytes[..]).unwrap();
    let mut linker=Linker::new(&engine);let mut store=Store::new(&engine,0);
    linker.func_wrap("wlam.unsupported","unsupported operand width 10 bytes",|mut caller:wasmi::Caller<'_,i32>,_:i32,_:i32|->i32 {
        *caller.data_mut()+=1;0
    }).unwrap();
    let instance=linker.instantiate(&mut store,&module).unwrap().start(&mut store).unwrap();
    assert!(instance.get_typed_func::<(),()>(&store,"entry").unwrap().call(&mut store,()).is_err());
    assert_eq!(*store.data(),1);
}

#[test]
fn float_unary_operations_preserve_rounding_and_signed_zero() {
    use wlam_emit::ir::FloatUnaryOp as U;
    let registers=vec!["ST0".to_string()];
    for (kind,input,expected) in [
        (U::Round,2.5f64,2.0f64),(U::Round,3.5,4.0),(U::Round,-2.5,-2.0),
        (U::Floor,-1.25,-2.0),(U::Ceil,-1.25,-1.0),
        (U::Neg,0.0,-0.0),(U::Abs,-0.0,0.0),(U::Sqrt,4.0,2.0),
    ] {
        let blocks=BTreeMap::from([(0x1000,Block {
            start:0x1000,end:0x1001,
            ops:vec![IR::FloatUnary {op:kind,dst:reg("ST0",10),src:Operand::Const {value:input.to_bits(),size:8}}],
            terminator:Terminator::Return,
        })]);
        let program=partition(&[0x1000],&blocks,&registers);
        let bytes=build_module_with_memory(&program,&blocks,16).unwrap();
        assert_eq!(execute(&bytes,"entry",&registers)[0],expected.to_bits(),"{kind:?}({input})");
    }
}

#[test]
fn unsupported_operations_fail_with_their_identity() {
    for dst in [None, Some(reg("EAX", 4))] {
        let result = translate_ops(
            &[IR::Userop {
                id: 12345,
                args: vec![],
                dst,
            }],
            LocalMap::new([("EAX".into(), 4)]),
        );
        assert!(matches!(
            result,
            Err(TranslateError::UnimplementedUserop(12345))
        ));
    }
    let result = translate_ops(&[IR::Unsupported("FloatSqrt".into())], LocalMap::default());
    assert!(matches!(result, Err(TranslateError::UnsupportedPcode(op)) if op == "FloatSqrt"));
}

#[test]
fn caller_adopts_stack_pointer_after_callee_cleanup() {
    let registers = vec!["ESP".to_string(), "EAX".to_string()];
    let blocks = BTreeMap::from([
        (
            0x1000,
            Block {
                start: 0x1000,
                end: 0x1005,
                ops: vec![IR::Call {
                    target: constant(0x2000),
                }],
                terminator: Terminator::Call { resume: 0x1005 },
            },
        ),
        (
            0x1005,
            Block {
                start: 0x1005,
                end: 0x1006,
                ops: vec![IR::Copy {
                    dst: reg("EAX", 4),
                    src: reg("ESP", 4),
                }],
                terminator: Terminator::Return,
            },
        ),
        (
            0x2000,
            Block {
                start: 0x2000,
                end: 0x2001,
                ops: vec![IR::Copy {
                    dst: reg("ESP", 4),
                    src: constant(0x4008),
                }],
                terminator: Terminator::Return,
            },
        ),
    ]);
    let program = partition(&[0x1000], &blocks, &registers);
    let bytes = build_module_with_memory(&program, &blocks, 16).unwrap();
    let values = execute(&bytes, "entry", &registers);
    assert_eq!(values[1] as u32, 0x4008);
}

#[test]
fn block_seeds_registers_before_executing_guest_operations() {
    let registers = vec!["EAX".to_string(), "EBX".to_string()];
    let locals = LocalMap::new(registers.iter().map(|r| (r.clone(), 4)));
    let ops = [IR::Copy {
        dst: reg("EBX", 4),
        src: reg("EAX", 4),
    }];
    let translated = translate_ops(&ops, locals.clone()).unwrap();
    let bytes = module_with_snapshot(&translated, &locals, &registers, &[]);
    let values = execute(&bytes, "run", &registers);
    assert_eq!(values[1] as u32, values[0] as u32);
}

#[test]
fn unsupported_instruction_names_the_gap_and_stops_even_in_probing_mode() {
    let blocks = BTreeMap::from([(
        0x1000,
        Block {
            start: 0x1000,
            end: 0x1001,
            ops: vec![
                IR::Userop {
                    id: 12345,
                    args: vec![],
                    dst: None,
                },
                IR::Store {
                    addr: constant(0x100),
                    value: constant(42),
                    size: 4,
                },
            ],
            terminator: Terminator::Return,
        },
    )]);
    let mut program = partition(&[0x1000], &blocks, &[]);
    program.register_host_imports(&blocks, |_| None);
    let bytes = build_module_with_memory(&program, &blocks, 16).unwrap();
    let engine = Engine::default();
    let module = Module::new(&engine, &bytes[..]).unwrap();
    let mut store = Store::new(&engine, 0u32);
    let mut linker = Linker::new(&engine);
    let import = &program.host_imports.values().next().unwrap();
    assert_eq!(import.library, "wlam.unsupported");
    assert!(import.name.contains("12345"));
    linker
        .func_wrap(
            &import.library,
            &import.name,
            |mut caller: wasmi::Caller<'_, u32>, _: i32, _: i32| -> i32 {
                *caller.data_mut() += 1;
                0 // Probing cannot turn an unknown instruction into a no-op.
            },
        )
        .unwrap();
    let instance = linker
        .instantiate(&mut store, &module)
        .unwrap()
        .start(&mut store)
        .unwrap();
    assert!(instance
        .get_typed_func::<(), ()>(&store, "entry")
        .unwrap()
        .call(&mut store, ())
        .is_err());
    assert_eq!(*store.data(), 1);
    let memory = instance.get_memory(&store, "memory").unwrap();
    assert_eq!(&memory.data(&store)[0x100..0x104], &[0; 4]);
}

#[test]
fn carry_and_signed_overflow_match_known_values_at_each_integer_width() {
    use wlam_emit::BinOp;
    for size in [1, 2, 4, 8] {
        let bits = size * 8;
        let max = if size == 8 {
            u64::MAX
        } else {
            (1u64 << bits) - 1
        };
        let sign = 1u64 << (bits - 1);
        for (kind, lhs, rhs, expected) in [
            (BinOp::Carry, max, 1, 1),
            (BinOp::Carry, max - 5, 1, 0),
            (BinOp::SCarry, sign - 1, 1, 1),
            (BinOp::SCarry, sign, max, 1),
            (BinOp::SCarry, 1, 1, 0),
            (BinOp::SBorrow, sign, 1, 1),
            (BinOp::SBorrow, sign - 1, max, 1),
            (BinOp::SBorrow, max, 1, 0),
            (BinOp::SBorrow, 1, 2, 0),
        ] {
            let registers = vec!["EAX".to_string()];
            let locals = LocalMap::new([("EAX".to_string(), 4)]);
            let ops = [IR::Bin {
                op: kind,
                dst: reg("EAX", 4),
                operands: Box::new((
                    Operand::Const { value: lhs, size },
                    Operand::Const { value: rhs, size },
                )),
            }];
            let translated = translate_ops(&ops, locals.clone()).unwrap();
            let module = module_with_snapshot(&translated, &locals, &registers, &[]);
            assert_eq!(
                execute(&module, "run", &registers)[0] as u32,
                expected,
                "{kind:?} at {bits} bits: {lhs:#x}, {rhs:#x}"
            );
        }
    }
}
