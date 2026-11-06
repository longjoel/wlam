# Interactive Win32 guest desktop

The desktop opens empty with a taskbar and a shortcut for every bundle the dev
server finds under `target/*/bundle/`. Every shortcut opens the same kind of
window: a title bar and the guest display, nothing else. Several guests can run
at once, each with its own taskbar button, Start menu entries and renderer.

Drag the title bar to move a window, use the title-bar buttons to
minimize/maximize/restore it, and drag the lower-right grip to resize it. The
guest surface always matches the window client area, so the window is the guest
window size; resizing sends the regular Win32 `WM_SIZE` message, and
applications such as TOLYMPIC update their viewport as the simulated window
changes size. Closing a window stops that guest. The taskbar button and the
Start menu restore or restart a guest that exited or was minimized.

The **Developer Console** (desktop icon, Start menu, or `?dev=1`) is the
development tool. It has the bundle-manifest field, Start/Stop, **Close guest**,
manual guest dimensions and **Guest diagnostics**. It never contains guest
pixels itself: Start opens a bare guest window for the entered manifest, and
the console attaches to an existing window for the same manifest instead of
starting a duplicate. `?manifest=...` opens the console with that manifest
prefilled. Bundles that import `OPENGL32.dll` use the WebGL renderer
automatically; the rest use the CPU framebuffer. **Refresh bundle shortcuts**
in the Start menu rescans after a build. Static hosting without the dev-server
scan still shows the developer console shortcut.

## Recompiling from the desktop

The dev server can recompile a new application without leaving the desktop. Drag
a ZIP, CAB, MSI, self-extracting EXE or installer stub anywhere onto the page, or
choose **Start → Recompile application…** / the **Recompile app** shortcut. The
wizard:

0. Detects the upload by magic bytes. ZIPs are unpacked; CAB, MSI, SFX and PE/NE
   installers are passed to the ingest layer, which extracts a static payload
   when one exists. Formats are recognised by content, not just extension.
1. Reads the archive, lets you pick the entry executable, and finds non-system
   DLLs. **Link supplied DLLs** passes them to the compiler; imports are matched
   by name or ordinal against their export tables, so an ordinal-only library
   such as MFC30 works without an export-name map. `DllMain` runs before the
   EXE entry. System libraries stay in the host layer even if a copy is present.
2. Runs `wlam analyze --json --no-oracle --dll ...` and shows libraries,
   compatibility counts and blocking imports. Imports satisfied by a supplied
   DLL appear as `linked` and do not block. Other blockers stop recompilation,
   matching Studio, rather than being hidden.
3. Runs `wlam recompile` in the background and reports each stage.
4. Publishes `target/<slug>/bundle/` and adds its shortcut to the desktop. The
   report is written beside the bundle as `report.json` and `report.md`, next to
   the full `compatibility.md` import matrix; all three are downloadable from the
   final wizard step, and the manifest records the bundle name so the shortcut
   keeps it after a refresh.

### Installer and archive ingestion

`scripts/installer_ingest.py` recognises ZIP, self-extracting ZIP (an MZ stub
with an appended ZIP), Microsoft Cabinet (`MSCF`), MSI/OLE compound documents,
InstallShield, Inno Setup, NSIS, Wise and generic PE/NE stubs. It extracts a
static payload when one is present:

- ZIP and SFX-ZIP use the standard library.
- CAB and MSI use `cabextract`/`7z`; `msiextract` is preferred when installed.
- Inno Setup uses `innoextract`, falling back to `7z`.
- InstallShield uses `unshield`, then `i5comp`/`i6comp`, then `7z`.
- Other PE/NE stubs are scanned for embedded ZIP/CAB signatures and carved.

Everything else falls back to `7z`. Uploaded files are never executed, and the
extracted tree is validated after extraction: absolute paths, `..`, drive
letters, symlinks and size/entry limits are rejected.

Uploads accept three modes via the `X-Mode` header: `auto` (default), `source`
(keep the upload as-is) and `installer` (force ingest of installer-looking ZIPs
and stubs). The wizard sends `auto`, so a plain ZIP containing an app is treated
as source while a setup/installer ZIP or a CAB/MSI/SFX upload is unpacked and its
payload recompiled. Static extraction cannot run setup logic, registry writes or
custom install scripts; when no payload can be found statically, the job reports
`needsRun` and explains how to extract it manually.

Data files in the archive other than the selected executable are copied to the
bundle's `files/` directory and registered in the manifest as `C:\…` paths. Clear
**Include data files** to compile code only. Recompilation is a native local
subprocess, not a browser compiler: the dev server only binds loopback and
requires the same-origin `Origin` header for writes. Uploads are limited to
64 MiB, 256 MiB expanded, 10,000 entries, and 192 MiB of packaged data; traversal,
absolute/Windows paths, duplicate entries, symlinks and encrypted entries are
rejected. Uploaded executables are analyzed and recompiled, never executed
natively. The generated guest still runs in the same Wasm module as the host
layer, so only import trusted applications.

Recompilation needs `target/debug/wlam` (build it with `cargo build -p wlam-cli`,
or point `WLAM_BIN` / `python3 scripts/serve-web.py --wlam PATH` at another
binary). Static hosting without the dev server hides the recompile controls; if
the endpoint is present but the binary is missing, the wizard says which path to
build. The API lives under `/apps/web/recompile`: `POST` a ZIP to create a job, `POST
…/<id>/analyze` and `…/<id>/build`, `GET …/<id>` for progress and the report, and
`DELETE …/<id>` for cleanup. This is a local developer tool, not a public upload
service.

From the repository root, with the workspace-local Rust toolchain installed:

```sh
./scripts/build-web.sh
python3 scripts/serve-web.py
```

Open http://127.0.0.1:8000/apps/web/ and click a desktop shortcut. The
**Interactive Fixture** is a real CRT-free PE32 application: it repaints its
client area and follows window resizes. Click it to change color and focus it;
press a letter to change color again. Escape and **Close guest** use the
guest's destruction/message path. **Stop** terminates the worker immediately,
including a guest in an infinite loop. Restart constructs fresh guest and host
instances.

When execution fails, **Guest diagnostics** opens with the failed API and guest
registers. Use **Download debug log** to share a JSON report: it includes the
latest snapshot, up to 256 recent API calls with shared API/callback sequence
numbers (excluding high-volume error/TLS/lock calls, which have per-API totals),
32 callback entries, 32 stack words,
and up to 16 EBP frame candidates, plus SHA-256 hashes identifying the guest,
image and host artifacts. Waiting and clean exit also publish snapshots.
The report identifies the active renderer and records the WebGL depth and
subpixel precision for browser-specific rendering comparisons.
The report retains the underlying error and JavaScript stack. EBP candidates
are not a verified unwind; optimized guest code may omit frame pointers.
This view captures failures; instruction stepping and breakpoints are not yet
implemented. Stop preserves the latest snapshot, and Restart clears it.

The local server supplies COOP/COEP headers. Ordinary hosting without isolation
headers cannot use this synchronous worker/input-queue implementation.

The page bridges the emitted guest to `wlam-browser-host.wasm`, which uses the
same Rust dispatcher as the native Wasmi test. Only the input queue uses shared
memory; the guest and Rust host retain separate Wasm memories. The default
renderer copies CPU-rendered pixels to the page. For the OpenGL demo, add
`?renderer=webgl&manifest=/target/tolympic/bundle/manifest.json` to use the
browser WebGL renderer. It submits ordered clear, triangle and point batches
directly to WebGL, skipping CPU fragment rasterization and full-surface pixel
copies. Its accelerated subset covers triangles, quad-strips converted to
triangles, and default-size `GL_POINTS`. Line modes and other primitives are
unsupported in WebGL mode; select the default CPU renderer for those demos.
The TOLYMPIC build script supplies an optimized release host for this path;
the default CPU renderer continues to use the development host build. The
WebGL canvas stays opaque and retains the last completed image between swaps,
matching the guest window's presentation behavior.

To build and launch the accelerated TOLYMPIC target in one step, run
`./scripts/run-tolympic.sh`. It builds the bundle, starts the local server when
one is not already listening on port 8765, and opens the demo URL. Set
`WLAM_GLINT_DIR` if `TOLYMPIC.exe` is not in the default Glint demo directory;
set `WLAM_TOLYMPIC_PORT` to use another port.

For renderer development, `runBundle` can opt into an ordered GPU packet with
its `onGpuFrame` callback. The callback receives an owned `Uint32Array` copied
from host Wasm memory. Packet words are: magic `0x574c474c`, version, width,
height, operation count; then ordered operations. Clear operations contain
opcode `0`, mask, four float-bit RGBA values and float-bit depth. Draw
operations contain opcode `1`, vertex count, primitive mode, viewport, state
flags/enums, fog values, then 13 float words per vertex (clip position, eye
distance, front color and back color). Packet capture is opt-in. In WebGL mode
the host skips CPU fragment rasterizing; without that URL option the existing
CPU framebuffer path remains the default.

## Verification

`apps/web/tests/run-tests.sh` is the single entry point. It resolves Playwright
and its browser binaries (honouring `WLAM_PLAYWRIGHT_MODULE` and
`PLAYWRIGHT_BROWSERS_PATH` if already set), starts the dev server on request,
and reports each check by name.

```sh
apps/web/tests/run-tests.sh              # browser checks that need the dev server
apps/web/tests/run-tests.sh --all        # every Node check plus the Python suite
apps/web/tests/run-tests.sh --python     # Python unittest suite only
apps/web/tests/run-tests.sh --serve --all  # start and stop the dev server itself
apps/web/tests/run-tests.sh bundles recompile   # selected checks by file stem
```

Exit status is non-zero if any selected check fails. The dev server defaults to
`http://127.0.0.1:8765/apps/web/`; set `WLAM_TEST_URL` to change it. Without
`--serve`, the script fails fast and prints how to start the server.

Individual checks are ordinary Node scripts and can still be run directly, e.g.
`node apps/web/tests/host-adapter.mjs` or `node apps/web/tests/bundles.mjs`.
`guest-worker.mjs` is a worker module used by `probe-worker.mjs` and
`worker-interaction.mjs`; it has no standalone entry point and is intentionally
excluded from `--all`.

The Python suite covers archive limits and escape attempts, executable
selection, blocker handling, bundle publishing, asset packaging, report
contents and the recompile HTTP routes. Installer ingest detection and
extraction are covered by `test_installer_ingest.py`.

For Chromium acceptance tests, install the pinned Playwright development tool:

```sh
cd apps/web
npm install
npx playwright install chromium
cd ../..
python3 scripts/serve-web.py --port 8765
# In another terminal, from the repository root:
node apps/web/tests/browser.mjs
node apps/web/tests/debugger.mjs
node apps/web/tests/desktop-window.mjs
node apps/web/tests/bundles.mjs
node apps/web/tests/recompile.mjs
node apps/web/tests/webgl-opengl-fixture.mjs
```

The browser test checks every pixel after actual mouse and keyboard input,
normal close, a responsive page while the worker waits, Stop/Restart, forced
termination of a looping guest, and named unsupported imports. A screenshot is
written to `target/interactive/browser.png`. `WLAM_TEST_URL` overrides the URL;
`WLAM_PLAYWRIGHT_MODULE` can point to an external Playwright module installation.
The desktop-window check opens TOLYMPIC from its desktop shortcut and exercises
the bare window: title-bar dragging, minimize/taskbar restore, maximize/restore,
pointer and keyboard resizing, viewport changes and the Start menu. The bundles
check reads the scan endpoint, confirms the desktop starts empty, opens the
interactive bundle as a canvas-only window, closes it, opens another bundle and
verifies its surface matches the window client area. The recompile check drags a
ZIP through the drop overlay, inspects compatibility, recompiles, downloads the
report, and launches the new desktop shortcut. The resize grip also supports
arrow keys (Shift+arrow for a larger step) and Enter/Space to grow both
dimensions. Changing the browser viewport keeps windows onscreen and resizes
the guest surfaces.

Modal message-box tests also check that Cancel and Escape return the correct
button ID to an actual Wasm import. Dialog text uses Windows-1252 decoding and
text-only DOM updates. The worker waits on shared input while the page stays
responsive; ordinary input already queued is deferred until the dialog closes.

For the original Hover target, use `./scripts/build-hover.sh` and open
`?manifest=/target/hover/bundle/manifest.json`. Hover uses an 8-bit guest
display profile without a display warning, loads maze and texture data, and reaches input waiting
in the Node diagnostic. Gameplay and controls have not yet been validated. See `benchmarks/hover/README.md` for measured progress and diagnostics.

This is a staged Win32 subset: one guest UI thread, unstyled windows, 32-bit
BI_RGB DIBs and SRCCOPY. It does not establish end-to-end compatibility with
Uxn32 or Minesweeper. Unsupported API variants fail explicitly.
