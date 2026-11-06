//! Probe: dump the address space of every branch destination in a short run.
//!
//! This is the probe that diagnosed the phantom-block bug. Run it on an address
//! where discovery produced a target of `0x1` or `0x2` and it shows why: the
//! instruction there is not a branch at all, and Sleigh's `CBranch` destination
//! is a `Const`-space microcode label rather than an address.
//!
//! ```text
//! $ cargo run --example probe_dest -- 4153f0 ../../benchmarks/minesweeper/bin/minesweeper-x86.exe
//! 0x4153f0  CMP EDI,EDX
//! 0x4153f2  JC 0x4154bd
//!       dest space=Ram offset=0x4154bd size=4      <- a real branch
//! 0x4153f8  BSR EAX,EDX
//!       dest space=Const offset=0x2 size=8         <- NOT a branch: internal label
//! ```
//!
//! The rule this establishes: a `Ram`-space destination is an address, a
//! `Const`-space destination is Sleigh's own label. Only `Ram` may terminate a
//! block. See `const_space_branch_is_not_a_terminator` in `src/lib.rs`.
//!
//! Usage: `cargo run --example probe_dest -- <hexaddr> [path.exe]`
use wlam_sleigh::{Architecture, Decoder, PcodeOp};

fn main() {
    let a = std::env::args().nth(1).expect("hex address");
    let start = u64::from_str_radix(a.trim_start_matches("0x"), 16).expect("hex address");
    let path = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "../../benchmarks/minesweeper/bin/minesweeper-x86.exe".into());
    let bytes = std::fs::read(&path).expect("image");
    let pe = wlam_pe::Pe::parse(&bytes).expect("parse");
    let base = pe.image_base as u64;

    // Checked subtraction: addresses below the base are exactly what this probe
    // is used to investigate, and `saturating_sub` would silently turn them into
    // RVA 0 -- the bug this probe documents.
    let rva = start
        .checked_sub(base)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or_else(|| {
            eprintln!("{start:#x} is below the image base {base:#x}; nothing to decode");
            std::process::exit(1);
        });
    let idx = pe.section_of(rva).expect("address not in a section");
    let sec = &pe.sections[idx];
    let mut off = (rva - sec.virtual_address) as usize;
    let mut d = Decoder::new(Architecture::X86_32);

    for _ in 0..6 {
        let at = base + (sec.virtual_address as u64) + off as u64;
        let Ok(insn) = d.decode(&sec.data[off..], at) else {
            break;
        };
        println!("{at:#x}  {}", insn.disassembly);
        for op in &insn.ops {
            if let PcodeOp::Branch { dest } | PcodeOp::CBranch { dest, .. } = op {
                let note = match dest.space {
                    wlam_sleigh::AddressSpaceId::Ram => "  <- a real branch target",
                    wlam_sleigh::AddressSpaceId::Const => "  <- NOT an address: internal label",
                    _ => "",
                };
                println!(
                    "      dest space={:?} offset={:#x} size={}{note}",
                    dest.space, dest.offset, dest.size
                );
            }
        }
        off += insn.len as usize;
    }
}
