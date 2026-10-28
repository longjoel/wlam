//! Semantic tests for the x86 lifter.
//!
//! Each test lifts real machine code (compiler output, hand-assembled bytes, or
//! a window of the north-star executable) and then either
//!
//!  * executes the IR with a tiny reference interpreter in this file, or
//!  * runs the *original x86 bytes* with an independent CPU emulator (planned,
//!    see the harness crate). For now expected values are derived by hand from
//!    the ISA definition and asserted against the IR's computed result.
//!
//! The interpreter below is independent of the wasm emitter, so once the
//! emitter exists the differential tests compare *two* independent consumers of
//! the same IR: this interpreter and wasm. Day-to-day, it catches lifting bugs
//! such as wrong flag rules, missing partial-register handling, or a memory
//! operand read twice.

use wlam_lift_x86::{lift_block, lift_block_with_base, lift_instruction, LiftError};
use wlam_pcode::{BinaryOp, Op, Register, UnaryOp, Varnode};
use wlam_pcode::{Space, Terminator};

/// A deliberately simple, fully-validating reference interpreter for the IR.
/// It panics on unknown varnodes rather than guessing, so a lifter referring
/// to a register the interpreter does not know is a test failure.
struct Machine {
    /// 64 KiB of guest memory, addressed directly by value (enough for tests).
    mem: Vec<u8>,
    regs: std::collections::HashMap<&'static str, u64>,
    /// Storage for `Unique` temporaries, keyed by index. Held in a map rather
    /// than an array so that a *wrong* temp index (a lifter bug) shows up as a
    /// read of None rather than silently reading another temp's value.
    uniques: std::collections::HashMap<u64, (u64, u8)>,
}

impl Machine {
    fn new() -> Self {
        let mut regs = std::collections::HashMap::new();
        for r in wlam_pcode::REGISTERS {
            regs.insert(r.name, 0);
        }
        Machine {
            mem: vec![0; 1 << 16],
            regs,
            uniques: std::collections::HashMap::new(),
        }
    }

    fn set(&mut self, r: Register, v: u64) {
        self.regs.insert(r.name, v & wlam_pcode::mask(r.size));
    }

    fn get(&self, r: Register) -> u64 {
        *self.regs.get(r.name).expect("known register")
    }

    fn load(&self, addr: u64, size: u8) -> u64 {
        let mut v = 0u64;
        for i in (0..size as u64).rev() {
            v = (v << 8) | self.mem[(addr + i) as usize] as u64;
        }
        v
    }

    fn store(&mut self, addr: u64, size: u8, value: u64) {
        for i in 0..size as u64 {
            self.mem[(addr + i) as usize] = ((value >> (8 * i)) & 0xff) as u8;
        }
    }

    /// Execute a block; returns the terminator for inspection. Only the first
    /// `max_insns` instructions are run (tests use straight-line code).
    fn run(&mut self, block: &wlam_pcode::BasicBlock) {
        for insn in &block.instructions {
            for op in &insn.ops {
                self.exec(op);
            }
        }
    }

    fn value(&self, v: &Varnode) -> u64 {
        match v {
            Varnode::Const(c, _) => *c,
            Varnode::Addr(a, size) => match a.space {
                Space::Register => {
                    let r = wlam_pcode::REGISTERS[a.offset as usize];
                    assert_eq!(
                        r.size, *size,
                        "register {} used at wrong size {}",
                        r.name, size
                    );
                    self.get(r)
                }
                Space::Ram => self.load(a.offset, *size),
                Space::Unique => match self.uniques.get(&a.offset) {
                    Some((v, s)) => {
                        assert_eq!(
                            *s, *size,
                            "unique {} read at {} bytes, was written at {s}",
                            a.offset, size
                        );
                        *v
                    }
                    None => panic!("unique {} read before being written", a.offset),
                },
            },
        }
    }

    fn write(&mut self, v: &Varnode, value: u64) {
        match v {
            Varnode::Const(..) => panic!("write to constant"),
            Varnode::Addr(a, size) => match a.space {
                Space::Register => {
                    let r = wlam_pcode::REGISTERS[a.offset as usize];
                    assert_eq!(
                        r.size, *size,
                        "register {} written at wrong size {}",
                        r.name, size
                    );
                    let m = wlam_pcode::mask(*size);
                    // Partial registers are written as separate variables; the
                    // lifter is responsible for merging. Writing only this
                    // register here is exactly what the IR says.
                    self.regs.insert(r.name, value & m);
                }
                Space::Ram => self.store(a.offset, *size, value),
                Space::Unique => {
                    self.uniques
                        .insert(a.offset, (value & wlam_pcode::mask(*size), *size));
                }
            },
        }
    }

    /// Read an operand per the IR's documented semantics.
    /// Read an operand. Per the IR contract every `Addr` operand is an lvalue
    /// and is loaded; `ReadKind` only tells consumers whether the operand is
    /// also used as an address (store address, branch destination), which does
    /// not change the value read.
    fn read(&self, v: &Varnode, _kind: wlam_pcode::ReadKind) -> u64 {
        match v {
            Varnode::Const(c, _) => *c,
            Varnode::Addr(a, size) => match a.space {
                Space::Register => self.get(wlam_pcode::REGISTERS[a.offset as usize]),
                Space::Unique => match self.uniques.get(&a.offset) {
                    Some((val, s)) => {
                        assert_eq!(
                            *s, *size,
                            "unique {} read at {} bytes, written at {s}",
                            a.offset, size
                        );
                        *val
                    }
                    None => panic!("unique {} read before written", a.offset),
                },
                Space::Ram => panic!("RAM operands are read through Load, not as raw addresses"),
            },
        }
    }

    fn exec(&mut self, op: &Op) {
        let reads = op.reads();
        match op {
            Op::Copy { out, .. } => {
                let v = self.read(&reads[0].0, reads[0].1);
                self.write(out, v);
            }
            Op::Load { out, input, size } => {
                let addr = self.read(input, wlam_pcode::ReadKind::Rvalue);
                let v = self.load(addr, *size);
                self.write(out, v);
            }
            Op::Store { address, size, .. } => {
                let addr = self.read(&reads[0].0, reads[0].1);
                let val = self.read(&reads[1].0, reads[1].1);
                self.store(addr, *size, val);
            }
            Op::Binary { out, op, .. } => {
                let av = self.read(&reads[0].0, reads[0].1);
                let bv = self.read(&reads[1].0, reads[1].1);
                let size = reads[0].0.size();
                let bits = size as u32 * 8;
                let mask = wlam_pcode::mask(size);
                let sa = sign_extend(av, size);
                let sb = sign_extend(bv, size);
                let r = match op {
                    BinaryOp::IntAdd => av.wrapping_add(bv),
                    BinaryOp::IntSub => av.wrapping_sub(bv),
                    BinaryOp::IntXor => av ^ bv,
                    BinaryOp::IntAnd => av & bv,
                    BinaryOp::IntOr => av | bv,
                    BinaryOp::IntLeft => {
                        if bv >= bits as u64 {
                            0
                        } else {
                            av.wrapping_shl(bv as u32)
                        }
                    }
                    BinaryOp::IntRight => {
                        if bv >= bits as u64 {
                            0
                        } else {
                            av >> bv
                        }
                    }
                    BinaryOp::IntSRight => {
                        let shift = if bv >= bits as u64 {
                            bits - 1
                        } else {
                            bv as u32
                        };
                        ((sa >> shift) as u64) & mask
                    }
                    BinaryOp::IntMult => av.wrapping_mul(bv),
                    BinaryOp::IntEqual => (av == bv) as u64,
                    BinaryOp::IntNotEqual => (av != bv) as u64,
                    BinaryOp::IntLess => (av < bv) as u64,
                    BinaryOp::IntLessEqual => (av <= bv) as u64,
                    BinaryOp::IntSLess => (sa < sb) as u64,
                    BinaryOp::IntSLessEqual => (sa <= sb) as u64,
                    other => panic!("reference interpreter does not model {other:?} yet"),
                };
                self.write(out, r & mask);
            }
            Op::Unary { out, input, op } => {
                let v = self.read(input, wlam_pcode::ReadKind::Load);
                let size = input.size();
                let r = match op {
                    UnaryOp::BoolNegate => (v == 0) as u64,
                    UnaryOp::Int2Comp | UnaryOp::IntNegate => v.wrapping_neg(),
                    UnaryOp::IntZext => v,
                    UnaryOp::IntSext => sign_extend(v, size) as u64,
                    UnaryOp::Popcount => v.count_ones() as u64,
                    other => panic!("reference interpreter does not model {other:?}"),
                };
                self.write(out, r & wlam_pcode::mask(out.size()));
            }
            Op::BranchIndirect { out, .. } => {
                let cond = self.read(&reads[0].0, reads[0].1);
                let v = if cond != 0 {
                    self.read(&reads[1].0, reads[1].1)
                } else {
                    self.read(&reads[2].0, reads[2].1)
                };
                self.write(out, v);
            }
            Op::CBranch { .. } => {}
            Op::CallOther { name, .. } => panic!("reference interpreter reached host call {name}"),
            Op::Nop => {}
            Op::Branch { .. }
            | Op::Call { .. }
            | Op::CallInd { .. }
            | Op::Return
            | Op::ReturnTo { .. } => {}
        }
    }
}

fn sign_extend(v: u64, size: u8) -> i64 {
    let bits = size as u32 * 8;
    if bits >= 64 {
        v as i64
    } else {
        let shift = 64 - bits;
        ((v << shift) as i64) >> shift
    }
}

/// Lift `bytes` (one instruction or a straight-line run) and run it.
fn run(bytes: &[u8], setup: impl FnOnce(&mut Machine)) -> Machine {
    let (block, err) = lift_block(bytes, 0);
    assert!(err.is_none(), "lift error: {err:?}");
    let mut m = Machine::new();
    setup(&mut m);
    m.run(&block);
    m
}

fn enc(s: &str) -> Vec<u8> {
    // Tiny hex helper so tests read like the bytes they claim to test.
    s.split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).expect("hex"))
        .collect()
}

// ------------------------------------------------------------------ basics

#[test]
fn add_lifts_and_executes() {
    // mov eax, [esp+8]; add eax, [esp+4]; ret
    let bytes = enc("8b 44 24 08 03 44 24 04 c3");
    let m = run(&bytes, |m| {
        m.set(Register::ESP, 0x1000);
        m.store(0x1004, 4, 7);
        m.store(0x1008, 4, 35);
    });
    assert_eq!(m.get(Register::EAX), 42);
    assert_eq!(m.get(Register::ZE), 0);
    assert_eq!(m.get(Register::CE), 0);
}

#[test]
fn sub_sets_carry_on_borrow() {
    // mov eax,[esp+4]; sub eax,[esp+8]
    let bytes = enc("8b 44 24 04 2b 44 24 08");
    let m = run(&bytes, |m| {
        m.set(Register::ESP, 0x1000);
        m.store(0x1004, 4, 5);
        m.store(0x1008, 4, 9);
    });
    assert_eq!(m.get(Register::EAX), 5u64.wrapping_sub(9) & 0xffff_ffff);
    assert_eq!(m.get(Register::CE), 1, "CF must be set on borrow");
    assert_eq!(m.get(Register::SE), 1, "SF set: result is negative");
}

#[test]
fn signed_comparison_flags() {
    // cmp ecx, [esp+8] with ecx = 1, mem = 2 -> OF=0 SF=1 -> jl taken.
    let bytes = enc("3b 4c 24 08");
    let m = run(&bytes, |m| {
        m.set(Register::ESP, 0x1000);
        m.set(Register::ECX, 1);
        m.store(0x1008, 4, 2);
    });
    assert_eq!(m.get(Register::SE), 1);
    assert_eq!(m.get(Register::OE), 0);
    assert_eq!(m.get(Register::CE), 1, "1 < 2 unsigned too");
}

#[test]
fn setcc_produces_zero_or_one() {
    // setl al on 0x80000000 < 1 (signed): true.
    let bytes = enc("0f 9c c0");
    let m = run(&bytes, |m| {
        m.set(Register::SE, 1);
        m.set(Register::OE, 0);
    });
    assert_eq!(m.get(Register::AL), 1);

    // setl al with SF != OF
    let m = run(&bytes, |m| {
        m.set(Register::SE, 1);
        m.set(Register::OE, 1);
    });
    assert_eq!(m.get(Register::AL), 0);
}

#[test]
fn jcc_condition_encoding() {
    // cmp eax, 3 ; jb -> taken when below
    let bytes = enc("83 f8 03 72 05");
    let (block, err) = lift_block(&bytes, 0);
    assert!(err.is_none(), "{err:?}");
    assert!(matches!(block.terminator, Terminator::Conditional { .. }));
    let mut m = Machine::new();
    m.set(Register::EAX, 2);
    m.run(&block);
    assert_eq!(m.get(Register::CE), 1);
    assert_eq!(m.get(Register::ZE), 0);
}

#[test]
fn inc_preserves_carry_flag() {
    let bytes = enc("40"); // inc eax
    let m = run(&bytes, |m| {
        m.set(Register::EAX, 0xffff_ffff);
        m.set(Register::CE, 1);
    });
    assert_eq!(m.get(Register::EAX), 0);
    assert_eq!(m.get(Register::ZE), 1);
    assert_eq!(m.get(Register::CE), 1, "INC must not touch CF");
}

#[test]
fn partial_register_writes() {
    // mov ax, cx  (66 89 c8) with EAX = 0x11110000, ECX = 0x22223333
    let bytes = enc("66 89 c8");
    let m = run(&bytes, |m| {
        m.set(Register::EAX, 0x1111_0000);
        m.set(Register::ECX, 0x2222_3333);
    });
    assert_eq!(m.get(Register::EAX), 0x1111_3333, "upper 16 bits preserved");

    // mov ah, dl (88 d4): EAX = 0x11223344, DL = 0xAB -> EAX = 0x1122AB44
    let bytes = enc("88 d4");
    let m = run(&bytes, |m| {
        m.set(Register::EAX, 0x1122_3344);
        m.set(Register::EDX, 0xAA_AA_AB);
    });
    assert_eq!(
        m.get(Register::EAX),
        0x1122_AB44,
        "AH write preserves neighbours"
    );

    // mov al, cl -> only low byte
    let bytes = enc("88 c8");
    let m = run(&bytes, |m| {
        m.set(Register::EAX, 0x1122_3344);
        m.set(Register::ECX, 0x5566_77EE);
    });
    assert_eq!(m.get(Register::EAX), 0x1122_33EE);
}

#[test]
fn sign_extension_instructions() {
    // cbw with AL=0x80 -> AX=0xff80
    let m = run(&enc("66 98"), |m| m.set(Register::AL, 0x80));
    assert_eq!(m.get(Register::AX), 0xff80);
    // cwde with AX=0x8000 -> EAX=0xffff8000
    let m = run(&enc("98"), |m| m.set(Register::AX, 0x8000));
    assert_eq!(m.get(Register::EAX), 0xffff_8000);
    // cdq with EAX negative -> EDX=0xffffffff
    let m = run(&enc("99"), |m| m.set(Register::EAX, 0x8000_0000));
    assert_eq!(m.get(Register::EDX), 0xffff_ffff);
    let m = run(&enc("99"), |m| m.set(Register::EAX, 0x7fff_ffff));
    assert_eq!(m.get(Register::EDX), 0);
}

#[test]
fn shifts_by_zero_leave_flags_alone() {
    // shl eax, cl with cl = 0: flags must be unchanged.
    let bytes = enc("d3 e0");
    let m = run(&bytes, |m| {
        m.set(Register::EAX, 1);
        m.set(Register::ECX, 0);
        m.set(Register::ZE, 1);
        m.set(Register::CE, 1);
    });
    assert_eq!(m.get(Register::EAX), 1);
    assert_eq!(m.get(Register::ZE), 1, "ZF preserved for count 0");
    assert_eq!(m.get(Register::CE), 1, "CF preserved for count 0");

    // shl eax, cl with cl = 1
    let m = run(&bytes, |m| {
        m.set(Register::EAX, 0x8000_0001);
        m.set(Register::ECX, 1);
    });
    assert_eq!(m.get(Register::EAX), 0x0000_0002);
    assert_eq!(m.get(Register::CE), 1, "last bit shifted out was 1");
}

#[test]
fn sar_is_arithmetic() {
    let m = run(&enc("c1 f8 04"), |m| m.set(Register::EAX, 0xffff_ff00u64));
    assert_eq!(m.get(Register::EAX), 0xffff_fff0);
}

#[test]
fn lea_with_scale() {
    // lea eax, [eax + eax*4 + 0xa]  (8d 44 80 0a)
    let m = run(&enc("8d 44 80 0a"), |m| m.set(Register::EAX, 3));
    assert_eq!(m.get(Register::EAX), 3 + 3 * 4 + 10);
}

#[test]
fn memory_operand_with_ebp_is_negative_displacement() {
    // mov eax, [ebp-0x18] (8b 45 e8)
    let m = run(&enc("8b 45 e8"), |m| {
        m.set(Register::EBP, 0x2000);
        m.store(0x2000 - 0x18, 4, 0xdead_beef);
    });
    assert_eq!(m.get(Register::EAX), 0xdead_beef);
}

#[test]
fn byte_load_zero_extends() {
    // movzbl eax, byte [eax+3] (0f b6 40 03)
    let m = run(&enc("0f b6 40 03"), |m| {
        m.set(Register::EAX, 0x1000 - 3);
        m.mem[0x1000] = 0x80;
    });
    assert_eq!(m.get(Register::EAX), 0x80, "MOVZX never sign-extends");
}

#[test]
fn push_and_pop_round_trip_through_stack() {
    // push eax (50); pop ebx (5b)
    let m = run(&enc("50 5b"), |m| {
        m.set(Register::ESP, 0x1000);
        m.set(Register::EAX, 0xcafe_f00d);
    });
    assert_eq!(m.get(Register::ESP), 0x1000);
    assert_eq!(m.get(Register::EBX), 0xcafe_f00d);
}

#[test]
fn push_imm8_sign_extends() {
    // push -1 (6a ff): stack holds 0xffffffff
    let m = run(&enc("6a ff"), |m| m.set(Register::ESP, 0x1000));
    assert_eq!(m.get(Register::ESP), 0xffc);
    assert_eq!(m.mem[0xffc..0x1000], [0xff, 0xff, 0xff, 0xff]);
}

#[test]
fn sahf_and_lahf_round_trip() {
    // sahf (9e) with AH=0xd7 -> SF ZF AC PF CF set, OF untouched
    let m = run(&enc("9e"), |m| {
        m.set(Register::AH, 0xd7);
        m.set(Register::OE, 1);
    });
    assert_eq!(m.get(Register::SE), 1);
    assert_eq!(m.get(Register::ZE), 1);
    assert_eq!(m.get(Register::AC), 1);
    assert_eq!(m.get(Register::PE), 1);
    assert_eq!(m.get(Register::CE), 1);
    assert_eq!(m.get(Register::OE), 1, "SAHF does not touch OF");

    // lahf (9f) with SF=1 ZF=0 AF=1 PF=1 CF=1 -> AH=0x97
    let m = run(&enc("9f"), |m| {
        m.set(Register::SE, 1);
        m.set(Register::ZE, 0);
        m.set(Register::AC, 1);
        m.set(Register::PE, 1);
        m.set(Register::CE, 1);
    });
    assert_eq!(m.get(Register::AH), 0x97);
}

#[test]
fn neg_flags() {
    // neg eax of INT_MIN: OF=1 CF=1
    let m = run(&enc("f7 d8"), |m| m.set(Register::EAX, 0x8000_0000));
    assert_eq!(m.get(Register::EAX), 0x8000_0000);
    assert_eq!(m.get(Register::OE), 1);
    assert_eq!(m.get(Register::CE), 1);
}

#[test]
fn bswap_reverses_bytes() {
    let m = run(&enc("0f c8"), |m| m.set(Register::EAX, 0x1122_3344));
    assert_eq!(m.get(Register::EAX), 0x4433_2211);
}

#[test]
fn imul_two_operand_truncates_and_sets_overflow() {
    // imul eax, ecx (0f af c1)
    let m = run(&enc("0f af c1"), |m| {
        m.set(Register::EAX, 0x1000_0000);
        m.set(Register::ECX, 4);
    });
    assert_eq!(m.get(Register::EAX), 0);
    assert_eq!(m.get(Register::OE), 1, "product does not fit in 32 bits");
    assert_eq!(m.get(Register::CE), 1);
}

#[test]
fn cmov_selects_without_touching_flags() {
    // cmovl ecx, eax (0f 4c c8) with SF=1 OF=0 -> ecx = eax
    let m = run(&enc("0f 4c c8"), |m| {
        m.set(Register::SE, 1);
        m.set(Register::OE, 0);
        m.set(Register::EAX, 7);
        m.set(Register::ECX, 9);
    });
    assert_eq!(m.get(Register::ECX), 7);
    let m = run(&enc("0f 4c c8"), |m| {
        m.set(Register::SE, 0);
        m.set(Register::OE, 0);
        m.set(Register::EAX, 7);
        m.set(Register::ECX, 9);
    });
    assert_eq!(m.get(Register::ECX), 9);
}

#[test]
fn call_pushes_return_address_and_marks_terminator() {
    // call rel32 (e8 00 00 00 00): target = next ip
    let bytes = enc("e8 00 00 00 00");
    let (block, err) = lift_block(&bytes, 0x1000);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(
        block.terminator,
        Terminator::Call {
            target: 0x1005,
            ret: 0x1005
        }
    );
    let mut m = Machine::new();
    m.set(Register::ESP, 0x2000);
    m.run(&block);
    assert_eq!(m.get(Register::ESP), 0x1ffc);
    assert_eq!(m.load(0x1ffc, 4), 0x1005, "return address is on the stack");
}

#[test]
fn ret_pops_and_returns() {
    let bytes = enc("c3");
    let (block, err) = lift_block(&bytes, 0x1000);
    assert!(err.is_none(), "{err:?}");
    assert_eq!(block.terminator, Terminator::Return);
    let mut m = Machine::new();
    m.set(Register::ESP, 0x2000);
    m.store(0x2000, 4, 0x900);
    m.run(&block);
    assert_eq!(m.get(Register::ESP), 0x2004);
}

// --------------------------------------------------- real compiler output

#[test]
fn clang_addition_and_lea_sequence() {
    // From clang -O2 --target=i686-pc-windows-msvc for
    //   int f(int a){ return a*3; }
    // 8b 44 24 04   mov eax, [esp+4]
    // 8d 0c 40      lea ecx, [eax+eax*2]
    // c3            ret
    let bytes = enc("8b 44 24 04 8d 0c 40 c3");
    let (block, err) = lift_block(&bytes, 0);
    assert!(err.is_none(), "{err:?}");
    let mut m = Machine::new();
    m.set(Register::ESP, 0x1000);
    m.store(0x1004, 4, 14);
    m.run(&block);
    assert_eq!(m.get(Register::ECX), 42);
}

#[test]
fn jump_table_style_load_lifts() {
    // mov eax, [ecx*4 + 0x10]  (8b 04 8d 10 00 00 00) -- clang's switch idiom
    let m = run(&enc("8b 04 8d 10 00 00 00"), |m| {
        m.set(Register::ECX, 2);
        m.store(0x10 + 8, 4, 0x30);
    });
    assert_eq!(m.get(Register::EAX), 0x30);
}

// ------------------------------------- unsupported code must fail loudly

#[test]
fn fs_segment_addressing_is_rejected_not_ignored() {
    // mov eax, fs:[0] (64 a1 00 00 00 00)
    let bytes = enc("64 a1 00 00 00 00");
    let (block, err) = lift_block(&bytes, 0xccb2);
    match err {
        Some(LiftError::Unsupported { at, reason, .. }) => {
            assert_eq!(at, 0xccb2);
            assert!(reason.contains("FS/GS"), "{reason}");
        }
        other => panic!("expected an explicit unsupported error, got {other:?}"),
    }
    // The block still ends in a Trap so an emitted program stops here.
    assert!(matches!(block.terminator, Terminator::Trap { .. }));
}

#[test]
fn rep_string_op_reports_that_loop_lowering_is_missing() {
    // rep movsd (f3 a5)
    let bytes = enc("f3 a5");
    let (_, err) = lift_block(&bytes, 0);
    match err {
        Some(LiftError::Unsupported { reason, .. }) => {
            assert!(reason.contains("REP"), "{reason}");
        }
        other => panic!("expected unsupported REP error, got {other:?}"),
    }
}

#[test]
fn non_rep_movsd_lifts_as_one_iteration() {
    // movsd (a5): copy 4 bytes ESI->EDI, advance both
    let m = run(&enc("a5"), |m| {
        m.set(Register::ESI, 0x100);
        m.set(Register::EDI, 0x200);
        m.store(0x100, 4, 0x1122_3344);
    });
    assert_eq!(m.load(0x200, 4), 0x1122_3344);
    assert_eq!(m.get(Register::ESI), 0x104);
    assert_eq!(m.get(Register::EDI), 0x204);
}

// -------------------------------------------- the north-star executable

/// The real prologue of Uxn32.exe at its entry point (RVA 0xccb2), through the
/// `call dword ptr [41106Ch]`. Everything except the `fs:[0]` SEH prologue must
/// lift; the fs access must produce an explicit error.
#[test]
fn uxn32_entry_prologue_behavior() {
    let bytes = enc(
        "55 8b ec 6a ff 68 d0 13 41 00 68 7c d9 40 00 64 a1 00 00 00 00 50 64 89 25 00 00 00 00 83 ec 58 53 56 57 89 65 e8 ff 15 6c 10 41 00",
    );
    let (block, err) = lift_block_with_base(&bytes, 0x0040_0000, 0xccb2);
    // Lifting proceeds through the standard prologue and stops exactly at the
    // SEH `mov eax, fs:[0]`, which is reported rather than silently dropped.
    let insns: Vec<u64> = block.instructions.iter().map(|i| i.address).collect();
    assert_eq!(
        insns,
        vec![
            0x400000 + 0xccb2,
            0x400000 + 0xccb3,
            0x400000 + 0xccb5,
            0x400000 + 0xccb7,
            0x400000 + 0xccbc,
            0x400000 + 0xccc1
        ],
        "the prologue instructions lift in order"
    );
    assert!(err.is_some(), "the fs:[0] read must be reported");

    // Run the lifted prologue up to that point: 5 instructions.
    let mut partial = block.clone();
    partial.instructions.truncate(5);
    let mut m = Machine::new();
    m.set(Register::ESP, 0x0012_ff00);
    m.run(&partial);
    assert_eq!(m.get(Register::EBP), 0x0012_ff00, "mov ebp, esp");
    assert_eq!(
        m.get(Register::ESP),
        0x0012_fef0,
        "three pushes + sub esp, 0x58"
    );
    assert_eq!(m.load(0x0012_fef0, 4), 0xffff_ffff, "push -1 (SEH scope)");
    assert_eq!(m.load(0x0012_fef4, 4), 0x4113_d0, "SEH handler address");
}

#[test]
fn indirect_call_through_iat_lifts() {
    // call dword ptr [0x41106c] (ff 15 6c 10 41 00)
    let bytes = enc("ff 15 6c 10 41 00");
    let (block, err) = lift_block_with_base(&bytes, 0x40_0000, 0xccd8);
    assert!(err.is_none(), "{err:?}");
    assert!(matches!(block.terminator, Terminator::IndirectCall { .. }));
    let mut m = Machine::new();
    m.set(Register::ESP, 0x1000);
    m.store(0x41106c, 4, 0x7c80_1234);
    m.run(&block);
    assert_eq!(m.get(Register::ESP), 0xffc, "return address pushed");
    assert_eq!(m.load(0xffc, 4), 0xccde, "returns to the next instruction");
}
