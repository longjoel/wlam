# Wine GDI conformance harness

This is a small adapter for running Wine's own `dlls/gdi32/tests` sources
through the recompiler. It is the closest practical equivalent of an Acid test
for Win32 GDI: Wine's suite exercises DCs, objects, bitmaps, brushes, clipping,
DIBs, fonts, mapping, metafiles, palettes, paths and pens.

The Wine sources are **not vendored**. They are fetched into the ignored
`target/` tree at a pinned revision:

```sh
./scripts/fetch-wine-tests.sh
```

Override `WINE_COMMIT` to test another revision, or `WINE_TESTS_DIR` to use an
existing checkout.

## Running one test

```sh
./scripts/wine-tests.sh brush
./scripts/wine-tests.sh gdiobj
./scripts/wine-tests.sh pen
```

The script:

1. compiles the unchanged Wine `.c` file with the local clang/ld.lld toolchain;
2. supplies `scripts/wine-tests/shim/wine/test.h` instead of Wine's full test
   runner (only `ok`, `skip`, `broken`, `todo_wine` and `START_TEST` plumbing);
3. generates one synthetic `WLAMTEST.dll` import library from the object's
   unresolved symbols;
4. links a tiny console driver that calls the test entry and prints
   `FAIL file:line` plus a final `RESULT passes failures skips`;
5. recompiles the PE and runs it with the browser host.

`WLAMTEST.dll` is resolved by symbol name: the browser runner maps an import
from that synthetic module to the matching host API. The recompiler's signature
table does the same by trying the known Win32/CRT libraries, so the test does
not need one `.def` file per DLL.

## Current status

The harness is deliberately incremental. A test stops at the first host API
that is not implemented; that refusal names the next piece to work on.

- `pen`: **907 passed, 0 failed**. The suite covers cosmetic/geometric pens,
  `CreatePenIndirect`, `ExtCreatePen`, `PS_NULL`, user styles, alternate pen
  pixels, `GetObject`, and bitmap drawing.
- `brush`: **699 passed, 0 failed**. Covers solid/hatch/pattern/null brushes,
  `CreateBrushIndirect`, `CreatePatternBrush`, `CreateDIBPatternBrushPt`,
  `FillRect`, brush-origin queries, and palette-indexed `PatBlt`/`PATCOPY`
  DIB pattern pixels (bottom-up pattern rows are flipped into device order).
- `dc`: **1604 passed, 0 failed, 6 skipped** (the skips are gamma-ramp and
  printer-device setup, which need real display/printer drivers). The suite covers
  device attributes, save/restore, display/compatible/metafile DCs, bitmap
  DCs, DC deletion, class/private window DCs, coordinate mapping, bounds
  rectangles and the wide-pen bounds heuristics.
- `gdiobj`: passes the initial `test_gdi_objects` section after adding
  `IsWow64Process`, `GetCurrentObject`, low-address `GetObjectA` validation and
  `GetObjectType` last-error behavior. It stops at `CreateThread` when the
  thread-object section starts.
- `palette`: the recompiler stops before execution on a 6-byte operand
  (compiler limitation), not in the host.
- `bitmap`: runs through the DIB-format sweep, DIB sections, DIB color
  tables and `Get/SetDIBits` (about 1.3M checks, 0 failures before
  `test_bitmap_colors`). The remaining failures are concentrated in Windows'
  monochrome colour semantics: solid-pen and mono-DIB `BitBlt` colour selection
  (233k checks in `test_bitmap_colors`). The run currently stops at
  `GdiTransparentBlt`.
- `dib` and `mapping`: compile errors remain because the shim has no `math.h`
  yet.
- `clipping` and `generated`: need additional shim headers
  (`sys/types.h`/winsock) before they reach the host.

`todo_wine`, `flaky` and Wine's child-process/subtest machinery are currently
skipped by the shim. A check inside a todo block is therefore not counted yet;
that is a harness feature gap, not a silent pass. Only checks that actually run
are reported.

The shim `wine/test.h` follows Wine's own header closely enough for tests that
include only it: it pulls in `windef.h`/`winbase.h`, standard CRT headers and
declares the `wine_dbgstr_*` helpers. `skip()` reports a skipped check without
returning from the caller, matching Wine. The runner supplies `_fltused` and
the compiler is invoked with `-mno-stack-arg-probe` so MSVC-target clang does
not require `__chkstk`.

`WLAM_API_LOG=1 ./scripts/wine-tests.sh <test>` (or `node
scripts/wine-tests/run.mjs <bundle>`) prints every host call; the last line
before a trap names the guest path that failed.

## Adding a test

Any `.c` file under `dlls/gdi32/tests` can be passed as the first argument. If
the script compiles but the guest stops on an unsupported import, implement the
named host API and rerun. If it stops in the recompiler, that is a translation
or operand-width issue to fix in the pipeline.
