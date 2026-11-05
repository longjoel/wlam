# Guest ↔ host ABI (work in progress)

This document is the contract between the recompiled guest and the host layer.
The current implementation is experimental; remaining semantic gaps are listed
in roadmap.md.

## Namespaces

Host imports use one wasm import namespace per Windows DLL:

```
(import "KERNEL32.dll" "VirtualAlloc" (func $os_VirtualAlloc (param i32 i32) (result i32)))
```

The two arguments are a pointer to guest stack arguments and the guest return
address. Results use i32. Keeping the namespace means the import table can be walked straight from the
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

## Guest address space layout

The guest's absolute addresses mean what they say, so every region is fixed and
the module's linear memory must cover all of them.

The module reserves the **whole 4 GiB** (65536 pages) unconditionally:
reservation is virtual, so it costs address space rather than RAM, and a PE32
program is entitled to address anything below 4 GiB.

| Region | Range | Notes |
| --- | --- | --- |
| Register snapshot | `0x8000` | `snapshot_address(index)` = base + index × 8; register order |
| Loader stack | grows down from `0x70000` | the harness seeds ESP here |
| TIB / SEH head | `0x60000` | `FS_OFFSET`; `fs:[0] = 0xFFFFFFFF` |
| Image | its PE `ImageBase` (`0x400000` for PE32) | headers + every section |
| Heap region | `0x1000_0000 .. 0x1_0000_0000` | where `HeapAlloc` hands out blocks |

Note the exclusive limit 2^32 does not fit in a `u32`; arena bookkeeping uses
`u64` for the limit and `u32` for addresses.

The heap region is the one entry that appears in **two** crates, because
`wlam-host` does not depend on `wlam-emit`:

- `wlam_emit::chain::HEAP_REGION_BASE` / `HEAP_REGION_SIZE` decide how much
  memory the module declares (`pages_for_image`), and
- `wlam_host::heap::HEAP_REGION_BASE` / `HEAP_REGION_SIZE` decide where blocks
  come from.

If they disagree, `HeapAlloc` returns an address outside the memory and the
guest traps on its first store through it — reported as a wild pointer in guest
code, not as a layout bug. `crates/wlam-sleigh/tests/heap_abi.rs` asserts the
two agree and that the region does not overlap the image or the stack.

## Host call semantics

- Host functions take **guest pointers** and validate them against the guest's
  mapped regions before touching memory.
- Handles (HANDLE, HWND, HDC, ...) are opaque `i32` values allocated by the
  host, never guest-chosen pointers.
- Anything that can block (file IO, waits) is either satisfied from memory or
  yields to the scheduler; the host never blocks the wasm thread on JS I/O
  except through `SharedArrayBuffer` + `Atomics.wait`, which must be given up
  before a physical thread is blocked.
- An unimplemented host function records a named refusal and traps. Probing
  can request a return value, but cannot bypass an unsupported-instruction trap.
- `wlam.unsupported` imports name unsupported P-code and user operations. They
  always stop execution; the emitter appends `unreachable` after the import.

## Floating point

- x87 stack registers currently use f64 locals and eight-byte snapshot slots.
  This approximates 80-bit precision; full x87 fidelity is not implemented.
- Sin/cos helpers use scratch doubles at `0x7000` and `0x7008`, below the
  snapshot area, and require host implementations.

## Known-unresolvable area

Hardware exceptions (access violations, divide-by-zero as a signal, SEH
dispatch) are not part of the V1 ABI. A fault traps with a symbolized PC. SEH
translation arrives with the x86-64 front-end.
