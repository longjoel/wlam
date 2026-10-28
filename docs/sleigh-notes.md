# Notes on lifting x86 with Ghidra Sleigh (via rsleigh)

These are the things we had to establish empirically. Each was verified by
decoding real bytes, not inferred from documentation, and each has a test.

## Branch targets are RAM-space varnodes

Sleigh emits `jmp rel8` as a varnode in the **RAM** address space holding the
already-computed absolute address — not the `Const` space a naive reader
expects. All five branch shapes share this:

```
jmp +5     at 0x1000  ->  Branch   { dest: Varnode { space: Ram,   offset: 0x1007 } }
jnz +5     at 0x1000  ->  CBranch  { dest: Varnode { space: Ram,   offset: 0x1007 } }
call foo              ->  Call     { dest: Varnode { space: Ram,   offset: <target> } }
call [IAT]            ->  CallInd  { dest: Varnode { space: Unique, offset: <temp> } }
ret                   ->  Return   { dest: Varnode { space: <loaded return addr> } }
```

Consequence: a branch destination must be read from `Ram`/`Const` alike, and
only a **non-constant** destination is a genuine indirect jump. Treating RAM as
indirect turns every `jmp` into an unresolved jump table.

## `Load`/`Store` operands are addresses, not pointers

`mov eax, [esp+4]` lowers to two ops:

```
IntAdd { out: Unique, left: Const(4), right: ESP }
Load   { out: EAX, ptr: Unique }        # the effective address, already computed
```

`call [IAT]` shows the full five-stage lowering including the pushed return
address:

```
Load    { out: Unique, ptr: Const(0x41106c) }
IntSub  { out: ESP, left: ESP, right: Const(4) }
Store   { ptr: ESP, val: Const(<return address>) }
CallInd { dest: Unique }
```

## `fs:[0]` is a segment-base add

`mov eax, fs:[0]` (the MSVC SEH prologue) lowers to
`IntAdd(FS_OFFSET, 0)` then a load. `FS_OFFSET` is a register Ghidra **never
assigns**. Setting it to the Win32 TIB base (`0x7ffde000`) is the entire
SEH/TLS story — no Sleigh patching required.

## User-defined pcodeops carry no names

`define pcodeop` declarations become `CallOther { func_id }` with only a
numeric id; the generated code contains no name table. We recovered the
mapping by decoding the real executable and reading the mnemonics that produce
each id:

| func_id | Userop | Mnemonics seen in Uxn32 |
|---|---|---|
| 1 | `PortIn` | `IN` ×12, `INSB` ×3, `INSD` ×1 |
| 2 | `PortOut` | `OUT` ×2 |
| 16 | `Breakpoint` | `INT3` ×106 |
| 17, 18 | `LockedXchg` | `XCHG` ×1 each |
| 72 | `Rdtsc` | (not reached in Uxn32) |

The decoded executable reaches **only these five**. Any other id appearing in
new code is reported as `Userop::Unknown`, never ignored.

## The decoder needs a big stack

In a debug build the vendored rsleigh decoder overflows the default 2 MiB test
thread stack on a *single* instruction (`call [IAT]`). Measured: fails at 2 MiB,
passes at 8 MiB. `wlam_sleigh::with_big_stack` runs lifting on a 16 MiB thread
and re-raises panics. This is a property of the dependency, so it is documented
and tested rather than hidden behind a build-profile change.

## Reference numbers (Uxn32.exe, MSVC-built, 0x1000-byte .text prefix)

- Instructions decoded across all executable sections: **23,150**, with 5
  undecodable bytes (resync rate < 0.03%).
- Distinct registers referenced: **60**, all name-resolvable.
- P-code ops per native instruction (**P-U / P-T**): **4.0** (77,059 ops for
  19,093 instructions). A low, linear ratio is what makes emitting one wasm
  function per basic block legitimate instead of trace interpretation.
