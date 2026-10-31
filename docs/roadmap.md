
# Roadmap and known gaps

This file records what is **not** working, so status can never be inferred from
optimistic narration. Update it whenever a claim would otherwise be stale.

## Working, with tests

- `wlam-pe`: PE32/PE32+ parsing; 1,461-image corpus at zero failures.
- `wlam analyze`: import inventory and compat matrix (ordinal resolution via
  reference DLL export tables, with provenance).
- `wlam-sleigh`: x86-32 lifting via Ghidra's Sleigh (71 tests, including the
  iced-x86 boundary cross-check). Measured on Uxn32: P-U/P-T = 4.0, five userops,
  60 registers.
- Discovery: 3,108 blocks / 74 functions on Uxn32; jump tables recovered for 16 of
  18 indirect sites. No block is claimed outside the image (see the fix below).
- `wlam-emit` translator: IR → wasm for straight-line code. Differential gate
  passes 7/7 on registers and flags.
- Host layer (`wlam-host`): cranky implementations of `GetVersion`,
  `GetVersionExA`, `GetStartupInfoA`, `SetHandleCount`, and the heap family
  (`HeapCreate`, `GetProcessHeap`, `HeapAlloc`, `HeapFree`, `HeapReAlloc`,
  `HeapSize`, `HeapDestroy`), each refusing when an argument cannot be honoured.
- `scripts/loop.sh` now scoreboards **two** images (Uxn32 gates the exit code;
  Minesweeper is reported with its blocking reason).

## Working (chaining, added this round)

- **Control-flow chaining executes correctly.** Direct jumps chain into the
  next block, and conditional branches take the right path in both directions,
  verified by executing modules and reading guest memory
  (`tests/chain.rs`, 3 tests).

Two real bugs were fixed to get there:
- The function section declared `order.len()` functions while the code section
  emitted one fewer when a target block was absent — caught by `wasm-tools
  validate` as "function and code section have inconsistent lengths".
- The chained entry function did not seed registers, so every local started at
  zero: a `jl` with `EAX=7` still branched as if `EAX` were 0. The entry now
  loads its registers from the snapshot area before running.

## Working (calls and returns, added this round)

- **Direct guest `call`/`ret` across functions executes correctly.** A `call`
  ends its block (`Terminator::Call { resume }`), the callee becomes its own
  wasm function, and control resumes at the instruction after the call.
  Verified by execution (`tests/calls.rs`, 2 tests): the callee runs, the caller
  continues afterwards, and the module terminates instead of re-entering the
  dispatcher.
- Registers are passed across a call through the shared snapshot area: the
  caller spills before `call`, the callee seeds from it, and both spill on
  `ret`. Without this the callee started at ESP = 0 and its `ret` popped the
  return address from address 0.

Two bugs found on the way:
- The lifter never ended a block at a `call`, so it lifted straight through and
  the emitter had nowhere to put the `call`.
- Discovery silently stopped at `max_passes`. Once calls became block
  boundaries every call adds a resume edge, so the limit was reached and work
  was dropped. `Program::pending()` now reports it and
  `discovery_converges_on_the_north_star` asserts it never happens. Raising the
  limit took prologue coverage from 15/25 to **23/25**.

## Working (IAT calls -> host imports, added this round)

- **A call through the IAT becomes a wasm import**, per `docs/abi.md`: the
  import namespace is the DLL (`KERNEL32.dll`) and the field the API name. `call
  dword ptr [slot]` lifts as `Load T <- [slot]; CallInd T` with a constant slot,
  which is statically a Win32 import, so the emitter resolves it at compile time
  and emits `call <import index>` instead of dispatching into guest code.
  Verified by execution (`tests/host_calls.rs`, 3 tests): a live host function is
  called, its return value lands in EAX, and the guest resumes after the call.
- On Uxn32 this resolves **150 of the 156 called IAT slots (96%)** to verified
  imports across 6 DLL namespaces, from **74 guest functions**.

Two bugs found on the way:
- The dead `Load` that reads the IAT slot was being emitted, so the module read
  an `.idata` address that need not be mapped and trapped in the browser.
  Only the load *feeding* the call is dropped -- dropping every load in the
  block removed temps that a neighbouring conditional's branch condition needed
  ("condition temp has no local").
- Guest function indices must be shifted past the imports; imports occupy the
  low function indices in the wasm index space.

## Working (cranky host layer, added this round)

- **The host refuses rather than guesses.** `wlam-host` implements nothing by
  default; a call to an unregistered API traps with a diagnostic naming the DLL,
  the symbol and the guest stack pointer. Verified against the real image
  (`tests/host_refusal.rs`). That test's first refusal was
  `KERNEL32.dll!GetVersion: not implemented (args at 0x6ff88)`; `GetVersion` is
  now implemented and the refusing set has moved on to `HeapCreate`,
  `GetStartupInfoA` and `SetHandleCount`.

  This is a deliberate inversion of normal shim behaviour. A stub returning `0`
  for `OpenFileA` is indistinguishable from a real implementation reporting "no
  file", so the guest continues with a garbage handle and fails far from the
  cause. The cost is that programs stop at the first gap; that is the trade, and
  `docs/abi.md` already required the diagnostic.

- `Host::declare` reserves a name without implementing it, `Host::implement`
  is the only way a call returns a real value, and `Host::out_of_scope` records
  a decision with a reason. `OnRefuse::Return(v)` exists for probing but still
  records the refusal, so probing does not erase the truth.

- `wlam-harness` links a recompiled module against the host and reports a
  refusal as `RunError::Refused` carrying the diagnostic, instead of an opaque
  wasmi trap.

**To make the guest run**, the harness seeds a stack pointer and TIB base the
way a loader would. Without that the first `push` writes below address zero —
the module is valid and instantiates, but was never actually runnable.

## Working (sub-registers and image loading, added this round)

- **Sub-registers alias their 32-bit parent.** Sleigh models `AL`, `AH`, `AX`
  and `EAX` as four independent varnodes, so `mov dl, ah` lifts to
  `Copy { dst: Reg("DL",1), src: Reg("AH",1) }` and the following
  `mov [slot], edx` reads a *different* name again. Giving each name its own
  wasm local made the copy dead code: Uxn32 called `GetVersion`, got 5.0 back,
  and stored a major version of 0.

  `wlam-pcode::regfile` now maps a name to the byte window it occupies
  (`AH` = EAX byte 1, `DH` = EDX byte 1, ...), and the emitter turns a narrow
  write into `parent = (parent & !mask) | ((value << shift) & mask)` and a
  narrow read into `(parent >> shift) & width_mask`. Both directions are pinned
  by `tests/subregisters.rs` (5 tests), including preserving the parent's other
  bytes. A cross-check test asserts the table agrees with
  `wlam_sleigh::X86_32_REGISTERS`, so the intentional duplication cannot drift.

  Two bugs found on the way: masking the value *before* shifting with the
  already-shifted mask cleared every bit (`DH` wrote byte 2), and the clear-mask
  immediate went through **unsigned** LEB128 — the same `i32.const` encoding
  mistake as `-4` earlier, caught by the runner as `extra bits in varint`.

- **The PE image is now mapped into guest memory.** Nothing was loading it. The
  harness seeded ESP and the TIB and stopped, so `.text` at `0x401000` read as
  zero and every "the guest didn't store it" observation was measuring
  uninitialised memory. `wlam_harness::load_image` maps headers and sections at
  the image base (BSS zero-filled, as the loader does) and can poison unfilled
  IAT slots with `0xFFFFFFFF` so a stray call traps instead of reaching
  address 0.

- **`KERNEL32.dll!GetVersion` is implemented** (`wlam-host/src/version.rs`).
  The host presents as Windows 2000 (`0x08930500`), the oldest NT-platform
  version, chosen once as `HOST_VERSION` so `GetVersion` and the future
  `GetVersionExA` cannot disagree. Uxn32 unpacks it exactly as expected and now
  records major 5 and minor 0 in the three slots at `0x415b78..0x415b84`.

- **Guest memory covers the image.** The memory section declared a fixed 16
  pages (1 MiB), but a PE32 image loads at `0x400000` — page 64 — so every
  absolute access would have trapped. `build_module_with_memory` sizes the
  memory to cover the image; the old `build_module` keeps the 16-page default
  for the byte-level fixtures.

- **The progress oracle no longer lies about its working directory.** It
  resolved `../../north-star/...` and the validator path relative to the cwd, so
  running it anywhere but `crates/wlam-sleigh` reported `RESULT: INVALID` for a
  module it never checked. Paths are anchored to the source location, and a
  missing validator now reports `UNCHECKED` rather than `INVALID`.

- **A guest run is fuel-metered.** Once the image loaded and `GetVersion` was
  served, the *probing* tests (which return 0 for every unimplemented API) let
  the guest run much further -- and then forever, hanging the suite with no
  output. `instantiate` now enables wasmi fuel with a generous budget, so a
  runaway guest fails by name instead of wedging the run.

## Minesweeper: first look

`benchmarks/minesweeper/bin/minesweeper-x86.exe` is PE32/i386 and gets further
than expected before stopping:

```
RESULT: BLOCKED  blocks=4203 functions=89 imports=125
REASON: unsupported operand width 10 bytes
```

(The block count was 4319 before the partition fix below; the 116 difference was
bogus blocks produced by phantom branch targets, not real code.)

Established by measurement:

- The image is well-formed: 17 sections, `.text` at `0x1000..0x1d1b0`, 125
  imports, entry `0x4014a0`.
- **35 blocks contain a 10-byte operand** (measured by `examples/probe_x87.rs`;
  the "11 of 1346" figure in an earlier revision was against the buggy block
  set). The operands are x87 `ST0`-style values and they are real: the bytes at
  `0x414b3a` decode to a function with `and esp,0xfffffff0` / `sub esp,0x24` /
  `fld qword [ebp+0x10]`, and `0x40190f` holds `FLD double ptr [0x426388]` /
  `FSTP double ptr [ESP+0x24]`. This is genuine x87 floating point, not garbage.
- Those blocks are reachable from the entry point.

**x87 remains the blocker for the benchmark.** The roadmap defers full x87 to
the 32-bit front-end work; the practical subset here is `fld`/`fstp` of doubles,
compared and stored back, which is a contained piece of work rather than the
whole FPU.

### Fixed: blocks below the image base were not mis-decoded padding

An earlier reading of this defect called the `0x1`/`0x2` blocks "mis-decoded
padding treated as code, with 10-byte operands". That was wrong on every count,
and the corrected diagnosis is recorded here because the wrong version cost a
round. The instructions were real and correctly decoded; only their *branch
targets* were garbage. The "10-byte operand" rejection is an unrelated x87 issue.

Two distinct bugs produced the phantom blocks, both now fixed with tests:

- **`Const`-space varnodes were read as addresses** (`wlam-sleigh/src/lib.rs`).
  A genuine relative branch arrives as a `Ram`-space varnode holding the resolved
  absolute address (`jmp +5` at `0x1000` gives `Branch{dest: Ram 0x1007}`). Sleigh
  also emits `Const`-space destinations, but those are *internal microcode labels*,
  not addresses: `BSR EAX,EDX` (`0f bd c2`) compiles to a conditional microcode
  path whose internal branch is `CBranch{dest: Const 2}`, meaning "the spec's own
  label 2". Treating `Const` as equivalent to `Ram` invented **70** phantom branch
  targets below the image base on Minesweeper (`0x1` and `0x2`), claimed by up to
  three unrelated functions at once. Guarded by
  `const_space_branch_is_not_a_terminator`, with the complementary
  `ram_space_branch_is_still_a_terminator` proving real branches still terminate.
- **`saturating_sub` turned "below the base" into "RVA 0"**
  (`wlam-sleigh/src/discover.rs`). `0x1.saturating_sub(0x400000)` is `0`, a valid
  RVA inside `.text`, so a nonsense target validated as code. Eight sites used this
  idiom; all now go through a checked `rva_of(base, addr)` helper. Guarded by
  `addresses_below_the_image_base_are_not_code` and the end-to-end
  `no_discovered_block_lies_outside_the_image`.

Effect: `claimed blocks below the image base: []` (was `[1, 2]`); phantom branch
targets 70 → 0; duplicate claims 161 → 157. The remaining 157 are *legitimate*
tail sharing (`0x402b60` is genuinely reached from two functions), where emitting
the block once per wasm function is correct.

### The progress oracle only watched one image

`scripts/loop.sh` reported `RESULT: OK` throughout the period this bug was open,
because the oracle defaults to `north-star/bin/Uxn32.exe` and never ran
Minesweeper. A scoreboard that inspects one image cannot report a regression in
the other. `loop.sh` now runs both: Uxn32 gates the exit code, and the benchmark is
reported with its blocking reason, announcing loudly if that reason ever changes
from the documented x87 gap.

### Cross-check: Sleigh vs iced-x86, as a test

Sleigh is the semantic source of truth, but its decoder is generated code and a
silent desync would corrupt every block after it. `tests/iced_crosscheck.rs`
compares instruction boundaries against iced-x86 (already vendored; now a
*dev-dependency*, so production code cannot reach it) at every block start
discovery chose:

| Image | Block starts | Instructions compared | Disagreements |
|---|---|---|---|
| Uxn32 | 3,108 | 24,864 | 0 |
| Minesweeper | 4,203 | 33,624 | 1 (`fwait`) |

The single disagreement is `9b df e0`: iced-x86 reports `FWAIT` + `FNSTSW AX` as
two instructions, Sleigh folds the prefix into one 3-byte `FSTSW AX`. They execute
identically, so it is a boundary difference, not a semantic one -- pinned
explicitly by `x87_fwait_prefix_is_a_known_boundary_difference` rather than
silently forgiven. A *length* disagreement on anything else fails the test.

A linear sweep of the whole section was tried first and abandoned: it walks
through data and can report "mismatches" at addresses that were never instruction
starts (`0x40c7b4`, mid-instruction inside a `MOV [ECX],AL` sequence). Comparing at
block starts is the question a recompiler actually asks.

### Tools we already had but were not using

Found while investigating, not yet acted on beyond the cross-check:

- **`wlam-sleigh` is not a workspace member.** `Cargo.toml` lists 8 crates; this
  one carries its own `Cargo.lock` and `target/`, and `loop.sh` builds it
  separately. That is why the bug above survived: nothing in the main build
  compiles against this crate. Moving it in is the structural fix.
- **`iced-x86` was only used by `wlam-lift-x86`**, the crate the CLI does *not*
  use for lifting. It now also backs the cross-check.
- **`goblin` 0.9 and `object` 0.36/0.40 are in the offline registry and unused.**
  PE parsing is hand-rolled in `wlam-pe`, and hand-rolled parsing is where the
  base-vs-RVA class of bug lives. Not adopted yet.
- **Zydis is not available offline** (crates.io is unreachable; only the vendored
  registry resolves), so it is not an option without network access. For the
  control-flow facts we needed, iced-x86 provides the same information and is
  already installed.

## x87: lifting fixed, emission reverted

The observable symptom was `Minesweeper: unsupported operand width 10 bytes`.
Investigating it turned up a **silent correctness bug** in the lifter that had
nothing to do with width, and which is now fixed.

**x87 arithmetic was being discarded entirely.** `FMUL ST1` lifted to three
`Nop`s. The cause was not the peephole optimizer (it behaves correctly — it sinks
the result straight into `ST0`) but `ir::lower()`, which had no arms for the
P-code float opcodes, so every one fell through to `IR::Nop`:

```text
FloatMult { out: ST0(10), left: ST0(10), right: ST1(10) }   -> Nop   (before)
IR::Float { op: Mul, dst: ST0, operands: (ST0, ST1) }       -> real  (now)
```

This is the worst kind of bug for a recompiler: the guest does not crash, it
silently reads a stale `ST0`. Lifted blocks now report `unlowered=0` for these
instructions instead of dropping them.

Fixed in the lifter (`wlam-sleigh/src/ir.rs`), all with the same root cause:

- `FloatAdd`, `FloatSub`, `FloatMult`, `FloatDiv`, `FloatEq`, `FloatNotEq`,
  `FloatLess`, `FloatLessEq` -> new `IR::Float` with a `FloatOp` enum, kept
  separate from `BinOp` so a real-number op can never reach an integer
  instruction.
- `Int2Float`, `Float2Float`, `Trunc` -> new `IR::Convert` with `ConvertOp`.
  `FSTP m64` lowers to `Float2Float ST0(80) -> tmp(64)`; without this the store
  received an untyped temp.
- Consequently `FLD`/`FSTP`/`FMUL`/`FADD`/`FXCH` now lower with
  `unlowered == 0` rather than one dropped op each.

Two genuine **emitter** bugs were found and fixed on the way, unrelated to x87:

- **`i64.const` used a broken LEB encoder.** The hand-written loop shifted a
  `u64` logically, so a negative constant never reached the terminator and the
  validator rejected the module with "unable to read i64 leb128". Now uses a
  proper signed `encode::leb_s64`.
- **`i64.load` / `i64.store` were never implemented.** They are needed for
  `CMPXCHG8B` (a genuine 64-bit integer compare-exchange), not only for floats.

**Emission of x87 was attempted and reverted.** The IR carries the operations
correctly, but representing the 80-bit stack as f64 locals regressed Uxn32
(which had been emitting a valid module) with thousands of type errors. The
emitter now refuses x87 explicitly (`Unsupported("x87 floating point")`) rather
than emitting an invalid module, so the blocker is still named honestly and
`RESULT: BLOCKED` is accurate.

## x87 emission: what now works, and the 21 remaining errors

The width-10 blocker is **gone**. Minesweeper emits a 632 KB module. It still
fails validation with 21 errors, all one family:

```text
type mismatch in local.set, expected [f64] but got [i64]
```

**Working now** (verified: Uxn32 emits `RESULT: OK` throughout):

- `ST0`-`ST7` are `f64` locals, seeded at fixed indices 12-19.
- `FLD`/`FSTP`/`FADD`/`FMUL`/`FSUB`/`FDIV`/`FXCH` emit real f64 wasm.
- `FILD`/`FISTP` convert via `f64.convert_i32_s` / `i32.trunc_f64_s`.

The single decision point is [`is_x87_register`] in `translate.rs`, answered by
register **name**. That was the lesson from the first failed attempt, where
floatness was decided in three competing places.

Three register-plumbing paths had to become type-aware together -- they all
assumed every register slot is 4 bytes, and adding f64 registers broke Uxn32
until all three were fixed:

- `spill_registers` (caller saves registers into the snapshot area)
- `reload_registers` (callee restores them)
- the program-entry seed loop in `build_module`

**The remaining 21 errors** are `FSINCOS`/`FPREM1`/`FXAM`. Sleigh compiles these
to *userops* (`id 99`, `105`, ...) whose results feed `ST0`:

```text
Userop { id: 99, args: [ST0], dst: Some(Temp{size: 4}) }
Copy   { dst: ST0(10), src: Temp{size: 4} }
```

The userop writes a 4-byte temp that is then copied into `ST0`. No
implementation exists, so the temp is never real-valued and the copy mismatches.
Two options, in order of honesty:

1. **Implement the userops.** `FXAM` is a classification (sign/exponent/zero/
   NaN) and is tractable. `FSINCOS` and `FPREM1` are the mingw-w64 `sinl` range
   reduction; `FPREM1` needs an iteration bound so a non-converging
   approximation becomes a named failure rather than a hang.
2. **Refuse them.** `TranslateError::UnimplementedUserop(id)` already exists for
   exactly this; the guard just does not yet catch the temp-mediated form.

Do not stub these with a plausible value: a wrong `ST0` silently corrupts every
subsequent x87 result, which is the class of bug this whole round was about.

## Minesweeper validation: 6255 -> 0 errors

**Both benchmark images now emit modules that pass `wasm-validate`:**

```text
Uxn32        RESULT: OK  blocks=3108 functions=74 imports=154
Minesweeper  RESULT: OK  blocks=4203 functions=89 imports=129
```

The path, in the order the errors surfaced:

| Errors | Cause | Fix |
|---|---|---|
| 6255 | `FSINCOS` userop results feeding `ST0`; float ops never lifted | `IR::Float`/`IR::Convert` lowering arms |
| 21 | x87 loads emitted as `i64.load` into an f64 local | restored the `operand_is_x87(dst)` branch |
| 12 | `IntToFloat` destinations declared i64; f64 widened as integer | mark conversion destinations f64 |
| 10 | left operand of a mixed-width binary op left as i32 | widen lhs before pushing rhs |
| 6 | f64 destination converted as if integer | skip conversion for f64 locals |
| 2 | carry/borrow helpers hardcoded i32 scratch | made them width-aware |

### The last fix, and why it took several attempts

`emit_borrow` and `emit_carry_signed` computed flags with fixed 4-byte i32
scratch locals and i32 opcodes. `CMPXCHG8B` compares `EDX:EAX` against a qword,
so the 64-bit subtract stored an i64 into an i32 slot.

Three earlier attempts made things worse and were reverted:

- a `stack: Vec<u32>` of every pushed width (45 / 171 errors)
- a single `last_pushed: u32` (45 / 149)
- widening every flag computation to i64 unconditionally (Uxn32 regressed to 878)

Both stack-tracking schemes drifted because `emit_bin`/`emit_float` push several
values and pop them in ways the tracking did not follow. The unconditional-widen
version was wrong for a different reason: a 32-bit `CMPXCHG8B`-style flag must
stay 32-bit, so forcing i64 changed results.

What worked was local and explicit: the helper pushes both operands first,
derives one `wide` flag from what `push_value` reported, widens the narrow
operand to match, and then uses the matching opcode family throughout -- including
the final mask and the `i32.wrap_i64` before `normalize` applies
`i32.extend8_s` (which has no i64 form).

### Method note

The bug that took longest to find was an `if self.operand_is_x87(dst)` branch
that had been **deleted by an earlier edit**, so every x87 load fell through to
`(8, _) => I64_LOAD`. A width trace in `push_value`/`pop_into` found it in one
run; several rounds of reasoning from disassembly had not.

Also: stripping debug code with a regex silently broke two functions. Add and
remove instrumentation by hand.

### Still not done

Validation passing is not execution. These modules have not been *run* yet
(no host layer for `WriteFile`, etc.), and the deferred work listed under
"Next round" still stands.

## Next round: what is actually in the way

Ordered by what the measurements say blocks progress, not by what looks topical.

1. **x87 subset, for Minesweeper.** `unsupported operand width 10 bytes`,
   measured as 1,160 operands across **35 blocks (0.8% of 4,203)**.

   **Status: attempted and reverted.** The lifter half is done and landed (see
   "x87: lifting fixed, emission reverted" below); the emitter half is not.

   *Design note for whoever picks this up.* The x87 stack must be modelled in
   exactly ONE place. An attempt that decided float-ness independently from
   (a) declaration by register name, (b) temp width, and (c) a per-block
   pre-pass produced 4,648 validation errors on Uxn32 and 6,255 on Minesweeper,
   all "expected [f64] but got [i32]", and broke Uxn32 which had been passing.
   Each fix moved the failure rather than removing it. The next attempt should
   keep the x87 stack entirely out of the integer register-local namespace --
   a separate f64 array indexed by stack position, with one function deciding
   what is real-valued -- rather than threading a "is this f64?" flag through
   `LocalMap`, `temp()`, `push_value` and `pop_into` at once.

   The mnemonic inventory was measured with `X87_SITES=1 examples/probe_x87.rs`
   and split by reachability from the entry point (87 instruction sites):
   **43 reachable, 44 unreachable**.

   Reachable — this is the work that actually matters:
   `FSTP` 19, `FLD` 9, `FXCH` 3, `FMUL` 2, `FNSTSW` 2, `FSINCOS` 2, `FADD` 1,
   `FLDZ` 1, `FLDPI` 1, `FSTSW` 1, `FXAM` 1, `FPREM1` 1.

   Unreachable, so *not* needed for this benchmark (do not implement them
   speculatively): `FLD` 8, `FSTP` 8, `FILD` 4, `FLDCW` 4, `FMUL` 4, `FSTSW` 2,
   `FXCH` 2, `FXAM` 2, `FISTP` 2, `FDIV` 2, `FADDP` 1, `FMULP` 1, `FLD1` 1,
   `FLDZ` 2, `FNSTCW` 1.

   Critically, the hard cases are mostly *unreachable*: `FILD`/`FISTP` (integer
   conversion), `FLDCW`/`FNSTCW` (rounding-mode save/restore), `FDIV`, `FMULP`,
   `FADDP` and `FLD1` are all off the executed path. Two reachable exceptions need
   care — `FSINCOS` and `FPREM1`, which are the CRT range-reduction/transcendental
   helpers at `0x41c8xx`, and `FXAM` used for float classification at `0x418322`.
   These are the genuinely hard part, and they are 4 of the 43 sites.

   So the practical target is a **double-precision subset**: `FLD`/`FSTP` of
   `float`/`double`, `FXCH`, `FADD`, `FMUL`, `FLDZ`/`FLD1`/`FLDPI`, plus status-word
   reads (`FNSTSW`/`FSTSW`) because MSVC tests them after compare/classify.

   Note that the boundary cross-check already surfaced one concrete x87 fact:
   Sleigh folds the `9b` FWAIT prefix (`9b df e0` → one `FSTSW AX`) where iced-x86
   reports `FWAIT` + `FNSTSW AX`. Any x87 work must decide which boundaries the
   recompiler uses, and the cross-check will hold either choice to account.
2. **Move `wlam-sleigh` into the workspace.** It is the one crate the root
   `cargo test` never compiles. Doing this first makes every later round cheaper,
   because a regression in the lifter would then break the main build instead of
   hiding in a separate `target/`.
3. **Populate indirect-call tables semantically.** 163 of 714 indirect calls on
   Uxn32 are through a register. They emit, but a target only resolves when the
   host supplies a table id, which needs `GetProcAddress` and a real module handle.
4. **Then the next unimplemented startup API.** The startup set
   (`GetStartupInfoA`, `SetHandleCount`, `GetVersionExA`) is implemented and tested
   but **not yet reached** on the Uxn32 run -- the guest still stops earlier, at
   the register-target call. So the next API to implement is whatever the guest
   asks for after that boundary moves; guessing now would repeat the mistake of
   implementing APIs that no execution path reaches yet.

## Working (process startup APIs, added this round)

- **`GetStartupInfoA`** (`wlam-host/src/startup.rs`), writing a full
  `STARTUPINFOA`. Uxn32 calls it at 0x40cd51 and branches on
  `si.dwFlags & STARTF_USESHOWWINDOW`, reading `si.wShowWindow` when set:

  ```text
    lea  eax, [ebp-0x5c]      ; &si
    push eax
    call [GetStartupInfoA]
    test byte [ebp-0x30], 1   ; si.dwFlags
    jz   default
    movzx eax, word [ebp-0x2c]; si.wShowWindow
  ```

  The field offsets were taken from that code rather than from memory. `cb` is
  validated first and a mismatch is refused *without writing anything* -- a
  half-filled struct is worse than none, and writing 68 bytes into a caller that
  passed a smaller one would corrupt its frame.

  Reporting `dwFlags = 0` (no parent process, no console, no standard handles)
  is truthful rather than a stub: zeros here *mean* "nothing", and the guest
  handles that by taking its documented default. That is different from
  inventing a value.

- **`SetHandleCount`** echoes the requested count. Not a constant: returning a
  fixed number would be wrong for any caller asking for a different one, and
  returning 0 would claim a process handle limit this host does not impose.

- `startup::write_cstr` for `GetModuleFileNameA`/`GetCommandLineA`: clamps to the
  caller's buffer and reports truncation, instead of overflowing.

- 8 new tests (6 on the struct, 2 through the host's dispatch path).

**These APIs are not yet reached on the Uxn32 run.** The guest still makes only
`GetVersion` and `HeapCreate` before exhausting fuel, so the stall is upstream of
startup. They were implemented because they are on a real execution path (the
`wasm-interp` run reaches all four), not because they unblock the stall.

## Fixed (dispatch ids were sign-extended, this round)

**Every block id >= 64 was silently corrupted.** `i32.const` takes a *signed*
LEB128 immediate, and the dispatcher sets its id local with
`i32.const <block id>`. `chain.rs` wrote those ids with **unsigned** LEB128:

```text
  id 64  -> unsigned byte 0x40 -> decoded as -64
  id 70  -> unsigned byte 0x46 -> decoded as -58
```

Below 64 the encodings coincide, so the 21-block `WinMain` fixture and every
hand-written test passed. From id 64 the dispatcher receives an id it never
assigned, `br_table` takes its default arm, and that default branches straight
back to the dispatcher with the same out-of-range id: an infinite loop that
leaves every guest register untouched.

`wasm-validate` accepts this module — it is perfectly well-formed — so nothing
caught it until a trace showed `br_table` dispatching on `-58`.

- Fixed by encoding every dispatch id (and the resume id for calls) with
  `leb_s32`. The `ESP+4` argument addend at the host-call site stays unsigned
  because it is an arithmetic constant, not an `i32.const` id.

- `tests/dispatch_ids.rs` pins it with a 71-block jump chain: ids below 64 pass
  either way, ids past 64 do not. Verified discriminating by reverting the fix
  and watching two of the three tests fail.

- `leb_s32` is now re-exported from `wlam-emit`, since callers outside the
  crate need the same encoding rule.

**Two diagnostic lessons, both now encoded in tools:**

- `run_entry_diagnosing` used to replace the engine's error with "guest trapped
  with no recorded refusal", hiding *both* "out of bounds memory access" and
  "all fuel consumed". It now passes the original error through.

- Tracing the guest needs the **image loaded**. An unseeded `wasm-interp` run
  reads zeroed `.text`, takes a different path, and *appears to succeed* while
  the real harness spins. `examples/probe_seed.rs` injects the PE image as data
  segments plus the loader seed, so any runtime reproduces exactly what the
  harness runs. That is what made the cycle visible:
  `fn156 -> fn157 -> fn170 -> fn169 -> fn156`, forever.

## Working (full 4 GiB address space, added this round)

- **The guest gets the whole 32-bit address space.** The module now declares
  65536 pages (4 GiB) unconditionally, and the heap region is everything from
  `0x1000_0000` to 4 GiB (3.75 GiB) rather than a 256 MiB island.

  This is cheap because wasm reservation is *virtual*: measured on V8, eight
  4 GiB memories cost 0.6 MiB of RSS, and pages commit on first touch. A PE32
  program is entitled to address anything below 4 GiB and its code hard-codes
  addresses accordingly, so giving it the full space removes one class of
  failure the original program never had.

  **Reserving is not allocating.** `HeapAlloc` must still return a pointer that
  cannot overlap anything live and can later be freed; a bump allocator that
  never reclaims exhausts 4 GiB just as surely as 256 MiB, only later.
  `HeapSet` therefore still does real first-fit allocation -- it just never has
  to guess how much room to reserve.

  The 4 GiB boundary needs `u64` arithmetic: the exclusive limit is 2^32, one
  past the largest `u32`, and computing it in `u32` overflowed (caught by the
  compiler).

- **`GetVersionExA` is implemented** (`wlam-host/src/version.rs`), filling an
  `OSVERSIONINFOA`. Uxn32 reaches it via a 4652-byte `_alloca_probe` frame
  (`mov eax,0x122c; call __alloca_probe`) at 0x40d6df and passes
  `dwOSVersionInfoSize = 0x94`. The size field decides both success and how
  much is written, so a size the API does not recognise is rejected *without
  writing anything* -- a half-filled struct is worse than none. `szCSDVersion`
  is left empty: this host offers no service pack, and inventing one would be
  the plausible lie this layer exists to avoid. `GetVersionExW` stays refused
  rather than half-implemented.

  `GetVersion` and `GetVersionExA` share `HOST_VERSION`, so the two cannot
  disagree with each other.

## Working (heap, added this round)

- **A real Win32 heap** (`wlam-host/src/heap.rs`): `HeapCreate`, `HeapAlloc`,
  `HeapFree`, `HeapReAlloc`, `HeapSize`, `HeapDestroy`, `GetProcessHeap`.

  This is not a stub. Uxn32 creates a heap at 0x40d838
  (`HeapCreate(flOptions, 0x1000, 0)`) and stores the handle at `0x415f48`,
  branching to its failure path when it is NULL; `HeapAlloc` then appears at
  seven call sites. A stub constant would send the guest down the success path
  to write through addresses nobody allocated. So allocations are carved from
  the guest's own linear memory, meaning the pointers `HeapAlloc` returns are
  addresses the guest can actually dereference.

  - First-fit free list with coalescing, living *in* guest memory (no shadow
    copy to drift). Blocks carry an 8-byte header so `HeapFree` needs only the
    pointer, as the real API does.
  - 8-byte alignment on every returned pointer, which callers rely on for
    doubles.
  - Handles are `0x00A70000 + n`, so "valid" and "NULL" can never be confused
    and a handle leaking into a pointer field is obviously wrong when read back.
  - A failed allocation returns NULL and a bad handle is refused — both answers
    the guest already knows how to handle. A double free is refused rather than
    silently accepted.
  - 10 unit tests on `HeapSet` plus 6 through the host's real dispatch path.

- **The heap region is part of the ABI.** `HeapAlloc` hands out addresses, so
  the module's memory must cover them. Sizing memory to the image alone made
  every allocation land outside the memory, and the guest trapped on its first
  store through one — reported as a wild pointer in guest code with nothing
  pointing at the layout. `pages_for_image` now covers the image *and*
  `0x1000_0000..0x2000_0000`, and `tests/heap_abi.rs` asserts the host and
  emitter constants agree and do not overlap the image or the stack.

- **`run_entry_diagnosing` no longer discards the trap.** With no refusal
  recorded it replaced the engine's message ("out of bounds memory access",
  "all fuel consumed") with a generic string that identified nothing. The
  original error is now passed through, which is what located the problem above.

**Where Uxn32 gets to now**: `GetVersion` and `HeapCreate` are served, and
nothing further is reached. The guest burns fuel indefinitely with registers
that look *plausible* rather than frozen:

```text
EAX=0x122c  ECX=0x5  EDX=0x5  ESP=0x6ff60  EBP=0x6ff64  EIP=0x0
```

Established this round, each by direct measurement:

- **`GetVersionExA` is never called**, and the function that calls it
  (`0x40d6df`, wasm function 219) is never entered. So the stall is *before*
  the version probe, not inside it.
- **The call site is emitted correctly.** `0x40d6ec` lifts to
  `Load{addr: Const 0x4110a4}` then `CallInd`, `iat_slot_of` resolves it, and
  the module contains exactly one `call 45` (the `GetVersionExA` import). The
  emitter is not at fault here.
- **`wasm-interp` reaches six host APIs where the Rust harness reaches two**,
  but that comparison is misleading: a zero-stub `HeapCreate` makes the guest
  take its *failure* branch at `0x40d843` (`jz`), skipping the whole successful-
  heap path. Comparing the two runtimes was chasing a difference in *input*,
  not in correctness.
- **ESP is `0x6ff60`, not `0x6edd4`.** The `_alloca_probe` at `0x40e990` should
  reserve a `0x122c` frame; only 160 bytes moved. `MOV ESP,ECX` lifts and emits
  correctly, so the frame size or `ECX` reaching it is wrong -- not yet found.

Two diagnostics were added and removed during this round; the durable one is
`examples/lift_at.rs` (disassemble and lift any guest address), which is what
made the call sites above readable.

The refusal-reporting tests were updated to *report* what the guest reaches
rather than require a refusal, since progress legitimately turns into a
different failure.

## Working (64-bit operands, added this round)

- **Uxn32 emits a complete, valid module.** `RESULT: OK` now means *validated*:
  the progress oracle runs `wasm-validate` before claiming success, and the
  module independently passes `wasm-validate` and **instantiates in V8**
  (74 guest functions, 154 host imports, a 74-entry funcref table,
  408,702 bytes).

i64 support required typed locals (`LocalMap` previously discarded width and
declared everything `i32`), the `i64` opcode family, and three real fixes:

- **`i32.const` was encoded as unsigned LEB128.** `i32.const` takes *signed*
  LEB128, so a negative constant like `-4` was emitted as `fc ff ff ff 0f`
  (5 bytes) instead of `7c` (1 byte) and the extra bytes desynced the entire
  instruction stream. This was latent for any negative constant, independent of
  i64.
- **The opcode family was chosen from `dst.size()`.** A flag store such as
  `CF = (EAX64 != tmp64)` compares qwords into a 1-byte destination, so the
  *operands* must select the family and the result is narrowed afterwards.
- **`Un::Neg` was silently wrong.** It stored into the scratch temp *before*
  pushing the zero, so it computed `0 - 0` and left the original operand
  orphaned on the stack -- a net +1 leak that made the enclosing block fail to
  validate. This predates the i64 work.

## Working (function table, added this round)

- **Calls through a computed target go through a guarded wasm table.** The
  module now emits a `funcref` table with `table[i]` = guest function `i` (shifted
  past the imports) and `call_indirect (type 0) (table 0)`. The target is
  bounds-checked first, so a NULL or wild target **traps** instead of silently
  reaching function 0 -- a wrong answer would be worse than a crash. Verified by
  execution (`tests/indirect_calls.rs`, 3 tests: reaches the intended function,
  out-of-range traps, null does not call function 0).

Bug found: the `entry` export pointed at guest function **0** unconditionally.
`partition` pops entries from a stack, so the program entry is not always index
0 -- with a table-only callee declared as an entry, `entry` exported the callee,
which ran and returned without ever dispatching. `Program::entry_function` now
records the real entry. This was latent for any multi-function module.

## Scope: 32-bit Win32 only

The recompiler targets **PE32 / i386** exclusively. `discover` now refuses any
other machine type (`Program::refused()` / `problems()`) instead of decoding
64-bit code with an x86-32 lifter and producing plausible nonsense.
`machine_guard_tests` pins both directions using the two benchmark builds.

Note that "8-byte" and "10-byte" operands below are **x86-32 constructs**, not
x64: 8-byte temporaries come from `MUL`/`IMUL` (dword x dword -> qword in
EDX:EAX) and 10-byte operands are the x87 FPU stack (`ST0`-`ST7`, 80-bit).

## Not working

- **x87 (80-bit)**: `unsupported operand width 10 bytes`. Measured with
  `examples/probe_x87.rs` on Minesweeper after the phantom-block fix: **35 blocks
  (0.8% of 4,203)** contain a 10-byte operand, totalling **1,160** such operands.
  The blocks are real x87 (`FLD double ptr [0x426388]`, `FSTP double ptr
  [ESP+0x24]` at `0x40190f`), not garbage. This is the last blocker for emitting
  Minesweeper.

  An earlier revision of this file claimed "580 operands across 158 of 4,319
  blocks (3.7%)". That figure is superseded: it was measured against the buggy
  block set that included 116 phantom blocks. Re-measure rather than quoting it.
- **`wlam-sleigh` is outside the workspace.** It has its own `Cargo.lock` and
  `target/`, so `cargo test` at the repository root never compiles it and
  `loop.sh` builds it separately. A bug in this crate can therefore exist without
  breaking the main build -- which is exactly what happened with the `Const`-space
  terminator bug. Moving it into the workspace is the structural fix; it is not
  done yet because the build-layout change deserves its own round.
- Calls through a register are 163 of 714 indirect calls on Uxn32. They now
  emit, but the table is not yet *populated semantically*: a target only
  resolves if the host supplies a table id (e.g. via `GetProcAddress`).
- 6 called IAT slots are not verified imports (1 unverified, 5 not import
  slots) and do not resolve.
- The emitter cannot yet build a full Uxn32 module: emission reaches a
  register-target call and stops there, which is the expected boundary above.
- The 13 failing tests in `wlam-lift-x86` are the retired hand-written lifter
  and should be deleted rather than fixed.
- Emitter lacks: x87 (80-bit), float ops, `Lzcount`, `MulFull`. (64-bit operands
  and the function table have since landed; see those sections above.)
- Remaining 8 indirect sites in Uxn32: 3 are static-unresolvable (register
  destination, two IAT thunk slots); 5 use `jmp [reg*4 + X]` where `X` is not
  the table — the correct rule must find the table from the load feeding the
  jump, not from the jump's address arithmetic.

### Resolved (recorded so they are not re-derived)

- **`MUL` reported `lsb = 0` for both halves of the product** (EDX = EAX). Root
  cause was a bytes-vs-bits units bug in rsleigh's code generator; fixed and the
  x86 tables regenerated. Evidence and the regenerate recipe are in
  `docs/sleigh-notes.md`, guarded by `ir::subpiece_tests`.
- **Blocks below the image base (`0x1`, `0x2`)** — see "Fixed: blocks below the
  image base" above. Two causes: `Const`-space varnodes read as addresses, and
  `saturating_sub` collapsing "below base" to "RVA 0". Both fixed with tests.
- **`partition` duplicating blocks across functions** — 161 → 157 claimed blocks.
  The remaining 157 are legitimate tail sharing, not a defect.

## Known process hazard

Twice this project lost a full round to a defect misdiagnosed repeatedly, in
both cases because probes and production code used different conventions
(RVA vs VA; seeded vs unseeded initial state). When a primitive works in
isolation but fails inside a pipeline, check the **units and the initial
state of the arguments** before re-deriving the logic.
