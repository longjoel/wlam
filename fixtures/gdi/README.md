# Game GDI behavior harness

This CRT-free PE32 fixture executes real GDI calls and emits JSON Lines through
standard output. It can run unchanged on Windows and through the recompiler.
Generated bitmaps avoid external images, fonts, file dialogs and sample-project
resources. It is an original workload informed by Microsoft's GDI documentation.

Build and run the local implementation:

```sh
./scripts/build-gdi.sh
```

Outputs:

- `target/gdi/gdi.exe`: Windows console executable.
- `target/gdi/bundle/`: recompiled guest.
- `target/gdi/observed.jsonl`: ordered observations, including case names,
  observed values and local expectations.

On Windows, capture a reference in Command Prompt:

```bat
gdi.exe > windows.jsonl
```

Copy that reference to this workspace and compare:

```sh
node scripts/run-gdi-fixture.mjs target/gdi/bundle /path/to/windows.jsonl
```

The comparison preserves order and repeated observations. Handle values are
compared as identity/validity predicates, not platform-specific integers.
Each executable reports its failed local expectation count as its exit code.
A host exception preserves partial stdout for investigation. Reference comparison
checks observed results, independently of the fixture's expectation fields.

**No Windows reference has been captured yet.** Passing local expectations is
regression evidence, not a claim of Windows conformance. Keep the Windows version,
display profile and fixture executable hash beside any reference capture.

The suite currently emits 231 observations covering DC defaults, the SYSTEM_FONT
stock index, brush selection, clipping, solid pattern fills, nested DC restoration,
overlapping blits, XOR, source-independent raster operations, bitmap ownership,
opaque text, glyph foreground output, indexed DIB color/stride conversion,
primitive geometry with ROP2, clipping and current-position state, and GDI
object/text queries: GetObjectType/GetObjectA for pens, brushes, fonts, bitmaps
and palettes, CreateCompatibleBitmap, GetNearestColor, GetTextFaceA,
GetCharWidthA, Set/GetTextCharacterExtra, poly fill mode and mapper flags.

The scaling group exercises StretchBlt and StretchDIBits: enlargement, reduction
under four modes, signed extents, destination clipping without changing sampling,
overlapping stretch copies, XOR, zero extents, saved stretch modes and brush
origins, bottom-up/top-down DIBs, subrectangles, indexed output, and noninteger
nearest-neighbor sampling. It also includes a compact same-DC four-mode reduction
based on the [Microsoft scaling sample](https://learn.microsoft.com/en-us/windows/win32/gdi/scaling-an-image),
which demonstrates `StretchBlt` with BLACKONWHITE, WHITEONBLACK, COLORONCOLOR,
and HALFTONE. Capability flags are checked too. A final completion record is
mandatory; partial stdout never counts as a passing suite.

The current scaling policies are provisional until Windows captures are compared.
COLORONCOLOR samples pixel centers. AND/OR reduction combines source partitions.
HALFTONE uses deterministic area filtering and nearest palette quantization;
it does not reproduce driver-specific dithering. Mirrored DIB scanline return
values and partially clipped source rectangles still need compatibility work.
The expected values are local assertions, not recorded Windows results. A native
Windows run can report expectation mismatches and still emit all observations
for comparison.

Text checks deliberately avoid exact glyph shapes across Windows versions.
Exact text-image comparisons require a controlled font and rasterization profile.
The host currently uses its bundled bitmap font as a fallback.

## Documentation anchors

- [GDI categories](https://learn.microsoft.com/en-us/windows/win32/gdi/windows-gdi)
- [DC state](https://learn.microsoft.com/en-us/windows/win32/gdi/saving--restoring--and-resetting-a-device-context)
- [Object selection](https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-selectobject)
- [Stock object indices](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-emf/d6dffd25-8615-42f8-aed1-309f1fe54ab2)
- [Text output](https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-exttextouta)
- [Scaling an image and its four-mode sample](https://learn.microsoft.com/en-us/windows/win32/gdi/scaling-an-image)
