# Win16 design and OpenGL status

Win16 remains a design proposal. OpenGL support is being developed against the
external TOLYMPIC demo. Keep both tracks independent of the Hover Win32 API
work.

## Recommendation

Start with a narrow Win32 OpenGL demonstration, then expand against a named
application. Develop Win16 as a separate executable frontend with its own ABI.
Both can reuse host window, file, input and rendering services, but neither
should expand the meaning of the existing Win32 ABI implicitly.

Hover is a PE32 target. Its startup progress is not evidence of Win16 support,
and OpenGL is not established as a Hover dependency.

## What the repository provides

- `wlam-pe` parses PE images, not NE executables.
- `wlam-sleigh/src/lib.rs` still executes through its existing 32-bit lifter.
  `rsleigh-api` exposes a decoder-only `X86_16Protected` mode backed by the
  generated x86-32 language with 16-bit default operand/address sizes. This
  establishes a decoding path, not a 16-bit execution mode.
- Discovery, compilation and bundle metadata use flat image addresses.
- `docs/abi.md` specifies a 32-bit stack and Win32 imports.
- `wlam-host` now has a narrow OPENGL32/GLU32 host surface and WGL/pixel-format
  lifecycle for the TOLYMPIC path. It is not a complete OpenGL 1.1 surface.
- `wlam-browser-host` is the browser-facing API adapter. Browser presentation
  reuses the window/DC surface lifecycle and `SwapBuffers` path.
- The x86 lifter remains 32-bit only. No NE parser, selector model or Win16 ABI
  has been implemented.

## Win16

The initial target should be Windows 3.x protected-mode NE applications.
DOS executables, real-mode Windows, drivers, Win32s and arbitrary mixed
16/32-bit thunking are separate compatibility commitments.

### Required architecture

1. Prove the CPU mode first with a tiny decoder experiment: default-16
   instruction decoding, 16-bit effective addresses, operand/address overrides,
   and far transfers. Record which behavior the pinned Sleigh decoder provides
   and which needs an extension. This is a low-cost go/no-go gate before the
   executable loader grows around a 32-bit-only decoder.
   **Decoder-only step implemented:** `rsleigh-api::Architecture::X86_16Protected`
   uses the generated x86-32 language with `addrsize=0`, `opsize=0`, and
   `protectedMode=1`, and reports a 2-byte address size. A direct generated-language probe configured
   with `addrsize=0` and `opsize=0` decodes `[BX+SI]` by default, applies 66/67
   overrides, and decodes far JMP/CALL and RETF/IRET. It used the generator's
   default `protectedMode=1`, so `5678:1234` becomes protected-mode packed
   target `0x56781234`; the real-mode processor spec instead linearizes it as
   `0x000579b4`. The patterns exist, but the generated context alone is not a
   Win16 runtime: the public mode still needs selector or segment-base
   semantics at execution, and the x86 lifter does not yet consume it. The
   public `Decoder` API was compiled with the pinned Rust toolchain and
   exercised for `MOV AX,imm16`, `[BX+SI]`, 66/67 overrides, `CALLF`, `RETF`,
   and CS override; its address-size query returns 2. In
   `toolchain/rsleigh/rsleigh-api/examples/x86_16_protected.rs` this probe is
   retained as a runnable P-Code inspection example. The API now normalizes
   direct near-relative branch and call targets to the instruction's selector
   and wraps their 16-bit IP; a call crossing IP `0xffff` stores return IP
   `0x0001`. The 66-overridden near branch remains 32-bit and is not normalized.
   A fresh compile of the public API and example against the cached generated
   crates confirms a remaining defect: indirect near CALL/JMP at `CS:FFFE`
   writes `CS+1` into the CS register before transfer. The attempted source
   rewrite did not fix that behavior and was removed. Direct near-relative
   targets are covered, but indirect transfers and segment-aware execution
   remain open.
   `X86_32`, `0x67` does
   decode a 16-bit effective address and `0x66` a 16-bit operand, but these are
   overrides inside a 32-bit-default decoder. A CS override is printed without
   adding a selector base to the load address. Worse, `66 9a 78 56 bc 9a`, an
   operand-size-overridden far call carrying `9abc:5678`, decodes to a bogus
   near-like target. Therefore `X86_32` cannot serve as the Win16 execution
   mode. The public 16-bit decoder still is not ready to feed the existing
   flat lifter: near targets are normalized in decoder P-Code, but CS/offset
   remain packed rather than resolved through an NE selector table. A far call
   decodes its target as packed `selector:offset` (`0x9abc5678` here), while
   its stack accesses use segmented-memory userops; those semantics must
   resolve through the selector table. A 66-overridden near transfer requires
   a code-location representation that can carry a 32-bit offset separately
   from CS. Keep selector-aware execution and that override as gates before
   adding the NE loader.
2. Add an NE parser and loader, preferably in a separate `wlam-ne` crate.
   Decode segments, entry bundles, module references, imports by ordinal/name,
   segment relocation chains, resources and initial CS:IP / SS:SP / DGROUP.
   Validate file ranges, relocation targets and allocation sizes. A zero-sized
   encoded segment can represent 64 KiB; do not treat it as an empty allocation.
3. Introduce a guest selector table with base, limit and access metadata.
   Protected-mode selectors are table references, not real-mode `segment << 4`
   addresses. Translate and validate selector:offset accesses with the relevant
   operand/address width. Preserve aliasing when selectors share storage.
4. Integrate the verified decoder mode with execution and extend it only where
   the experiment showed gaps.
   Cover default operand/address sizes, overrides, segment register loads,
   near/far calls and returns, stack wrapping and cross-segment dispatch.
   Code identity needs segment/mode context; a flat byte address alone is not
   a sufficient general key. Do not globally switch the existing PE decoder.
5. Define a Win16 guest ABI: word-sized arguments, Pascal conventions where
   applicable, far return frames, DX:AX results where applicable, and callback
   entry state including DS. Use per-export signatures rather than assuming
   every API has the same stack layout.
6. Add Win16 KERNEL/USER/GDI adapters. Explicitly convert strings, far pointers,
   handles, packed structures and messages to shared host service types.
   Similar API names do not imply identical Win32 layouts. Implement
   MakeProcInstance/FreeProcInstance semantics for callbacks as required by
   the target, along with local/global heap and selector lifetime behavior.
7. Implement a single cooperative task first. Add task switching, reentrancy
   and DLL shared-state behavior only with fixtures that require them.

Movable/discardable segments and executable memory modification can invalidate
static code assumptions. Initially pin supported segments and explicitly refuse
unsupported mutation/reload paths; later support requires recompilation or a
verified fallback. Never silently execute stale compiled code.

### Acceptance sequence

- Decoder probe demonstrating the 16-bit mode behavior and concrete gaps.
- Handcrafted NE parser fixtures for malformed segments and relocation chains.
- CPU fixtures for selector aliases, bounds, near/far calls, DS changes and
  16-bit stack behavior.
- A redistributable Win16 application creates a window, receives input and
  paints text through a callback with the expected DS.
- A dialog/resource/global-memory fixture, followed by one named historical
  application using externally supplied binaries.

## OpenGL on Win32

Implement an `opengl32.dll` host surface and WGL integration. Initially aim for
an explicitly documented subset of legacy OpenGL 1.x; do not advertise complete
OpenGL 1.1 or extensions until their required behavior is covered.

### Context and dispatch

- Add GDI pixel format operations: ChoosePixelFormat, DescribePixelFormat,
  SetPixelFormat and GetPixelFormat, plus SwapBuffers.
- Add wglCreateContext, wglDeleteContext, wglMakeCurrent,
  wglGetCurrentContext and wglGetCurrentDC. Track the current context per guest
  thread and validate HDC/HGLRC ownership and window lifetime.
- Extend module/export registration for LoadLibrary/GetProcAddress. Handle
  wglGetProcAddress separately: return guest-callable trampoline addresses for
  supported extension names, never JS functions or host pointers. These
  addresses must participate in the guest's indirect-call dispatch.
- Decode GL signatures correctly: floats and doubles passed on the x86 stack
  are bit patterns, not integer conversions. Return ABI handling must cover
  each exported signature.
- Keep GL errors and capability queries consistent with implemented behavior.
  Ordinary GL validation errors should use GL error state; missing host API
  implementations should retain explicit project diagnostics.

### Rendering strategy and current progress

The guest GL calls are handled by a host-side fixed-function state machine.
The default path rasterizes on the CPU; the optional
`?renderer=webgl&manifest=/target/tolympic/bundle/manifest.json` path sends
ordered transformed batches to a browser WebGL renderer. The demonstrated
subset covers TOLYMPIC's triangles and quad strips. Both paths handle the
target's immediate-mode vertex and normal submission, display list, matrix
helpers, depth, culling, lighting, material colors, blending, fog and basic
surface presentation. An external TOLYMPIC bundle executes in Chromium and
produces the settled five-ring layout after the guest's Space-triggered offsets
decay to zero. See the CPU capture in
[`benchmarks/glint/tolympic-settled.png`](../benchmarks/glint/tolympic-settled.png).
The WebGL frame-90 capture is in
[`benchmarks/glint/tolympic-webgl-frame90.png`](../benchmarks/glint/tolympic-webgl-frame90.png).
The CPU rasterizer applies inverse-transpose normal transformation and
perspective-correct interpolation for smooth colors and fog distance. It
respects the default disabled `GL_NORMALIZE` state and supports enabling normal
normalization when requested.
Resizing from 400×300 to 600×450 delivers `WM_SIZE`; TOLYMPIC updates
`glViewport` and presents all five rings at the new dimensions. See
[`benchmarks/glint/tolympic-resized-settled.png`](../benchmarks/glint/tolympic-resized-settled.png).
The `b` blend and `d` geometry callbacks reach the guest GL path, and Escape
exits cleanly. Teardown successfully unbinds and deletes the WGL context,
releases the DC and destroys the window. Browser Stop and Restart also produce
a fresh frame after restart. Functional resize is demonstrated; pixel-level
fidelity remains provisional until a native reference is available.

The settled `b` key path enables blending for the green ring. TOLYMPIC selects
`GL_SRC_ALPHA, GL_ONE`; the enabled capture differs from the disabled frame
only around the green ring and its overlap. See
[`tolympic-blend-enabled.png`](../benchmarks/glint/tolympic-blend-enabled.png).
TOLYMPIC sets local-viewer and two-sided lighting false, scene ambient to zero,
and a white specular material/light with shininess 40. The renderer uses the
OpenGL default infinite viewer along eye-space +Z, which is material to the
specular highlights in the saved frame. Local-viewer state is decoded and
applied. Two-sided lighting now selects front or back material state, reverses
back-face normals, honors per-face color-material tracking, and retains the
primitive's provoking color through clipping and triangulation.

Batch glBegin/glEnd vertices into draw commands; avoid a JS message per vertex.
Capture state at the correct API boundary. Copy client data when semantics
require it, validating guest ranges and checked stride/count arithmetic.
Texture upload/readback must honor pixel-store alignment and supported formats.
Triangulate legacy primitive types while preserving relevant winding and
flat-shading semantics.

The supported renderer is target-driven. Textures, client arrays, alpha test,
raster operations, selection/feedback, broad GLU and extensions remain outside
the current demonstrated subset. A native reference capture is still needed
to judge pixel-level fidelity.

Integrate presentation with the existing HWND/HDC surface model. Define how
OpenGL surfaces and GDI content compose, how resize works, and when SwapBuffers
becomes visible. Preserve command ordering; synchronous queries/readback need
an explicit worker/backend response path. Account for browser context loss.

### Acceptance sequence

1. A TOLYMPIC PE bundle selects its pixel format, creates a context, replays
   display lists and presents frames in Chromium (demonstrated; see
   `benchmarks/glint/tolympic-plan.md`).
2. The guest reaches its settled five-ring framing after 65 swaps in the
   diagnostic run; compare with a native reference capture for fidelity.
3. Exercise Space, `b`, `d`, Escape, context teardown, resize, Stop and Restart
   (all are demonstrated).
4. Expand the documented API subset only against a named target and its actual
   runtime trace; dynamic GL lookup, broader GLU and other GLINT demos remain
   follow-up work.

## Shared work worth coordinating now

Agree on stable window/DC identities, surface ownership and lifecycle hooks;
reusable host services below Win32-shaped argument decoding; and a registry for
guest-callable dynamic exports. Keep Win16 selectors and handles distinct from
Win32 values. These are interfaces to coordinate with the Hover implementation,
not reasons to interrupt its current startup work.

## References

- [Microsoft WGL and Windows reference](https://learn.microsoft.com/en-us/windows/win32/opengl/wgl-and-windows-reference)
- [Microsoft pixel formats](https://learn.microsoft.com/en-us/windows/win32/opengl/pixel-formats)
- [Khronos fixed-function pipeline](https://wikis.khronos.org/opengl/Fixed_Function_Pipeline)
- [Khronos WebGL specifications and tests](https://github.com/KhronosGroup/WebGL)
- [Wine developer hints: selectors and NE segments](https://goma.googlesource.com/wine/+/14f9b844ce56a2f160a230da1da79a3da4d2f124/DEVELOPERS-HINTS)

The implementation choices and milestone ordering above are proposals derived
from these API models and the current repository, not claims of compatibility.
