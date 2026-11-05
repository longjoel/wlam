# Original Windows 95 Hover target

Original binaries and assets are external inputs and are not redistributed.
The local source directory is
`/home/longjoel/Downloads/Windows 95 English/funstuff/hover/`.
Set `WLAM_HOVER_DIR` to use another copy.

## Build and run

From the repository root, with the workspace-local Rust toolchain installed:

```sh
./scripts/run-hover.sh
```

The launcher builds Hover, starts the local server if needed, and opens the
demo in your browser. Select **Start / Restart** and acknowledge the display
warning. Stop a server started by the launcher with Ctrl+C. Set
`WLAM_HOVER_PORT` to use a port other than 8765. Compilation validates the emitted Wasm and
copies a fresh bundle to `target/hover/bundle`. Native and browser-host build
caches are separate to avoid competing Cargo locks.

## Measured status

Hover recompiles and executes its original CRT/MFC startup in Chromium using
the Rust browser host. It passes CPU/memory requirements and creates its named
startup event. Its nonfatal warning about the 32-bit display appears as an
interactive browser dialog; clicking OK returns to the guest.

The guest now loads `small.tex` and `small.maz` and delivers three rendered
frames (410×340, 422×299 and 420×297), then reaches the input-waiting state
in the bounded Node worker diagnostic. The parser's two-stage switch table
was missing from static discovery; the compiler now recognizes a loaded
switch index and recovers its scale-four target table.

The MFC list corruption during startup is fixed. `SetRectEmpty` was declared
with two arguments instead of one, causing stdcall cleanup to advance ESP
four bytes too far. Subsequent argument pushes overwrote saved EDI in the
wrapper at `0x45dfbb`. The resulting wrong object pointer caused hash-table
initialization to overwrite an MFC thread-list link, which later pointed into
the parser token `POD`. Correcting the signature preserves the saved pointer.
An executable regression models that wrapper and fails with the old signature.

Import tail calls now also publish guest registers before entering the host,
so reentrant callbacks receive the current ESP. A separate nested callback
regression covers that path.

**Gameplay is not yet validated.** Startup now reaches input waiting without
the previous corruption; controls, gameplay rendering and audio still need
browser acceptance checks.

The host advertises an i486 instruction profile, a 640×480 32-bit RGB display,
and memory capacity derived from the guest linear memory. This is a virtual
machine profile, not a measurement of the browser computer. Only implemented
APIs are callable; unused unsupported imports may link, but execution reports
the missing library/symbol, arguments and guest state.

The current compiler handles recovered function pointers and jump tables,
computed IAT calls, guest callbacks and instruction-scoped temporary values.
REP string operations snapshot their addresses before updating ESI/EDI; this
fixes the initialization memset that previously erased the MFC application
pointer. Unsupported 80-bit memory operations and other unimplemented paths
still fail explicitly when executed.

The PE parser identifies 350 verified static imports, including COMCTL32
ordinal 17 (`InitCommonControls`) after the WINSPOOL.DRV descriptor. Numeric
resources are recorded in the bundle manifest and mapped into the host.
`dashres.dll`, `hoverhlp.dll`, maze files, sounds and other original assets
remain external; their loading and initialization are not yet implemented.

## Diagnostics and verification

```sh
# Bounded Node worker, using the actual guest and Rust host Wasm:
node apps/web/tests/probe-worker.mjs target/hover/bundle
# Chromium diagnostic; requires Playwright and the local server:
WLAM_PROBE_MANIFEST=/target/hover/bundle/manifest.json node apps/web/tests/probe.mjs
```

The Node diagnostic logs dialogs and acknowledges OK for startup tracing.
The Chromium probe displays and clicks the real warning, captures
`target/hover/dialog.png`, and reports the next refusal. These are diagnostics,
not gameplay acceptance tests. `WLAM_PLAYWRIGHT_MODULE` can select a local
Playwright installation; `PLAYWRIGHT_BROWSERS_PATH` selects its browser cache.

Workspace regression tests cover calling conventions, computed calls, switch
tables, floating-point lowering, forward/backward REP string operations,
resources, events, window hooks, subclassing and continuation handling. The
web acceptance tests use a redistributable PE fixture for exact pixels/input
and synthetic Wasm for modal dialog return values; they do not require Hover.
See `apps/web/README.md` for those commands.

Earlier investigation notes are retained in
`docs/history/hover-startup-investigation.md`. CD-wide compatibility is not
claimed; the supplied CD contains both NE and PE binaries.
