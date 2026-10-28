
# Roadmap and known gaps

This file records what is **not** working, so status can never be inferred from
optimistic narration. Update it whenever a claim would otherwise be stale.

## Working, with tests

- `wlam-pe`: PE32/PE32+ parsing; 1,461-image corpus at zero failures.
- `wlam analyze`: import inventory and compat matrix (ordinal resolution via
  reference DLL export tables, with provenance).
- `wlam-sleigh`: x86-32 lifting via Ghidra's Sleigh (28 tests). Measured on
  Uxn32: P-U/P-T = 4.0, five userops, 60 registers.
- Discovery: 1,687 blocks / 120 functions; jump tables recovered for 16 of 18
  indirect sites.
- `wlam-emit` translator: IR → wasm for straight-line code. Differential gate
  passes 7/7 on registers and flags.

## Not working

- **Control-flow chaining does not execute correctly yet.** Block chaining
  emits a validated module for a four-block counting loop, but running it does
  not terminate — the loop's exit condition is not taking effect. This is the
  current top blocker; the single-instruction differential gate passes, so the
  defect is in the chaining epilogue (how a block's `CBranch` condition reaches
  the `if`), not in instruction translation.
- Guest calls/returns across functions are not chained; a `Call` needs a
  function table, a `Ret` needs a return stack.
- The 13 failing tests in `wlam-lift-x86` are the retired hand-written lifter
  and should be deleted rather than fixed.
- Emitter lacks: 64-bit ops, x87 (80-bit), float ops, `Lzcount`, `MulFull`.
- Remaining 8 indirect sites in Uxn32: 3 are static-unresolvable (register
  destination, two IAT thunk slots); 5 use `jmp [reg*4 + X]` where `X` is not
  the table — the correct rule must find the table from the load feeding the
  jump, not from the jump's address arithmetic.

## Known process hazard

Twice this project lost a full round to a defect misdiagnosed repeatedly, in
both cases because probes and production code used different conventions
(RVA vs VA; seeded vs unseeded initial state). When a primitive works in
isolation but fails inside a pipeline, check the **units and the initial
state of the arguments** before re-deriving the logic.
