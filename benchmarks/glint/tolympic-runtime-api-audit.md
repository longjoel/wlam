# TOLYMPIC runtime API audit

Captured 2026-11-03 with [`audit_runtime.mjs`](audit_runtime.mjs) against the
external `TOLYMPIC.exe` bundle. The probe ran 110 presented frames, sent Space,
`b`, `VK_D` (translated to `d`), resized the window to 600×450, toggled `b`
back off, and sent Escape. It exited normally after 108 `SwapBuffers` calls.
The full ordered trace is in [`tolympic-api-trace.json`](tolympic-api-trace.json): 9,447 API
calls, arguments and return values across 79 distinct imports. It also decodes
pointer-backed lighting, material, fog, color and `SYSTEMTIME` values, plus GLU
double arguments. This is one bounded interaction trace, not every runtime
branch.

## Observed path

No handler refusal or guest runtime error occurred. `glGetError`, `glIsEnabled`,
`glDrawBuffer` and `glIndexi` are statically imported but not called in this
trace. The OpenGL call totals and key dispositions are:

| API | Calls | Disposition |
|---|---:|---|
| `wglCreateContext` / `wglMakeCurrent` / `wglDeleteContext` | 1 / 2 / 1 | Context lifecycle handled |
| `ChoosePixelFormat` / `DescribePixelFormat` / `SetPixelFormat` / `GetPixelFormat` | 1 each | RGBA double-buffer format path handled; selected format 1 |
| `SwapBuffers` | 108 | Presents the CPU-rendered RGBA frame |
| `glBegin` / `glEnd` | 16 / 16 | Initial geometry plus the `d` rebuild |
| `glNewList` / `glEndList` / `glCallList` | 2 / 2 / 540 | List 1 compiled initially and rebuilt on `VK_D`; replayed five times per frame |
| `glVertex3f` / `glNormal3f` / `glColor3ubv` | 832 / 832 / 540 | Initial and alternate geometry; vertex positions differ while counts match |
| `glPushMatrix` / `glPopMatrix` | 648 / 648 | Matrix stacks handled |
| `gluPerspective` / `gluLookAt` | 1 / 108 | Matrix helpers handled |
| `glLightfv` / `glLightModelfv` / `glMaterialfv` / `glColorMaterial` | 4 / 3 / 4 / 1 | Fixed-function lighting state handled |
| `glBlendFunc` | 1 | `GL_SRC_ALPHA, GL_ONE` additive factors |
| `glClearIndex` / `glFlush` | 1 / 108 | No-op for selected RGBA mode / synchronous renderer |

The trace values verify:

- `glViewport` is `[0, 0, 400, 300]` initially and `[0, 0, 600, 450]` after
  the resize. Frames were presented at both sizes.
- The input PFD requests RGBA, window drawing, OpenGL and double buffering
  (flags `0x25`, 24 color bits). `ChoosePixelFormat` returns 1; its described
  format has 32 color bits, 8 alpha bits, 24 depth bits and 8 stencil bits.
- `gluPerspective` receives `(45, 1.33, 0.1, 100)`; each `gluLookAt` uses eye
  `(0, 0, 10)`, center `(0, 0, 0)`, and up `(0, 1, 0)`.
- Each `glClear` mask is `0x4100` (color and depth); `glClearDepth` is 1.
- Light-model local viewer, two-sided lighting and global ambient are all
  explicitly zero. The light position has `w = 0`; material shininess is 40;
  color material is `GL_FRONT_AND_BACK`/`GL_DIFFUSE`.
- All 832 submitted normals have length within `3.1e-8` of 1, so disabled
  `GL_NORMALIZE` does not change these target normals.
- `b` enables blending for the green ring while disabling it for the other
  rings. `VK_D` recompiles list 1; the new list has the same vertex count and
  different positions. A second `b` disables blending. The normal ring frame
  before the geometry toggle is byte-for-byte equal to the saved settled
  capture. The resized doughnut frames with blending enabled and disabled are
  shown in [`tolympic-after-d.png`](tolympic-after-d.png) and
  [`tolympic-after-d-no-blend.png`](tolympic-after-d-no-blend.png). After
  switching depth interpolation to `f64` and increasing internal depth
  precision beyond the 24-bit request, the crossing speckles disappeared and
  both previously large depth-conflict samples now agree with Mesa. The blended
  and opaque frames differ in
  26,974 RGB pixels, within the green ring and overlap region (bounding box
  258,108–491,341).
- Startup calls `GetTimeZoneInformation` once. Its host implementation now
  fills a zero-bias UTC structure and correctly returns
  `TIME_ZONE_ID_UNKNOWN` (`0`); this previously returned the invalid sentinel
  `0xffffffff`. Two `GetProcAddress` probes for `CompareStringW` and
  `CompareStringA` return null because those optional exports are not
  registered; the runtime continues through its fallback and reaches the same
  rendered output.
- Startup also calls `GetLocalTime` once. Its `SYSTEMTIME` now reports
  `[2000, 1, 6, 1, 0, 0, 0, 0]`: January 1, 2000 was Saturday (weekday 6). The
  previous virtual clock reported Sunday. This correction leaves the settled
  target captures unchanged.

The full JSON contains every observed call's raw arguments and return value,
register snapshot, order, message data and pixel-format data. Pointer-backed
values used by the rendering checks above are included alongside those calls.
Static but uncalled imports are not claimed as implemented.

After correcting the time-zone result and weekday, the interaction trace was
rerun on 2026-11-03 against the rebuilt bundle and again completed 110 frames
with the same 9,447 calls and 79 distinct APIs. `GetTimeZoneInformation` now
returns `0`. A Chromium browser run against the rebuilt bundle exited with
status 0 after Escape, with no page error.
On 2026-11-04, a fresh Chromium UI smoke loaded `/target/tolympic/bundle/manifest.json`,
ran the guest, accepted Escape, and reported `Exited (0)` with an empty error
panel. This checks the real page/worker/presentation path; it does not replace
the bounded API trace or matched native visual comparison.
Fresh RGBA readbacks for the settled, resized, blended-doughnut and opaque-
doughnut frames were byte-identical to their saved browser PNG captures. The
Mesa replays at frames 68, 90, 102 and 108 also repeated the exact-pixel counts
in the comparison table below.

## Open fidelity evidence

This trace establishes the demonstrated browser path, including both geometry
modes and the blend toggle. It does not prove pixel equivalence with the
original demo on a Windows OpenGL implementation. A matched Windows capture
remains necessary to compare ring shape, overlap, lighting, blending and
rasterization against the original.

As an independent renderer check, the captured OpenGL/GLU stream was replayed
with `python3 benchmarks/glint/replay_mesa.py` at frames 68, 90, 102 and 108 in a
surfaceless Mesa 26.2.4 llvmpipe compatibility context. The EGL pbuffer uses
24-bit depth, 8-bit alpha and 8-bit stencil, matching the selected pixel-format
request; Mesa reports 8 bits of window-coordinate subpixel precision. Comparison
uses RGB because the browser window surface is opaque while EGL pbuffer alpha
readback is implementation-specific. After flipping Mesa's bottom-up readback,
results are:

| Trace frame | Render mode | Exact RGB pixels | Pixels over 20 RGB levels | Mean absolute RGB channel difference |
|---:|---|---:|---:|---:|
| 68 | Settled rings, 400×300 | 117,270 / 120,000 | 0 | 0.013794 |
| 90 | Settled rings after resize, 600×450 | 264,241 / 270,000 | 0 | 0.013172 |
| 102 | Doughnuts with blending, 600×450 | 232,370 / 270,000 | 0 | 0.087379 |
| 108 | Doughnuts without blending, 600×450 | 232,046 / 270,000 | 0 | 0.084338 |

The software renderer preserves `f64` incoming window depth and uses 32-bit
normalized fixed-point internal depth storage, while the selected pixel format
requests 24 bits. OpenGL 1.1 requires window-depth precision of at least the
depth-buffer precision ([§2.10.1 and §4.1.5](https://registry.khronos.org/OpenGL/specs/gl/glspec11.pdf)).
This resolves both earlier high-difference samples: at `(389,213)`, the
black/red candidates had incoming depths 16,616,274.285 and 16,616,274.018, so
the red fragment is nearer but fails against a 24-bit-quantized black value;
the higher-precision internal buffer lets it pass, matching Mesa. The yellow /
green crossing at `(299,312)` also matches Mesa. `gluPerspective` now computes
its matrix from the recorded double arguments before converting coefficients
to the renderer's f32 matrix state; this slightly improves exact-pixel and mean
error counts. All four compared frames now have no pixels differing by more
than 20 RGB levels. Snapping window coordinates to Mesa's reported 8 subpixel
bits removes the large silhouette differences. The replay
captures are
[`normal`](tolympic-mesa-replay.png),
[`blended doughnuts`](tolympic-mesa-doughnuts-blend.png), and
[`opaque doughnuts`](tolympic-mesa-doughnuts-opaque.png). This compares the
CPU renderer with a second OpenGL implementation using the same call stream; it
is not a run of the original Windows executable or a substitute for a matched
native Windows reference.

The Mesa replay utility accepts `--reference` and `--output` overrides and
stores each frame's raw readback in a frame-specific temporary file. This
keeps concurrent frame replays independent. Replaying frames 68, 90, 102 and
108 concurrently reproduces the four exact-pixel counts above; the browser
reference PNGs for all four frames are pixel-identical to the current bounded
trace captures.

The CPU framebuffer models OpenGL's default-enabled `GL_DITHER` state with a
stable 4×4 ordered pattern on RGBA fragment writes. Color clears use one
uniform nearest-candidate choice to match llvmpipe's observed clear behavior.
OpenGL does not standardize the pattern, and
[OpenGL 1.1's literal candidate-value wording](https://registry.khronos.org/OpenGL/specs/gl/glspec11.pdf)
permits adjacent fixed-point candidates; the current quantizer selects within
that allowed set. Its Bayer phase is an implementation choice and may differ
from Mesa. [Microsoft's `glClear` reference](https://learn.microsoft.com/en-us/windows/win32/opengl/glclear)
also identifies dithering as affecting color clears. With dithering disabled,
the renderer truncates RGBA components to the 8-bit framebuffer. With the
uniform clear choice, all four frames now match 117,270 / 264,241 / 232,370 /
232,046 pixels respectively, with no pixel over 20 RGB levels. The ordered
fragment pattern differs from llvmpipe's implementation pattern. A crossing
remains different at `(208,168)` in both doughnut frames: CPU gray `(20,20,20)`
versus Mesa dark yellow `(16,16,0)`. A targeted trace found that the CPU's gray
fragment writes depth `0.990338522706`; a later yellow fragment at
`0.990338598294` fails `GL_LESS`. Mesa reports final depth `0.990338504314`
(24-bit value 16,615,122) with yellow color. The CPU gray depth rounds to that
same 24-bit value, so the remaining difference is a near-equal depth outcome
within about two depth-buffer units. A 24-bit storage trial left the pixel gray
and reduced agreement slightly; f32 depth interpolation and f64 clip-space
depth calculations each worsened the whole-frame comparison, while f64
viewport arithmetic left captures unchanged. These alternatives were reverted.
The replay script forces llvmpipe so repeated runs cannot silently switch to a
hardware driver with a different dither and rasterization pattern.

A controlled renderer experiment split each `GL_QUAD_STRIP` quad along its
other diagonal (`v1-v2`) instead of the current fan diagonal (`v0-v3`). This
made the comparison worse: exact RGB pixels fell to 113,303 / 120,000 for
settled rings, 255,444 / 270,000 after resize, 179,709 / 270,000 for blended
doughnuts and 178,297 / 270,000 for opaque doughnuts. Pixels differing by more
than 20 levels rose to 455, 1,009, 7,774 and 6,950 respectively. The mismatch
at `(208,168)` became gray `(42,42,42)`, farther from Mesa's `(16,16,0)`. The
renderer retains the original fan diagonal. OpenGL 1.1 describes the quad as
a polygon and permits triangulation as one way to interpolate polygon data,
without prescribing a diagonal ([specification §3.5.1](https://registry.khronos.org/OpenGL/specs/gl/glspec11.pdf)).
Matching this isolated crossing therefore remains reference dependent; the
Mesa comparison does not establish which diagonal the original Windows driver
used.
