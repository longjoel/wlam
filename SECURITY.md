# Security policy and threat model

## Current status: no guest sandbox (V1)

`wlam` runs a recompiled Windows program **in the same WebAssembly module as
the Win32 host layer**. Consequences, stated plainly:

- A malicious executable can call any host API the layer exposes: write to the
  OPFS virtual filesystem, play sound, open windows, and read any file the page
  has been granted access to.
- WebAssembly memory safety protects the *host's* Rust code only in the sense
  that the guest cannot corrupt host structures through out-of-bounds guest
  pointers (all pointers are validated), but it does not stop a guest from
  *actively misusing* the APIs it is given.
- Therefore: **do not run untrusted `.exe` files.** Treat `wlam` like an
  emulator for software you already trust enough to run natively.

This was a deliberate V1 tradeoff (see the project plan): a sandbox costs a
boundary crossing on every API call and delays a working end-to-end pipeline.

## Planned hardening

1. **Two-module isolation.** Split the guest into its own wasm module that can
   only reach the host through declared imports. File IO becomes mediated:
   a read-only virtual FS plus explicit per-run grants. Every new API must cross
   the boundary, which is the point.
2. **Multi-instance process isolation.** One wasm instance per Win32 process,
   each in its own Worker, with message-passing RPC. This also buys us
   preemptive thread semantics that a single-instance design cannot provide.

Both are reflected in the ABI design (`docs/abi.md`) so that switching does not
require rewriting the host layer's call sites.

## Reporting

Once this project is published, security issues should go to the maintainers
privately rather than the public issue tracker.
