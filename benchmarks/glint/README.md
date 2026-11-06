# 3Dlabs OpenGL demo candidates

Assessed from `/home/longjoel/Downloads/glint_demos/` on 2026-11-01; status
updated 2026-11-05. Original binaries/assets remain external. `imports.json`
records static imports, not reachable API coverage. Runtime use and dynamic
lookup need a separate trace for each demo.

TOLYMPIC has since been run through the browser pipeline: its settled rings,
Space animation, `b` blend toggle, `d` geometry toggle, resize, Escape,
teardown, Stop and Restart have been exercised. Browser captures are recorded
in this directory. A Proton/Wine 11.0 run under Mesa llvmpipe also produced a
settled 400×300 reference that differs from the browser CPU capture at 8 of
120,000 RGB pixels. Windows-native driver behavior remains unverified. See
[the current implementation plan](tolympic-plan.md).
The OpenGL host and PE32 integration fixtures are documented in
[`fixtures/opengl`](../../fixtures/opengl/README.md); the PE smoke path covers
window creation, WGL lifecycle, a captured triangle, swap and teardown.

The supplied README identifies release 1.5 for Windows NT 3.5/3.51 and Windows
95, dated December 14, 1995. All nine executables are PE32/i386, including
TOLYMPIC, whose PE subsystem is console. None requires an NE/Win16 loader.
All statically import KERNEL32, USER32, GDI32 and OPENGL32; eight also import
GLU32. There are no GLINT-specific static DLL dependencies. This does not prove
the absence of hardware-sensitive capability checks or dynamic dependencies.

## TOLYMPIC status and suggested next targets

| Target | OpenGL DLL imports, including WGL | GLU imports | Assessment |
|---|---:|---:|---|
| TOLYMPIC.exe | 39 | 2 | Browser path and Proton/Wine Mesa reference closely match; native Windows driver fidelity remains open. |
| TEXLIST.exe | 34 | 2 | Texture milestone: texture generation, pixel storage and GLU mipmap building |
| x29.exe | 50 | 0 | Broader untextured rendering and menus; no GLU DLL dependency |
| FF.exe | 38 | 2 | Animation, attribute stacks, display-list text and wireframe modes |
| IDEAS.exe | 34 | 2 | Lighting, matrix operations and polygon stipple |
| cb.exe | 49 | 2 | Stencil, color masks, wireframe and antialiasing modes |
| SMOOTH.exe | 63 | 2 | Later: selection/name stacks, feedback, line stipple and polygon antialiasing |
| RG.exe | 72 | 1 | Later: accumulation, feedback, readback, texturing and larger scene |
| rc.exe | 71 | 1 | Later: similar broad surface, plus depth range and point size |

Ordering is a proposal based on imports and the supplied descriptions. Static
import counts are not effort estimates. Imports may support optional modes.
The supplied README calls the large scenes RollerCoaster and Racing Car;
the exact RG/rc mapping has not been established by running them.

## Observed TOLYMPIC behavior and remaining evidence

TOLYMPIC is the first real application milestone, with context lifecycle,
presentation, its observed fixed-function rendering path and controls
implemented. A bounded per-call audit covers startup, both geometry modes,
blending, resize and exit. The Proton/Wine reference closely matches the
browser image; native Windows driver-specific fidelity remains unverified. Its
GLU imports are only gluPerspective and gluLookAt, implemented as matrix
helpers without a general GLU port.

The static imports also include palette operations and both ANSI and Unicode
window-class/creation APIs. Those imports do not prove that a demo calls those
paths. TOLYMPIC's bounded trace records the calls it reaches; each next target
needs its own trace before its startup requirements are known. The supplied
README says every demo is double buffered; SMOOTH additionally requires eight
alpha bits. Pixel format advertisement must match implemented surfaces.

Browser-path acceptance evidence includes visible lit rings, resize handling,
Space animation, blend and geometry toggles, Escape exit, teardown and
Stop/Restart. The per-call trace and normal/doughnut captures are in this
directory. The Wine/Mesa comparison narrows the remaining visual uncertainty
to Windows-specific graphics behavior; successful compilation or context
creation alone is insufficient.

The recorded GL command stream was replayed through Mesa llvmpipe in a
surfaceless compatibility context. The CPU renderer now models default-enabled
`GL_DITHER` with a stable ordered pattern on fragment writes and a uniform
clear-color choice. At 400×300, 117,270 of 120,000 RGB pixels match exactly; at
600×450, the resized, blended and opaque frames match 264,241, 232,370 and
232,046 of 270,000 pixels. No pixels differ by more than 20 RGB levels. The
fragment pattern is implementation-specific. The CPU renderer keeps `f64` incoming depth and
32-bit normalized fixed-point depth internally, while the selected pixel
format requests 24 bits. The
`gluPerspective` matrix is computed from its double arguments. Eight-bit
subpixel snapping removed the large silhouette differences. This
cross-implementation comparison does not replace a run of the original Windows
application. Details and limitations, plus the replay command, are in the
[runtime API audit](tolympic-runtime-api-audit.md) and
[Mesa replay script](replay_mesa.py).

- [Normal rings at 400×300](tolympic-before-d.png)
- [Mesa replay of the captured GL command stream](tolympic-mesa-replay.png)
- [Resized settled rings at 600×450](tolympic-resized-settled.png)
- [Mesa replay of resized rings](tolympic-mesa-resized-settled.png)
- [Doughnuts with blending at 600×450](tolympic-after-d.png)
- [Mesa replay of blended doughnuts](tolympic-mesa-doughnuts-blend.png)
- [Doughnuts without blending at 600×450](tolympic-after-d-no-blend.png)
- [Mesa replay of opaque doughnuts](tolympic-mesa-doughnuts-opaque.png)

TEXLIST's gluBuild2DMipmaps and glTexGeniv make it a useful second milestone,
but it is not just a basic texture upload test. SMOOTH and the racing demos
should wait until the simpler rendering path works.

See [the architecture proposal](../../docs/win16-and-opengl.md) for shared
surface ownership, guest-callable exports and browser backend considerations.
