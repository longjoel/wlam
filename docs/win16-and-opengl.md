# Win16 and OpenGL status

Win16 has a checked NE loader and limited 16-bit execution prototype; its
Windows API runtime is still under development. OpenGL support is being
developed against the external TOLYMPIC demo. Keep both tracks independent of
the Hover Win32 API work.

## Recommendation

Start with a narrow Win32 OpenGL demonstration, then expand against a named
application. Develop Win16 as a separate executable frontend with its own ABI.
Both can reuse host window, file, input and rendering services, but neither
should expand the meaning of the existing Win32 ABI implicitly.

Hover is a PE32 target. Its startup progress is not evidence of Win16 support,
and OpenGL is not established as a Hover dependency.

## What the repository provides

- `wlam-pe` parses PE images. `wlam-ne` parses MZ/NE metadata, loads
  selector-backed segments, applies supported fixups and steps a limited
  subset of decoded 16-bit x86 instructions.
- `wlam-sleigh/src/lib.rs` still executes PE code through its existing 32-bit
  lifter; the separate `NeMachine` bridge handles the limited Win16 subset.
  `rsleigh-api` exposes a decoder-only `X86_16Protected` mode backed by the
  generated x86-32 language with 16-bit default operand/address sizes. This
  establishes a decoding path; the separate `NeMachine` bridge executes only a
  subset of its P-code.
- Discovery, compilation and bundle metadata use flat image addresses.
- `docs/abi.md` specifies a 32-bit stack and Win32 imports.
- `wlam-host` now has a narrow OPENGL32/GLU32 host surface and WGL/pixel-format
  lifecycle for the TOLYMPIC path. It is not a complete OpenGL 1.1 surface.
- `wlam-browser-host` is the browser-facing API adapter. Browser presentation
  reuses the window/DC surface lifecycle and `SwapBuffers` path.
- The general x86 lifter remains 32-bit only. `wlam-ne` has a separate,
  checked instruction-step bridge and bounded NE resource-data access. Broad
  CPU coverage and the Win16 ABI are not implemented.

## Win16

The initial target should be Windows 3.x protected-mode NE applications.
DOS executables, real-mode Windows, drivers, Win32s and arbitrary mixed
16/32-bit thunking are separate compatibility commitments.

### Required architecture

1. Prove the CPU mode first with a tiny decoder experiment: default-16
   instruction decoding, 16-bit effective addresses, operand/address overrides,
   and far transfers. Record which behavior the pinned Sleigh decoder provides
   and which needs an extension. This was the low-cost go/no-go gate before
   growing an executable loader around a 32-bit-only decoder.
   **Decoder-only step implemented:** `rsleigh-api::Architecture::X86_16Protected`
   uses the generated x86-32 language with `addrsize=0`, `opsize=0`, and
   `protectedMode=1`, and reports a 2-byte address size. A direct generated-language probe configured
   with `addrsize=0` and `opsize=0` decodes `[BX+SI]` by default, applies 66/67
   overrides, and decodes far JMP/CALL and RETF/IRET. It used the generator's
   default `protectedMode=1`, so `5678:1234` becomes protected-mode packed
   target `0x56781234`; the real-mode processor spec instead linearizes it as
   `0x000579b4`. The patterns exist, but the generated context alone is not a
   Win16 runtime: the public mode still needs selector or segment-base
   semantics at execution, and the general x86 lifter does not consume it. A
   separate `NeMachine` bridge now handles a limited operation subset. The
   public `Decoder` API was compiled with the pinned Rust toolchain and
   exercised for `MOV AX,imm16`, `[BX+SI]`, 66/67 overrides, `CALLF`, `RETF`,
   and CS override; its address-size query returns 2. In
   `toolchain/rsleigh/rsleigh-api/examples/x86_16_protected.rs` this probe is
   retained as a runnable P-Code inspection example. The API normalizes direct
   near-relative branch and call targets to the instruction's selector and
   wraps their 16-bit IP; a call crossing IP `0xffff` stores return IP `0x0001`.
   Default-16 indirect near CALL/JMP now retain the instruction's selector
   across IP wrap. Regression tests cover both transfers at `CS:FFFE` and a
   non-wrapping address; the runnable example shows their CS assignments as
   `0x1234`. The 66-overridden near branch remains 32-bit and is not normalized.
   General segment-aware execution remains open beyond the limited bridge.
   `X86_32`, `0x67` does
   decode a 16-bit effective address and `0x66` a 16-bit operand, but these are
   overrides inside a 32-bit-default decoder. A CS override is printed without
   adding a selector base to the load address. Worse, `66 9a 78 56 bc 9a`, an
   operand-size-overridden far call carrying `9abc:5678`, decodes to a bogus
   near-like target. Therefore `X86_32` cannot serve as the Win16 execution
   mode. The public 16-bit decoder still cannot feed the existing flat lifter
   directly: CS:IP targets remain packed rather than resolved through an NE
   selector table. The separate `NeMachine` bridge handles a limited subset.
   Immediate default-16 far CALL/JMP targets are normalized to packed `selector:offset`
   (`0x9abc5678` for the probe) because the generated P-Code mispacked them;
   their stack accesses still use segmented-memory userops that must resolve
   through the selector table. Indirect far transfers also need selector-aware
   execution. A 66-overridden near transfer requires
   a code-location representation that can carry a 32-bit offset separately
   from CS. The NE parser and selector loader now exist; selector-aware
   execution and the 66 override remain gates before expanding binary support.
2. Add an NE parser and loader, preferably in a separate `wlam-ne` crate.
   **Parser milestone implemented:** `wlam-ne` locates the NE header through
   the DOS stub, validates the header and segment-table ranges, checks each
   file-backed segment range, and exposes initial CS:IP / SS:SP, flags, and
   allocation metadata. Encoded zero length for a file-backed segment and zero
   minimum allocation are represented as 64 KiB; sector offset zero instead
   denotes uninitialized storage with no file bytes. It also parses segment
   relocation records and validates non-additive source chains against segment
   bytes, rejecting cycles and truncated/out-of-range sources. Internal targets
   are checked against segment allocation bounds or movable entry ordinals;
   import targets are checked against module/name tables, using the NE
   one-based module index. Target words remain available in parsed records for
   inspection. It also parses
   fixed, movable and unused entry bundles into ordinal/segment/offset records,
   validating table bounds, segment references and offsets. Unit fixtures cover
   valid metadata, alignment scaling, 64 KiB segments, entry bundles, valid and
   additive relocations, malformed chains, invalid signatures/alignment and
   truncated segment data. It also parses module-reference indices and the
   length-prefixed imported names they reference, plus imported names used by
   import-name relocations; names remain raw bytes. Import ordinal/name indices
   are range-checked. `NeImage::load_segments` now allocates segment storage,
   copies initialized bytes, zero-fills allocation tails, and maps segment
   numbers to selectors (successive descriptors advance the selector index by
   eight). Selector reads, writes and execute checks validate
   limits and code/data/read-only permissions; aliases share the same backing
   storage. `NeLoadedImage::apply_relocations` applies internal segment/entry
   targets for byte, selector, 16-bit offset and 32-bit far-pointer fixups;
   additive fixups add the target to the existing source value. The typed
   resolver binds imported names or ordinals to explicit guest far addresses
   inside mapped selectors and applies them through the same all-or-nothing
   fixup path. Unsupported fixup kinds and unresolved imports fail without
   committing partial changes.
   `NeImage::start_state` resolves initial CS:IP and SS:SP to allocated
   selectors, checking that CS is executable, IP is in bounds, and SS:SP has
   writable stack storage (SP may equal the allocation limit). `NeLoadedImage::decode_next`
   fetches through execute-checked selector memory and decodes using
   `X86_16Protected`; it wraps instruction fetch at IP overflow when the
   selector allocation is at least 64 KiB. The loader reserves declared heap
   and stack sizes; start-state setup aligns SP and reserves the initial
   four-byte task frame. If header SP is zero, it derives the stack top from
   the declared segment allocation. `NeMachine::new` initializes DS
   from the automatic data segment and leaves ES null until task startup
   supplies the PSP selector. `NeLoadedImage::add_region` maps task-owned
   memory through `NeLoadedImage::add_region_auto`,
   `NeMachine::initialize_task_psp` fills the 256-byte DOS-compatible
   process block and command tail, and `NeMachine::initialize_task_start` sets
   ES and the Windows entry registers from caller-supplied task handles. The
   caller still allocates parent/environment/instance selectors, supplies the
   DOS dispatcher target, and provides the API runtime. The checked `NeMachine::step` bridge
   executes a limited set of decoded integer, boolean, shift, load/store and
   transfer P-code operations.
   Handcrafted coverage currently includes a far call, `RETF`, and a
   register-immediate move. Indirect branches resolve decoder RAM-varnode
   aliases to their produced Unique temporary when that target is present.
   Unsupported P-code traps explicitly; this is not a complete 8086/286 CPU
   and does not yet establish compatibility with real Win16 binaries.
   `NeImage::parse` now reads the Windows NE resource table,
   preserves named/ordinal type and resource identifiers, and validates
   payload offsets and lengths after applying the table's alignment shift;
   `resource_data` exposes bounded raw bytes. The `inspect_ne` example prints
   resource IDs, payload ranges and lengths for real NE binaries.
   `NeResourceManager` provides lazy Find/Load/Lock/Unlock/Free operations with
   separate 16-bit resource/global handles, guest identifier-pointer decoding,
   and selector-backed payloads up to one 64 KiB region. Host KERNEL export
   wiring, full TDB/task structures, instance-handle management and API runtime
   startup remain unimplemented.
3. Expand selector-aware CPU execution. Protected-mode selectors are table
   references, not real-mode `segment << 4` addresses. The `NeMachine` bridge
   now routes decoded fetches and data loads/stores through selector checks and
   preserves shared selector storage. Expand instruction and address-size
   coverage while retaining those bounds and permissions.
4. Integrate the verified decoder mode with execution and extend it only where
   the experiment showed gaps.
   Cover default operand/address sizes, overrides, segment register loads,
   near/far calls and returns, stack wrapping, conditional and internal P-code
   control flow, and cross-segment dispatch.
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
- Handcrafted NE fixtures now cover segment allocation and selector loading,
  relocation chains, additive fixups, internal and caller-resolved targets,
  and initial register mapping.
- CPU fixtures for selector aliases, bounds, near/far calls, DS changes,
  conditional flow and 16-bit stack behavior.
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
The original executable also ran under Proton/Wine 11.0 on Mesa llvmpipe.
Its 400×300 frame differs from the browser CPU capture in 8 pixels, with
0.00304 mean absolute channel error on an 8-bit scale. See
[`benchmarks/glint/tolympic-wine-settled.png`](../benchmarks/glint/tolympic-wine-settled.png).
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
comparison against the documented Wine/Mesa run is near pixel-identical;
native Windows driver-specific behavior remains unverified.

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
the current demonstrated subset. A native Windows capture would further check
driver-specific fidelity beyond the documented Wine/Mesa comparison.

Integrate presentation with the existing HWND/HDC surface model. Define how
OpenGL surfaces and GDI content compose, how resize works, and when SwapBuffers
becomes visible. Preserve command ordering; synchronous queries/readback need
an explicit worker/backend response path. Account for browser context loss.

### Acceptance sequence

1. A TOLYMPIC PE bundle selects its pixel format, creates a context, replays
   display lists and presents frames in Chromium (demonstrated; see
   `benchmarks/glint/tolympic-plan.md`).
2. The guest reaches its settled five-ring framing after 65 swaps in the
   diagnostic run; a Proton/Wine Mesa frame provides a documented reference
   with only 8 differing RGB pixels. A native Windows capture would further
   check driver-specific fidelity.
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
- [WineVDM NE task startup register setup](https://github.com/otya128/winevdm/blob/master/krnl386/ne_module.c#L1383-L1468)

The implementation choices and milestone ordering above are proposals derived
from these API models and the current repository, not claims of compatibility.
