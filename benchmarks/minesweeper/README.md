# Benchmark: Minesweeper Classic (Win32)

**Upstream:** <https://github.com/enis1enis2/minesweeper-classic> (MIT)
**Binaries:** GitHub release `v1.0.1`, built with MSYS2 **mingw-w64 GCC** (`-O2 -mwindows`)
Both a 32-bit (`minesweeper-x86.exe`) and 64-bit (`minesweeper-x64.exe`) build.

This is the second program the toolchain must run, and deliberately a **different
shape** from the Uxn32 north star:

| | Uxn32.exe | Minesweeper Classic |
|---|---|---|
| Built with | MSVC (proprietary-style `fs:[0]` SEH frames) | mingw-w64 GCC |
| Graphics | Direct GDI blits via `SetDIBitsToDevice` | GDI primitives: `Rectangle`, `Ellipse`, `Polygon`, `Arc`, `LineTo`, `DrawEdge` |
| CRT | custom (no CRT imports) | `msvcrt.dll`, 65 imports |
| Scale | 175 imports / 8 libs | 201 imports / 7 libs |
| Extras | none | Winsock, WinHTTP, registry, INI profiles |

Being GCC-built makes it valuable: it exercises a *different* compiler's code
patterns (different register allocation, different switch lowering) and a
different exception model, so a lifter that only handled MSVC idioms would fail
here.

## Compatibility snapshot

Produced with `wlam analyze benchmarks/minesweeper/bin/minesweeper-x86.exe`.
Reproduce with `scripts/analyze-benchmarks.sh`.

| State | Count |
|---|---|
| native | 59 |
| emulated | 24 |
| replaced | 66 (all `msvcrt.dll`) |
| UNSUPPORTED | 34 |
| unknown | 18 |

52 blocking imports, in three groups:

1. **Core GUI/GDI (18)** — `Rectangle`, `Ellipse`, `Polygon`, `Arc`,
   `MoveToEx`, `LineTo`, `GetDeviceCaps`, `DrawEdge`, `CheckMenuRadioItem`,
   `CheckRadioButton`, `CheckDlgButton`, `IsDlgButtonChecked`, `GetDlgItemInt`,
   `EnumDisplaySettingsA`, plus `GetPrivateProfileStringA`,
   `WritePrivateProfileStringA`, `GlobalMemoryStatusEx`, `IsDBCSLeadByte`.
   These are all straightforward and belong in the V1 host layer.
2. **Networking (31)** — Winsock (`WSAStartup`, `socket`, `connect`, `select`,
   `send`/`recv`, `getaddrinfo`, ...) and WinHTTP (`WinHttpOpen`,
   `WinHttpSendRequest`, ...). **Not optional at startup**: telemetry is on by
   default, so `net_telemetry_start` runs from `WinMain` unless
   `--no-telemetry` is passed.
3. **Registry (3)** — `RegOpenKeyExA` / `RegQueryValueExA` / `RegCloseKey` from
   `diag.c`.

## Things to know before using this as a benchmark

- **It phones home by default.** `net_endpoint_default()` in `network.c`
  base64-obfuscates a production host and port, and `net_telemetry_start()` is
  called from `WinMain` whenever a port is configured. Hourglass runs should
  pass `--no-telemetry` (after verifying that argument parsing is lifted) and
  should assume the executable attempts a network connection otherwise.
- **Standalone CLI mode:** `--listen <port>` opens a TCP control surface
  (`cli_start`). That is Winsock again, and a reason to keep the
  `--no-telemetry`/no-listen path as the primary benchmark target.
- **The entry point starts with `GetDC(NULL)` + `GetDeviceCaps`** before any
  window exists, so the host layer must support screen DCs early.
- The binary carries 12 extra unnamed read-only sections (`/4`, `/14`, `/29`,
  ...) beyond the usual ones — the parser handles them (they are valid
  sections), but they are worth knowing about when mapping memory.

## Files

- `bin/minesweeper-x86.exe` — sha256 `02220c5683e78e291d3cf6ff3fcfae0ae0fdd4eeafd3fd2664c4c7530cbb9ab3`
- `bin/minesweeper-x64.exe` — sha256 `4d43336d9a53d1ed1e6a116c2cb3a0e21521eb702724d4a3ac003957a9739d09`

Binaries are fetched by `scripts/fetch-benchmarks.sh`, not tracked in git.
