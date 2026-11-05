//! Reentrant host-to-guest stdcall callback trampoline.
use crate::{
    chain::Program,
    encode::{leb_s32, leb_u32},
    snapshot_address,
};
fn local(out: &mut Vec<u8>, op: u8, index: u32) {
    out.push(op);
    out.extend(leb_u32(index));
}
fn constant(out: &mut Vec<u8>, value: u32) {
    out.push(0x41);
    out.extend(leb_s32(value as i32));
}
fn access(out: &mut Vec<u8>, op: u8) {
    out.extend([op, 0, 0]);
}
/// `(guest_address, argument_count, guest_argument_vector) -> EAX`.
/// Raw eight-byte snapshots are parked in Wasm locals, so nested invocations
/// have independent save areas, including floating-point register bits.
pub(crate) fn body(program: &Program) -> Option<Vec<u8>> {
    let esp_slot = program.registers.iter().position(|r| r == "ESP")?;
    let eax_slot = program.registers.iter().position(|r| r == "EAX")?;
    let n = program.registers.len() as u32;
    let esp = 3 + n;
    let index = esp + 1;
    let result = esp + 2;
    let target = esp + 3;
    let mut out = vec![2];
    out.extend(leb_u32(n));
    out.push(0x7e);
    out.extend([4, 0x7f]);
    // Only registered guest entry addresses are callable. Slot zero is valid.
    constant(&mut out, u32::MAX);
    local(&mut out, 0x21, target);
    for (address, slot) in &program.function_index {
        // Some PE vtables in Hover contain image-relative function offsets
        // rather than fully relocated virtual addresses. The general indirect
        // jump dispatcher already accepts both forms; callbacks must do the
        // same or an otherwise compiled MFC virtual method reaches `unreachable`.
        local(&mut out, 0x20, 0);
        constant(&mut out, *address as u32);
        out.push(0x46);
        let rva = (*address as u32).checked_sub(program.image_base);
        if let Some(rva) = rva.filter(|_| program.image_base != 0) {
            local(&mut out, 0x20, 0);
            constant(&mut out, rva);
            out.extend([0x46, 0x72]); // exact VA OR image-base-relative RVA
        }
        out.extend([0x04, 0x40]);
        constant(&mut out, *slot);
        local(&mut out, 0x21, target);
        out.push(0x0b);
    }
    local(&mut out, 0x20, target);
    constant(&mut out, u32::MAX);
    out.extend([0x46, 0x04, 0x40, 0x00, 0x0b]);
    local(&mut out, 0x20, 1);
    constant(&mut out, 64);
    out.extend([0x4b, 0x04, 0x40, 0x00, 0x0b]);
    for i in 0..n {
        constant(&mut out, snapshot_address(i as usize) as u32);
        access(&mut out, 0x29);
        local(&mut out, 0x21, 3 + i);
    }
    constant(&mut out, snapshot_address(esp_slot) as u32);
    access(&mut out, 0x28);
    local(&mut out, 0x21, esp);
    // Reject stack underflow before subtracting the callback frame.
    local(&mut out, 0x20, esp);
    local(&mut out, 0x20, 1);
    constant(&mut out, 4);
    out.push(0x6c);
    constant(&mut out, 4);
    out.push(0x6a);
    out.extend([0x49, 0x04, 0x40, 0x00, 0x0b]);
    local(&mut out, 0x20, esp);
    local(&mut out, 0x20, 1);
    constant(&mut out, 4);
    out.push(0x6c);
    out.push(0x6b);
    constant(&mut out, 4);
    out.push(0x6b);
    local(&mut out, 0x21, esp);
    local(&mut out, 0x20, esp);
    constant(&mut out, 0xffff_fffe);
    access(&mut out, 0x36);
    constant(&mut out, 0);
    local(&mut out, 0x21, index);
    out.extend([0x02, 0x40, 0x03, 0x40]);
    local(&mut out, 0x20, index);
    local(&mut out, 0x20, 1);
    out.extend([0x4f, 0x0d, 1]);
    local(&mut out, 0x20, esp);
    constant(&mut out, 4);
    out.push(0x6a);
    local(&mut out, 0x20, index);
    constant(&mut out, 4);
    out.extend([0x6c, 0x6a]);
    local(&mut out, 0x20, 2);
    local(&mut out, 0x20, index);
    constant(&mut out, 4);
    out.extend([0x6c, 0x6a]);
    access(&mut out, 0x28);
    access(&mut out, 0x36);
    local(&mut out, 0x20, index);
    constant(&mut out, 1);
    out.push(0x6a);
    local(&mut out, 0x21, index);
    out.extend([0x0c, 0, 0x0b, 0x0b]);
    constant(&mut out, snapshot_address(esp_slot) as u32);
    local(&mut out, 0x20, esp);
    access(&mut out, 0x36);
    local(&mut out, 0x20, target);
    out.extend([0x11, 0, 0]);
    constant(&mut out, snapshot_address(eax_slot) as u32);
    access(&mut out, 0x28);
    local(&mut out, 0x21, result);
    for i in 0..n {
        constant(&mut out, snapshot_address(i as usize) as u32);
        local(&mut out, 0x20, 3 + i);
        access(&mut out, 0x37);
    }
    local(&mut out, 0x20, result);
    out.push(0x0b);
    Some(out)
}
