# Interactive PE32 fixture

Build with `scripts/build-interactive-fixture.sh` (clang, llvm-dlltool, and the
local Rust lld). Outputs are under `target/interactive`, not tracked binaries.
The fixture has no Windows SDK or CRT dependency. It uses `-O0` and
`-fno-jump-tables`: indirect switch dispatch remains outside this initial host fixture.
`WLAM_FIXTURE_OPT=-O1` enables optimized compilation, including cached import
pointers; the native interactive test also passes with that setting. Tail-call
optimization is disabled until indirect tail imports are supported. Import libraries use stdcall
symbol decoration for linking and undecorated PE imports via dlltool's `-k`.

Compile a bundle with:

```
cargo run -p wlam-cli -- recompile target/interactive/window.exe -o target/interactive/bundle
```

The window procedure is exported so static discovery retains it as a callback.
It creates a top-down 32×24 DIB, initially BGR `0x123456`. Left click changes it
to `0xe04020`; a non-Escape key changes it to `0x20c060`. Escape or WM_CLOSE
destroys the window and posts WM_QUIT. Exported counters expose paint, click,
key and destruction progress for integration tests.

The native Wasmi test (`cargo test -p wlam-sleigh --test interactive`) executes
the PE through the shared Rust dispatcher, asserts every pixel in three frames,
and verifies process exit after Escape. The browser adapter is still pending.
