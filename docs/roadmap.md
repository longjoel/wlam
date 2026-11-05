# Current status and remaining work

The active pipeline is PE parsing → Sleigh x86-32 lifting → emitter IR → Wasm.
`wlam-sleigh` is a root workspace member. The older handwritten
`wlam-lift-x86` is retained separately for reference; it is not used by the CLI
or active pipeline and has known failing semantic tests.

## What is implemented

- PE parsing/import analysis and CLI bundle generation with `wlam recompile`.
- Sleigh lifting, block discovery, Wasm emission and a fuel-metered test harness.
- Execution tests for arithmetic, branches, calls, register aliases and host calls.
- A small Win32 host subset: version, startup information and heap APIs.
- A shared Rust USER32/GDI dispatcher with nested callbacks, window/message
  state, typed handles, DIB decoding, clipped SRCCOPY and reusable allocations.
- A real PE32 fixture tested in Wasmi and Chromium: exact framebuffer pixels,
  click/key interaction, repainting, normal exit, responsive waiting, Stop/Restart
  and termination of runaway guest execution. See apps/web/README.md.
- Register snapshots use eight bytes per register through one shared address
  function. Block execution loads the snapshot before guest operations.
- Guest calls adopt the callee's resulting ESP, including return/argument cleanup.
- Carry and signed overflow flags are computed at the operands' precision.
  The differential gate captures input state before interpreting, then tests
  boundary register values against emitted execution.

## Unsupported semantics

Unknown P-code retains its identity in an `Unsupported` IR operation. Standalone
translation returns an error. Whole programs register diagnostic imports under
`wlam.unsupported`; executing one reports the missing operation through the host
and traps even when probing mode returns a value. No zero result is fabricated.
Internal P-code branches are also unsupported until microcode control flow is
implemented. x87 uses an f64 approximation of 80-bit state; numerical compatibility
with native x87 is not established. Sin/cos helpers require host implementations.

The dependency fetch script pins rsleigh and applies tracked patches for
SUBPIECE units and absolute segment addressing, so regeneration preserves the
semantics covered by the tests.

## Checks

Use the local toolchain setup in README.md, then:

```sh
cargo test --workspace --offline
cargo check --workspace --all-targets --offline
scripts/loop.sh
```

`loop.sh` runs the active workspace suite and validates both benchmark images.
Both images gate its exit status, including missing inputs, failed validation and
incomplete discovery. `--fast` omits execution tests and says so explicitly.
A valid module does not establish correct guest execution.

Real-image tests require the fetched images; corpus tests may skip when their
external inputs are absent. The browser runner supports the small interactive
fixture, not arbitrary Win32 applications. Neither benchmark is claimed to run
end to end. The CLI `run` command is not implemented; use the browser runner.

## Next work

1. Extend the dispatcher in response to real Uxn32 startup/runtime refusals.
2. Resolve cached import pointers and computed guest addresses dynamically.
3. Implement indirect switch dispatch and internal P-code control flow.
4. Extend the staged windowing, GDI, storage and audio API coverage.

Earlier investigation notes are preserved in
[history/development-notes.md](history/development-notes.md). They are historical,
not a current compatibility specification.
