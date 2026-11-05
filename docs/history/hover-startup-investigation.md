# Historical Hover startup investigation

These notes record earlier probes. They are superseded by
`benchmarks/hover/README.md`; their blockers are not the current status.

# Original Windows 95 Hover target

User-provided input directory:
`/home/longjoel/Downloads/Windows 95 English/funstuff/hover/`.
Original binaries/assets are external inputs and are not redistributed here.

Initial measured inventory:

- `hover.exe`: PE32/i386, preferred base `0x400000`, entry `0x43fb73`.
  349 static imports: WINMM 27, KERNEL32 100, USER32 147, GDI32 56,
  COMDLG32 3, WINSPOOL 3, ADVAPI32 9, SHELL32 3, COMCTL32 1.
- `dashres.dll`: 7 imports from MSVCRT20/KERNEL32; no exported functions
  reported by llvm-objdump. It still has code, an entry point, and resources;
  absence of exports does not establish that it is safe to ignore initialization.
- `hoverhlp.dll`: 31 imports from WINMM/USER32/KERNEL32; exports PlayWave,
  StopWave, SaveWindowRect and RestoreWindowRect. These may serve the help
  integration; their presence does not establish a dependency during gameplay.
- Original directory includes maze files, sound files, help files and WAVEMIX
  configuration. Runtime file/resource access has not yet been traced.

First compilation completed to `target/hover-first-bundle`: 65 guest function
entries, 591 Wasm imports, including 312 explicit unsupported-operation
imports. Node's WebAssembly.Module accepts it. This is validation only;
startup, menus, rendering, controls and audio are not yet verified.

The first entry import is GetVersion. Investigation exposed reversed major/minor
byte packing in the host and tests; the documented contract places major in the
low byte. Startup must be measured again after that correction.

Reproduce compilation (choose a fresh output directory):

```sh
cargo run -p wlam-cli -- recompile \
  '/home/longjoel/Downloads/Windows 95 English/funstuff/hover/hover.exe' \
  -o target/hover-bundle
```

A filesystem inventory of the supplied CD found 203 NE-format and 116 PE-format
binaries among EXE/DLL/DRV/SCR/CPL files. Broad CD compatibility therefore extends
beyond the current PE32/i386 pipeline. Hover remains the current playable target;
CD-wide compatibility is not claimed.

## Startup after cached-import dispatch fix

`target/hover-cached-import-bundle` now executes through one GetVersion call,
three InitializeCriticalSection calls, TlsAlloc, and EnterCriticalSection.
The next refusal is VirtualAlloc. The earlier repeated startup was an emitter
bug: deleting a cached IAT load left a register at zero, then the computed call
entered guest function zero. Verified IAT loads now preserve address tokens and
computed calls recognize those tokens as host imports. An optimized native
fixture and a dedicated cached-call/stack-cleanup regression both pass.

The startup probe is reusable:

```sh
cargo run -p wlam-sleigh --example probe_image --   '/home/longjoel/Downloads/Windows 95 English/funstuff/hover/hover.exe'   target/hover-cached-import-bundle
```

## Startup after environment and calling-convention fixes

`GetEnvironmentStrings` is now served: the host returns a stable pointer to an
empty (double-NUL) block from the same process-string allocator as the command
line, so the guest's enumeration loop stops at its first byte. With that wired,
startup reaches `GetVersion`, `InitializeCriticalSection`, TlsAlloc/SetValue,
`VirtualAlloc`, `GetStartupInfoA`, the three `GetStdHandle`/`GetFileType` pairs,
`GetACP`/`GetCPInfo`, `GetCommandLineA`, `GetModuleFileNameA`, and
`GetEnvironmentStrings`.

Two emitter fixes landed while chasing the next failure:

- **`__cdecl` imports.** Every call through the IAT used to pop
  `return_pop()`, i.e. `4 + arguments`. A `__cdecl` import is cleaned up by its
  caller instead (`add esp, 4` after the call, as hover does after
  `call [0x4ca8e8]`), so the call site must pop only the return address.
  `pop_import_frame` now distinguishes the conventions and both the normal and
  tail-call paths use it; there is a regression test.
- **Indirect targets are visible.** A lifted `CallInd` now stores its target in
  the guest's `EIP`, and the out-of-bounds guard stores the rejected target
  before trapping, so a failed indirect call names the address it tried to
  reach instead of reporting a bare table failure.

## Historical investigation: the loader-hook dispatch never appears in a full-image build

Not resolved, and now narrowed down. Hover's startup reaches a call site that
loads an import through a register instead of calling it directly:

```text
0x442a30  mov esi, dword ptr [0x4ca898]   ; InitializeCriticalSection
0x442a36  call esi
0x442a3e  call esi
0x442a46  call esi
```

Because the target is a register, the emitter cannot prove it statically, so
these go through the function table -- and an IAT address is not a table index,
so the run traps. The emitter does have a run-time fallback for exactly this
(the shadowing table), and it is generated when the input assembles:

- patching those three calls into direct `call [0x4ca898]` instructions and
  recompiling the patched image produces `i32.eq` comparisons for the full
  `.idata` slot list in that function, and hover then runs past the site;
- the unpatched image produces **zero** such comparisons there, though other
  blocks in the same module do get dispatch code.

So the hook exists and works; what is unexplained is why
`register_host_imports` yields an empty slot map for this image. The next step
is to print `prog.iat_imports.len()` and `prog.host_imports.len()` directly in
`wlam recompile`, and to check whether `iat_slot_verified` comes out false for
hover's slots (the CLI builds its import map from verified slots only, while
`dump_imports` reports every slot it checks as verified). Do not assume the two
paths see the same data.

With the call site patched into direct IAT calls, hover reaches a second
`InitializeCriticalSection` with a bad pointer and the host panics
(`invalid critical-section pointer`) -- a host-side gap, not a recompiler one.

Other open item: `wlam-sleigh`'s `cached_import_register_survives_calls_across_blocks`
fails. It is the regression test for a cached import register being reused for
two calls; the register write-back now exists, but that fixture still traps, so
the fix is not complete for the multi-block shape.

Useful diagnostics added for this:

- `probe_image` — whole startup trace with register/stack dump on trap.
- `trace_run image.exe bundle [--call g_ADDR]` — logs every host call with
  live EIP/ESP and supports `WLAM_BP=0xADDR` breakpoints.
- `indirect_sites image.exe [--dump ADDR ...]` — every indirect site and
  whether its targets were recovered.
- `disasm image.exe VA|file:OFFSET COUNT` — disassembly straight from the
  image, independent of section mapping (also by raw file offset).
- `lift_image_block`, `dump_imports`, `call_export` — single-block IR, import
  verification, and single-function execution.

## Startup after cached-call dispatch and state fixes

The loader-hook issue above is resolved. `register_host_imports` copied
`host_imports` into `iat_imports` before collecting imports, leaving runtime
dispatch empty on its first invocation. The snapshot now follows collection.
Indirect host returns also preserve their updated ESP and EAX instead of
reloading the pre-call snapshot. Indirect guest calls adopt the callee's state.
The cached-import regression now checks both calls, stack cleanup, and EAX.

The unmodified Hover image was rebuilt to `target/hover-dispatch-fix-bundle`
and probed with `probe_image`. It reaches four InitializeCriticalSection calls,
six Enter/LeaveCriticalSection pairs, three VirtualAlloc calls, and the process
environment/startup APIs without the earlier critical-section pointer failure.

The next measured failure is a computed **guest** call:

```text
0x442523  mov eax, [0x4c7498]
0x442528  test eax, eax
0x44252a  jz 0x44252e
0x44252c  call eax                 ; EAX = 0x43e2de
```

The probe reports an out-of-bounds table access with EIP `0x43e2de` and
`0x44252e` at the top of the guest stack. Guest virtual addresses still need
resolution to discovered function IDs; imports use a separate address-token
path. The bundle's 65 function entries do not include `0x43e2de`, so discovery
must also include this callback target before it can run.
Menus, rendering, controls and audio remain unverified.

Validation: the host-call and indirect-call suites pass (eight tests). The
workspace run stops at `interactive::pe_draws_responds_to_input_and_exits` with
`BitBlt: UnsupportedOperation`; rerunning that test with the emitter changes
removed reproduces the same failure in the starting working tree.

