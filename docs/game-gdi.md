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

| Area | Current implementation and evidence | Remaining work |
| --- | --- | --- |
| DCs and objects | Per-DC brush/pen/font/bitmap/palette state; nested save/restore; selected-object retention; shared fonts and exclusive bitmaps. PE fixture and Rust tests. | Windows comparison; DC origin/mapping transforms; full object queries and stock monochrome bitmap drawing. |
| Colors and palettes | Text/background colors; 8-bit and RGB DIB conversion; system palette subset. Indexed PE cases. | Palette-index COLORREFs, palette-change notifications, animation, DIB_PAL_COLORS, background palette mapping. |
| Raster transfers | Window and DIB sources/destinations; overlap snapshots; ternary raster operations with solid brush patterns; destination clipping. PE overlap/XOR/pattern cases. | Windows cases for all ROP3 truth tables, monochrome conversion, patterned brushes, device-dependent bitmaps. |
| Clipping/regions | Rectangular clipping saved with DC state and applied to text/fills/blits. | Complex regions, exclusions and region objects. |
| Text | Set/get colors, background mode, alignment; TextOutA/ExtTextOutA with advances and opaque/clipped rectangles on both target types. Fallback bitmap font. | Font realization/selection fidelity, Unicode, kerning, complex scripts, DrawText, Windows font-controlled comparison. |
| Stretching | Stretch-mode attribute is stored and restored. | StretchBlt/StretchDIBits and all four stretch modes, negative dimensions and brush-origin behavior; fixture derived from scaling workload. |
| Geometry | Pen/brush object creation and selection. | Lines, shapes, paths, ROP2 and fill rules. |
| Painting | Existing USER32 paint callbacks and window presentation. | Dirty regions, background erase fidelity and palette-related repaint cases. |

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

After correcting the eight-argument ExtTextOutA ABI and rebuilding the guest,
F2 progresses past progress-text painting and reaches maze/object processing.
The next measured refusal is USER32!CharUpperA on the maze token BEACON_17.
Gameplay remains unverified. The stock SYSTEM_FONT index is 13; DEFAULT_PALETTE
is 15. Earlier code conflated these; both the implementation and fixture now
use the documented distinction.
