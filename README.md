# `wlam` — Win32 PE → WebAssembly recompiler

Take a Windows executable and run it in a browser. No guest OS, no Linux
syscalls, and **no Wine or Proton in the pipeline** — Windows APIs are
implemented directly on top of the web platform.

## How it works

```
foo.exe (PE32)                 static recompiler                 browser
┌───────────────┐    ┌──────────────────────────────┐    ┌─────────────────────┐
│ .text x86-32  │───▶│ lift → P-code → wasm functions│───▶│ guest wasm module   │
│ import table  │    │ trampolines for Win32 imports │    │  + host imports     │
│ resources     │    └──────────────────────────────┘    │    WebGPU / WebAudio │
└───────────────┘                                        │    OPFS / DOM        │
                                                         └─────────────────────┘
```

Windows API calls are never translated as machine code: the import table is
rewritten so each slot points at a **host trampoline** into our Win32 layer.
That layer is ordinary Rust compiled to `wasm32-unknown-unknown` with no WASI,
because WASI cannot host threads, WebGPU, or Web Audio.

## Why not compile Proton/Wine to wasm?

Proton is Wine: an ELF loader that maps a PE's imports onto **Linux** libc, X11
and Vulkan. It implements the Linux side of the boundary, so it exports no
"Windows bindings" to reuse; reimplementing Wine faithfully means an x86-64 JIT,
DRM, ACP, Vulkan call conversion and a large Linux backend — none of which
compiles to portable wasm, and all of it is what the host layer replaces anyway.
We read Wine's sources as a **behavioral reference**, not as a build input.

## Status

| Component | State |
|---|---|
| `wlam-pe` PE32/PE32+ parser, imports/exports/relocs/resources | ✅ passes on 1,461 real Windows images |
| `wlam-pcode` IR | stub |
| `wlam-lift-x86` lifter | stub |
| `wlam-translate` discovery/call resolution | stub |
| `wlam-emit` wasm emission | stub |
| `wlam-host` Win32 layer | stub |
| differential harness | stub |

North star (V1): [`randrew/uxn32`](https://github.com/randrew/uxn32), ~3,000
lines of pure Win32 C (GDI blits, `waveOut`, file IO, menus) with no SDL or
DirectX. It is the first program the toolchain must run end to end.

## Building

The toolchain is **workspace-local** (this machine's `$HOME` is on a read-only
filesystem and Homebrew installs are blocked), so everything lives in
`toolchain/`:

```sh
export CARGO_HOME=$PWD/toolchain/cargo
export RUSTUP_HOME=$PWD/toolchain/rustup
export PATH=$PWD/toolchain/cargo/bin:$PWD/toolchain/bin:$PWD/toolchain/tools/bin:$PWD/toolchain/wabt-src/build:$PATH

cargo test --workspace        # unit + real-image corpus tests
```

`toolchain/bin/wasm-ld` symlinks the `rust-lld` shipped with the Rust toolchain;
`toolchain/tools/bin/wasm-tools` and `toolchain/wabt-src/build/*` are built from
source because Homebrew is unavailable here.

### Toolchain requirements

- Rust 1.99.1 with `wasm32-unknown-unknown` and `wasm32-wasip1`.
- `clang` (for differential fixtures, `--target=i686-pc-windows-msvc`).
- Nothing else: the recompiler emits wasm itself.

## Tests against real images

`crates/wlam-pe/tests/real_images.rs` parses every DLL under a local
Proton/Wine install (set `WLAM_TEST_WIN_DIR` to use a different corpus, or the
tests skip cleanly when none is present). At the time of writing that is
**1,461/1,461 images, 43,428 imports, 88,234 exports, zero failures**.

## Security

V1 does **not** sandbox untrusted executables: the guest runs in the same wasm
module as the host layer. See `SECURITY.md`. Do not run untrusted `.exe` files
with it.

## Documentation

- `docs/architecture.md` — component responsibilities and data flow
- `docs/abi.md` — guest/host ABI contract (work in progress)
- `docs/compat-matrix.md` — every imported API: implemented / emulated /
  unsupported (generated)

## License

MIT OR Apache-2.0.
