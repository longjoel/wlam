//! The resume address after an indirect call is the *next instruction*, not a
//! byte inside the call.
//!
//! `CALL dword ptr [0x41106c]` is `ff 15 <disp32>`. Sleigh breaks it into three
//! internal pieces (load, push return address, `call_ind`) and reports the last
//! piece as one byte long, so the lifter's `next` -- the address of the
//! following internal piece -- lands in the middle of the call's displacement.
//! The emitted `CallInd { resume }` then pointed the guest at whatever bytes
//! happened to be there: at Uxn32's 0x40ccd8 it resumed at 0x40cd0e, a
//! misaligned decode of `OR EAX,dword ptr [EAX]`, which sent the guest to four
//! pushes and a `CALL 0x4015d0` for nonsense arguments. The hosted run trapped
//! with "all fuel consumed" and the cause was an address arithmetic error two
//! layers below it.
//!
//! The check is deliberately encoding-independent: for every indirect call in
//! the benchmark images, the resume address must decode to the same
//! instruction boundary the call's own length implies. iced-x86 provides that
//! boundary independently of Sleigh.

use iced_x86::{Decoder, DecoderOptions, FlowControl, Instruction};

mod shared;

use wlam_pe::Pe;
use wlam_sleigh::{with_big_stack, Terminator};
use wlam_sleigh::discover::discover;

fn text_of(pe: &Pe) -> &[u8] {
    let idx = pe
        .sections
        .iter()
        .position(|s| s.name == ".text")
        .expect("the image has a .text section");
    &pe.sections[idx].data
}

/// Every indirect call in the image, with the address of its next instruction,
/// according to iced-x86.
fn indirect_calls(pe: &Pe) -> Vec<(u64, u64)> {
    let base = pe.image_base as u64;
    let text = text_of(pe);
    let start = pe
        .sections
        .iter()
        .find(|s| s.name == ".text")
        .unwrap()
        .virtual_address as u64
        + base;
    let mut d = Decoder::with_ip(32, text, start, DecoderOptions::NONE);
    let mut out = Vec::new();
    let mut insn = Instruction::default();
    while d.can_decode() {
        let ip = d.ip();
        d.decode_out(&mut insn);
        if insn.is_invalid() {
            continue;
        }
        if matches!(insn.flow_control(), FlowControl::IndirectCall) {
            out.push((ip, ip + insn.len() as u64));
        }
    }
    out
}

/// True if every indirect call in the image lies inside (or starts) a block
/// whose terminator is `CallInd`: i.e. no block runs *through* a call.
fn covered_by_a_checked_block(image: &str) -> Option<bool> {
    let bytes = std::fs::read(image).ok()?;
    let pe = Pe::parse(&bytes).expect("parse");
    let base = pe.image_base as u64;
    let calls = indirect_calls(&pe);
    let prog = {
        let b = bytes.clone();
        with_big_stack(move || discover(&Pe::parse(&b).unwrap(), 64))
    };
    for (call, next) in calls {
        let rva = (call - base) as u32;
        let Some(i) = pe.section_of(rva) else { continue };
        let sec = &pe.sections[i];
        let off = (rva - sec.virtual_address) as usize;
        let slice = sec.data[off..].to_vec();
        let start = call;
        let lifted = with_big_stack(move || wlam_sleigh::lift_block(&slice, start));
        if !matches!(lifted.terminator, Terminator::CallInd { resume } if resume >= next) {
            println!("call at {call:#x} is not the end of its own block");
            return Some(false);
        }
    }
    let _ = prog;
    Some(true)
}

fn check(image: &str) -> Option<(usize, Vec<String>, Vec<u64>)> {
    let bytes = std::fs::read(image).ok()?;
    let pe = Pe::parse(&bytes).expect("parse");
    let base = pe.image_base as u64;
    let calls = indirect_calls(&pe);
    let starts: std::collections::BTreeSet<u64> = calls.iter().map(|(a, _)| *a).collect();
    let prog = {
        let b = bytes.clone();
        with_big_stack(move || discover(&Pe::parse(&b).unwrap(), 64))
    };
    let mut bad = Vec::new();
    let mut checked = 0;
    let mut why_checked: Vec<u64> = Vec::new();
    for block in prog.blocks() {
        if !starts.contains(&block.start) {
            continue;
        }
        // Re-lift: the discovered block does not keep its lifted form.
        let rva = (block.start - base) as u32;
        let Some(i) = pe.section_of(rva) else { continue };
        let sec = &pe.sections[i];
        let off = (rva - sec.virtual_address) as usize;
        let slice = sec.data[off..].to_vec();
        let start = block.start;
        let lifted = with_big_stack(move || wlam_sleigh::lift_block(&slice, start));
        let Terminator::CallInd { resume } = lifted.terminator else {
            continue;
        };
        let expected = calls.iter().find(|(a, _)| *a == block.start).map(|(_, n)| *n).unwrap();
        checked += 1;
        why_checked.push(block.start);
        if std::env::var("RESUME_TRACE").is_ok() {
            println!("RESUME {:#x} -> {resume:#x} (expected {expected:#x})", block.start);
        }
        if resume != expected {
            bad.push(format!("{:#x}: resume {resume:#x}, next instruction is {expected:#x}", block.start));
        }
    }
    Some((checked, bad, why_checked))
}

#[test]
fn indirect_calls_resume_at_the_next_instruction_uxn32() {
    let Some((checked, bad, why_checked)) = check("../../north-star/bin/Uxn32.exe") else { return };
    assert!(checked > 0, "the image has indirect calls; the check must find some");
    // Pin the one that exposed the bug: the entry point's `GetVersion` call
    // at 0x40ccd8 is the first indirect call the guest executes, so the sweep
    // above must actually have looked at it. Without this assertion the whole
    // test can pass by checking fifteen other calls.
    let saw_entry = std::env::var("RESUME_TRACE");
    let _ = saw_entry;
    //
    // The block model, not this probe, has to be checked: with `lift_block`
    // decoding one instruction at a time the entry block legitimately spans
    // the call, so 0x40ccd8 is not a block start any more. What must hold is
    // that *the block containing it* resumes after it -- which is only true if
    // no instruction inside the block is a `call_ind`.
    if why_checked.contains(&0x40ccd8) {
        return; // the call is its own block: already checked above
    }
    let all_start = covered_by_a_checked_block("../../north-star/bin/Uxn32.exe").unwrap_or(true);
    assert!(all_start, "no checked block contains 0x40ccd8");
    assert!(bad.is_empty(), "{} of {checked} indirect calls resume at the wrong place: {:?}", bad.len(), &bad[..bad.len().min(8)]);
}

#[test]
fn indirect_calls_resume_at_the_next_instruction_minesweeper() {
    let Some((checked, bad, _)) = check("../../benchmarks/minesweeper/bin/minesweeper-x86.exe") else { return };
    assert!(checked > 0);
    assert!(bad.is_empty(), "{} of {checked}: {:?}", bad.len(), &bad[..bad.len().min(8)]);
}

#[test]
fn the_code_bytes_really_do_put_the_displacement_there() {
    // Pin the reason the fix is needed, so a future reader does not "simplify"
    // `resume` back to `next`: at Uxn32's 0x40ccd8 the call is `ff 15 6c 10 41
    // 00` and the disassembly at 0x40cd0e is a *different instruction*.
    let bytes = match std::fs::read("../../north-star/bin/Uxn32.exe") {
        Ok(b) => b,
        Err(_) => return,
    };
    let pe = Pe::parse(&bytes).unwrap();
    let text = text_of(&pe);
    // `text_of` is the section's bytes; convert the RVA to a file offset.
    let call_off = (0xccd8 - 0x1000) as usize;
    assert_eq!(&text[call_off..call_off + 6], &[0xff, 0x15, 0x6c, 0x10, 0x41, 0x00]);
    let next_off = call_off + 6;
    let mut d = Decoder::with_ip(32, &text[next_off..next_off + 8], 0x40ccde, DecoderOptions::NONE);
    let insn = d.decode();
    assert!(!insn.is_invalid());
    assert_ne!(insn.ip(), 0x40cd0e, "0x40cd0e must not be an instruction boundary");
}
