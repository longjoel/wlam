# Guest ↔ host ABI (work in progress)

This document is the contract between the recompiled guest and the host layer.
It is intentionally written down before the emitter exists, because every API
decided here is expensive to change later.

## Namespaces

Host imports use one wasm import namespace per Windows DLL:

```
(import "kernel32" "VirtualAlloc" (func $os_VirtualAlloc (param i32 i32 i32 i32) (result i32)))
```

Keeping the namespace means the import table can be walked straight from the
PE, and the compat matrix maps one-to-one onto generated imports.

## Guest ABI (x86-32)

- The guest stack is a region of linear memory; ESP is a wasm `i32` local in
  each lifted function.
- Arguments follow the callee's declared convention. `stdcall`/`fastcall`
  callees pop their own arguments; `cdecl` callers adjust ESP after the call.
- Return values use `EAX` (`EDX:EAX` for 64-bit results). x87 results are
  returned on the FPU stack; see below.
- `RET` at the end of a lifted block returns to the caller's resume PC, which
  the emitted dispatch uses to select the continuation.

## Host call semantics

- Host functions take **guest pointers** and validate them against the guest's
  mapped regions before touching memory.
- Handles (HANDLE, HWND, HDC, ...) are opaque `i32` values allocated by the
  host, never guest-chosen pointers.
- Anything that can block (file IO, waits) is either satisfied from memory or
  yields to the scheduler; the host never blocks the wasm thread on JS I/O
  except through `SharedArrayBuffer` + `Atomics.wait`, which must be given up
  before a physical thread is blocked.
- A host function that cannot be implemented returns the documented failure
  value *and* records a diagnostic, so the compat matrix stays truthful.

## Floating point

- x87 80-bit values live in a 10-byte-per-register area of guest memory, not in
  wasm locals (wasm has no x87). Software `f80` helpers operate on that area.
- SSE doubles use wasm `f64` locals; `mxcsr` is emulated where exposed.

## Known-unresolvable area

Hardware exceptions (access violations, divide-by-zero as a signal, SEH
dispatch) are not part of the V1 ABI. A fault traps with a symbolized PC. SEH
translation arrives with the x86-64 front-end.
