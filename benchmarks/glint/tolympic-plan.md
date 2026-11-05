# TOLYMPIC implementation plan

## Goal and scope

Run the original external `TOLYMPIC.exe` in the browser through the existing
PE32 → Sleigh → Wasm pipeline. Display correctly lit interlocking rings,
animate with Space, toggle blending with `b` and ring/doughnut geometry with
`d`, resize, and exit with Escape. Stop/Restart must remain responsive.

Implement the APIs and rendering semantics needed for that result. Win16,
other GLINT demos, a complete GL 1.1 implementation and general GLU support
are follow-up work. The measured status below records the current browser run
and the remaining acceptance gaps.

## Starting evidence

The static inventory in `imports.json` contains 116 imports: 39 OPENGL32
(including three WGL calls), two GLU32, and 75 KERNEL32/USER32/GDI32 imports.
The executable is PE32/i386 with a console subsystem. Its lack of PE resources
and small GLU surface make it a reasonable first historical target.

The existing browser runner executes guest and host Wasm in separate memories
on a worker, with page-side pixel presentation. TOLYMPIC now uses a host CPU
OpenGL rasterizer connected to that surface path. The native runtime and
browser adapter still have separate API registration surfaces that must agree.

## 1. Establish a reproducible startup baseline

- Add `scripts/build-tolympic.sh`, using `WLAM_GLINT_DIR` with the supplied
  directory as the local default. Keep binaries/assets external. Produce
  `target/tolympic/bundle` using separate guest/host build caches.
- Recompile and validate Wasm. Run a bounded startup probe using the existing
  diagnostic patterns; record the first refusal, guest PC and arguments.
- Inventory reached startup APIs, dynamic lookups, requested pixel formats,
  window styles and the actual rendering loop. Determine whether Unicode
  window functions, console initialization and palette paths execute.
- Resolve CPU/lifting issues separately from missing host APIs. Static imports
  alone must not force implementation of unused SEH or alternate UI paths.

Exit check: a repeatable diagnostic reaches the first graphics operation or
identifies a concrete compiler blocker. Capture the baseline before changes.

### Measured progress (2026-11-02)

`scripts/build-tolympic.sh` produces a valid bundle from the external demo.
The target creates a 400×300 window, chooses an RGBA double-buffered pixel
format with 24-bit depth, creates and binds a WGL context, compiles and replays
display list 1, and presents frames. The renderer currently uses a host-side
CPU rasterizer and the existing RGBA surface path.

Two runtime faults were isolated and fixed: x87 `FSIN`/related userop results
were disconnected from the subsequent `ST0` write by mismatched temporary
namespaces, and the GDI surface retained zero alpha despite using a pixel
format with no alpha channel. The fixed-function color-material path also now
recognizes `GL_DIFFUSE` (`0x1201`). Early frames show only part of the rings
because the guest initializes random offsets and decays them over time. A
direct worker run sent Space, observed all five guest animation counters reach
zero by swap 65, and captured the settled five-ring layout in
[`tolympic-settled.png`](tolympic-settled.png). Chromium input required
draining queued events at guest host-call boundaries because the demo polls
with filtered `PeekMessage` calls. The host honors HWND/message filters.
`TranslateMessage` delivers the registered character callbacks: `b` produces
`glEnable(GL_BLEND)` calls, and `d` rebuilds the display list with a different
vertex count. Space-driven settling and normal Escape exit are demonstrated.
A settled `VK_B` input enables blending for the green ring. The target selects
`glBlendFunc(GL_SRC_ALPHA, GL_ONE)`; the captured enabled frame changes pixels
only around the green ring and its overlap. See
[`tolympic-blend-enabled.png`](tolympic-blend-enabled.png).
A teardown trace confirmed `wglMakeCurrent(hdc, NULL)`, successful context
deletion, `ReleaseDC` and `DestroyWindow`. Browser UI checks also confirmed Stop
and a fresh frame after Restart. Browser resize from 400×300 to 600×450 sends
`WM_SIZE`; TOLYMPIC updates its viewport to 600×450 and renders all five rings
in the resized settled capture at
[`tolympic-resized-settled.png`](tolympic-resized-settled.png).
The rasterizer now uses inverse-transpose normal transformation and
honors the OpenGL default of disabled `GL_NORMALIZE` (while supporting its
enable toggle), as well as perspective-correct interpolation for smooth colors
and fog distance. The settled and resized captures were regenerated from this
implementation; TOLYMPIC does not enable `GL_NORMALIZE` and uses unit normals.
Fixed-function specular lighting now follows the OpenGL 1.1 default
`GL_LIGHT_MODEL_LOCAL_VIEWER = false`, using an infinite viewer along eye-space
`+Z`. Runtime argument inspection confirms TOLYMPIC sets local-viewer and
two-sided lighting false, scene ambient to zero, and white specular light and
material values with shininess 40. The viewer convention therefore affects its
visible specular highlights. A fresh 70-frame 400×300 capture matches the
saved settled image exactly.
The renderer now applies `GL_LIGHT_MODEL_TWO_SIDE`, tracks front and back
materials separately, and preserves the source primitive's flat-shading
provoking vertex through clipping and raster fan triangulation. The target's
explicit two-sided false setting leaves its rendered output unchanged.
An independent replay of the captured settled GL/GLU call stream in Mesa
llvmpipe's compatibility context now includes the CPU renderer's enabled
`GL_DITHER` behavior, using an ordered pattern on fragment writes and a uniform
clear-color choice. It matches 117,270 of 120,000 RGB pixels in the 400×300
normal-ring frame and 264,241 / 232,370 / 232,046 of 270,000 pixels in resized,
blended and opaque 600×450 frames, with no differences over 20 RGB levels. The
fragment pattern is implementation-specific. The
renderer preserves incoming `f64` depth and stores 32-bit normalized
fixed-point depth internally while the selected pixel format requests 24 bits.
`gluPerspective` retains its double inputs through matrix calculation before
entering f32 state. Exact comparison results and the depth evidence are in the
[runtime audit](tolympic-runtime-api-audit.md).
Double-precision incoming depth with 32-bit normalized internal storage, plus
8-bit subpixel coordinate snapping, removed the large silhouette discrepancies.
This is not a comparison with the original Windows executable. See the
[runtime audit](tolympic-runtime-api-audit.md),
[normal replay](tolympic-mesa-replay.png),
[blended doughnut replay](tolympic-mesa-doughnuts-blend.png), and
[opaque doughnut replay](tolympic-mesa-doughnuts-opaque.png).
Native-reference fidelity remains open. The observed settling count depends on
this run's virtual timer advancement; 65 swaps is not a fixed timing guarantee.

The initial 16-bit feasibility proposal has passed its public decoder API gate:
`rsleigh-api` exposes decoder-only `X86_16Protected`, and its example covers
default 16-bit addressing/operands, 66/67 overrides, direct near wraparound,
indirect transfers and far transfers. Direct near-relative wraparound is
normalized, but indirect near CALL/JMP at `CS:FFFE` still carries into CS.
The NE loader remains gated on fixing and testing that transfer behavior and
designing selector, segment-base and far-control-transfer execution. Details
and remaining Win16 scope are in
[`docs/win16-and-opengl.md`](../../docs/win16-and-opengl.md).

## Immediate plan from the current state

The end-to-end path is running and its controls/lifecycle have been exercised.
The remaining TOLYMPIC work is a fidelity and completeness audit, not another
large rendering feature pass. Keep the current settled capture as a regression
baseline, then close these gates in order:

1. **Close the runtime API audit.** Capture the complete executed host-call
   trace through startup, steady-state rendering, `b`, `d`, resize, Escape and
   teardown. For each call, record its arguments, return value and handling
   (implemented, valid RGBA no-op, or unsupported). Resolve every executed
   refusal or silent stub. In particular, check the imported palette, Unicode,
   console, color-index and draw-buffer paths rather than inferring behavior
   from the import table. The bounded interaction trace now covers those
   controls and lifecycle through normal exit, with per-call arguments, return
   values, message data and the relevant pointer-backed rendering values.
   See the [runtime API audit](tolympic-runtime-api-audit.md) and its full
   [JSON trace](tolympic-api-trace.json). Static but uncalled imports remain
   outside the demonstrated path.
2. **Compare against native output.** Run the original on a compatible native
   Windows/OpenGL system (or a documented equivalent), matching window size,
   animation state and controls. Compare settled ring shape, overlap, lighting,
   blend mode and resized output. Until this reference exists, label visual
   fidelity provisional; a self-consistent capture is not proof of correctness.
   The CPU path now models default-enabled `GL_DITHER` with an ordered pattern
   on fragment writes and a uniform nearest-candidate choice for clears.
   Forced-llvmpipe replay differs at 2,730/120,000 pixels for settled rings,
   5,759/270,000 after resize, and 37,630–37,954/270,000 for doughnuts. No
   pixel differs by more than 20 RGB levels. The ordered fragment pattern is a
   valid implementation choice but differs from llvmpipe's. Native comparison
   is still needed to judge the target Windows renderer.
3. **Turn observed edge cases into deterministic coverage.** Keep the original
   external executable as an end-to-end smoke target. Add/extend a small
   redistributable fixture for invalid pixel formats and handles, display-list
   compile/replay behavior, front/back materials, clipping and resize. Check
   primitive provoking vertices and GL error behavior where the original
   reaches those rules. Do not broaden into unreached GL 1.1 features to satisfy
   static import counts.
   Source-only fixtures now check clipped triangles, near/far depth overlap,
   `GL_QUAD_STRIP` assembly, display-list compile/replay under a changed matrix,
   resize at two framebuffer sizes, and WGL pixel-format/context-handle
   validation, front/back lighting materials, and flat-shaded provoking-vertex
   preservation across clipping and quad triangulation, `glBegin`/`glEnd`
   command legality (with vertex attribute calls allowed inside a primitive
   and invalid operations leaving the primitive active),
   matrix-stack bounds, negative-viewport rejection, invalid-clear-mask
   handling, `glBegin`/`glEnd` error retrieval, current color/normal updates
   outside primitives, generated list-name reservation, undefined-list no-op
   behavior, nested list compilation and invocation inside primitives, and
   sticky first-error reporting through `glGetError`.
   The renderer suite also covers `GL_POINTS`, `GL_LINES`, `GL_LINE_STRIP`,
   and `GL_LINE_LOOP`, including homogeneous line clipping, clipped color
   interpolation and second-vertex color in flat-shaded lines.
   A redistributable PE32 smoke fixture
   exercises window creation, WGL context lifecycle, `glGetError` ABI use, one
   presented triangle and teardown through the guest/browser host path
   ([fixture notes](../../fixtures/opengl/README.md)).
4. **Confirm lifecycle and scheduling from traces.** Ensure no executed API or
   instruction is silently stubbed, and repeat Stop/Restart and context
   teardown after input and resize. Record bounded startup/runtime diagnostics
   and confirm the browser remains responsive while the guest polls messages.
5. **Document the support boundary and freeze the milestone.** List the
   reached API/state subset, valid RGBA no-ops, unsupported behavior and known
   visual differences. Call TOLYMPIC complete only after the native comparison
   and runtime audit are resolved; otherwise report the exact open evidence
   needed.

### Parallel follow-on work

- Continue the independent OpenGL math/state review against the behavior
  exercised by TOLYMPIC. Keep changes localized and report concrete mismatches
  with a minimal reproduction and reference rule.
- Audit Win16 separately. First establish default 16-bit decoder and control
  transfer correctness with fixtures; then propose the NE module, segmented
  memory, callback and USER/GDI architecture. Do not let the Win16 loader
  change the running PE32 TOLYMPIC path.
- Use IDEAS as the next OpenGL candidate only after TOLYMPIC's acceptance gates
  are closed. Its discovery needed 272 passes (4 unresolved indirect jumps,
  no pending work); the recompiler's bound is now 512, and a fresh IDEAS bundle
  compiled successfully. Keep the unresolved jumps explicit during a runtime
  trace rather than treating compilation as proof of complete execution
  coverage. TEXLIST
  has a confirmed earlier runtime gap at `glPixelStorei` and will also require
  texture upload and GLU mipmap support, so it is a distinct later milestone.

### Next concrete actions

1. Arrange or locate a native reference capture; otherwise keep the milestone
   marked “browser behavior demonstrated; native visual fidelity unverified.”
2. Make only fixes backed by reference comparisons, then refresh
   the captures and support inventory.

The IDEAS discovery diagnostic and 512-pass bound are built; convergence and
recompile are confirmed (272 passes, zero pending items, four unresolved
indirect jumps). Keep those unresolved jumps explicit during a runtime trace;
compilation alone does not prove complete execution coverage.

## Follow-on: WebGL presentation

The current browser backend rasterizes every fragment in the Rust CPU renderer,
copies the full RGBA surface at `SwapBuffers`, and sends that image to the page.
An untraced Node `runBundle` pass presented 110 target frames in 30.4 seconds
(3.62 frames/s); this includes guest execution and is not a browser-only
benchmark. Uploading those same CPU-rendered pixels as a WebGL texture would
not remove the rasterization bottleneck.

For a useful acceleration path, keep guest API/state handling in Rust but emit
compact transformed, lit triangle batches at each successful swap. Submit the
batches through a browser WebGL shader with depth testing, culling and blending
configured from the captured OpenGL state. WebGL is shader-based rather than a
fixed-function GL 1.1 context, so transforms, lighting and interpolation must
be represented explicitly. Keep the CPU renderer available as fallback and as
the deterministic comparison path. Start with TOLYMPIC's triangles and
quad-strips, current lighting/material subset, `GL_SRC_ALPHA, GL_ONE`, resize
and the `b`/`d` controls; compare captures against the CPU path and existing
Mesa replay before broadening to other demos.

The browser page owns the WebGL canvas and receives transferable draw
batches from the worker at `SwapBuffers`. Pace each swap against a page
`requestAnimationFrame` acknowledgement so faster GPU work does not consume the
guest's per-frame animation counters too quickly or queue unbounded frames.
This main-thread submission path also fits the current synchronous guest loop;
worker-side `OffscreenCanvas` can be evaluated later if the guest runner yields
between frames. Measure displayed FPS, guest frame count and input/Stop
responsiveness together. Do not use WebGL image equivalence to close the
separate native Windows fidelity gate.

The GPU-only path now submits ordered clear/draw packets with clip-space
vertices, prelit front/back colors, viewport and per-draw depth/cull/blend/fog
state. The browser-host ABI serializes these into an owned packet; the page
renders them with a WebGL shader and acknowledges each frame on
`requestAnimationFrame`. In GPU-only mode the host skips CPU fragment
rasterization and the page avoids full-surface pixel copies. The fixture and
TOLYMPIC browser acceptance cover drawing, resize, input, Stop/Restart and
Escape. A frame-90 capture at 600×450 matched 261,883/270,000 CPU-reference
pixels exactly; 65 pixels differ by more than 20 RGB levels. The capture is
[`tolympic-webgl-frame90.png`](tolympic-webgl-frame90.png). Recreate it with
`WLAM_WEBGL_CAPTURE=benchmarks/glint/tolympic-webgl-frame90.png node apps/web/tests/webgl-reference-capture.mjs`.
Headless Chromium presented 8 additional frames in 2 seconds on this host, so
rendering is continuous. The optimized host produced 110 frames in 0.76 seconds
without browser presentation pacing (145 FPS); the browser requestAnimationFrame
path caps display to the browser's refresh rate. The WebGL context is opaque, matching the guest window, and
preserves the last completed buffer between swaps. The CPU renderer remains the
default and comparison path. This does not close the separate native Windows
fidelity gate.

## 2. Agree on integration interfaces with the Hover work

- Coordinate changes to `wlam-host/src/runtime.rs`, module registration,
  `wlam-browser-host/src/lib.rs` and worker/presentation plumbing before editing
  shared areas. Reuse landed startup/window/input implementations.
- Define HDC → HWND/surface identity, surface resize and destruction hooks,
  and a graphics effect/command boundary from the runtime to the browser.
- Keep graphics state in a dedicated module/crate rather than embedding it in
  the window dispatcher. API adapters decode guest arguments; shared graphics
  services own state and produce validated commands.
- Use a shared or mechanically checked API signature registry so browser/native
  dispatch agrees on argument widths, stack cleanup and result handling.

Exit check: a small documented interface and owned files; Hover's current
acceptance tests still pass after any common infrastructure changes.

## 3. Prove context creation and presentation

- Implement ChoosePixelFormat, DescribePixelFormat, SetPixelFormat,
  GetPixelFormat and SwapBuffers in GDI32.
- Implement wglCreateContext, wglMakeCurrent and wglDeleteContext in OPENGL32.
  Enforce valid DCs, compatible pixel formats, context ownership and unbinding.
  Apply Windows pixel-format lifetime rules to window DCs.
- Register OPENGL32 and GLU32 builtins and supported exports. Trace whether
  this executable needs dynamic export addresses; if so, wire them into guest
  indirect dispatch using guest-callable trampolines.
- Current TOLYMPIC integration uses a host CPU rasterizer and copies the
  completed RGBA surface through the existing `SwapBuffers` presentation path.
  Keep rendering synchronous with the guest's ordered command stream; do not
  make the blocked execution worker wait for a browser callback.
- Decide surface ownership so GDI and GL do not both acquire incompatible
  contexts on the same canvas. Route resize and restart through that lifecycle.

Exit check: a small redistributable PE fixture creates a context, clears to a
known color and swaps a visible frame. Verify invalid handles, repeated pixel
format assignment and context deletion. Guest loops must still be terminable.

## 4. Implement geometry, state and matrices

- Add immediate-mode glBegin/glEnd, glVertex3f, glNormal3f and glColor3ubv.
  Batch vertices within the worker; do not post a page message per vertex.
  Trace primitive modes and triangulate the ones actually used with correct
  winding, normal/color capture and provoking-vertex behavior where relevant.
- Add model-view/projection stacks: glMatrixMode, glLoadIdentity,
  glPushMatrix, glPopMatrix, glTranslatef, glRotatef and glOrtho.
- Implement gluPerspective and gluLookAt as matrix helpers, preserving OpenGL
  multiplication order and conventions. No general GLU library is required.
- Implement viewport, clears, draw-buffer selection, shade model, culling,
  depth state and glEnable/glDisable for reached capabilities.
- Decode f32/f64 stack arguments by their IEEE bit representation; validate
  guest arrays before reading. glOrtho, glClearDepth and the GLU helpers require
  double-width stack decoding. Use declared byte counts for callee cleanup.

Exit check: fixture pixels verify transforms, overlapping depth-tested geometry,
flat/smooth colors and resizing. A matrix unit test covers multiplication order
and the two GLU helpers, with degenerate inputs handled consistently.

## 5. Implement display lists and the visible ring material

- Implement glGenLists, glNewList, glEndList and glCallList. Store typed commands
  with compile/execute semantics; copy pointer-supplied values at the correct
  time and limit recursive execution safely. Matrix and current-attribute
  effects must match list replay semantics, not just cached vertex buffers.
- Implement glLightfv, glLightModelfv, glMaterialfv and glColorMaterial.
  Transform light positions when specified; use a correct normal transform.
  Reproduce fixed-function vertex lighting and specular highlights in the
  current CPU renderer.
- Implement glBlendFunc and the reached fog modes through glFogf/glFogfv/glFogi.
  The target uses `GL_SRC_ALPHA, GL_ONE`; its blend toggle is captured and
  changes the green ring and its overlap.
- Handle glFlush without swapping buffers. Support the selected RGBA path;
  glClearIndex/glIndexi are imported too, so establish whether they execute and
  apply their RGBA-mode semantics. Do not silently claim color-index rendering.
  Measured guest execution reaches glClearIndex (a no-op in RGBA mode) but does
  not reach glIndexi; color-index rendering is not claimed.
- Keep GL error state separate from explicit project refusals for missing APIs.

Exit check: the original executable shows recognizable lit rings and renders
both geometry modes. Replaying lists preserves transforms, lighting and color.
Record a frame and the reached API/state inventory.

## 6. Complete interaction, timing and lifecycle

- Connect the actual message/idle loop to keyboard state and window messages.
  Implement only remaining reached startup APIs, including palette or Unicode
  paths if execution requires them.
- Space animates, `b` changes blending, `d` changes geometry, Escape exits.
  Validate behavior against the executable; do not invent toggle semantics
  solely from the README.
- Supply elapsed time and Sleep behavior if reached without starving input,
  presentation or Stop. Yield at safe scheduling boundaries as needed.
- Resize updates the drawable and viewport through the guest's message path.
  Restart resets contexts, lists, matrices and guest state. Context loss must
  produce a clear recoverable stop/restart path rather than a frozen frame.

Exit check: Chromium automation exercises all controls, resize, normal exit,
Stop and Restart; no executed API or instruction is silently stubbed. Current
evidence covers Space, `b`, `d`, Escape, resize, teardown, Stop and Restart.

## 7. Verify and report measured compatibility

- Run focused Rust tests for GL state/list/context semantics, browser adapter
  checks, and the existing interactive PE and Hover regression checks relevant
  to shared changes.
- Use a redistributable fixture for deterministic pixel tests. Use tolerances
  for lit edges and interpolation; avoid GPU-specific whole-frame hashes.
- For external TOLYMPIC acceptance, collect screenshots, frame count, startup
  refusals, frame timing and input results. Obtain a native reference capture
  when available; without one, identify visual checks that remain provisional.
- Document the supported API/state subset and any remaining fidelity gaps.
  Publish no frame-rate target until measurements establish a useful baseline.

Deliverables: build script, graphics implementation, fixture and tests, browser
probe, external-target run instructions and updated measured status.

## Work order and decision gates

Follow stages 1–7 in order. Stage 2 can be settled during the startup audit.
Keep stage 3's clear-window fixture working while expanding rendering.
If CPU recompilation blocks startup, fix that before broadening GL coverage.
If worker-side WebGL is unsuitable, settle backend/transport at stage 3 before
implementing the fixed-function renderer. Choose further GLINT demos only
after TOLYMPIC passes interaction and lifecycle acceptance.
