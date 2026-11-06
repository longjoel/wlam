# User-supplied DLLs

Some applications import a non-system DLL such as `MFC30.DLL`, `MFC42.DLL`, or
a game's own helper library. The recompiler can link a supplied copy of that DLL
into the guest instead of treating every import as a host API.

## Compiler contract

```sh
wlam recompile game.exe -o target/game/bundle --dll path/to/mfc30.dll
wlam analyze game.exe --json --no-oracle --dll path/to/mfc30.dll
```

- A supplied DLL is parsed as a PE image and mapped at its preferred
  `ImageBase`. Overlapping image ranges are rejected; rebasing is not
  implemented.
- Only DLLs reachable from the EXE's (or another included DLL's) import table
  are included. A supplied file nobody imports does not enlarge the bundle.
- Imports are matched by **name** or **ordinal** against the DLL's export
  table. Ordinals use the export directory's `Base`, not a 1-based index;
  MFC30's ordinals start at 268.
- A matched IAT slot is patched to the export's guest virtual address, and the
  exported code is discovered and emitted into the same Wasm module. The import
  no longer appears in the bundle's host-import list.
- A missing export is a compile error naming the library and symbol. It is not
  deferred to a runtime refusal.
- Known system libraries (`KERNEL32`, `USER32`, `GDI32`, `ADVAPI32`,
  `COMCTL32`, `SHELL32`, `OPENGL32`, `MSVCRT`/`UCRT`, Windows API sets, ...)
  remain host imports even if a copy is supplied. The desktop wizard lists only
  non-system DLLs from the archive; the compiler applies the same filter.
- The manifest gains a `dlls` array with `name`, `base`, and `entry`. The
  browser runner invokes each entry point as
  `DllMain(hinstDLL, DLL_PROCESS_ATTACH, NULL)` before the EXE entry. List a
  dependency before modules that import it; dependency ordering is not inferred.
- Supplied DLL files are not duplicated under the bundle's `files/` data
  assets. Their code and resources live in `guest.wasm` and `image.bin`.

## Analysis

`wlam analyze --dll ...` marks imports satisfied by a supplied DLL as
`linked`. They are not blocking. The desktop wizard shows the same state and
lets the user turn linking off before inspecting compatibility. The bundle
report records which DLLs were actually linked.

## Limits

- The loader has handles for built-in host DLLs. `GetProcAddress` can resolve
  an API that is present in the EXE's import table, because the browser runner
  registers each imported API address with its DLL before execution. This also
  supports the common pattern of calling `LoadLibrary` and then looking up an
  API that the program imported elsewhere.
- `GetProcAddress` cannot currently resolve an arbitrary, unimported host API.
  A supplied game DLL is linked into the guest at compile time; `LoadLibrary`
  does not load one on demand, and its exports are not registered for dynamic
  lookup. `FreeLibrary` only updates reference counts for built-in modules;
  there is no per-DLL unload.
- Forwarded exports, delay-loaded supplied DLLs, TLS callbacks, and per-DLL
  relocations are not implemented.
- Resources from linked DLLs are included in the manifest; DllMain detach is
  not called on guest exit yet.
- Linking a DLL does not make an unimplemented Win32 or CRT dependency work.
  The next refusal still names the exact import.
