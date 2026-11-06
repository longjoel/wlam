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
demo in your browser. Select **Start / Restart**, focus the canvas and press
**F2** to enter Start Game. Stop a server started by the launcher with Ctrl+C. Set
`WLAM_HOVER_PORT` to use a port other than 8765. Compilation validates the emitted Wasm and
copies a fresh bundle to `target/hover/bundle`. Native and browser-host build
caches are separate to avoid competing Cargo locks.

## Measured status

Hover recompiles and executes its original CRT/MFC startup in Chromium using
the Rust browser host. It passes CPU/memory requirements and creates its named
startup event. The bundle selects an 8-bit guest display profile, and Chromium
confirms startup reaches input waiting without the 256-color display warning.

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

Chromium now confirms that startup reaches input waiting without an error,
but the final 420×297 client surface is black. The page forwards F1–F12, and
the host translates the original accelerator resource into synchronous
WM_COMMAND callbacks. Focus the canvas and press **F2** to enter Start Game.
This reaches game reset, paints progress text, and enters maze/object
processing, then reaches dialog initialization. `CheckDlgButton` and
`CheckRadioButton` now operate on dialog controls. The ANSI case-conversion
group, window-state queries, and EnableWindow now execute. SetTextColor and
ExtTextOutA also execute; ExtTextOutA's eight-argument ABI required rebuilding
the guest to preserve stack cleanup.
The preceding zero-ID multimedia timer cancellation, current-thread priority
change, whole-window immediate redraw, and stock default palette lookup now
execute. Nonzero multimedia timers remain unsupported. Redraw covers whole
windows with invalidation and immediate erase/paint, with other flags refused.
Menus are not yet displayed by the browser runner.

The browser acceptance flow now dismisses Quick Help and recovers presentation
from the destroyed owned dialog HWND back to Hover's main window. Sending F2
again opens the expected “quit the game in progress?” prompt; choosing No
returns to a 420×297 waiting surface without a guest error. That surface is
still black, and an ArrowUp input has not yet produced visible pixels. This
proves the dialog/input path, not gameplay rendering. Startup tracing observes
one initial BitBlt and the help text's ExtTextOutA calls, but no subsequent
gameplay GDI transfers. A fresh trace shows the F2 command creating the game
shell and showing Quick Help (dialog resource 115), which returns IDOK, but the
level-reset path that starts the game clock is never entered; the main loop
only polls afterward. `timeSetEvent` is implemented now, but still has not been
called. `ShowCursor` and a NULL-target `RedrawWindow` (desktop redraw) also
execute instead of refusing.

The game GDI foundation and portable behavior harness are documented in
[`docs/game-gdi.md`](../../docs/game-gdi.md) and
[`fixtures/gdi/README.md`](../../fixtures/gdi/README.md). The portable fixture
currently passes 190 local observations, including four stretch modes and
initial line/rectangle/pixel operations. Its same-DC four-mode reduction follows
[Microsoft's scaling sample](https://learn.microsoft.com/en-us/windows/win32/gdi/scaling-an-image).
Native Windows comparison remains outstanding. HALFTONE driver dithering is
not yet reproduced.

The host advertises an i486 instruction profile, a 640×480 8-bit display for Hover,
and memory capacity derived from the guest linear memory. This is a virtual
machine profile, not a measurement of the browser computer. Only implemented
APIs are callable; unused unsupported imports may link, but execution reports
the missing library/symbol, arguments and guest state.

Hover's asset packer sets `display.bitsPerPixel` to 8 in its manifest. Other
bundles retain the default 32-bit profile. The indexed profile reports
RC_PALETTE, 256 system palette entries and 20 reserved colors. Palette
realization updates the available system entries while preserving the first
and last ten static colors. Before application realization, those 236 slots
use a deterministic 6×6×6 color cube and 20 gray levels. This is a practical
virtual display profile, not a capture of a particular Windows 95 driver.
Indexed RGB DIBs decode through their own RGBQUAD
tables; final 8-bit window frames map colors through the realized system
palette before reaching the browser's RGBA canvas. Black/white PatBlt operations
work on window DCs and indexed DIBs. This does not establish full palette
animation, palette-change notifications, or DIB_PAL_COLORS support.

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
# Browser regression for Quick Help close, modal return and arrow input:
WLAM_PLAYWRIGHT_MODULE=/path/to/playwright/index.mjs node apps/web/tests/hover-start.mjs
# Enter Start Game using the original F2 accelerator:
WLAM_PROBE_KEY=F2 node apps/web/tests/probe.mjs
# Require startup without the display warning:
WLAM_PROBE_NO_DIALOG=1 node apps/web/tests/probe.mjs
```

The Node diagnostic logs dialogs and acknowledges OK for startup tracing.
The Chromium probe handles any startup dialog and captures
`target/hover/browser.png`, reporting the next refusal. These are diagnostics,
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
