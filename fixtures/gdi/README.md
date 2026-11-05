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

The initial 50 observations cover DC defaults, the SYSTEM_FONT stock index,
brush selection, clipping, solid pattern fills, nested absolute/relative DC
restoration, overlapping blits, XOR, source-independent raster operations,
exclusive bitmap selection, opaque text rectangles, glyph foreground output,
and 8-bit DIB palette translation, stride and conversion to RGB.

Text checks deliberately avoid exact glyph shapes across Windows versions.
Exact text-image comparisons require a controlled font and rasterization profile.
The host currently uses its bundled bitmap font as a fallback.

## Documentation anchors

- [GDI categories](https://learn.microsoft.com/en-us/windows/win32/gdi/windows-gdi)
- [DC state](https://learn.microsoft.com/en-us/windows/win32/gdi/saving--restoring--and-resetting-a-device-context)
- [Object selection](https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-selectobject)
- [Stock object indices](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-emf/d6dffd25-8615-42f8-aed1-309f1fe54ab2)
- [Text output](https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-exttextouta)
- [Scaling sample for the next fixture group](https://learn.microsoft.com/en-us/windows/win32/gdi/scaling-an-image)
