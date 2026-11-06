//! x86-32 general register file layout: which byte of which register a name
//! refers to.
//!
//! ## Why this exists
//!
//! Sleigh models the sub-registers as independent varnodes: `AL`, `AH`, `AX`
//! and `EAX` are four separate registers, and `mov dl, ah` lifts to
//!
//! ```text
//! Copy { dst: Reg("DL", 1), src: Reg("AH", 1) }
//! ```
//!
//! That is a faithful model of *this* instruction, but it is only half the
//! story: the guest's next instruction is `mov [slot], edx`, which reads
//! `Reg("EDX", 4)`. Unless something connects `DL` back to `EDX`, the write to
//! `DL` is dead and the store sees whatever `EDX` held before -- observed in
//! Uxn32, which calls `GetVersion` and then stores a major version of 0.
//!
//! This module is that connection: it maps a register name to the byte window
//! it occupies inside the 32-bit register file, so an emitter can implement a
//! narrow write as a read-modify-write and a narrow read as a shift-and-mask.
//!
//! ## Source of truth
//!
//! The offsets are Ghidra's, from the x86-32 Sleigh specification, and match
//! `wlam_sleigh::X86_32_REGISTERS` exactly (see `registers_match_sleigh`).
//! They are duplicated rather than imported because `wlam-emit` sits *below*
//! `wlam-sleigh` in the dependency graph; the test makes the duplication safe.
//!
//! Only the general-purpose registers and the flags that live in `EFLAGS` have
//! their own storage here. Registers with real separate state (`EIP`,
//! `FS_OFFSET`, the x87 stack, the hidden segment descriptors) are not part of
//! this file and must be kept as their own locals.

/// A 32-bit register that owns bytes in the guest register file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parent {
    Eax,
    Ecx,
    Edx,
    Ebx,
    Esp,
    Ebp,
    Esi,
    Edi,
}

impl Parent {
    /// The cannonical 32-bit name, which is also the local the emitter holds
    /// the value in.
    pub fn name(self) -> &'static str {
        match self {
            Parent::Eax => "EAX",
            Parent::Ecx => "ECX",
            Parent::Edx => "EDX",
            Parent::Ebx => "EBX",
            Parent::Esp => "ESP",
            Parent::Ebp => "EBP",
            Parent::Esi => "ESI",
            Parent::Edi => "EDI",
        }
    }

    /// Index of this register's 4-byte slot in the guest register file, in
    /// 32-bit words: EAX=0, ECX=1, EDX=2, EBX=3, ESP=4, EBP=5, ESI=6, EDI=7.
    ///
    /// This is the intel register order, which the register *snapshot* area
    /// (`REG_SNAPSHOT_BASE`) uses for its first eight entries.
    pub fn word(self) -> u32 {
        match self {
            Parent::Eax => 0,
            Parent::Ecx => 1,
            Parent::Edx => 2,
            Parent::Ebx => 3,
            Parent::Esp => 4,
            Parent::Ebp => 5,
            Parent::Esi => 6,
            Parent::Edi => 7,
        }
    }
}

/// A register name resolved to the byte window it occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    /// The 32-bit register the value lives inside.
    pub parent: Parent,
    /// Byte offset within the parent, little-endian: 0 = low byte.
    pub offset: u32,
    /// Width in bytes: 1, 2 or 4.
    pub width: u32,
}

impl Slot {
    /// The mask of the bits this slot covers within its parent, e.g. `AH` is
    /// `0x0000_ff00` and `AX` is `0x0000_ffff`.
    pub fn mask(self) -> u32 {
        let bits = self.width * 8;
        let full = if bits >= 32 {
            u32::MAX
        } else {
            (1u32 << bits) - 1
        };
        full << (self.offset * 8)
    }

    /// How far right to shift the parent to put this slot's value in the low
    /// bits, i.e. the shift for a narrow READ.
    pub fn shift(self) -> u32 {
        self.offset * 8
    }
}

/// Resolve an x86-32 general-purpose register name to its slot.
///
/// Returns `None` for anything that is not part of the 32-bit register file:
/// flags (`CF`, `ZF`, ...), `EIP`, `FS_OFFSET`, `ST0`, the segment registers,
/// and Sleigh's synthetic `reg_XXXX` names. Those genuinely have their own
/// storage and must not be aliased onto a parent.
pub fn slot_of(name: &str) -> Option<Slot> {
    /// (name, parent, byte offset within parent, width in bytes)
    const TABLE: &[(&str, Parent, u32, u32)] = &[
        // Accumulator
        ("EAX", Parent::Eax, 0, 4),
        ("AX", Parent::Eax, 0, 2),
        ("AL", Parent::Eax, 0, 1),
        ("AH", Parent::Eax, 1, 1),
        // Counter
        ("ECX", Parent::Ecx, 0, 4),
        ("CX", Parent::Ecx, 0, 2),
        ("CL", Parent::Ecx, 0, 1),
        ("CH", Parent::Ecx, 1, 1),
        // Data
        ("EDX", Parent::Edx, 0, 4),
        ("DX", Parent::Edx, 0, 2),
        ("DL", Parent::Edx, 0, 1),
        ("DH", Parent::Edx, 1, 1),
        // Base
        ("EBX", Parent::Ebx, 0, 4),
        ("BX", Parent::Ebx, 0, 2),
        ("BL", Parent::Ebx, 0, 1),
        ("BH", Parent::Ebx, 1, 1),
        // Stack pointer
        ("ESP", Parent::Esp, 0, 4),
        ("SP", Parent::Esp, 0, 2),
        // Base pointer
        ("EBP", Parent::Ebp, 0, 4),
        ("BP", Parent::Ebp, 0, 2),
        // Source index
        ("ESI", Parent::Esi, 0, 4),
        ("SI", Parent::Esi, 0, 2),
        // Destination index
        ("EDI", Parent::Edi, 0, 4),
        ("DI", Parent::Edi, 0, 2),
    ];
    TABLE
        .iter()
        .find(|(n, _, _, _)| *n == name)
        .map(|(_, parent, offset, width)| Slot {
            parent: *parent,
            offset: *offset,
            width: *width,
        })
}

/// Whether writes to `name` must update a 32-bit parent rather than a local of
/// its own.
///
/// True for every sub-register (`AL`, `AX`, `AL`...); false for `EAX` itself
/// and for everything outside the register file.
pub fn is_partial(name: &str) -> bool {
    matches!(slot_of(name), Some(s) if s.width < 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ah_is_the_second_byte_of_eax() {
        // This is the case that matters: `mov dl, ah` then `mov [slot], edx`.
        let ah = slot_of("AH").expect("AH is a register");
        assert_eq!(ah.parent, Parent::Eax);
        assert_eq!(ah.offset, 1);
        assert_eq!(ah.width, 1);
        assert_eq!(ah.shift(), 8);
    }

    #[test]
    fn dl_is_the_low_byte_of_edx() {
        let dl = slot_of("DL").expect("DL is a register");
        assert_eq!(dl.parent, Parent::Edx);
        assert_eq!(dl.offset, 0);
        assert_eq!(dl.shift(), 0);
        assert_eq!(dl.mask(), 0x0000_00ff);
    }

    #[test]
    fn masks_cover_the_right_bits() {
        assert_eq!(slot_of("AL").unwrap().mask(), 0x0000_00ff);
        assert_eq!(slot_of("AH").unwrap().mask(), 0x0000_ff00);
        assert_eq!(slot_of("AX").unwrap().mask(), 0x0000_ffff);
        assert_eq!(slot_of("EAX").unwrap().mask(), 0xffff_ffff);
        assert_eq!(slot_of("DX").unwrap().mask(), 0x0000_ffff);
        assert_eq!(slot_of("DH").unwrap().mask(), 0x0000_ff00);
    }

    #[test]
    fn full_width_registers_are_slots_too() {
        // EAX resolves, but is not *partial*: writes go straight to the local.
        assert_eq!(slot_of("EAX").unwrap().offset, 0);
        assert_eq!(slot_of("EAX").unwrap().width, 4);
        assert!(!is_partial("EAX"));
        assert!(is_partial("AL"));
        assert!(is_partial("AX"));
    }

    #[test]
    fn things_outside_the_register_file_are_not_slots() {
        // Each of these holds independent state and must keep its own local.
        for name in [
            "CF",
            "ZF",
            "PF",
            "SF",
            "OF",
            "EIP",
            "FS_OFFSET",
            "ST0",
            "reg_10a0_2",
            "ES",
        ] {
            assert!(slot_of(name).is_none(), "{name} must not alias a parent");
            assert!(!is_partial(name), "{name} must not be treated as partial");
        }
    }

    #[test]
    fn parents_are_the_eight_32_bit_registers_in_intel_order() {
        let order = [
            (Parent::Eax, 0),
            (Parent::Ecx, 1),
            (Parent::Edx, 2),
            (Parent::Ebx, 3),
            (Parent::Esp, 4),
            (Parent::Ebp, 5),
            (Parent::Esi, 6),
            (Parent::Edi, 7),
        ];
        for (parent, word) in order {
            assert_eq!(parent.word(), word);
            let full = slot_of(parent.name()).expect("parent is a slot");
            assert_eq!(full.parent, parent);
            assert_eq!(full.width, 4);
        }
    }
}
