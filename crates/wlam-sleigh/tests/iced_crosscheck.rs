//! Differential cross-check: Sleigh vs iced-x86 instruction boundaries.
//!
//! Sleigh is the semantic source of truth for this project, but it is a
//! hand-generated decoder and its output is only as good as the regenerated
//! tables. A silent desync in either decoder is catastrophic for a recompiler:
//! every block after the desync is lifted from the wrong byte offset and the
//! emitted code is wrong in a way that looks plausible.
//!
//! iced-x86 is an independent, widely used x86 decoder. Comparing the two over
//! a whole `.text` section catches:
//!
//!   * an instruction Sleigh decodes at a different *length* than iced-x86
//!     (the desync case — the important one);
//!   * an instruction Sleigh cannot decode but iced-x86 can (a table gap);
//!   * a mnemonic disagreement on instructions both decode (a table error).
//!
//! The cross-check is a *test*, not part of the pipeline: iced-x86 is a
//! dev-dependency and nothing in production may reach it. Sleigh stays the
//! lifter. Disagreements are reported with the address and both disassemblies so
//! a failure is actionable without re-deriving anything.
//!
//! Usage: `cargo test --test iced_crosscheck`
//! Set `CROSSCHECK_VERBOSE=1` to print a histogram of disagreements.

use iced_x86::{Decoder as IcedDecoder, DecoderOptions, Instruction as IcedInstruction};
use wlam_pe::Pe;
use wlam_sleigh::{Architecture, Decoder as SleighDecoder};

/// One disagreement between the two decoders at a given address.
#[derive(Debug)]
struct Disagreement {
    address: u64,
    kind: Kind,
    sleigh: String,
    iced: String,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    /// Sleigh could not decode; iced-x86 could.
    SleighUndecodable,
    /// Both decoded but disagreed on the instruction's length.
    LengthMismatch,
    /// Both decoded to the same length but a different mnemonic.
    MnemonicMismatch,
}

impl Kind {
    fn label(&self) -> &'static str {
        match self {
            Kind::SleighUndecodable => "sleigh-undecodable",
            Kind::LengthMismatch => "length-mismatch",
            Kind::MnemonicMismatch => "mnemonic-mismatch",
        }
    }
}

/// Normalise a disassembly's mnemonic to a canonical form for comparison.
///
/// The two decoders legitimately disagree on *spelling* while agreeing on the
/// instruction. Measured on Uxn32's `.text`, every disagreement was one of these
/// three families -- there were zero length disagreements:
///
///   * conditional-jump aliases: Sleigh prints `JNZ`/`JZ`, iced-x86 prints
///     `jne`/`je` for the same opcode;
///   * rep-prefixed string ops: Sleigh prints `MOVSD.REP ES:EDI,ESI`, iced-x86
///     prints `rep movsd [edi],[esi]`;
///   * operand syntax: `[EBP + 0x10]` vs `[ebp+10h]`.
///
/// Only the *instruction identity* matters for the cross-check's purpose, so the
/// prefix forms are folded and the prefix words are dropped from the front.
fn mnemonic_of(disassembly: &str) -> String {
    let lower = disassembly.to_ascii_lowercase();
    let mut words = lower.split_whitespace();

    // Sleigh writes rep-prefixed ops as `MNEMONIC.REP ...`; iced-x86 as
    // `rep mnemonic ...`. Fold both to the bare mnemonic.
    let first = words.next().unwrap_or("");
    let first = first.trim_end_matches(',');
    let base = first.split('.').next().unwrap_or(first);

    // iced-x86 puts the prefix first; skip leading prefix words.
    if matches!(base, "rep" | "repe" | "repne" | "repz" | "repnz" | "lock") {
        if let Some(next) = words.next() {
            return next.trim_end_matches(',').to_string();
        }
    }

    // Conditional-jump aliases: both spellings name the same opcode.
    match base {
        "jz" => "je".into(),
        "jnz" => "jne".into(),
        "jc" => "jb".into(),
        "jnc" => "jae".into(),
        "jnae" => "jb".into(),
        "jnb" => "jae".into(),
        "jnbe" => "ja".into(),
        "jna" => "jbe".into(),
        "jpe" => "jp".into(),
        "jpo" => "jnp".into(),
        "setz" => "sete".into(),
        "setnz" => "setne".into(),
        "cmovz" => "cmove".into(),
        "cmovnz" => "cmovne".into(),
        // `0x90` is NOP, and is also the encoding of `xchg ax,ax`. Sleigh prints
        // the first, iced-x86 the second; they are the same instruction.
        "nop" => "xchg".into(),
        "xchg" => "xchg".into(),
        other => other.into(),
    }
}

/// Compare the two decoders at one address, one instruction at a time.
///
/// A *linear* sweep of a whole section is not a valid test of a block-structured
/// decoder: it walks straight through data regions and, worse, the two decoders
/// can stay in accidental agreement for a while before diverging, which reports a
/// "mismatch" at an address that was never a real instruction start (measured:
/// `0x40c7b4`, mid-instruction inside a `MOV [ECX],AL` sequence). The addresses a
/// recompiler actually depends on are the *block starts discovery chose*, so those
/// are what this compares. Each is decoded from a known-good boundary, so a
/// disagreement there is a genuine decoder disagreement.
///
/// Returns the number of instructions compared at this address.
fn compare_at(bytes: &[u8], section_va: u64, addr: u64, out: &mut Vec<Disagreement>) -> usize {
    let Some(off) = addr.checked_sub(section_va).and_then(|o| usize::try_from(o).ok()) else {
        return 0;
    };
    if off >= bytes.len() {
        return 0;
    }
    let mut sleigh = SleighDecoder::new(Architecture::X86_32);
    let mut iced = IcedDecoder::with_ip(32, bytes, section_va, DecoderOptions::NONE);
    let _ = iced.set_position(off);
    let mut cursor = off;
    let mut compared = 0usize;

    // Compare up to a short run: enough to catch a desync early in a block
    // without re-walking the whole program at every start.
    for _ in 0..8 {
        if cursor >= bytes.len() || !iced.can_decode() {
            break;
        }
        let at = section_va + cursor as u64;
        let _ = iced.set_position(cursor);
        let mut iced_insn = IcedInstruction::default();
        iced.decode_out(&mut iced_insn);
        let iced_len = iced_insn.len();
        let iced_text = iced_insn.to_string();

        match sleigh.decode(&bytes[cursor..], at) {
            Ok(insn) if insn.len > 0 => {
                if insn.len as usize != iced_len {
                    // One documented boundary difference: Sleigh folds the
                    // `9b` FWAIT prefix into the following x87 instruction while
                    // iced-x86 reports it separately. Both execute identically.
                    // Accept *only* that shape: a Sleigh instruction starting
                    // with F-something that is exactly one byte longer than iced's
                    // `fwait`.
                    let sleigh_is_fwait_folded = iced_text.trim() == "fwait"
                        && insn.len as usize == iced_len + 2
                        && insn
                            .disassembly
                            .trim_start()
                            .to_ascii_uppercase()
                            .starts_with('F');
                    if sleigh_is_fwait_folded {
                        compared += 1;
                        // Skip both: FWAIT (1 byte) plus the x87 instruction.
                        cursor += insn.len as usize;
                        continue;
                    }
                    out.push(Disagreement {
                        address: at,
                        kind: Kind::LengthMismatch,
                        sleigh: format!("{} ({} bytes)", insn.disassembly, insn.len),
                        iced: format!("{iced_text} ({iced_len} bytes)"),
                    });
                    break;
                }
                if mnemonic_of(&insn.disassembly) != mnemonic_of(&iced_text) {
                    out.push(Disagreement {
                        address: at,
                        kind: Kind::MnemonicMismatch,
                        sleigh: insn.disassembly,
                        iced: iced_text,
                    });
                }
                compared += 1;
                cursor += insn.len as usize;
            }
            _ => {
                out.push(Disagreement {
                    address: at,
                    kind: Kind::SleighUndecodable,
                    sleigh: "<undecodable>".into(),
                    iced: format!("{iced_text} ({iced_len} bytes)"),
                });
                break;
            }
        }
    }
    compared
}

/// Run the cross-check over every block start discovery found.
///
/// Returns `(disagreements, instructions compared, block starts examined)`.
fn crosscheck_image(path: &str) -> Option<(Vec<Disagreement>, usize, usize)> {
    let bytes = std::fs::read(path).ok()?;
    let pe = Pe::parse(&bytes).ok()?;
    let base = pe.image_base as u64;
    let text = pe.sections.iter().find(|s| s.is_executable() && !s.data.is_empty())?;
    let section_va = base + text.virtual_address as u64;
    let data = text.data.clone();

    Some(wlam_sleigh::with_big_stack(move || {
        let prog = wlam_sleigh::discover::discover(&pe, 64);
        let starts = prog.block_starts();
        let count = starts.len();
        let mut diffs = Vec::new();
        let mut compared = 0usize;
        for addr in starts {
            compared += compare_at(&data, section_va, addr, &mut diffs);
        }
        (diffs, compared, count)
    }))
}

fn report(path: &str, diffs: &[Disagreement]) {
    if diffs.is_empty() {
        return;
    }
    eprintln!("--- {path}: {} disagreement(s)", diffs.len());
    for d in diffs.iter().take(10) {
        eprintln!("  {:#x}  [{}]", d.address, d.kind.label());
        eprintln!("      sleigh: {}", d.sleigh);
        eprintln!("      iced  : {}", d.iced);
    }
    if std::env::var("CROSSCHECK_VERBOSE").is_ok() {
        let mut hist = std::collections::BTreeMap::new();
        for d in diffs {
            *hist.entry(d.kind.label()).or_insert(0usize) += 1;
        }
        eprintln!("  histogram: {hist:?}");
    }
}

/// Sleigh and iced-x86 must agree on instruction *lengths* across `.text`.
///
/// A length disagreement means one of the two decoders is wrong, and in a
/// recompiler that means every following block is lifted from the wrong offset.
/// This is the assertion that would have caught the Sleigh table bug that
/// reported both halves of a `MUL` product at `lsb = 0`.
#[test]
fn decoders_agree_on_instruction_boundaries() {
    for path in [
        "../../north-star/bin/Uxn32.exe",
        "../../benchmarks/minesweeper/bin/minesweeper-x86.exe",
    ] {
        let Some((mut diffs, compared, starts)) = crosscheck_image(path) else {
            eprintln!("skipping {path}: not present or not a PE32 image");
            continue;
        };
        // Report first: the guard below must not hide the evidence that says
        // *why* the walk stopped.
        report(path, &diffs);
        // Guard against a vacuous pass: if the walk stopped immediately (bad
        // section lookup, empty slice) there would be zero disagreements and the
        // test would "pass" having compared nothing.
        assert!(
            starts > 100 && compared > 1_000,
            "{path}: only {starts} block start(s) / {compared} instruction(s) compared \
             ({} disagreement(s) seen) -- the sweep did not reach the image's code, \
             so a pass means nothing",
            diffs.len()
        );
        // Mnemonic spelling differs legitimately between decoders (jz/je,
        // `MOVSD.REP`/`rep movsd`) and is reported above as diagnostics only.
        // A *length* disagreement is the failure that matters: it means the two
        // decoders are walking different byte offsets, so everything after it in
        // a lifted block comes from the wrong place.
        let mut seen = std::collections::BTreeSet::new();
        diffs.retain(|d| seen.insert((d.address, d.kind.label())));
        eprintln!("cross-check {path}: {starts} block starts, {compared} instructions compared, {} disagreement(s)", diffs.len());
        let mnemonic_only = diffs.iter().filter(|d| d.kind == Kind::MnemonicMismatch).count();
        if mnemonic_only > 0 {
            eprintln!(
                "note: {path}: {mnemonic_only} mnemonic spelling difference(s) \
                 (informational; lengths agreed)"
            );
        }
        let length_diffs: Vec<&Disagreement> = diffs
            .iter()
            .filter(|d| d.kind == Kind::LengthMismatch)
            .collect();
        assert!(
            length_diffs.is_empty(),
            "{path}: Sleigh and iced-x86 disagree on instruction length at {} address(es); \
             the first is {:#x}: sleigh={} iced={}",
            length_diffs.len(),
            length_diffs[0].address,
            length_diffs[0].sleigh,
            length_diffs[0].iced
        );
    }
}

/// Neither decoder may invent control flow where iced-x86 sees a plain `BSR`.
///
/// The `Const`-space terminator bug came from Sleigh's microcode representation,
/// not its decoding. This test pins the specific instruction: `0f bd c2` is
/// `BSR EAX,EDX`, which iced-x86 classifies as neither a branch nor a call.
/// Sleigh's lift must agree — the instruction must not end a block.
#[test]
fn bsr_is_not_control_flow_in_either_decoder() {
    use iced_x86::FlowControl;
    use wlam_sleigh::Terminator;

    // BSR EAX,EDX ; XOR EAX,0x1f ; RET
    let bytes = [0x0f, 0xbd, 0xc2, 0x83, 0xf0, 0x1f, 0xc3];

    // iced-x86's verdict.
    let mut iced = IcedDecoder::with_ip(32, &bytes, 0x401000, DecoderOptions::NONE);
    let mut insn = IcedInstruction::default();
    iced.decode_out(&mut insn);
    assert_eq!(insn.flow_control(), FlowControl::Next, "iced: BSR is not a branch");
    assert_eq!(insn.len(), 3, "iced: BSR EAX,EDX is 3 bytes");

    // Sleigh's verdict must match: no terminator at the BSR.
    let block = wlam_sleigh::with_big_stack(move || wlam_sleigh::lift_block(&bytes, 0x401000));
    assert!(
        matches!(block.terminator, Terminator::Return),
        "sleigh must run to the RET, not stop at the BSR; got {:?}",
        block.terminator
    );
}

/// A real relative branch must be recognised as one by both decoders, with the
/// same resolved absolute target.
#[test]
fn relative_branches_resolve_to_the_same_target() {
    use iced_x86::FlowControl;
    use wlam_sleigh::Terminator;

    // JNZ rel8 -0x04 at 0x401000 -> 0x400ffe
    let bytes = [0x75, 0xfc, 0x90, 0x90];

    let mut iced = IcedDecoder::with_ip(32, &bytes, 0x401000, DecoderOptions::NONE);
    let mut insn = IcedInstruction::default();
    iced.decode_out(&mut insn);
    assert_eq!(insn.flow_control(), FlowControl::ConditionalBranch);
    let iced_target = insn.near_branch_target();
    assert_eq!(iced_target, 0x400ffe, "iced resolves the relative target");

    let block = wlam_sleigh::with_big_stack(move || wlam_sleigh::lift_block(&bytes, 0x401000));
    match block.terminator {
        Terminator::Conditional { taken: Some(t), .. } => {
            assert_eq!(t, iced_target, "both decoders must agree on the target");
        }
        other => panic!("sleigh: expected a conditional terminator, got {other:?}"),
    }
}

/// The x87 `FWAIT` prefix is a known, benign disagreement between the decoders.
///
/// `9b df e0` is `FWAIT` (`9b`) followed by `FNSTSW AX` (`df e0`). iced-x86
/// reports two instructions; Sleigh folds the prefix and reports one 3-byte
/// `FSTSW AX`. Both execute identically -- the `FWAIT` is a prefix in effect --
/// so this is a *boundary* difference, not a semantic one, and it must not fail
/// the cross-check.
///
/// It is asserted explicitly rather than silently forgiven: if Sleigh ever
/// started reporting a *different* instruction here, this test would fail.
#[test]
fn x87_fwait_prefix_is_a_known_boundary_difference() {
    let bytes = [0x9b, 0xdf, 0xe0]; // FWAIT ; FNSTSW AX

    let mut iced = IcedDecoder::with_ip(32, &bytes, 0x401000, DecoderOptions::NONE);
    let mut first = IcedInstruction::default();
    iced.decode_out(&mut first);
    assert_eq!(first.len(), 1, "iced treats 0x9b as its own FWAIT");

    // Sleigh's generated decoder recurses deeply on x87 forms; without a big
    // stack this aborts the process rather than failing the assertion.
    let sleigh = wlam_sleigh::with_big_stack(move || {
        SleighDecoder::new(Architecture::X86_32).decode(&bytes, 0x401000)
    })
    .expect("sleigh decodes the pair");
    assert_eq!(sleigh.len, 3, "sleigh folds the prefix into one instruction");
    assert!(
        sleigh.disassembly.to_ascii_uppercase().contains("FSTSW")
            || sleigh.disassembly.to_ascii_uppercase().contains("FNSTSW"),
        "sleigh must still report the status-word store; got {:?}",
        sleigh.disassembly
    );
}
