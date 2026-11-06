# Game-focused GDI implementation

Objective: a coherent classic raster GDI implementation supporting Hover and
other game programs, checked by portable Win32 behavior fixtures and real games.
The objective is active; the current implementation is incomplete.

## Verification model

Use the [GDI category index](https://learn.microsoft.com/en-us/windows/win32/gdi/windows-gdi)
as the coverage taxonomy, specific API references as contracts, and Windows runs
as differential evidence. Documentation examples motivate interaction workloads;
examples alone are not expected-output oracles. Local Rust tests isolate state
and pixel failures; the PE harness also covers ABI, translation and host dispatch.
Hover is the integration target, not the only source of acceptance criteria.

Run `./scripts/build-gdi.sh`. See `fixtures/gdi/README.md` for native Windows
capture and reference comparison. No native Windows evidence is available yet.

Wine's upstream GDI conformance tests can be fetched and run incrementally with
the adapter in [`scripts/wine-tests`](../scripts/wine-tests/README.md). That
harness compiles Wine's unchanged test sources, links them through one synthetic
import module, and stops at the first unimplemented host API so the next GDI
gap is explicit.

Current Wine status: `pen` passes 907/0, `brush` passes 699/0 and `dc` passes
1604/0 with 6 hardware skips. `dc` includes coordinate mapping, bounds
rectangles, region clipping, display/metafile device contexts and
class/private window DCs. `bitmap` runs through the full DIB-format sweep, DIB
sections, colour tables and `Get/SetDIBits` (about 1.3M checks) before Windows'
monochrome pen and mono-DIB `BitBlt` colour rules fail; it stops at
`GdiTransparentBlt`. `gdiobj` passes its initial object section and stops at
`CreateThread`, because the browser host has no guest thread scheduler yet.
`palette` is still blocked inside the recompiler on an unsupported 6-byte
operand.

| Area | Current implementation and evidence | Remaining work |
| --- | --- | --- |
| DCs and objects | Per-DC brush/pen/font/bitmap/palette state, map mode, window/viewport origins and extents, layout, miter limit and bounds rectangle; display DCs (`CreateDC`), enhanced/old metafile DCs, class/private window DCs; rectangle-list regions (`CreateRectRgn`, `GetRegionData`, `GetClipRgn`, `SelectClipRgn`); nested save/restore; selected-object retention; shared fonts and exclusive bitmaps; GetObjectType/GetObjectA/GetCurrentObject for pens, brushes, fonts, bitmaps and palettes; CreateCompatibleBitmap, CreateDiscardableBitmap, CreateBitmap/CreateBitmapIndirect at 1/2/4/8/16/24/32 bpp, CreateDIBitmap/GetDIBits/SetDIBits/SetDIBitsToDevice/GetDIBColorTable/SetDIBColorTable, Get/SetBitmapBits; hatch, pattern and palette-indexed DIB pattern brushes. PE fixture and Rust tests. | Windows comparison; full monochrome pen/BitBlt colour rules; 2-bpp DDB drawing; compressed (RLE) DIB decoding. |
| Colors and palettes | Text/background colors; 8-bit and RGB DIB conversion; 20 reserved system colors; logical palette creation, selection and realization; final indexed-display frame mapping through the realized palette; DIBINDEX/PALETTEINDEX/PALETTERGB pixels and DIB_PAL_COLORS sections. Rust regression verifies that realization changes presented pixels. | Palette-change notifications, animation, background palette mapping, and Windows comparisons. |
| Raster transfers | Window and DIB sources/destinations; overlap snapshots; ternary raster operations with solid brush patterns; destination clipping. PE overlap/XOR/pattern cases. | Windows cases for all ROP3 truth tables, monochrome conversion, patterned brushes, device-dependent bitmaps. |
| Clipping/regions | Rectangular clipping saved with DC state and applied to text/fills/blits; rectangle-list region objects and clip-region APIs. | Region booleans (union/intersection/difference) for complex shapes. |
| Text | Set/get colors, background mode, alignment; TextOutA/ExtTextOutA with advances and opaque/clipped rectangles on both target types; GetTextFaceA, GetCharWidthA, Set/GetTextCharacterExtra and mapper flags. Fallback bitmap font. | Font realization/selection fidelity, Unicode, kerning, complex scripts, DrawText, Windows font-controlled comparison. |
| Stretching | StretchBlt/StretchDIBits share DC clipping, RGB/indexed targets and ROP3. Four filtering policies, signed extents, source snapshots, and saved brush origins. PE and Rust cases, including the same-DC four-mode reduction from Microsoft's scaling sample. | Windows differential results; exact pixel-center/end-point rules, mirrored DIB scanline returns, partial source clipping, HALFTONE dithering, DIB_PAL_COLORS. |
| Geometry | Solid cosmetic lines and rectangles, solid/null pens and brushes, GetPixel/SetPixel, ROP2, current position, DC clipping and save/restore; polygon fill mode stored per DC; ExtCreatePen object/query semantics, PS_NULL, user styles and alternate-pen pixels. PE cases. | Windows differential results; thick/dashed/geometric pen rasterization, ellipses, polygon/path APIs, fill-rule application, complex clip regions. |
| Painting | Existing USER32 paint callbacks, child-control initialization, template-order composition and window presentation. Hover Quick Help browser case. | Dirty regions, background erase fidelity, z-order changes after creation and palette-related repaint cases. |

Printing, metafiles and multiple monitors are outside the initial game/display
milestone, but should remain explicitly classified in the coverage taxonomy.
Unsupported operations must remain explicit failures rather than successful no-ops.

## Completion evidence still needed

- Documented supported subset with API return/state/pixel behavior.
- Native Windows comparison captures for each implemented fixture group.
- Broader games/raster fixtures covering transfers, scaling, palette changes,
  clipping, primitive drawing and text through shared DC state.
- Hover starts, loads a game, draws gameplay, responds to controls and exits
  without unsupported imports, corruption or unexplained guest traps.
- Existing browser interaction and OpenGL paths retain their behavior.

## Latest Hover evidence

The 190-observation local harness covers scaling and initial raster geometry,
including a small same-DC version of Microsoft's four-mode scaling example; no
Windows comparison has been recorded. HALFTONE currently means deterministic
area filtering and nearest palette quantization, not a verified match for
Windows driver dithering. Rectangular mapping, mirroring and nearest-neighbor
decisions also await native reference captures.

After correcting ExtTextOutA's ABI and adding the ANSI case-conversion group,
window-state queries and EnableWindow's cancellation/enable notifications,
F2 reaches Quick Help, whose instruction text and checkbox now paint and respond
to pointer/keyboard input in the browser acceptance fixture. The host now
provides an in-memory registry for Hover's key/value operations, and supports
window properties used by its startup flow. Destroying an owned modal now
returns browser presentation to the visible application window. Dismissing
Quick Help and declining Hover's quit-in-progress prompt return to a 428×362
waiting window without a guest error. A fresh Chromium trace confirms ArrowUp
reaches the main window procedure; sequence-numbered tracing shows no GDI call
at all after that key event. Across the captured run, the host saw one initial
BitBlt, one PatBlt and 558 ExtTextOutA calls (mostly UI/help text). The surface
remains black, so gameplay rendering and controls are not yet verified. This
points to game-state or invalidation flow as the next issue, rather than
evidence of a failing pixel transfer. A later trace narrows it further: the F2
command creates the game shell and shows Quick Help (dialog resource 115),
which returns IDOK, but the level-reset path that sets the game running and
starts its clock is never entered. `timeSetEvent` is now implemented (delay,
one-shot/periodic, cancellation and guest callback delivery), yet Hover has not
reached it. `ShowCursor` and NULL-target `RedrawWindow` also execute. The
downloadable debugger log now retains 256 recent non-noise API calls and
lifetime call totals; high-volume error/TLS polling is summarized so it does
not displace useful events. The stock SYSTEM_FONT index is 13;
DEFAULT_PALETTE is 15. Earlier code conflated these; both the implementation
and fixture now use the documented distinction.
