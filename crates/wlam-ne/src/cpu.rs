//! Checked instruction-step executor for the useful integer/memory subset of
//! the 16-bit x86 decoder's P-code. Unsupported operations trap explicitly.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use rsleigh_api::{AddressSpaceId, Decoder, PcodeOp, Varnode};

use crate::{NeAccess, NeError, NeFarAddress, NeImage, NeLoadedImage};

const REGISTER_BYTES: usize = 0x2000;
const SP_OFFSET: u64 = 0x10;
const BP_OFFSET: u64 = 0x14;
const SI_OFFSET: u64 = 0x18;
const DI_OFFSET: u64 = 0x1c;
const CX_OFFSET: u64 = 0x04;
const BX_OFFSET: u64 = 0x0c;
const ES_OFFSET: u64 = 0x100;
const CS_OFFSET: u64 = 0x102;
const SS_OFFSET: u64 = 0x104;
const DS_OFFSET: u64 = 0x106;
const EIP_OFFSET: u64 = 0x284;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NeCpuFlow {
    Sequential,
    Branch,
    Call,
    Return,
}

#[derive(Clone, Debug)]
pub struct NeStepResult {
    pub cs: u16,
    pub ip: u16,
    pub next_cs: u16,
    pub next_ip: u16,
    pub flow: NeCpuFlow,
    pub disassembly: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NeTaskStartContext {
    pub psp_selector: u16,
    pub instance: u16,
    pub previous_instance: u16,
    /// Parent task's PSP selector. Zero is appropriate for the initial task.
    pub parent_psp: u16,
    /// Selector of a DOS-style environment block, or zero when unavailable.
    pub environment_selector: u16,
    /// KERNEL DOS3Call far entry used by the PSP's INT 21h dispatcher stub.
    pub dos_dispatcher: NeFarAddress,
    /// Arguments after the executable name, in the guest's active code page.
    pub command_line: Vec<u8>,
}

#[derive(Clone)]
pub struct NeMachine {
    registers: Vec<u8>,
    cs: u16,
    ip: u16,
}

impl NeMachine {
    pub fn new(image: &NeImage, loaded: &NeLoadedImage) -> Result<Self, NeError> {
        let start = image.start_state(loaded)?;
        let mut machine = Self {
            registers: vec![0; REGISTER_BYTES],
            cs: start.cs,
            ip: start.ip,
        };
        machine.write_register(CS_OFFSET, 2, u64::from(start.cs))?;
        machine.write_register(SS_OFFSET, 2, u64::from(start.ss))?;
        machine.write_register(SP_OFFSET, 2, u64::from(start.sp))?;
        let data_selector = if image.auto_data_segment == 0 {
            0
        } else {
            u64::from(loaded.selector_for_segment(image.auto_data_segment).ok_or(
                NeError::MissingSegment {
                    segment: image.auto_data_segment,
                },
            )?)
        };
        machine.write_register(DS_OFFSET, 2, data_selector)?;
        // Task startup supplies ES with the PSP selector. Keep it null until
        // that separate task context has been allocated.
        machine.write_register(ES_OFFSET, 2, 0)?;
        machine.write_register(EIP_OFFSET, 4, pack_far(start.cs, start.ip))?;
        Ok(machine)
    }

    pub fn cs(&self) -> u16 {
        self.cs
    }

    pub fn ip(&self) -> u16 {
        self.ip
    }

    /// Byte-addressed register ABI shared with the NE Wasm compiler.
    pub fn register_bytes(&self) -> &[u8] {
        &self.registers
    }

    pub fn restore_registers(&mut self, bytes: &[u8]) -> Result<(), NeError> {
        if bytes.len() != REGISTER_BYTES {
            return Err(NeError::UnsupportedPcode {
                index: 0,
                operation: "incorrect register snapshot size".into(),
            });
        }
        self.registers.copy_from_slice(bytes);
        let pc = self.register(EIP_OFFSET, 4)?;
        (self.cs, self.ip) = unpack_pointer(pc)?;
        self.write_register(CS_OFFSET, 2, u64::from(self.cs))
    }

    pub fn register(&self, offset: u64, size: u32) -> Result<u64, NeError> {
        let node = Varnode::register(offset, size);
        self.read_node(&node, &BTreeMap::new())
    }

    /// Set a guest register while keeping the machine's packed CS:IP state in
    /// sync with the decoder register file.
    pub fn set_register(&mut self, offset: u64, size: u32, value: u64) -> Result<(), NeError> {
        self.write_register(offset, size, value)?;
        let end = offset.saturating_add(u64::from(size));
        let overlaps_cs = offset < CS_OFFSET + 2 && end > CS_OFFSET;
        let overlaps_eip = offset < EIP_OFFSET + 4 && end > EIP_OFFSET;
        if overlaps_eip {
            let packed = self.register(EIP_OFFSET, 4)?;
            (self.cs, self.ip) = unpack_pointer(packed)?;
            self.write_register(CS_OFFSET, 2, u64::from(self.cs))?;
        } else if overlaps_cs {
            self.cs = self.register(CS_OFFSET, 2)? as u16;
            self.write_register(EIP_OFFSET, 4, pack_far(self.cs, self.ip))?;
        }
        Ok(())
    }

    /// Populate the Win16 EXE entry registers once task memory and instance
    /// handles have been allocated by the caller.
    pub fn initialize_task_start(
        &mut self,
        image: &NeImage,
        loaded: &NeLoadedImage,
        context: &NeTaskStartContext,
    ) -> Result<(), NeError> {
        loaded.translate(context.psp_selector, 0, 0x100, NeAccess::Write)?;
        self.set_register(0, 2, 0)?; // AX
        self.set_register(BX_OFFSET, 2, u64::from(image.stack_size))?;
        self.set_register(CX_OFFSET, 2, u64::from(image.heap_size))?;
        self.set_register(SI_OFFSET, 2, u64::from(context.previous_instance))?;
        self.set_register(DI_OFFSET, 2, u64::from(context.instance))?;
        self.set_register(BP_OFFSET, 2, 0)?;
        self.set_register(ES_OFFSET, 2, u64::from(context.psp_selector))?;
        Ok(())
    }

    /// Populate an allocated 256-byte PSP/PDB using the Win16 task layout.
    pub fn initialize_task_psp(
        loaded: &mut NeLoadedImage,
        context: &NeTaskStartContext,
    ) -> Result<(), NeError> {
        loaded.translate(context.psp_selector, 0, 0x100, NeAccess::Write)?;
        loaded.write(context.psp_selector, 0, &initialize_psp(context))
    }

    pub fn step(
        &mut self,
        loaded: &mut NeLoadedImage,
        decoder: &mut Decoder,
    ) -> Result<NeStepResult, NeError> {
        let decoded = loaded.decode_next(decoder, self.cs, self.ip)?;
        let original_cs = self.cs;
        let original_ip = self.ip;
        let mut unique: BTreeMap<u64, u8> = BTreeMap::new();
        let mut transfer = None;
        for (index, op) in decoded.instruction.ops.iter().enumerate() {
            match op {
                PcodeOp::Copy { out, input } => {
                    let value = self.read_node(input, &unique)?;
                    self.write_node(out, value, &mut unique)?;
                }
                PcodeOp::IntAdd { out, left, right } => {
                    self.binary(out, left, right, &mut unique, |a, b, _| a.wrapping_add(b))?;
                }
                PcodeOp::IntSub { out, left, right } => {
                    self.binary(out, left, right, &mut unique, |a, b, _| a.wrapping_sub(b))?;
                }
                PcodeOp::IntMult { out, left, right } => {
                    self.binary(out, left, right, &mut unique, |a, b, _| a.wrapping_mul(b))?;
                }
                PcodeOp::IntDiv { out, left, right } => {
                    self.require_nonzero(right, &unique)?;
                    self.binary(out, left, right, &mut unique, |a, b, _| a / b)?;
                }
                PcodeOp::IntSDiv { out, left, right } => {
                    self.require_nonzero(right, &unique)?;
                    self.binary(out, left, right, &mut unique, |a, b, bits| {
                        signed(a, bits).wrapping_div(signed(b, bits)) as u64
                    })?;
                }
                PcodeOp::IntRem { out, left, right } => {
                    self.require_nonzero(right, &unique)?;
                    self.binary(out, left, right, &mut unique, |a, b, _| a % b)?;
                }
                PcodeOp::IntSRem { out, left, right } => {
                    self.require_nonzero(right, &unique)?;
                    self.binary(out, left, right, &mut unique, |a, b, bits| {
                        signed(a, bits).wrapping_rem(signed(b, bits)) as u64
                    })?;
                }
                PcodeOp::IntAnd { out, left, right } => {
                    self.binary(out, left, right, &mut unique, |a, b, _| a & b)?;
                }
                PcodeOp::IntOr { out, left, right } => {
                    self.binary(out, left, right, &mut unique, |a, b, _| a | b)?;
                }
                PcodeOp::IntXor { out, left, right } => {
                    self.binary(out, left, right, &mut unique, |a, b, _| a ^ b)?;
                }
                PcodeOp::IntEq { out, left, right } => {
                    self.binary(out, left, right, &mut unique, |a, b, _| u64::from(a == b))?;
                }
                PcodeOp::IntNotEq { out, left, right } => {
                    self.binary(out, left, right, &mut unique, |a, b, _| u64::from(a != b))?;
                }
                PcodeOp::IntLess { out, left, right } => {
                    self.binary(out, left, right, &mut unique, |a, b, _| u64::from(a < b))?;
                }
                PcodeOp::IntLessEq { out, left, right } => {
                    self.binary(out, left, right, &mut unique, |a, b, _| u64::from(a <= b))?;
                }
                PcodeOp::IntSLess { out, left, right } => {
                    self.binary(out, left, right, &mut unique, |a, b, bits| {
                        u64::from(signed(a, bits) < signed(b, bits))
                    })?;
                }
                PcodeOp::IntSLessEq { out, left, right } => {
                    self.binary(out, left, right, &mut unique, |a, b, bits| {
                        u64::from(signed(a, bits) <= signed(b, bits))
                    })?;
                }
                PcodeOp::IntCarry { out, left, right } => {
                    self.binary(out, left, right, &mut unique, |a, b, bits| {
                        if bits >= 64 {
                            u64::from(a.overflowing_add(b).1)
                        } else {
                            u64::from((a & bit_mask(bits)) + (b & bit_mask(bits)) > bit_mask(bits))
                        }
                    })?;
                }
                PcodeOp::IntSCarry { out, left, right } => {
                    self.binary(out, left, right, &mut unique, |a, b, bits| {
                        let mask = bit_mask(bits);
                        let result = a.wrapping_add(b) & mask;
                        let sign = 1u64 << bits.saturating_sub(1);
                        u64::from((!(a ^ b) & (a ^ result) & sign) != 0)
                    })?;
                }
                PcodeOp::IntSBorrow { out, left, right } => {
                    self.binary(out, left, right, &mut unique, |a, b, bits| {
                        let mask = bit_mask(bits);
                        let result = a.wrapping_sub(b) & mask;
                        let sign = 1u64 << bits.saturating_sub(1);
                        u64::from(((a ^ b) & (a ^ result) & sign) != 0)
                    })?;
                }
                PcodeOp::IntNeg { out, input } => {
                    let value = self.read_node(input, &unique)?;
                    self.write_node(out, value.wrapping_neg(), &mut unique)?;
                }
                PcodeOp::IntNot { out, input } => {
                    let value = self.read_node(input, &unique)?;
                    self.write_node(out, !value, &mut unique)?;
                }
                PcodeOp::IntZext { out, input } => {
                    let value = self.read_node(input, &unique)?;
                    self.write_node(out, value, &mut unique)?;
                }
                PcodeOp::IntSext { out, input } => {
                    let value = self.read_node(input, &unique)?;
                    let bits = input.size.saturating_mul(8);
                    self.write_node(out, signed(value, bits) as u64, &mut unique)?;
                }
                PcodeOp::Subpiece { out, input, lsb } => {
                    let value = self.read_node(input, &unique)?;
                    let piece = if *lsb >= 8 { 0 } else { value >> (lsb * 8) };
                    self.write_node(out, piece, &mut unique)?;
                }
                PcodeOp::BoolAnd { out, left, right } => {
                    self.binary(out, left, right, &mut unique, |a, b, _| {
                        u64::from(a != 0 && b != 0)
                    })?;
                }
                PcodeOp::BoolOr { out, left, right } => {
                    self.binary(out, left, right, &mut unique, |a, b, _| {
                        u64::from(a != 0 || b != 0)
                    })?;
                }
                PcodeOp::BoolXor { out, left, right } => {
                    self.binary(out, left, right, &mut unique, |a, b, _| {
                        u64::from((a != 0) ^ (b != 0))
                    })?;
                }
                PcodeOp::BoolNot { out, input } => {
                    let value = self.read_node(input, &unique)?;
                    self.write_node(out, u64::from(value == 0), &mut unique)?;
                }
                PcodeOp::IntLsl { out, left, right } => {
                    self.binary(out, left, right, &mut unique, |a, b, _| {
                        if b >= u64::from(left.size.saturating_mul(8)) {
                            0
                        } else {
                            a << (b as u32)
                        }
                    })?;
                }
                PcodeOp::IntLsr { out, left, right } => {
                    self.binary(out, left, right, &mut unique, |a, b, _| {
                        if b >= u64::from(left.size.saturating_mul(8)) {
                            0
                        } else {
                            a >> (b as u32)
                        }
                    })?;
                }
                PcodeOp::IntAsr { out, left, right } => {
                    self.binary(out, left, right, &mut unique, |a, b, bits| {
                        if b >= u64::from(bits) {
                            u64::from(signed(a, bits) < 0).wrapping_neg()
                        } else {
                            (signed(a, bits) >> (b as u32)) as u64
                        }
                    })?;
                }
                PcodeOp::Popcount { out, input } => {
                    let value = self.read_node(input, &unique)?;
                    self.write_node(out, u64::from(value.count_ones()), &mut unique)?;
                }
                PcodeOp::Load { out, space, ptr } if *space == AddressSpaceId::Ram => {
                    let pointer = self.read_node(ptr, &unique)?;
                    let (selector, offset) = unpack_pointer(pointer)?;
                    let data =
                        loaded.read(selector, u32::from(offset), out.size, NeAccess::Read)?;
                    self.write_node(out, bytes_to_u64(&data), &mut unique)?;
                }
                PcodeOp::Store { space, ptr, val } if *space == AddressSpaceId::Ram => {
                    let pointer = self.read_node(ptr, &unique)?;
                    let (selector, offset) = unpack_pointer(pointer)?;
                    let value = self.read_node(val, &unique)?;
                    loaded.write(selector, u32::from(offset), &u64_to_bytes(value, val.size)?)?;
                }
                // An instruction executes atomically in this single-task
                // bridge. LOCK/UNLOCK delimit that instruction's memory RMW.
                PcodeOp::CallOther {
                    out: None,
                    func_id: 17 | 18,
                    inputs,
                } if inputs.is_empty() => {}
                PcodeOp::CallOther {
                    out,
                    func_id: 0,
                    inputs,
                } if inputs.len() == 2 => {
                    let selector = self.read_node(&inputs[0], &unique)? as u16;
                    let offset = self.read_node(&inputs[1], &unique)? as u16;
                    if let Some(out) = out {
                        self.write_node(out, pack_far(selector, offset), &mut unique)?;
                    }
                }
                PcodeOp::Branch { dest } | PcodeOp::Call { dest } | PcodeOp::Return { dest } => {
                    let value = branch_target(dest, self, &unique)?;
                    let flow = match op {
                        PcodeOp::Branch { .. } => NeCpuFlow::Branch,
                        PcodeOp::Call { .. } => NeCpuFlow::Call,
                        _ => NeCpuFlow::Return,
                    };
                    transfer = Some((value, flow));
                    break;
                }
                PcodeOp::CBranch { dest, cond } => {
                    if self.read_node(cond, &unique)? != 0 {
                        transfer = Some((branch_target(dest, self, &unique)?, NeCpuFlow::Branch));
                        break;
                    }
                }
                PcodeOp::BranchInd { dest } | PcodeOp::CallInd { dest } => {
                    let value = indirect_target(dest, self, &unique)?;
                    let flow = if matches!(op, PcodeOp::CallInd { .. }) {
                        NeCpuFlow::Call
                    } else {
                        NeCpuFlow::Branch
                    };
                    transfer = Some((value, flow));
                    break;
                }
                other => {
                    return Err(NeError::UnsupportedPcode {
                        index,
                        operation: operation_name(other),
                    });
                }
            }
        }

        let (next_cs, next_ip, flow) = if let Some((target, flow)) = transfer {
            let (cs, ip) = unpack_pointer(target)?;
            (cs, ip, flow)
        } else {
            (self.cs, decoded.next_ip, NeCpuFlow::Sequential)
        };
        self.cs = next_cs;
        self.ip = next_ip;
        self.write_register(CS_OFFSET, 2, u64::from(next_cs))?;
        self.write_register(EIP_OFFSET, 4, pack_far(next_cs, next_ip))?;
        Ok(NeStepResult {
            cs: original_cs,
            ip: original_ip,
            next_cs,
            next_ip,
            flow,
            disassembly: decoded.instruction.disassembly,
        })
    }

    fn binary<F>(
        &mut self,
        out: &Varnode,
        left: &Varnode,
        right: &Varnode,
        unique: &mut BTreeMap<u64, u8>,
        operation: F,
    ) -> Result<(), NeError>
    where
        F: FnOnce(u64, u64, u32) -> u64,
    {
        let left_value = self.read_node(left, unique)?;
        let right_value = self.read_node(right, unique)?;
        self.write_node(
            out,
            operation(
                left_value,
                right_value,
                left.size.saturating_mul(8),
            ),
            unique,
        )
    }

    fn require_nonzero(
        &self,
        divisor: &Varnode,
        unique: &BTreeMap<u64, u8>,
    ) -> Result<(), NeError> {
        if self.read_node(divisor, unique)? == 0 {
            Err(NeError::UnsupportedPcode {
                index: 0,
                operation: String::from("integer division by zero"),
            })
        } else {
            Ok(())
        }
    }

    fn read_node(&self, node: &Varnode, unique: &BTreeMap<u64, u8>) -> Result<u64, NeError> {
        let mask = value_mask(node.size)?;
        Ok(match node.space {
            AddressSpaceId::Const | AddressSpaceId::Ram => node.offset & mask,
            AddressSpaceId::Unique => {
                let mut value = 0;
                for index in 0..node.size {
                    let offset = node.offset.checked_add(u64::from(index)).ok_or(
                        NeError::UnsupportedPcode {
                            index: 0,
                            operation: String::from("unique varnode offset overflow"),
                        },
                    )?;
                    value |= u64::from(*unique.get(&offset).unwrap_or(&0)) << (index * 8);
                }
                value & mask
            }
            AddressSpaceId::Register => {
                let start =
                    usize::try_from(node.offset).map_err(|_| NeError::UnsupportedPcode {
                        index: 0,
                        operation: String::from("register offset overflow"),
                    })?;
                let end =
                    start
                        .checked_add(node.size as usize)
                        .ok_or(NeError::UnsupportedPcode {
                            index: 0,
                            operation: String::from("register range overflow"),
                        })?;
                let bytes = self
                    .registers
                    .get(start..end)
                    .ok_or(NeError::UnsupportedPcode {
                        index: 0,
                        operation: String::from("register range outside register file"),
                    })?;
                bytes_to_u64(bytes) & mask
            }
        })
    }

    fn write_node(
        &mut self,
        node: &Varnode,
        value: u64,
        unique: &mut BTreeMap<u64, u8>,
    ) -> Result<(), NeError> {
        let value = value & value_mask(node.size)?;
        match node.space {
            AddressSpaceId::Unique => {
                for index in 0..node.size {
                    let offset = node.offset.checked_add(u64::from(index)).ok_or(
                        NeError::UnsupportedPcode {
                            index: 0,
                            operation: String::from("unique varnode offset overflow"),
                        },
                    )?;
                    unique.insert(offset, (value >> (index * 8)) as u8);
                }
                Ok(())
            }
            AddressSpaceId::Register => self.write_register(node.offset, node.size, value),
            AddressSpaceId::Const | AddressSpaceId::Ram => Err(NeError::UnsupportedPcode {
                index: 0,
                operation: String::from("write to constant or RAM varnode"),
            }),
        }
    }

    fn write_register(&mut self, offset: u64, size: u32, value: u64) -> Result<(), NeError> {
        if size > 8 {
            return Err(NeError::UnsupportedPcode {
                index: 0,
                operation: format!("{size}-byte register write"),
            });
        }
        let start = usize::try_from(offset).map_err(|_| NeError::UnsupportedPcode {
            index: 0,
            operation: String::from("register offset overflow"),
        })?;
        let end = start
            .checked_add(size as usize)
            .ok_or(NeError::UnsupportedPcode {
                index: 0,
                operation: String::from("register range overflow"),
            })?;
        let target = self
            .registers
            .get_mut(start..end)
            .ok_or(NeError::UnsupportedPcode {
                index: 0,
                operation: String::from("register range outside register file"),
            })?;
        for (index, byte) in target.iter_mut().enumerate() {
            *byte = (value >> (index * 8)) as u8;
        }
        Ok(())
    }
}

fn branch_target(
    dest: &Varnode,
    machine: &NeMachine,
    unique: &BTreeMap<u64, u8>,
) -> Result<u64, NeError> {
    if dest.space == AddressSpaceId::Ram {
        Ok(dest.offset)
    } else if dest.space == AddressSpaceId::Const {
        // The 16-bit protected decoder represents some direct targets as a
        // constant varnode; the value is still the packed selector:offset PC.
        Ok(dest.offset)
    } else {
        machine.read_node(dest, unique)
    }
}

fn indirect_target(
    dest: &Varnode,
    machine: &NeMachine,
    unique: &BTreeMap<u64, u8>,
) -> Result<u64, NeError> {
    if dest.space == AddressSpaceId::Ram && dest.size != 0 {
        // Sleigh represents `JMP [expr]` as a BranchInd whose RAM varnode
        // offset aliases the Unique temporary holding the computed pointer.
        // Resolve that alias when its bytes were produced by this instruction;
        // otherwise preserve ordinary RAM-varnode literal handling.
        let alias = Varnode::unique(dest.offset, dest.size);
        let end = dest.offset.checked_add(u64::from(dest.size));
        if end.is_some_and(|end| (dest.offset..end).all(|offset| unique.contains_key(&offset))) {
            return machine.read_node(&alias, unique);
        }
    }
    machine.read_node(dest, unique)
}

fn unpack_pointer(pointer: u64) -> Result<(u16, u16), NeError> {
    if pointer > u64::from(u32::MAX) {
        return Err(NeError::InvalidPcodeAddress { pointer });
    }
    Ok(((pointer >> 16) as u16, pointer as u16))
}

fn pack_far(selector: u16, offset: u16) -> u64 {
    (u64::from(selector) << 16) | u64::from(offset)
}

fn initialize_psp(context: &NeTaskStartContext) -> [u8; 0x100] {
    let mut psp = [0; 0x100];
    let put_u16 = |bytes: &mut [u8], offset: usize, value: u16| {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    };
    let put_u32 = |bytes: &mut [u8], offset: usize, value: u32| {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    };

    // DOS-compatible process database fields used by Win16 startup and DOS
    // file APIs. The dispatcher target is runtime supplied and stays zero.
    put_u16(&mut psp, 0x00, 0x20cd); // INT 20h
    psp[0x05] = 0x9a; // far-call DOS dispatcher opcode
    put_u32(
        &mut psp,
        0x06,
        pack_far(
            context.dos_dispatcher.selector,
            context.dos_dispatcher.offset,
        ) as u32,
    );
    put_u16(&mut psp, 0x16, context.parent_psp);
    psp[0x18..0x2c].fill(0xff); // standard handles are host-runtime supplied
    put_u16(&mut psp, 0x2c, context.environment_selector);
    put_u32(&mut psp, 0x2e, 0);
    put_u16(&mut psp, 0x32, 20);
    put_u32(&mut psp, 0x34, pack_far(context.psp_selector, 0x18) as u32);
    put_u16(&mut psp, 0x38, 0);

    let command_len = context.command_line.len().min(126);
    psp[0x80] = command_len as u8;
    psp[0x81..0x81 + command_len].copy_from_slice(&context.command_line[..command_len]);
    psp[0x81 + command_len] = 0x0d;
    psp
}

fn value_mask(size: u32) -> Result<u64, NeError> {
    match size {
        1..=7 => Ok((1u64 << (size * 8)) - 1),
        8 => Ok(u64::MAX),
        _ => Err(NeError::UnsupportedPcode {
            index: 0,
            operation: format!("unsupported {size}-byte integer width"),
        }),
    }
}

fn signed(value: u64, bits: u32) -> i64 {
    if bits == 0 || bits >= 64 {
        value as i64
    } else {
        let shift = 64 - bits;
        ((value << shift) as i64) >> shift
    }
}

fn bit_mask(bits: u32) -> u64 {
    match bits {
        0 => 0,
        1..=63 => (1u64 << bits) - 1,
        _ => u64::MAX,
    }
}

fn bytes_to_u64(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .take(8)
        .enumerate()
        .fold(0, |value, (index, byte)| {
            value | (u64::from(*byte) << (index * 8))
        })
}

fn u64_to_bytes(value: u64, size: u32) -> Result<Vec<u8>, NeError> {
    if size > 8 {
        return Err(NeError::UnsupportedPcode {
            index: 0,
            operation: format!("{size}-byte memory write"),
        });
    }
    Ok((0..size)
        .map(|index| (value >> (index * 8)) as u8)
        .collect())
}

fn operation_name(op: &PcodeOp) -> String {
    match op {
        PcodeOp::CallOther { func_id, .. } => format!("CallOther({func_id})"),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arithmetic_right_shift_uses_operand_width_instead_of_count_width() {
        std::thread::Builder::new().stack_size(8 * 1024 * 1024).spawn(|| {
            let mut memory = NeLoadedImage { selectors: Vec::new(), storage: Vec::new(), next_allocation_id: 1 };
            memory.add_region(0x28, 64, true, true).unwrap();
            memory.write(0x28, 0, &[0xd1, 0xf8, 0xd0, 0xf8]).unwrap(); // SAR AX,1; SAR AL,1
            let mut cpu = NeMachine { registers: vec![0; REGISTER_BYTES], cs: 0x28, ip: 0 };
            cpu.set_register(CS_OFFSET, 2, 0x28).unwrap();
            cpu.set_register(0, 4, 0xaabb8002).unwrap();
            let mut decoder = Decoder::new(rsleigh_api::Architecture::X86_16Protected);
            cpu.step(&mut memory, &mut decoder).unwrap();
            assert_eq!(cpu.register(0, 4).unwrap(), 0xaabbc001);
            cpu.set_register(0, 4, 0xaabb1282).unwrap();
            cpu.step(&mut memory, &mut decoder).unwrap();
            assert_eq!(cpu.register(0, 4).unwrap(), 0xaabb12c1);
        }).unwrap().join().unwrap();
    }
    #[test]
    fn exchange_preserves_old_values_across_overlapping_register_views(){
        std::thread::Builder::new().stack_size(8*1024*1024).spawn(|| {
            let mut memory=NeLoadedImage {selectors:Vec::new(),storage:Vec::new(),next_allocation_id:1};
            memory.add_region(0x28,64,true,true).unwrap();
            memory.write(0x28,0,&[0x93,0x86,0xe0]).unwrap(); // XCHG AX,BX; XCHG AL,AH
            let mut cpu=NeMachine {registers:vec![0;REGISTER_BYTES],cs:0x28,ip:0};
            cpu.set_register(CS_OFFSET,2,0x28).unwrap();cpu.set_register(0,4,0xaabb1234).unwrap();cpu.set_register(BX_OFFSET,4,0xccdd5678).unwrap();
            let mut decoder=Decoder::new(rsleigh_api::Architecture::X86_16Protected);
            cpu.step(&mut memory,&mut decoder).unwrap();
            assert_eq!(cpu.register(0,4).unwrap(),0xaabb5678);
            assert_eq!(cpu.register(BX_OFFSET,4).unwrap(),0xccdd1234);
            cpu.step(&mut memory,&mut decoder).unwrap();
            assert_eq!(cpu.register(0,4).unwrap(),0xaabb7856);
        }).unwrap().join().unwrap();
    }
    #[test]
    fn indirect_near_calls_and_returns_with_argument_cleanup_keep_selectors() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let mut memory = NeLoadedImage {
                    selectors: Vec::new(),
                    storage: Vec::new(),
                    next_allocation_id: 1,
                };
                for (selector, code) in [(0x28, true), (0x30, false), (0x38, true)] {
                    memory.add_region(selector, 65536, true, code).unwrap();
                }
                memory.write(0x28, 0, &[0xff, 0xd0]).unwrap(); // CALL AX
                memory.write(0x28, 0x20, &[0xc2, 4, 0]).unwrap(); // RET 4
                memory.write(0x28, 0x40, &[0xca, 6, 0]).unwrap(); // RETF 6
                let mut cpu = NeMachine {
                    registers: vec![0; REGISTER_BYTES],
                    cs: 0x28,
                    ip: 0,
                };
                for (reg, value) in [
                    (CS_OFFSET, 0x28),
                    (SS_OFFSET, 0x30),
                    (SP_OFFSET, 0x200),
                    (0, 0x20),
                ] {
                    cpu.set_register(reg, 2, value).unwrap();
                }
                let mut decoder = Decoder::new(rsleigh_api::Architecture::X86_16Protected);
                cpu.step(&mut memory, &mut decoder).unwrap();
                assert_eq!((cpu.cs(), cpu.ip()), (0x28, 0x20));
                cpu.step(&mut memory, &mut decoder).unwrap();
                assert_eq!((cpu.cs(), cpu.ip()), (0x28, 2));
                assert_eq!(cpu.register(SP_OFFSET, 2).unwrap(), 0x204);
                memory.write(0x30, 0x100, &[0x50, 0, 0x38, 0]).unwrap();
                cpu.set_register(SP_OFFSET, 2, 0x100).unwrap();
                cpu.set_register(EIP_OFFSET, 4, 0x00280040).unwrap();
                cpu.step(&mut memory, &mut decoder).unwrap();
                assert_eq!((cpu.cs(), cpu.ip()), (0x38, 0x50));
                assert_eq!(cpu.register(SP_OFFSET, 2).unwrap(), 0x10a);
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn segmented_modrm_accesses_keep_ds_ss_and_cs_distinct() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let mut memory = NeLoadedImage {
                    selectors: Vec::new(),
                    storage: Vec::new(),
                    next_allocation_id: 1,
                };
                memory.add_region(0x28, 65536, true, true).unwrap();
                memory.add_region(0x30, 65536, true, false).unwrap();
                memory.add_region(0x38, 65536, true, false).unwrap();
                // MOV [0100],ES; MOV AX,[BP]; MOV DX,CS:[0100].
                memory
                    .write(
                        0x28,
                        0,
                        &[
                            0x8c, 0x06, 0x00, 0x01, 0x8b, 0x46, 0x00, 0x2e, 0x8b, 0x16, 0x00, 0x01,
                        ],
                    )
                    .unwrap();
                memory.write(0x28, 0x100, &[0x78, 0x56]).unwrap();
                memory.write(0x38, 0x100, &[0x34, 0x12]).unwrap();
                let mut cpu = NeMachine {
                    registers: vec![0; REGISTER_BYTES],
                    cs: 0x28,
                    ip: 0,
                };
                for (reg, value) in [
                    (CS_OFFSET, 0x28),
                    (DS_OFFSET, 0x30),
                    (SS_OFFSET, 0x38),
                    (ES_OFFSET, 0x40),
                    (BP_OFFSET, 0x100),
                ] {
                    cpu.set_register(reg, 2, value).unwrap();
                }
                let mut decoder = Decoder::new(rsleigh_api::Architecture::X86_16Protected);
                cpu.step(&mut memory, &mut decoder).unwrap();
                assert_eq!(
                    memory.read(0x30, 0x100, 2, NeAccess::Read).unwrap(),
                    [0x40, 0]
                );
                cpu.step(&mut memory, &mut decoder).unwrap();
                assert_eq!(cpu.register(0, 2).unwrap(), 0x1234);
                cpu.step(&mut memory, &mut decoder).unwrap();
                assert_eq!(cpu.register(8, 2).unwrap(), 0x5678);
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn far_indirect_jump_loads_both_offset_and_selector() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let mut memory = NeLoadedImage {
                    selectors: Vec::new(),
                    storage: Vec::new(),
                    next_allocation_id: 1,
                };
                memory.add_region(0x28, 256, true, true).unwrap();
                memory.add_region(0x30, 256, true, false).unwrap();
                memory.add_region(0x38, 256, true, true).unwrap();
                memory.write(0x28, 0, &[0xff, 0x2e, 0x10, 0]).unwrap();
                memory.write(0x30, 0x10, &[0x40, 0, 0x38, 0]).unwrap();
                memory.write(0x38, 0x40, &[0xb8, 0x34, 0x12]).unwrap();
                let mut cpu = NeMachine {
                    registers: vec![0; REGISTER_BYTES],
                    cs: 0x28,
                    ip: 0,
                };
                cpu.set_register(CS_OFFSET, 2, 0x28).unwrap();
                cpu.set_register(DS_OFFSET, 2, 0x30).unwrap();
                let mut decoder = Decoder::new(rsleigh_api::Architecture::X86_16Protected);
                let result = cpu.step(&mut memory, &mut decoder).unwrap();
                assert_eq!((result.next_cs, result.next_ip), (0x38, 0x40));
                cpu.step(&mut memory, &mut decoder).unwrap();
                assert_eq!(cpu.register(0, 2).unwrap(), 0x1234);
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn rep_strings_honor_count_comparison_and_zero_count_memory_guard() {
        std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let mut memory = NeLoadedImage {
                    selectors: Vec::new(),
                    storage: Vec::new(),
                    next_allocation_id: 1,
                };
                for (selector, code) in [(0x28, true), (0x30, false), (0x38, false)] {
                    memory.add_region(selector, 65536, true, code).unwrap();
                }
                memory.write(0x28, 0, &[0xf3, 0xa4, 0xf3, 0xa6]).unwrap();
                memory.write(0x30, 0x100, b"ABC").unwrap();
                let mut cpu = NeMachine {
                    registers: vec![0; REGISTER_BYTES],
                    cs: 0x28,
                    ip: 0,
                };
                for (reg, value) in [
                    (CS_OFFSET, 0x28),
                    (DS_OFFSET, 0x30),
                    (ES_OFFSET, 0x38),
                    (SI_OFFSET, 0x100),
                    (DI_OFFSET, 0x200),
                    (CX_OFFSET, 3),
                ] {
                    cpu.set_register(reg, 2, value).unwrap();
                }
                let mut decoder = Decoder::new(rsleigh_api::Architecture::X86_16Protected);
                let repeated = memory.decode_next(&mut decoder, 0x28, 0).unwrap();
                assert!(repeated
                    .instruction
                    .ops
                    .iter()
                    .filter_map(|op| match op {
                        PcodeOp::Branch { dest } | PcodeOp::CBranch { dest, .. } => Some(dest),
                        _ => None,
                    })
                    .all(|dest| dest.space == AddressSpaceId::Ram));
                for _ in 0..4 {
                    cpu.step(&mut memory, &mut decoder).unwrap();
                }
                assert_eq!((cpu.cs(), cpu.ip()), (0x28, 2));
                assert_eq!(memory.read(0x38, 0x200, 3, NeAccess::Read).unwrap(), b"ABC");
                assert_eq!(cpu.register(SI_OFFSET, 2).unwrap(), 0x103);
                assert_eq!(cpu.register(DI_OFFSET, 2).unwrap(), 0x203);
                memory.write(0x38, 0x200, b"ABX").unwrap();
                for (reg, value) in [(SI_OFFSET, 0x100), (DI_OFFSET, 0x200), (CX_OFFSET, 5)] {
                    cpu.set_register(reg, 2, value).unwrap();
                }
                for _ in 0..3 {
                    cpu.step(&mut memory, &mut decoder).unwrap();
                }
                assert_eq!(cpu.ip(), 4);
                assert_eq!(cpu.register(CX_OFFSET, 2).unwrap(), 2);
                assert_eq!(cpu.register(0x206, 1).unwrap(), 0); // ZF
                                                                // No selector is valid for these zero-count source/destination
                                                                // pointers. The repeat guard must prevent both memory accesses.
                cpu.set_register(EIP_OFFSET, 4, 0x00280000).unwrap();
                for reg in [DS_OFFSET, ES_OFFSET, CX_OFFSET] {
                    cpu.set_register(reg, 2, 0).unwrap();
                }
                cpu.step(&mut memory, &mut decoder).unwrap();
                assert_eq!(cpu.ip(), 2);
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
