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

A real PE32 fixture now runs interactively in the browser through the Rust
Win32 host: window creation callbacks, DIB/BitBlt output, mouse and keyboard
input, repainting, and normal exit. Chromium acceptance tests verify pixels,
input, waiting, and Stop/Restart. See [apps/web/README.md](apps/web/README.md)
for build, launch, and test instructions.

`wlam analyze` reports imports; `wlam recompile <exe> -o <new-directory>` emits
a bundle for the browser loader. This remains an experimental Win32 subset;
Uxn32 and Minesweeper are not claimed to run end to end. See
[docs/roadmap.md](docs/roadmap.md) for compatibility gaps.

The root workspace tests the active `wlam-sleigh` lifter. The older handwritten
`wlam-lift-x86` is retained separately for reference with known semantic failures.

## Building

The toolchain is **workspace-local** (this machine's `$HOME` is on a read-only
filesystem and Homebrew installs are blocked), so everything lives in
`toolchain/`:

```sh
export CARGO_HOME=$PWD/toolchain/cargo
export RUSTUP_HOME=$PWD/toolchain/rustup
export PATH=$PWD/toolchain/cargo/bin:$PWD/toolchain/bin:$PWD/toolchain/tools/bin:$PWD/toolchain/wabt-src/build:$PATH

cargo test --workspace --offline  # active pipeline + available corpus tests
scripts/loop.sh                  # tests + validation of both benchmark images
```

`toolchain/bin/wasm-ld` symlinks the `rust-lld` shipped with the Rust toolchain;
`toolchain/tools/bin/wasm-tools` and `toolchain/wabt-src/bin/*` are built from
source because Homebrew is unavailable here.

### Toolchain requirements

- Rust 1.99.1 with `wasm32-unknown-unknown` and `wasm32-wasip1`.
- `clang` (for differential fixtures, `--target=i686-pc-windows-msvc`).
- Generated Sleigh decoders: run `scripts/fetch-deps.sh rsleigh` once after
  installing the local Rust toolchain. The script pins rsleigh and applies the
  tracked SUBPIECE and segment-base patches before generation.
- The recompiler emits wasm itself.

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
- `docs/crt.md` — how versioned MSVCRT/UCRT imports are satisfied by the host
- `docs/linked-dlls.md` — supplying and statically linking non-system DLLs
- `scripts/wine-tests/README.md` — run Wine's GDI conformance tests incrementally
- `docs/compat-matrix.md` — every imported API: implemented / emulated /
  unsupported (generated)

## License

MIT OR Apache-2.0.
