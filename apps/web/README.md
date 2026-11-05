# Interactive Win32 runner

From the repository root, with the workspace-local Rust toolchain installed:

```sh
./scripts/build-web.sh
python3 scripts/serve-web.py
```

Open http://127.0.0.1:8000/apps/web/ and select **Start / Restart**. The default
bundle is a real CRT-free PE32 fixture. Its 32×24 DIB is scaled for display.
Click it to change color and focus it; press a letter to change color again.
Escape and **Close guest** use the guest's destruction/message path. **Stop**
terminates the worker immediately, including a guest in an infinite loop.
Restart constructs fresh guest and host instances.

The local server supplies COOP/COEP headers. Ordinary hosting without isolation
headers cannot use this synchronous worker/input-queue implementation.

The page bridges the emitted guest to `wlam-browser-host.wasm`, which uses the
same Rust dispatcher as the native Wasmi test. Only the input queue uses shared
memory; the guest and Rust host retain separate Wasm memories. The default
renderer copies CPU-rendered pixels to the page. For the OpenGL demo, add
`?renderer=webgl&manifest=/target/tolympic/bundle/manifest.json` to use the
browser WebGL renderer. It submits ordered clear and triangle batches directly
to WebGL, skipping CPU fragment rasterization and full-surface pixel copies.
The TOLYMPIC build script supplies an optimized release host for this path;
the default CPU renderer continues to use the development host build.
The current accelerated subset covers TOLYMPIC's triangle and quad-strip draws;
other primitive modes remain on the CPU renderer. Its canvas stays opaque and
retains the last completed image between swaps, matching the guest window's
presentation behavior.

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

```sh
node apps/web/tests/host-adapter.mjs
node apps/web/tests/worker-interaction.mjs
```

For Chromium acceptance tests, install the pinned Playwright development tool:

```sh
cd apps/web
npm install
npx playwright install chromium
cd ../..
python3 scripts/serve-web.py --port 8765
# In another terminal, from the repository root:
node apps/web/tests/browser.mjs
```

The browser test checks every pixel after actual mouse and keyboard input,
normal close, a responsive page while the worker waits, Stop/Restart, forced
termination of a looping guest, and named unsupported imports. A screenshot is
written to `target/interactive/browser.png`. `WLAM_TEST_URL` overrides the URL;
`WLAM_PLAYWRIGHT_MODULE` can point to an external Playwright module installation.

Modal message-box tests also check that Cancel and Escape return the correct
button ID to an actual Wasm import. Dialog text uses Windows-1252 decoding and
text-only DOM updates. The worker waits on shared input while the page stays
responsive; ordinary input already queued is deferred until the dialog closes.

For the original Hover target, use `./scripts/build-hover.sh` and open
`?manifest=/target/hover/bundle/manifest.json`. Hover currently displays its
startup display warning, loads maze and texture data, and reaches input waiting
in the Node diagnostic. Gameplay and controls have not yet been validated. See `benchmarks/hover/README.md` for measured progress and diagnostics.

This is a staged Win32 subset: one guest UI thread, one unstyled fixed window,
32-bit BI_RGB DIBs and SRCCOPY. It does not establish end-to-end compatibility
with Uxn32 or Minesweeper. Unsupported API variants fail explicitly.
