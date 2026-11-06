# Architecture

## Pipeline

```
             ┌──────────────┐
 .exe/.dll ─▶│  wlam-pe     │─▶ typed image: sections, imports, exports,
             │  (parse)     │   relocations, resources
             └──────┬───────┘
                    ▼
             ┌──────────────┐   discovery from entry point + exports +
             │ wlam-sleigh│   IAT callers; jump-table recovery
             │  x86 → P-code│
             └──────┬───────┘
                    ▼
             ┌──────────────┐   per-PC dispatch, call/ret ABI, indirect
             │wlam-translate│   target resolution, unsupported diagnostics
             └──────┬───────┘
                    ▼
             ┌──────────────┐
             │  wlam-emit   │─▶ wasm module: one function per block, a
             │ P-code→wasm  │   shared linear memory, host trampoline imports
             └──────┬───────┘
                    ▼
             ┌──────────────┐
             │  wlam-host   │─▶ Rust → wasm32-unknown-unknown (no WASI)
             │ Win32 on web │   + wlam-runtime JS glue (WebGPU/WebAudio/OPFS)
             └──────────────┘
```

## Key decisions

### Static recompilation + OS emulation, not an x86 interpreter
Code is lifted to P-code and emitted as native wasm functions. This is the
N64Recomp/Cxbx model. Direct calls between known functions become direct wasm
calls; only genuinely dynamic targets go through a dispatch table. An
interpreter core (`crates/wlam-interp`) is planned as the fallback for
dynamically generated code — a recompiler cannot see ahead of a JIT.

### Win32 imports are not lifted
Each IAT slot is rewritten to reference a host trampoline. Windows API
functions in `kernel32.dll` etc. are never translated; they are reimplemented in
`wlam-host`. This is what makes the approach tractable: the guest's code is
reachable, but the OS is ours.

### Host layer is Rust → `wasm32-unknown-unknown`
WASI is deliberately not used: it cannot provide threads, WebGPU or Web Audio,
which is exactly the "low-level IO, graphics and sound" the project exists to
provide. Effects happen through JS host bindings.

### Two memory spaces
Guest memory is the module's linear memory; guest pointers are never
dereferenced by the host as host pointers. Every pointer crossing the boundary
is validated against the guest's mapped regions. `VirtualAlloc` reservations and
`CreateFileMapping` are host-side address-space maps over the guest heap.

### x86-32 first, x86-64 second
One IR serves both front-ends. 32-bit is the cheaper initial target
(register-starved, standard calling conventions, no unwind tables) and where
most "win32.exe" software lives. 64-bit adds `UNWIND_INFO`/`__C_specific_handler`
translation and far-pointer context emulation for SEH.

## Crates

| Crate | Responsibility | Not responsible for |
|---|---|---|
| `wlam-pe` | parse PE32/PE32+; imports, exports, relocs, resources | any recompilation policy |
| `wlam-pcode` | IR definition, semantics, C-like printer | knowing x86 or wasm |
| `wlam-sleigh` | x86 → P-code | discovery, ABI decisions |
| `wlam-translate` | discovery, call resolution, translation orchestration | emitting bytes |
| `wlam-emit` | P-code → wasm binary | parsing anything |
| `wlam-host` | Win32 API and MSVCRT replacement implementation | interpreting guest code |
| `wlam-harness` | differential native-vs-translated execution | being part of the product |
| `wlam-cli` | `analyze` / `recompile` / `run` | business logic |

## Reality checks recorded during design

- `uxn32.exe`, a *single 3,000-line C file*, touches roughly **150 distinct
  Win32 functions** across kernel32, user32, gdi32, winmm, comdlg32, shell32,
  shlwapi and commctrl. Scope estimates that ignore this are wrong.
- Wine-built (Proton) DLLs put the import directory immediately before `.rsrc`
  and their delay-import descriptor stores a **virtual address** (image base +
  import RVA), not an RVA. The parser must handle both.
- Section virtual size can be *smaller* than the raw file size; Windows maps
  `max(VirtualSize, SizeOfRawData)`, and real import tables rely on that.
