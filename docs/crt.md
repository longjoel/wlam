# The C runtime in the browser host

Windows programs rarely contain their own C runtime. A typical MSVC executable
imports `MSVCRT.dll` (or a versioned name such as `MSVCRT20.dll`, `MSVCR80.dll`,
`UCRTBASE.dll`, or `VCRUNTIME140.dll`) and calls `malloc`, `_initterm`, `exit`,
and friends through its import table. A recompiled guest has no Windows loader,
so those slots must be satisfied by the same host layer that provides USER32 and
KERNEL32.

## Why the CRT lives in the host

The recompiler currently turns every verified IAT slot into a host import. It
has no multi-image loader, no PE dynamic loader, and no mechanism to link a
second recompiled PE into the guest. Shipping a prebuilt MSVCRT DLL would
therefore require building a loader and a second translation pipeline before it
could run at all.

Implementing the C entry points in `wlam-host` is consistent with the rest of
the architecture: Windows APIs are replaced by ordinary Rust on top of guest
memory. The guest-visible contract stays the same:

- pointers come from the guest's own address space (the process arena), not
  from a host shadow heap;
- arguments and return values use the MSVC C ABI (`cdecl`, so the guest removes
  its own arguments);
- unsupported behavior fails at the call site instead of returning a fabricated
  result.

Versioned CRT library names are normalized to one family. The translator,
`wlam-pcode` signatures, `Runtime::call`, and the browser runner all agree that
`MSVCRT20.dll!malloc` and `MSVCRT.dll!malloc` refer to the same host function.

## Implemented

Memory:

- `malloc`, `free`, `calloc`, `realloc`
- C++ `operator new` and `operator delete` (`??2@YAPAXI@Z`, `??3@YAXPAX@Z`)

Startup and termination:

- `__getmainargs` — argc/argv/envp for the synthetic `C:\PROGRAM.EXE` process
- `_initterm` — calls each non-NULL initializer in the table in order, using the
  host's guest-callback mechanism
- `_onexit`, `atexit`, `__dllonexit` — register guest callbacks
- `exit` — runs registered callbacks in reverse order, then terminates
- `_exit` — terminates immediately without running callbacks

Globals and floating-point state:

- `__p__acmdln`, `__p__commode`, `__p__fmode`
- `_controlfp`, `_control87`, `_adjust_fdiv` (a virtual x87/SSE control word)

Exception entry points:

- `_XcptFilter` returns `EXCEPTION_CONTINUE_SEARCH`
- `__CxxFrameHandler` and `_except_handler3` return `ExceptionContinueSearch`

## Deliberate gaps

There is no SEH or C++ unwinding engine yet. The exception entry points decline
the exception rather than pretending to have handled it. Programs that never
throw are unaffected; a real throw will surface as an unhandled guest fault.

Not yet implemented: stdio (`printf` family, `fopen`), locale, errno/TLS
variables, CRT string/math helpers, and `_beginthread`/`_endthread`. These
symbols are listed in the compatibility matrix and remain visible there. A
runtime call that reaches an unimplemented CRT symbol is refused by name.

## Tests

`crates/wlam-host` covers the allocator, `__getmainargs`, the `__p__*` globals,
the control word, operator new/delete, `_initterm` callback chaining, and exit
handler ordering. The browser host exposes the same entry points; a sparse-memory
Node check can dispatch them against `wlam_browser_host.wasm` without a full PE.
