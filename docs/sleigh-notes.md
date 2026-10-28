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

## Indirect jumps: what arrives, and what still blocks recovery

`Operand`/`IR` classify Sleigh varnodes. **A computed jump destination arrives in
the *register* space**, not as a `Ram` varnode:

```
jmp [ebx*4 + 0x406054]  ->  Mul(EBX, 4) into temp A
                            Add(0x406054, A) into temp B      # B is Reg-space
                            IndirectJump(target = B)
```

With `Operand::Reg { name: "t<hex>" }` this follows directly. A destination that
really is in `Ram` means the encoding baked in a fixed slot address — an import
thunk (`jmp [0x411124]`), which has no guest targets.

### Open problem: switch table length

Recovery of the *bound* works: `cmp <reg>, imm` encodings (`3C ib`, `80 /7 ib`,
`3D id`, `83 /7 ib`) are matched in a byte window before the site and yield the
right bound at every switch in Uxn32 (15, 16, 17, 20, ...). Two traps found on
the way, both of which cost real debugging time:

- **Never decode forward from `site - N`.** x86 is variable-length; at
  `0x405ebc` a 96-byte window decoded into garbage (starting mid-instruction).
- **Do not look for the comparison in P-code.** The bound `cmp eax, 0x1f` at
  `0x405eb9` compiles to `t = LessU(...)` **without a consumer**; reading the
  *encoding* is what works.

What is still unresolved is linking the bound to the table start when both do
not appear in the jump's own block, and validating table entries whose
alignment is not 4. Until that lands, discovery reports every indirect site in
`unresolved_indirect()` — never silently guessed — and the emitter will refuse
those blocks rather than miscompile them.

## Jump-table recovery: what worked, and one bug worth remembering

### The bug: virtual address passed where an RVA was required

`Pe::read_rva` takes an **RVA**. The table start computed from the jump's
`Add` operand is a **virtual address**. Passing the VA made every entry read
return `None`, so recovery silently produced zero targets.

It survived a full debugging round because every probe I wrote used RVAs
(`0x2938`) while the code used VAs (`0x402938`), and *both* "worked" — the
probe honestly, the code by finding nothing. The lesson: when a primitive is
verified in isolation but fails in situ, check the **units of the argument**,
not the logic around it.

### Verified facts now pinned by tests

| site | table | entries |
|---|---|---|
| `0x40172a` | `0x402938` | 7 (`401739 4019c2 … 402913`) |
| `0x40222f` | `0x402984` | 8 |
| `0x405ebc` | `0x406054` | 11 |

Each table is immediately followed by data (`0x6060100`, `0x7070700`, or
machine code), so these counts are the real ends, not guesses. MSVC places the
tables **inside `.text`** (`.text` raw size `0x10000`; `.rdata` starts at VA
`0x11000`), so a "tables live in `.rdata`" rule is wrong for this toolchain.

Recovery currently resolves **16 of 18** indirect sites in Uxn32 (55 targets,
block count 1,639 → 1,687).

### The 2 unrefined cases (plus 6 beyond the scope of the rule)

| site | shape | why unresolved |
|---|---|---|
| `0x4016c9` | `jmp eax` | register destination; no static table |
| `0x40ddcb`, `0x40dde0` | `jmp [0x411124]` | IAT thunk slots hold `0x125da`/`0x125e4`, not code |
| `0x40e2e8`, `0x40ff08`, `0x410018`, … | `jmp [reg*4 + 0x40e300]` | the `Add` addend is **not** the table: entries there decode as instructions (`0x900040e3`, `0x5e08458b`), i.e. the guessed address lands in code. The real table is elsewhere. |

The last group is the one tempting shortcut: "the addend is the table". It is
not, at least not for MSVC's `jmp [reg*4 + X]` form with these functions. A
correct rule will need to locate the table from the *load* that feeds the jump
rather than from the jump's address arithmetic alone.
