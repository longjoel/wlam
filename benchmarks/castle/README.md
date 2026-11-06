# Castle of the Winds: Win16 target

Goal: the original Castle of the Winds is playable through recompilation.
**Not playable yet.** The current AOT artifact executes real startup code and
runs main-window creation callbacks and enters a modal dialog callback.
Input, complete dialogs, presentation and gameplay still need verification.

External input: `/home/longjoel/Desktop/win3_CasWin1.zip`, extracted locally to
`target/castle/input`. Archive and executable bytes are not repository fixtures.
The Olympics application is independent of this target.

## Recompile and verify startup

Run from the repository root:

```sh
export CARGO_HOME=$PWD/toolchain/cargo
export RUSTUP_HOME=$PWD/toolchain/rustup
cargo run --offline -p wlam-ne --example compile_ne -- \
  target/castle/input/CASTLE1.EXE target/castle/aot
cargo run --offline -p wlam-ne --example run_compiled_ne -- \
  target/castle/input/CASTLE1.EXE target/castle/aot/guest.wasm 20000 --differential
```

The compiler discovers startup, exported entries and internal code-pointer
relocations, then emits protected-mode 16-bit Sleigh operations into `guest.c`.
Clang and the local Wasm linker produce `guest.wasm`; `CLANG` and `WLAM_WASM_LD`
override those tools. Castle1 currently produces 73,459 discovered instruction
sites and a roughly 9.2 MB artifact. `unsupported.txt` records 48 explicit trap
sites (DOS interrupt paths and a breakpoint). Missing code never falls back to
an interpreter.

The runner imports checked segmented memory operations and fault reporting.
The task layer services Win16 imports using Pascal arguments, word handles,
far pointers and far returns. `--differential` compares every compiled
instruction's full register file and segmented memory against an independent
snapshot executed by the checked CPU bridge. Without that flag, guest
instructions execute only through the compiled artifact. `WLAM_NE_TRACE=1`
adds raw API stack arguments to diagnostics.

Verified Castle1 progress: task and CRT startup, DOS vectors, local allocations,
bitmap/icon loading, pattern brushes, display/memory DCs, compatible bitmaps,
fonts, menus, supplied INI settings and all 13 window-class registrations.
The runner has matched over 156,000 compiled instructions, including
NCCREATE/CREATE callbacks, DOS date/time, a converted dialog resource, its
initialization callback, dialog control text, MoveWindow, input wait and
Enter-driven EndDialog. GlobalCompact, main-window display, child button creation/subclassing and
BeginPaint now run as well. GetObject converts BITMAP16 and raster blits use
the shared GDI renderer. Current execution diagnostics remain authoritative
for the next missing API or instruction. Cooperative waits preserve the suspended Pascal call; polling
does not consume its return frame. Unsupported APIs still stop
explicitly. Logs are in `target/castle/compiled-runtime.log`.

Named-resource lookup now ignores case. Castle requests `Castle`, while its NE
menu resource is named `CASTLE`. Exact matching caused the game's misleading
"Not enough memory to initialize Castle" message. The corrected lookup gets
past that error without dismissing it.

## Other diagnostics

```sh
cargo run --offline -p wlam-ne --example run_ne -- target/castle/input/CASTLE1.EXE
cargo run --offline -p wlam-ne --example trace_ne -- target/castle/input/CASTLE1.EXE
cargo run --offline -p wlam-ne --example dump_16_pcode -- \
  target/castle/input/CASTLE1.EXE 01100db3
```

`run_ne` uses the checked CPU with the same task runtime. `trace_ne` stops at
the first imported API without servicing it. The dump accepts a packed
hexadecimal selector:offset. Both runners register neighboring input files in
the task's virtual filesystem. INI updates remain task-owned bytes, exposed
through `file_data`. Message boxes yield an event and require a real button
response before `answer_message_box` resumes the guest.

| Image | Segments | Resources | Distinct imported exports |
| --- | ---: | ---: | ---: |
| CASTLE1.EXE | 32 | 894 | 187 |
| CASTLE2.EXE | 28 | 1090 | 187 |

Castle2's NE loading and first API boundary were checked; its AOT startup has
not yet been verified. Castle1's resources were individually loaded, locked,
compared with the source, unlocked and freed using `inspect_ne`.

## Correctness work driven by Castle

- Segment userop results receive the same temporary namespace remapping as
  their consumers. Previously the first memory store used an undefined pointer.
- Indirect far JMP loads both destination words. RET/RETF with argument cleanup
  and indirect near transfers retain the appropriate selector.
- REP instruction-start/next targets are distinguished from internal P-code
  labels, retaining the selector across IP wrap.
- Heap tests cover fixed/movable handles, locks, zero initialization, reuse and
  preservation of neighboring allocations.
- Bitmap conversion covers packed 1/4/8-bit pixels, BGR pixels and RLE4/RLE8
  bounds/padding. Pattern brushes own pixel snapshots and preserve device phase
  under clipping.
- Menus, TEXTMETRIC, LOGFONT and WNDCLASS use explicit Win16 layout conversion.
- INI sections/keys compare without case and support task-owned updates.

## Remaining path to playability

Complete messages, dialog continuations, menus and input from
execution evidence. `CreateWindow` now uses real Pascal callbacks and explicit
CREATESTRUCT conversion; classic dialog templates convert into the shared host
layout with checked bounds and control alignment. Connect the NE artifact/task ABI to browser presentation. Verify
character creation, movement, interactions/combat and save/load with the original
binary. Continue checking discovery, traps and code/selector lifetimes as later
paths become reachable.

References:
[Wine task contracts](https://github.com/wine-mirror/wine/blob/master/dlls/krnl386.exe16/task.c),
[Win16 USER layouts](https://github.com/wine-mirror/wine/blob/master/include/wine/winuser16.h),
[NE resource lookup](https://github.com/wine-mirror/wine/blob/master/dlls/krnl386.exe16/resource.c),
[Microsoft bitmap compression](https://learn.microsoft.com/en-us/windows/win32/gdi/bitmap-compression),
[Microsoft menu template history](https://devblogs.microsoft.com/oldnewthing/20080709-00/?p=21693).

## Capture and input probe

```sh
cargo run --offline -p wlam-ne --example run_compiled_ne -- \
  target/castle/input/CASTLE1.EXE target/castle/aot/guest.wasm 1000000 \
  --differential --capture=target/castle/frames --key=13
```

At each input wait, `--capture` writes window surfaces as PPM files. Each
`--key=<virtual-key>` supplies one key press/release to the most recently
mapped visible top-level window at a wait. With no queued keys, the diagnostic
runner reports the wait and exits successfully. The task itself remains
suspended and can resume when its host receives input. These probes exercise
the original dialog procedure, including EndDialog and FreeProcInstance.

The checked CPU arithmetic-shift reference now uses the data operand width,
independent of the count width. Regression cases cover SAR AX and SAR AL,
including preservation of unaffected register bytes.

The splash reveal is a bounded 2,898-blit loop in the original executable.
A 100,000-step budget stops during that effect; use a million steps for later
startup paths. Initial WM_SIZE delivery is now part of the Win16 CreateWindow
continuation so layout initialization precedes subsequent child creation.

Latest full differential run: 160,773 compiled instructions matched, including
message retrieval, translation and dispatch. Startup now reaches a normal
GetMessage input wait after creating its child windows and initializing menus.
The combined capture is `target/castle/frames/main-window.png`; layout and
presentation remain incomplete. Character creation, gameplay, save/load and
browser interaction still require implementation and verification.

The task now serves GetMessage/PeekMessage, TranslateMessage, DispatchMessage
and SendMessage/PostMessage with explicit 18-byte MSG16 conversion. Message
waits keep the Pascal frame suspended; WM_COMMAND translates its control
handle and notification code in both directions. PeekMessage with no result
leaves the guest buffer untouched. Temporary MSG storage survives callbacks.
Scrollbar ranges/positions and menu enable/check flags retain state. Frame
capture includes visible child surfaces and also runs on API failures.

## New Game probe

The File menu's New Game command is 117. Inputs execute in argument order:

```sh
cargo run --offline -p wlam-ne --example run_compiled_ne -- \
  target/castle/input/CASTLE1.EXE target/castle/aot/guest.wasm 1000000 \
  --capture=target/castle/frames --key=13 --command=117 \
  --text=151:Hero --key=13 --answer=6
```

The command probe validates the loaded menu item and enabled state, following
owned popups to their menu owner. `--text` updates a dialog control through the
shared USER service. `--answer` supplies an explicit, validated message-box
button only when a box is actually pending. It never dismisses other boxes.
Castle's character dialog initializes its custom statbar controls, scrollbars,
radio/check buttons and real icon portraits, then reaches an input wait. The
character-dialog capture is `target/castle/frames/character-dialog.png`.

Character creation now reaches its starter-spell chooser. Win16 list-box and
button messages translate to their Win32 control counterparts, while custom
classes retain their private WM_USER numbering. The chooser displays actual
spell names and the default selection; append `--key=13` to the New Game probe
to confirm it. That path currently stops explicitly at USER!#159 (GetSubMenu),
which is needed for the dynamic spell menus. Character creation/gameplay are
not yet verified complete.

New correctness checks cover host-origin CREATESTRUCT strings in guest
selectors, dialog BOOL return width, custom dialog creation callbacks and
class extra words, classic icon XOR/AND decoding, partial repaint clipping,
caption callbacks, local heap resizing/relocation/locks/discard and mutable
menu labels/removal.

The full New Game probe with the starter spell confirmed has been run with
`--differential`: over 174,600 compiled instructions matched the independent
CPU/segmented-memory reference before the explicit GetSubMenu stop.
