# Sega Saturn Hardware Reference — SH-2 CPU Cores (Master & Slave)

> **Provenance.** Everything in this document is derived **exclusively** from reading the
> following Yabause / YabaSanshiro C sources:
>
> - `yabause/src/sh2int.c` — the SH-2 interpreter (decode, dispatch, per-instruction semantics)
> - `yabause/src/sh2int.h` — instruction field macros, interpreter interface
> - `yabause/src/sh2core.c` — shared CPU state, on-chip peripheral registers, DMAC, FRT, WDT, DIVU
> - `yabause/src/sh2core.h` — struct layouts for CPU registers and on-chip registers
> - `yabause/src/sh2cache.c` / `sh2cache.h` — cache emulation
> - `yabause/src/sh2d.c` / `sh2d.h` — disassembler (used as an independent cross-check on
>   mnemonics and operand encoding)
>
> Two small excursions outside that set are explicitly marked where they were unavoidable to
> explain a value that appears inside the SH-2 code: `yabause/src/memory.c` (the `cycle`
> out-parameter of `MappedMemoryRead*/Write*`, and the address-space decode that turns an
> on-chip access into the register offsets used below) and `yabause/src/CMakeLists.txt` (the
> `CACHE_ENABLE` build option). Those are cited as such.
>
> **No SH-2 manual, no external Saturn documentation, and no general CPU-architecture
> knowledge has been used to fill gaps.** Where the code is ambiguous, contradictory, buggy, or
> where the hardware rationale is not deducible from the code, this document says so explicitly
> rather than inventing an explanation. Search this file for the word **DEVIATION** or
> **UNCLEAR** to find every such point.
>
> Both SH-2s (master `MSH2` and slave `SSH2`) are the *same* emulated core: one
> `SH2_struct` each, one shared `SH2Interface_struct` implementation. Everything here applies
> identically to both unless a difference is called out (there are exactly three:
> the `BCR1` MASTER bit, the input-capture write hooks, and one DIVU interrupt-level quirk).

---

## 1. Conventions used in this document

### 1.1 Instruction-field naming

`sh2int.h:46-51` defines the nibble accessors used by every handler. This document uses the
same letters:

| Macro | Expression | Nibble | This doc calls it |
|---|---|---|---|
| `INSTRUCTION_A(x)` | `(x & 0xF000) >> 12` | 15–12 | **A** (opcode group) |
| `INSTRUCTION_B(x)` | `(x & 0x0F00) >> 8` | 11–8 | **B** |
| `INSTRUCTION_C(x)` | `(x & 0x00F0) >> 4` | 7–4 | **C** |
| `INSTRUCTION_D(x)` | `(x & 0x000F)` | 3–0 | **D** |
| `INSTRUCTION_CD(x)` | `(x & 0x00FF)` | 7–0 | **CD** (8-bit imm/disp) |
| `INSTRUCTION_BCD(x)` | `(x & 0x0FFF)` | 11–0 | **BCD** (12-bit disp) |

Encodings are given as 16 binary digits (`nnnn` = destination register index, `mmmm` = source
register index, `dddd`/`dddddddd` = displacement, `iiiiiiii` = immediate) **as the interpreter
actually uses them**. Note that `n` is not always in nibble B — for the `0x8...` group the
register operand is in nibble **C**, and the doc states which nibble is used for each
instruction. This was cross-checked against `sh2d.c`'s tables (`sh2d.c:190-333`), which agree.

### 1.2 Cycle notation

Every handler ends by adding to `sh->cycles` (`SH2_struct.cycles`, `sh2core.h:413`). The
"Cycles" column reproduces the *literal* expression in the code. Terms named `rcycle`,
`wcycle`, `cycle` are the out-parameter of the memory accessors:

`MappedMemoryReadByte/Word/Long(addr, u32 *cycle)` and `MappedMemoryWriteByte/Word/Long(addr,
val, u32 *cycle)` write a region-dependent wait-state penalty into `*cycle`
(`yabause/src/memory.c:769-796`, macros `GET_MEM_CYCLE_R` / `GET_MEM_CYCLE_W`):

| Region (`addr & 0xDFF00000`) | Read penalty | Write penalty |
|---|---|---|
| `0x00000000` BIOS ROM, `0x00100000` backup | 16 | 0 |
| `0x00200000` Low Work RAM | 12 | 7 |
| `0x02000000` CS0, `0x05800000` CS2 | 24 | 0 |
| `0x05A00000` Sound RAM, `0x05B00000` Sound regs | 50 | 7 (sound only) |
| `0x05C00000` VDP1 RAM | 50 | 2 |
| `0x05E00000` VDP2 RAM | `getVramCycle(addr)` | `getVramCycle(addr)` |
| `0x06000000` High Work RAM | 0 | 2 |
| anything else | 0 | 0 |

Passing `NULL` for `cycle` skips the computation. Several handlers compute a penalty and then
**forget to add it** — each such case is flagged as a DEVIATION in the tables.

### 1.3 On-chip register offsets

The address decode in `MappedMemoryRead*/Write*` masks an on-chip access with `addr & 0x1FF`
before calling `OnchipRead*/OnchipWrite*` (`yabause/src/memory.c:1012-1017` and siblings), and
the region is `addr >= 0xFFFFFE00`. So the `case` labels inside `OnchipReadByte`
(`sh2core.c:1133`), `OnchipReadWord` (`:1244`), `OnchipReadLong` (`:1299`),
`OnchipWriteByte` (`:1377`), `OnchipWriteWord` (`:1559`), `OnchipWriteLong` (`:1682`) are
offsets from **`0xFFFFFE00`**. This document gives both: `offset 0x011 → 0xFFFFFE11`. The
addresses in the `Onchip_struct` comments (`sh2core.h:132-313`) confirm the same mapping.

---

## 2. Register file

`sh2regs_struct`, `sh2core.h:92-130`.

| Register | C field | Width | Role per the code |
|---|---|---|---|
| `R0`–`R15` | `u32 R[16]` | 32 | General purpose. `R0` is the implicit operand of the GBR-relative, `@(R0,Rn)`, `@(disp,Rn)` byte/word, and all `#imm` ALU forms. `R15` is the hardware stack pointer: it is the register that exception/interrupt entry pushes to and `RTE` pops from (`sh2int.c:186-189`, `:2073`). Nothing else in the interpreter treats `R15` specially. |
| `SR` | `union { struct {...} part; u32 all; }` | 32 | Status register; see §3. |
| `GBR` | `u32` | 32 | Global base register. Used as the base for the `@(disp,GBR)` and `@(R0,GBR)` forms. Loaded/stored by `LDC`/`STC` (`sh2int.c:1063`, `:2242`). |
| `VBR` | `u32` | 32 | Vector base register. Every exception/interrupt entry fetches its new PC from `MappedMemoryReadLong(VBR + (vector << 2))` (`sh2int.c:197`, `:352`, `:2516`, `:3025`). |
| `MACH` | `u32` | 32 | High half of the 64-bit multiply/accumulate result. |
| `MACL` | `u32` | 32 | Low half. Also the sole destination of `MUL.L`, `MULS.W`, `MULU.W`. |
| `PR` | `u32` | 32 | Procedure register (link register). Written by `BSR`, `BSRF`, `JSR` as `PC_of_branch + 4`. Read by `RTS`. |
| `PC` | `u32` | 32 | Program counter. **Critically: throughout the interpreter, `PC` holds the address of the instruction *currently executing*, not a pipeline-advanced value.** Every non-branch handler ends with `PC += 2`. PC-relative formulas therefore all contain an explicit `+4` (see §9.2 and §8). |

There is no separate `SP` field; there are no banked registers, no floating-point registers,
no `SSR`/`SPC`, and no user/privileged distinction anywhere in the code.

### 2.1 Other per-CPU state (`SH2_struct`, `sh2core.h:389-453`)

| Field | Purpose per the code |
|---|---|
| `onchip` | `Onchip_struct` — all on-chip peripheral registers (§11). |
| `frc.leftover`, `frc.shift` | Free-running-timer prescaler accumulator and log2 divisor (§11.3). |
| `wdt.isenable`, `wdt.isinterval`, `wdt.leftover`, `wdt.shift` | Watchdog state (§11.5). |
| `interrupts[50]`, `NumberOfInterrupts` | Pending-interrupt queue (§10). `MAX_INTERRUPTS` is 50 (`sh2core.h:53`). |
| `AddressArray[0x100]`, `DataArray[0x1000]` | Backing store for the cache address/data arrays **when cache emulation is compiled out** (§12.6). |
| `delay` | Reset to 0, saved/loaded in save states — **never read or written by the interpreter**. Vestigial. |
| `cycles` | Monotonically increasing cycle counter; the exec loop compares it against a target. |
| `pre_cycle` | Cycle overshoot carried from one `Exec` call into the next (§7). |
| `isslave` | 0 for MSH2, 1 for SSH2 (`sh2core.c:85`, `:121`). |
| `isIdle`, `isSleeping` | Cleared by interrupt delivery (`sh2int.c:200-201`). `isIdle` is otherwise only touched by the idle-loop detector, which is compiled out when `EXEC_FROM_CACHE` is defined (`sh2int.c:3156-3161`). **`isSleeping` is never set anywhere in these files** — `SLEEP` does not set it (§9.7). |
| `instruction` | The 16-bit opcode currently being executed. |
| `cache` | `cache_enty` (inside `onchip`), §12. |
| `dma_ch0`, `dma_ch1` | `Dmac` pointer bundles into the on-chip DMAC registers (`sh2core.h:378-386`, wired in `sh2core.c:87-115`). |
| `depth` | Re-entrancy guard used only by `SSH2InputCaptureWriteWord` (`sh2core.c:2614`). |
| `bp`, `bt`, `stepOverOut`, `trackInfLoop` | Debugger facilities, active only in the debug interpreter. |

---

## 3. Status register (SR)

Bitfield layout, `sh2core.h:109-122` (little-endian variant; the big-endian variant at
`:96-108` declares the identical bits in reverse declaration order, so the *value* layout is
the same):

```
 31                                   10  9  8  7  6  5  4  3  2  1  0
+---------------------------------------+--+--+-----------+-----+--+--+
|            reserved1 (22 bits)        | M| Q|  I3..I0   |rsv0 | S| T|
+---------------------------------------+--+--+-----------+-----+--+--+
```

| Bit(s) | Name | Value mask | Meaning as implemented |
|---|---|---|---|
| 0 | **T** | `0x00000001` | Condition/carry/borrow/test bit. See table below. |
| 1 | **S** | `0x00000002` | Saturation-mode enable for `MAC.L` and `MAC.W` only. Read at `sh2int.c:1213` and `:1329`. **No instruction in the dispatch table sets or clears S** — there is no `SETS`/`CLRS`; the only way to change it is `LDC …,SR` / `LDC.L @Rm+,SR` / `RTE`. |
| 3–2 | reserved0 | `0x0000000C` | Always forced to 0 by the `0x000003F3` mask applied on every SR load (§3.2). |
| 7–4 | **I3–I0** | `0x000000F0` | Interrupt mask level, 0–15. An interrupt is taken only if `pending.level > SR.part.I` (`sh2int.c:182`). Reset value is `0xF` (all masked) via `SetSR(0x000000F0)` (`sh2core.c:183`). |
| 8 | **Q** | `0x00000100` | Division quotient/sign state bit, used only by `DIV0S`, `DIV0U`, `DIV1`. |
| 9 | **M** | `0x00000200` | Division divisor-sign bit, used only by `DIV0S`, `DIV0U`, `DIV1`. |
| 31–10 | reserved1 | `0xFFFFFC00` | Forced to 0 by the SR load mask. |

### 3.1 Complete list of what touches each flag

**T is written by** (all in `sh2int.c`):

| Instruction | Rule |
|---|---|
| `CLRT` (`:619`) | `T = 0` |
| `SETT` (`:2102`) | `T = 1` |
| `ADDC` (`:379`) | `tmp1 = Rm+Rn; T = (Rn_old > tmp1); Rn = tmp1 + T_old; if (tmp1 > Rn) T = 1` — carry out of the two-step add |
| `ADDV` (`:401`) | Signed overflow of `Rn += Rm`, computed via the sign-count method: `src = sign(Rn)+sign(Rm)`; `ans = sign(result)+sign(Rn_old)`; `T = (src==0 \|\| src==2) && (ans==1)` |
| `SUBC` (`:2385`) | Borrow out of the two-step subtract, symmetric to `ADDC` |
| `SUBV` (`:2409`) | Signed overflow of `Rn -= Rm`: `T = (src==1) && (ans==1)` |
| `NEGC` (`:1902`) | `temp = 0-Rm; Rn = temp - T_old; T = (0 < temp); if (temp < Rn) T = 1` |
| `CMP/EQ Rm,Rn` (`:628`) | `T = (Rn == Rm)` unsigned equality |
| `CMP/GE` (`:640`) | `T = ((s32)Rn >= (s32)Rm)` |
| `CMP/GT` (`:653`) | `T = ((s32)Rn > (s32)Rm)` |
| `CMP/HI` (`:665`) | `T = ((u32)Rn > (u32)Rm)` |
| `CMP/HS` (`:678`) | `T = ((u32)Rn >= (u32)Rm)` |
| `CMP/EQ #imm,R0` (`:691`) | `T = (R0 == (u32)(s32)(s8)imm)` — the immediate is sign-extended to 32 bits, then compared as unsigned |
| `CMP/PL` (`:708`) | `T = ((s32)Rn > 0)` |
| `CMP/PZ` (`:720`) | `T = ((s32)Rn >= 0)` |
| `CMP/STR` (`:732`) | `temp = Rn ^ Rm`; `T = 1` iff **any** of the four bytes of `temp` is zero (coded as `HH = HH && HL && LH && LL; T = (HH == 0)`) |
| `TST Rm,Rn` (`:2525`) | `T = ((Rn & Rm) == 0)` |
| `TST #imm,R0` (`:2541`) | `T = ((R0 & imm) == 0)`, `imm` zero-extended |
| `TST.B #imm,@(R0,GBR)` (`:2559`) | `T = ((mem8 & imm) == 0)` |
| `TAS.B @Rn` (`:2481`) | `T = (mem8 == 0)` (tested **before** the `\|= 0x80` write-back) |
| `DT Rn` (`:973`) | `Rn--; T = (Rn == 0)` |
| `DIV0S` (`:754`) | `Q = sign(Rn); M = sign(Rm); T = !(M == Q)` |
| `DIV0U` (`:773`) | `M = Q = T = 0` |
| `DIV1` (`:782`) | `T = (Q == M)` after the step (§9.4) |
| `ROTL` (`:2029`) | `T = bit31(Rn)` (before shift), then bit0 of the result is set from T |
| `ROTR` (`:2051`) | `T = bit0(Rn)`, then bit31 of the result is set from T |
| `ROTCL` (`:1973`) | old bit31 → new T; old T → new bit0 |
| `ROTCR` (`:2001`) | old bit0 → new T; old T → new bit31 |
| `SHAL` (`:2111`) | `T = bit31(Rn)`; `Rn <<= 1` |
| `SHAR` (`:2125`) | `T = bit0(Rn)`; logical `>>1` then sign bit manually restored |
| `SHLL` (`:2153`) | `T = bit31(Rn)`; `Rn <<= 1` |
| `SHLR` (`:2196`) | `T = bit0(Rn)`; `Rn >>= 1` (logical, `Rn` is `u32`) |
| `LDC …,SR`, `LDC.L @Rm+,SR`, `RTE` | Whole SR replaced (masked, §3.2) |

**T is read by:** `ADDC`, `SUBC`, `NEGC`, `ROTCL`, `ROTCR`, `DIV1`, `MOVT`, `BT`, `BT/S`,
`BF`, `BF/S`.

`SHLL2/8/16` and `SHLR2/8/16` (`sh2int.c:2169-2238`) **do not touch T at all** — they are plain
`<<=`/`>>=` on the `u32` register. `SHLR2/8/16` are therefore logical (zero-filling) shifts.

**Q and M** are written only by `DIV0S`, `DIV0U`, `DIV1`, and whole-SR loads. **S** is written
only by whole-SR loads. **I3–I0** are written by whole-SR loads and by interrupt entry
(§10.3).

### 3.2 The `0x000003F3` SR load mask

Three sites replace SR wholesale, and all three apply the same mask:

```c
regs.SR.all = <value> & 0x000003F3;   // sh2int.c:1090 (LDC.L @Rm+,SR)
                                      // sh2int.c:1113 (LDC Rm,SR)
                                      // sh2int.c:2081 (RTE)
```

`0x3F3` = `M \| Q \| I3..I0 \| S \| T`. It clears reserved bits 3–2 and 31–10. Note that
`STC SR,Rn` / `STC.L SR,@-Rn` / interrupt-entry push all store `SR.all` **unmasked** — but
since every write path masks, the stored value can only ever contain those bits anyway.

`SH2InterpreterSetSR` (`sh2int.c:3259`), used by the debugger and save-state loader,
does **not** mask.

### 3.3 `LDC Rm,SR` re-checks interrupts immediately

`SH2ldcsr` (`sh2int.c:1111-1117`) is the only opcode handler that calls
`SH2HandleInterrupts(sh)` — right after writing SR and advancing PC. So lowering the I mask
with `LDC Rm,SR` delivers a pending higher-priority interrupt at once, without waiting for the
next `Exec` entry. `LDC.L @Rm+,SR` and `RTE` do **not** do this.

---

## 4. Reset and power-on state

`SH2Reset` (`sh2core.c:173-219`):

```
for (i = 0; i < 15; i++) SetGPR(i, 0)   // NOTE: R15 is NOT reset — the loop stops at 14
SR   = 0x000000F0    // I = 15, everything else 0
GBR  = 0
VBR  = 0
MACH = 0
MACL = 0
PR   = 0
delay = 0; cycles = 0; isIdle = 0
frc.leftover = 0;  frc.shift = 3            // FRT divide-by-8
wdt.isenable = 0;  wdt.isinterval = 1;  wdt.shift = 1;  wdt.leftover = 0
interrupts[] zeroed, NumberOfInterrupts = 0
OnchipReset(context)                        // §11.1
CurrentSH2 = context
cache_clear(&onchip.cache)
bt.numbacktrace = 0
```

`SH2Reset` **does not set PC**. PC is set separately by `SH2PowerOn` (`sh2core.c:223-227`):

```
PC  = MappedMemoryReadLong(VBR + 0)
R15 = MappedMemoryReadLong(VBR + 4)
```

Since `VBR` is 0 after reset, this reads the reset vector pair from physical `0x00000000` and
`0x00000004` — i.e. the first two longwords of the BIOS ROM.

`MSH2->onchip.BCR1 = 0x0000` and `SSH2->onchip.BCR1 = 0x8000` are set once at
`SH2Init` (`sh2core.c:84` and `:120`), and `OnchipReset` preserves bit 15 (`sh2core.c:1121`),
so bit 15 of BCR1 is the permanent MASTER/SLAVE identity bit.

---

## 5. Instruction fetch

### 5.1 The fetch table

`fetchlist[0x100]` (`sh2int.h:88`, `sh2int.c:174`) is indexed by **`(PC >> 20) & 0xFF`**, built
in `SH2InterpreterInit` (`sh2int.c:2934-2971`):

| Index | Region | Handler | Behaviour |
|---|---|---|---|
| `0x000` | BIOS ROM | `FetchBios` (`:209`) | With `CACHE_ENABLE`: `cache_memory_read_w(&cache, addr)`. Without: `T2ReadWord(BiosRom, addr & 0x7FFFF)`. Additionally, when `yabsys.extend_backup` is set it returns 0 for the BUP trampoline address `0x0007D600` and for `0x0380..0x03A8`, so those fetch as `0x0000` and fall into `SH2undecoded`'s HLE path. |
| `0x002` | Low Work RAM | `FetchLWram` (`:243`) | cache read, else `T2ReadWord(LowWram, addr & 0xFFFFF)` |
| `0x020` | CS0 | `FetchCs0` (`:232`) | cache read, else `CartridgeArea->Cs0ReadWord(addr)` |
| `0x05C` | VDP1 VRAM | `FetchVram` (`:265`) | `T1ReadWord(Vdp1Ram, addr & 0x07FFFF)`. The comment says `// Fighting Viper` — i.e. this entry exists because at least one game executes code out of VDP1 RAM. Never cached. |
| `0x060`–`0x06F` | High Work RAM | `FetchHWram` (`:255`) | cache read, else `T2ReadWord(HighWram, addr & 0xFFFFF)` |
| everything else | — | `FetchInvalid` (`:274`) | returns `0xFFFF` |

`0xFFFF` decodes (A=15) to `SH2undecoded` (§9.9), so executing from an unmapped region raises
the illegal-instruction exception.

Because the index is `(PC >> 20) & 0xFF`, the **top 4 bits of PC are ignored for fetch**. That
is what makes the cached (`0x0…`), uncached (`0x2…`), and other mirrors of an address all
fetch the same code.

### 5.2 `EXEC_FROM_CACHE` — executing from the cache data array

`sh2int.c:55` does `#define EXEC_FROM_CACHE` **unconditionally**, so this path is always
compiled in regardless of the CMake `YAB_WANT_SH2_CACHE` option:

```c
if ((PC & 0xC0000000) == 0xC0000000) instruction = DataArrayReadWord(PC);
else                                 instruction = fetchlist[(PC >> 20) & 0x0FF](PC);
```

(`sh2int.c:3167-3171` for the fast interpreter, `:3117-3121` for the debug interpreter,
`:291-295` inside `SH2delay`.)

The test `(PC & 0xC0000000) == 0xC0000000` matches address regions `0xC…` and `0xE…`. That is
the cache **data array** region (§12.5) plus the on-chip-IO region. `DataArrayReadWord` then
either indexes the emulated cache ways or `CurrentSH2->DataArray[addr & 0xFFF]`
(`sh2core.c:1973-1981`).

`EXEC_FROM_CACHE` also disables the idle-loop detector: `SH2idleParse`/`SH2idleCheck` are
inside `#ifndef EXEC_FROM_CACHE` (`sh2int.c:3156-3161`) and are therefore dead in the shipped
configuration.

### 5.3 Dispatch

`opcodes[0x10000]` (`sh2int.h:91`, `sh2int.c:90`) is a flat table of 65536 function pointers,
filled at init by calling `decode(i)` for every possible 16-bit value (`sh2int.c:2931-2932`).
Dispatch is a single indirect call: `opcodes[context->instruction](context)`.

**Consequence for a re-implementation:** `decode()` (`sh2int.c:2639-2922`) is a nested switch
that, for many encodings, *never inspects some nibbles*. Those nibbles are don't-cares. For
example `CLRT` is decoded from A=0, D=8, C=0 with **nibble B unexamined**, so `0x0008`,
`0x0108`, … `0x0F08` all execute `CLRT`. Every such "don't care" is noted in the tables in §9
by simply showing the nibble as `xxxx`.

---

## 6. Address-space decode used by loads/stores

Not part of the SH-2 files themselves, but load/store semantics depend on it. From
`MappedMemoryReadLong` (`yabause/src/memory.c:984-1020`), the top 3 bits of the address select:

| `addr >> 29` | Region | Behaviour |
|---|---|---|
| 0, 1, 4 | cached / uncached / (a further mirror) | normal `ReadLongList[(addr>>16)&0xFFF]` dispatch |
| 2, 5 | associative purge area | **reads return `0xFFFFFFFF`** |
| 3 | cache address array | `AddressArrayReadLong` (§12.4) |
| 6 | cache data array | `DataArrayReadLong` (§12.5) |
| 7 | on-chip | if `addr >= 0xFFFFFE00`: `OnchipRead*(addr & 0x1FF)` |

`sh2cache.c` uses a different, 3-bit region encoding for its own dispatch — see §12.1.

---

## 7. Execution loop and cycle accounting

`SH2Exec` (`sh2core.c:231-245`) is the outer entry point:

```c
CurrentSH2 = context;
SH2Core->Exec(context, cycles);   // -> SH2InterpreterExec
FRTExec(cycles);                  // free-running timer,  §11.3
WDTExec(cycles);                  // watchdog,            §11.5
DMAProc(cycles);                  // on-chip DMAC,        §11.8
```

Note that `FRTExec`, `WDTExec` and `DMAProc` all operate on `CurrentSH2` and are advanced by
the **requested** cycle count, not by the number of cycles actually consumed.

`SH2InterpreterExec` (`sh2int.c:3151-3178`):

```c
int target_cycle = context->cycles + cycles - context->pre_cycle;
SH2HandleInterrupts(context);            // exactly once, before the loop
while (context->cycles < target_cycle) {
    fetch();                             // §5.2
    opcodes[context->instruction](context);
}
context->pre_cycle = context->cycles - target_cycle;   // carry the overshoot forward
```

Key properties:

- `cycles` is a **monotonically increasing** counter, never decremented (the decrementing code
  at `sh2core.c:241-244` is commented out).
- Interrupts are polled **once per `Exec` call**, before the first instruction — plus the
  extra poll inside `LDC Rm,SR` (§3.3). They are never polled between two instructions inside
  the loop.
- `pre_cycle` carries the overshoot of the last instruction into the next slice, so a long
  instruction shortens the following slice.

`SH2DebugInterpreterExec` (`sh2int.c:3054-3146`) is the same loop plus, per instruction:
`SH2HandleBreakpoints`, `SH2HandleBackTrace`, `SH2HandleStepOverOut`, `SH2HandleTrackInfLoop`,
and (under `#ifdef SH2_UBC`, not defined by default) the user-break-controller checks.

`SH2Step` (`sh2core.c:272-286`) executes `SH2Exec(context, 1)` and, if PC did not change, runs
it a second time — because a single "1 cycle" request does not always retire an instruction.

---

## 8. Delay-slot semantics

This is the single most important implementation detail to reproduce exactly.

### 8.1 The mechanism

`SH2delay(SH2_struct *sh, u32 addr)` (`sh2int.c:284-307`):

```c
// Fetch the delay-slot instruction from `addr`
if ((addr & 0xC0000000) == 0xC0000000) sh->instruction = DataArrayReadWord(addr);
else sh->instruction = fetchlist[(addr >> 20) & 0x0FF](addr);

sh->regs.PC -= 2;                    // <-- pre-compensation
opcodes[sh->instruction](sh);        // execute it
```

Every delayed-branch handler follows the same three-step shape:

```c
u32 temp = sh->regs.PC;        // 1. save the address of the BRANCH instruction
sh->regs.PC = <target>;        // 2. commit the branch target FIRST
sh->cycles += 2;
SH2delay(sh, temp + 2);        // 3. THEN run the delay slot, fetched from branch_addr + 2
```

So the ordering is: **compute and commit the branch target → fetch the delay-slot instruction
from `branch_addr + 2` → decrement PC by 2 → execute the delay-slot instruction (whose own
trailing `PC += 2` restores PC to the branch target)**.

The `PC -= 2` / `PC += 2` pair is a trick, not architecture: it exists so that a normal
(non-branch) delay-slot instruction leaves PC exactly at the branch target.

### 8.2 Observable consequences of this implementation

These follow directly from the code and must be reproduced bit-for-bit by any emulator that
wants Yabause-compatible behaviour:

1. **A PC-relative instruction in a delay slot sees `PC = target - 2`, not its own address.**
   `MOV.W @(disp,PC),Rn`, `MOV.L @(disp,PC),Rn` and `MOVA` all read `sh->regs.PC` at execution
   time, which by then is `target - 2`. **UNCLEAR:** whether real silicon behaves this way is
   not deducible from this code; it is simply what the interpreter does.
2. **A branch in a delay slot is not blocked.** It executes normally and computes its own
   target from `PC = target - 2`, then calls `SH2delay` again (recursively). No check exists.
3. **The delay-slot instruction's own cycle cost is added on top** of the branch's `+2`.
4. **No interrupt can be taken between a branch and its delay slot** — `SH2HandleInterrupts`
   is not called there. (Except: if the delay-slot instruction is `LDC Rm,SR`, which polls.)
5. **Breakpoints/backtrace/step-over hooks do not run for delay-slot instructions** — those
   calls live in the exec loop, not in `SH2delay`.
6. `SH2delay` re-uses `sh->instruction`, so after a delayed branch retires, `sh->instruction`
   holds the *delay-slot* opcode, not the branch. This matters for the debug interpreter's
   hooks that read `context->instruction` (`sh2core.c:963`, `:1015`, `:1039`).

### 8.3 Which instructions have delay slots

`BRA`, `BRAF`, `BSR`, `BSRF`, `JMP`, `JSR`, `RTS`, `RTE` (always), and `BT/S`, `BF/S` (only
when the branch is taken). `BT` and `BF` have **no** delay slot.

---

## 9. Complete opcode reference

Every entry below corresponds to an entry in `decode()` (`sh2int.c:2639-2922`). Nothing in the
dispatch table is omitted. Where `decode()` returns `SH2undecoded` for a sub-encoding, that is
stated at the end of the group.

All handlers not listed as branches end with `PC += 2`.

### 9.1 Data transfer — register / immediate

| Mnemonic | Encoding | Handler | Operation (exactly as coded) | Cycles |
|---|---|---|---|---|
| `MOV Rm,Rn` | `0110 nnnn mmmm 0011` | `SH2mov` `:1408` | `Rn = Rm` | 1 |
| `MOV #imm,Rn` | `1110 nnnn iiiiiiii` | `SH2movi` `:1557` | `Rn = (s32)(s8)imm` (sign-extended) | 1 |
| `MOVA @(disp,PC),R0` | `1100 0111 dddddddd` | `SH2mova` `:1417` | `R0 = ((PC + 4) & 0xFFFFFFFC) + (disp << 2)` | 1 |
| `MOVT Rn` | `0000 nnnn 0010 1001` | `SH2movt` `:1706` | `Rn = SR.all & 0x00000001` (the T bit) | 1 |
| `SWAP.B Rm,Rn` | `0110 nnnn mmmm 1000` | `SH2swapb` `:2451` | `Rn = (Rm & 0xFFFF0000) \| ((Rm & 0xFF) << 8) \| ((Rm >> 8) & 0xFF)` | 1 |
| `SWAP.W Rm,Rn` | `0110 nnnn mmmm 1001` | `SH2swapw` `:2467` | `Rn = (Rm << 16) \| ((Rm >> 16) & 0xFFFF)` | 1 |
| `XTRCT Rm,Rn` | `0010 nnnn mmmm 1101` | `SH2xtrct` `:2617` | `Rn = ((Rm << 16) & 0xFFFF0000) \| ((Rn >> 16) & 0xFFFF)` | 1 |
| `EXTS.B Rm,Rn` | `0110 nnnn mmmm 1110` | `SH2extsb` `:988` | `Rn = (u32)(s8)Rm` | 1 |
| `EXTS.W Rm,Rn` | `0110 nnnn mmmm 1111` | `SH2extsw` `:1000` | `Rn = (u32)(s16)Rm` | 1 |
| `EXTU.B Rm,Rn` | `0110 nnnn mmmm 1100` | `SH2extub` `:1012` | `Rn = (u32)(u8)Rm` | 1 |
| `EXTU.W Rm,Rn` | `0110 nnnn mmmm 1101` | `SH2extuw` `:1024` | `Rn = (u32)(u16)Rm` | 1 |

### 9.2 Data transfer — loads (memory → register)

All byte and word loads sign-extend. All longword loads are plain 32-bit.

| Mnemonic | Encoding | Handler | Effective address / operation | Cycles |
|---|---|---|---|---|
| `MOV.B @Rm,Rn` | `0110 nnnn mmmm 0000` | `SH2movbl` `:1428` | `Rn = (s32)(s8)[Rm]` | `1 + rcycle` |
| `MOV.W @Rm,Rn` | `0110 nnnn mmmm 0001` | `SH2movwl` `:1728` | `Rn = (s32)(s16)[Rm]` | `1 + cycle` |
| `MOV.L @Rm,Rn` | `0110 nnnn mmmm 0010` | `SH2movll` `:1582` | `Rn = [Rm]` | `1 + cycle` |
| `MOV.B @Rm+,Rn` | `0110 nnnn mmmm 0100` | `SH2movbp` `:1493` | `Rn = (s32)(s8)[Rm]`; **`if (n != m) Rm += 1`** | `1 + rcycle` |
| `MOV.W @Rm+,Rn` | `0110 nnnn mmmm 0101` | `SH2movwp` `:1793` | `Rn = (s32)(s16)[Rm]`; **`if (n != m) Rm += 2`** | `1 + cycle` |
| `MOV.L @Rm+,Rn` | `0110 nnnn mmmm 0110` | `SH2movlp` `:1642` | `Rn = [Rm]`; **`if (n != m) Rm += 4`** | `1 + cycle` |
| `MOV.B @(R0,Rm),Rn` | `0000 nnnn mmmm 1100` | `SH2movbl0` `:1441` | `Rn = (s32)(s8)[Rm + R0]` | **`rcycle` (no base cycle — DEVIATION, see §13)** |
| `MOV.W @(R0,Rm),Rn` | `0000 nnnn mmmm 1101` | `SH2movwl0` `:1741` | `Rn = (s32)(s16)[Rm + R0]` | `1 + cycle` |
| `MOV.L @(R0,Rm),Rn` | `0000 nnnn mmmm 1110` | `SH2movll0` `:1592` | `Rn = [Rm + R0]` | `1 + cycle` |
| `MOV.B @(disp,Rm),R0` | `1000 0100 mmmm dddd` | `SH2movbl4` `:1454` | `R0 = (s32)(s8)[Rm + disp]` — **`m` is nibble C**, `disp` is nibble D, **unscaled** | `1 + rcycle` |
| `MOV.W @(disp,Rm),R0` | `1000 0101 mmmm dddd` | `SH2movwl4` `:1754` | `R0 = (s32)(s16)[Rm + (disp << 1)]` — `m` is nibble C | `1 + cycle` |
| `MOV.L @(disp,Rm),Rn` | `0101 nnnn mmmm dddd` | `SH2movll4` `:1602` | `Rn = [Rm + (disp << 2)]` | `1 + cycle` |
| `MOV.B @(disp,GBR),R0` | `1100 0100 dddddddd` | `SH2movblg` `:1467` | `R0 = (s32)(s8)[GBR + disp]`, unscaled | `1 + rcycle` |
| `MOV.W @(disp,GBR),R0` | `1100 0101 dddddddd` | `SH2movwlg` `:1767` | `R0 = (s32)(s16)[GBR + (disp << 1)]` | `1 + cycle` |
| `MOV.L @(disp,GBR),R0` | `1100 0110 dddddddd` | `SH2movllg` `:1616` | `R0 = [GBR + (disp << 2)]` | `1 + cycle` |
| `MOV.W @(disp,PC),Rn` | `1001 nnnn dddddddd` | `SH2movwi` `:1715` | `Rn = (s32)(s16)[PC + (disp << 1) + 4]` — **no PC alignment mask** | `1 + cycle` |
| `MOV.L @(disp,PC),Rn` | `1101 nnnn dddddddd` | `SH2movli` `:1569` | `Rn = [((PC + 4) & 0xFFFFFFFC) + (disp << 2)]` — **PC aligned to 4** | `1 + cycle` |

### 9.3 Data transfer — stores (register → memory)

| Mnemonic | Encoding | Handler | Effective address / operation | Cycles |
|---|---|---|---|---|
| `MOV.B Rm,@Rn` | `0010 nnnn mmmm 0000` | `SH2movbs` `:1508` | `[Rn] = Rm` (byte) | `1 + cycle` |
| `MOV.W Rm,@Rn` | `0010 nnnn mmmm 0001` | `SH2movws` `:1808` | `[Rn] = Rm` (word) | `1 + cycle` |
| `MOV.L Rm,@Rn` | `0010 nnnn mmmm 0010` | `SH2movls` `:1656` | `[Rn] = Rm` (long) | **`cycle` (no base cycle — DEVIATION)** |
| `MOV.B Rm,@-Rn` | `0010 nnnn mmmm 0100` | `SH2movbm` `:1479` | `[Rn - 1] = Rm` **then** `Rn -= 1` (write happens first, at the pre-decremented address) | `1 + rcycle` |
| `MOV.W Rm,@-Rn` | `0010 nnnn mmmm 0101` | `SH2movwm` `:1779` | `[Rn - 2] = Rm` then `Rn -= 2` | `1 + cycle` |
| `MOV.L Rm,@-Rn` | `0010 nnnn mmmm 0110` | `SH2movlm` `:1628` | `[Rn - 4] = Rm` then `Rn -= 4` | `1 + cycle` |
| `MOV.B Rm,@(R0,Rn)` | `0000 nnnn mmmm 0100` | `SH2movbs0` `:1521` | `[Rn + R0] = Rm` | `1 + cycle` |
| `MOV.W Rm,@(R0,Rn)` | `0000 nnnn mmmm 0101` | `SH2movws0` `:1821` | `[Rn + R0] = Rm` | `1 + cycle` |
| `MOV.L Rm,@(R0,Rn)` | `0000 nnnn mmmm 0110` | `SH2movls0` `:1669` | `[Rn + R0] = Rm` | `1 + cycle` |
| `MOV.B R0,@(disp,Rn)` | `1000 0000 nnnn dddd` | `SH2movbs4` `:1532` | `[Rn + disp] = R0` — **`n` is nibble C**, `disp` nibble D, unscaled | `1 + cycle` |
| `MOV.W R0,@(disp,Rn)` | `1000 0001 nnnn dddd` | `SH2movws4` `:1832` | `[Rn + (disp << 1)] = R0` — `n` is nibble C | `1 + cycle` |
| `MOV.L Rm,@(disp,Rn)` | `0001 nnnn mmmm dddd` | `SH2movls4` `:1680` | `[Rn + (disp << 2)] = Rm` | `1 + cycle` |
| `MOV.B R0,@(disp,GBR)` | `1100 0000 dddddddd` | `SH2movbsg` `:1545` | `[GBR + disp] = R0`, unscaled | `1 + cycle` |
| `MOV.W R0,@(disp,GBR)` | `1100 0001 dddddddd` | `SH2movwsg` `:1845` | `[GBR + (disp << 1)] = R0` | `1 + cycle` |
| `MOV.L R0,@(disp,GBR)` | `1100 0010 dddddddd` | `SH2movlsg` `:1694` | `[GBR + (disp << 2)] = R0` | `1 + cycle` |

### 9.4 Arithmetic

| Mnemonic | Encoding | Handler | Operation | T | Cycles |
|---|---|---|---|---|---|
| `ADD Rm,Rn` | `0011 nnnn mmmm 1100` | `SH2add` `:358` | `Rn += Rm` | — | 1 |
| `ADD #imm,Rn` | `0111 nnnn iiiiiiii` | `SH2addi` `:367` | `Rn += (s32)(s8)imm` | — | 1 |
| `ADDC Rm,Rn` | `0011 nnnn mmmm 1110` | `SH2addc` `:379` | `tmp1 = Rm + Rn; tmp0 = Rn; Rn = tmp1 + T` | carry (§3.1) | 1 |
| `ADDV Rm,Rn` | `0011 nnnn mmmm 1111` | `SH2addv` `:401` | `Rn += Rm` | signed overflow | 1 |
| `SUB Rm,Rn` | `0011 nnnn mmmm 1000` | `SH2sub` `:2374` | `Rn -= Rm` | — | 1 |
| `SUBC Rm,Rn` | `0011 nnnn mmmm 1010` | `SH2subc` `:2385` | `tmp1 = Rn - Rm; tmp0 = Rn; Rn = tmp1 - T` | borrow | 1 |
| `SUBV Rm,Rn` | `0011 nnnn mmmm 1011` | `SH2subv` `:2409` | `Rn -= Rm` | signed underflow | 1 |
| `NEG Rm,Rn` | `0110 nnnn mmmm 1011` | `SH2neg` `:1893` | `Rn = 0 - Rm` | — | 1 |
| `NEGC Rm,Rn` | `0110 nnnn mmmm 1010` | `SH2negc` `:1902` | `temp = 0 - Rm; Rn = temp - T` | borrow | 1 |
| `DT Rn` | `0100 nnnn 0001 0000` | `SH2dt` `:973` | `Rn--` | `Rn == 0` | 1 |
| `CMP/EQ Rm,Rn` | `0011 nnnn mmmm 0000` | `SH2cmpeq` `:628` | compare | `Rn == Rm` | 1 |
| `CMP/HS Rm,Rn` | `0011 nnnn mmmm 0010` | `SH2cmphs` `:678` | unsigned ≥ | see §3.1 | 1 |
| `CMP/GE Rm,Rn` | `0011 nnnn mmmm 0011` | `SH2cmpge` `:640` | signed ≥ | | 1 |
| `CMP/HI Rm,Rn` | `0011 nnnn mmmm 0110` | `SH2cmphi` `:665` | unsigned > | | 1 |
| `CMP/GT Rm,Rn` | `0011 nnnn mmmm 0111` | `SH2cmpgt` `:653` | signed > | | 1 |
| `CMP/PL Rn` | `0100 nnnn 0001 0101` | `SH2cmppl` `:708` | `(s32)Rn > 0` | | 1 |
| `CMP/PZ Rn` | `0100 nnnn 0001 0001` | `SH2cmppz` `:720` | `(s32)Rn >= 0` | | 1 |
| `CMP/STR Rm,Rn` | `0010 nnnn mmmm 1100` | `SH2cmpstr` `:732` | any byte equal | | 1 |
| `CMP/EQ #imm,R0` | `1000 1000 iiiiiiii` | `SH2cmpim` `:691` | `R0 == (u32)(s32)(s8)imm` | | 1 |
| `DIV0S Rm,Rn` | `0010 nnnn mmmm 0111` | `SH2div0s` `:754` | `Q = sign(Rn); M = sign(Rm)` | `!(M == Q)` | 1 |
| `DIV0U` | `0000 xxxx 0001 1001` | `SH2div0u` `:773` | `M = Q = T = 0` | 0 | 1 |
| `DIV1 Rm,Rn` | `0011 nnnn mmmm 0100` | `SH2div1` `:782` | one restoring-division step, see below | `Q == M` | 1 |
| `MUL.L Rm,Rn` | `0000 nnnn mmmm 0111` | `SH2mull` `:1857` | `MACL = Rn * Rm` (low 32 bits only; **MACH untouched**) | — | 2 |
| `MULS.W Rm,Rn` | `0010 nnnn mmmm 1111` | `SH2muls` `:1869` | `MACL = (s32)(s16)Rn * (s32)(s16)Rm` | — | 1 |
| `MULU.W Rm,Rn` | `0010 nnnn mmmm 1110` | `SH2mulu` `:1881` | `MACL = (u32)(u16)Rn * (u32)(u16)Rm` | — | 1 |
| `DMULS.L Rm,Rn` | `0011 nnnn mmmm 1101` | `SH2dmuls` `:870` | `result = (s64)(s32)Rn * (s32)Rm; MACL = result; MACH = result >> 32` | — | 2 |
| `DMULU.L Rm,Rn` | `0011 nnnn mmmm 0101` | `SH2dmulu` `:936` | 64-bit unsigned product via explicit 16×16 partial-product expansion into `MACH:MACL` | — | 2 |
| `MAC.L @Rm+,@Rn+` | `0000 nnnn mmmm 1111` | `SH2macl` `:1195` | see below | — | `3 + rcycle1 + rcycle2` |
| `MAC.W @Rm+,@Rn+` | `0100 nnnn mmmm 1111` | `SH2macw` `:1312` | see below | — | `3 + rcycle1 + rcycle2` |
| `CLRMAC` | `0000 xxxx 0010 1000` | `SH2clrmac` `:609` | `MACH = MACL = 0` | — | 1 |

#### `DIV1 Rm,Rn` — exact algorithm (`sh2int.c:782-866`)

```c
old_q = Q;
Q  = (u8)((Rn & 0x80000000) != 0);   // Q takes the pre-shift MSB of Rn
Rn = (Rn << 1) | T;                  // shift the dividend left, T into bit 0

if (old_q == 0) {
    if (M == 0) { tmp0 = Rn; Rn -= Rm; tmp1 = (Rn > tmp0);
                  Q = (Q == 0) ? tmp1 : (tmp1 == 0); }
    else        { tmp0 = Rn; Rn += Rm; tmp1 = (Rn < tmp0);
                  Q = (Q == 0) ? (tmp1 == 0) : tmp1; }
} else {
    if (M == 0) { tmp0 = Rn; Rn += Rm; tmp1 = (Rn < tmp0);
                  Q = (Q == 0) ? tmp1 : (tmp1 == 0); }
    else        { tmp0 = Rn; Rn -= Rm; tmp1 = (Rn > tmp0);
                  Q = (Q == 0) ? (tmp1 == 0) : tmp1; }
}
T = (Q == M);
```

Note carefully: the inner `switch (SR.part.Q)` reads the **newly assigned** Q (the old MSB of
`Rn`), not `old_q`. `tmp1` is the borrow/carry of the add-or-subtract.

#### `MAC.L @Rm+,@Rn+` (`sh2int.c:1195-1308`)

```c
m1 = (s32)[Rn]; Rn += 4;      // note: Rn is read FIRST
m0 = (s32)[Rm]; Rm += 4;
a   = MACL | ((u64)MACH << 32);
b   = (s64)m0 * m1;
sum = a + b;
if (S == 1 && sum > 0x00007FFFFFFFFFFF && sum < 0xFFFF800000000000)
    sum = ((s64)b < 0) ? 0xFFFF800000000000 : 0x00007FFFFFFFFFFF;   // 48-bit saturation
MACL = (u32)sum;
MACH = (u32)(sum >> 32);
```

There is no `if (n != m)` guard here, so `MAC.L @R4+,@R4+` increments `R4` twice.

#### `MAC.W @Rm+,@Rn+` (`sh2int.c:1312-1347`)

```c
m0 = (s16)[Rm]; Rm += 2;      // note: Rm is read FIRST (opposite order from MAC.L)
m1 = (s16)[Rn]; Rn += 2;
b   = (s32)m0 * m1;
sum = (s64)(s32)MACL + b;     // <-- MACH is NOT part of the accumulator
if (S == 1) {
    if (sum > 0x7FFFFFFF && sum < 0xFFFFFFFF80000000) {
        MACH |= 1;
        sum = (b < 0) ? 0x80000000 : 0x7FFFFFFF;
    }
    MACL = (u32)sum;          // MACH otherwise untouched in saturating mode
} else {
    MACL = (u32)sum;
    MACH = (u32)(sum >> 32);  // MACH overwritten with the sign extension of the 33-bit sum
}
```

**DEVIATION:** in non-saturating mode (`S == 0`) the previous `MACH` is *not* added into the
accumulation — it is overwritten. Whether this matches hardware is not deducible from this
source. It is called out here because it changes results for any code doing repeated `MAC.W`
with `S == 0`.

### 9.5 Logic

| Mnemonic | Encoding | Handler | Operation | Cycles |
|---|---|---|---|---|
| `AND Rm,Rn` | `0010 nnnn mmmm 1001` | `SH2y_and` `:441` | `Rn &= Rm` | 1 |
| `AND #imm,R0` | `1100 1001 iiiiiiii` | `SH2andi` `:450` | `R0 &= imm` (zero-extended) | 1 |
| `AND.B #imm,@(R0,GBR)` | `1100 1101 iiiiiiii` | `SH2andm` `:459` | `[GBR+R0] = [GBR+R0] & imm` | `3 + rcycle + wcycle` |
| `OR Rm,Rn` | `0010 nnnn mmmm 1011` | `SH2y_or` `:1939` | `Rn \|= Rm` | 1 |
| `OR #imm,R0` | `1100 1011 iiiiiiii` | `SH2ori` `:1948` | `R0 \|= imm` | 1 |
| `OR.B #imm,@(R0,GBR)` | `1100 1111 iiiiiiii` | `SH2orm` `:1957` | `[GBR+R0] \|= imm` | **`3` — computes `rcycle`/`wcycle` and discards them (DEVIATION vs. `AND.B`/`XOR.B`)** |
| `XOR Rm,Rn` | `0010 nnnn mmmm 1010` | `SH2y_xor` `:2579` | `Rn ^= Rm` | 1 |
| `XOR #imm,R0` | `1100 1010 iiiiiiii` | `SH2xori` `:2591` | `R0 ^= imm` | 1 |
| `XOR.B #imm,@(R0,GBR)` | `1100 1110 iiiiiiii` | `SH2xorm` `:2601` | `[GBR+R0] ^= imm` | `3 + rcycle + wcycle` |
| `NOT Rm,Rn` | `0110 nnnn mmmm 0111` | `SH2y_not` `:1930` | `Rn = ~Rm` | 1 |
| `TST Rm,Rn` | `0010 nnnn mmmm 1000` | `SH2tst` `:2525` | `T = ((Rn & Rm) == 0)` | 1 |
| `TST #imm,R0` | `1100 1000 iiiiiiii` | `SH2tsti` `:2541` | `T = ((R0 & imm) == 0)` | 1 |
| `TST.B #imm,@(R0,GBR)` | `1100 1100 iiiiiiii` | `SH2tstm` `:2559` | `T = (([GBR+R0] & imm) == 0)` | `3 + rcycle` |
| `TAS.B @Rn` | `0100 nnnn 0001 1011` | `SH2tas` `:2481` | `temp = [Rn]; T = (temp == 0); [Rn] = temp \| 0x80` | `4 + cycle + wcycle` |

`TAS.B` uses the ordinary `MappedMemoryReadByte`/`WriteByte` path — it does **not** bypass the
cache and does not lock the bus in any way in this implementation.

### 9.6 Shift and rotate

All operate on `Rn` in place. `Rn` is `u32`, so all `>>=` are logical.

| Mnemonic | Encoding | Handler | Operation | T |
|---|---|---|---|---|
| `SHLL Rn` | `0100 nnnn 0000 0000` | `SH2shll` `:2153` | `Rn <<= 1` | old bit 31 |
| `SHLR Rn` | `0100 nnnn 0000 0001` | `SH2shlr` `:2196` | `Rn >>= 1` (logical) | old bit 0 |
| `SHAL Rn` | `0100 nnnn 0010 0000` | `SH2shal` `:2111` | `Rn <<= 1` | old bit 31 |
| `SHAR Rn` | `0100 nnnn 0010 0001` | `SH2shar` `:2125` | `Rn >>= 1`, then bit 31 restored from the old bit 31 | old bit 0 |
| `SHLL2 Rn` | `0100 nnnn 0000 1000` | `SH2shll2` `:2169` | `Rn <<= 2` | unchanged |
| `SHLL8 Rn` | `0100 nnnn 0001 1000` | `SH2shll8` `:2178` | `Rn <<= 8` | unchanged |
| `SHLL16 Rn` | `0100 nnnn 0010 1000` | `SH2shll16` `:2187` | `Rn <<= 16` | unchanged |
| `SHLR2 Rn` | `0100 nnnn 0000 1001` | `SH2shlr2` `:2212` | `Rn >>= 2` (logical) | unchanged |
| `SHLR8 Rn` | `0100 nnnn 0001 1001` | `SH2shlr8` `:2222` | `Rn >>= 8` (logical) | unchanged |
| `SHLR16 Rn` | `0100 nnnn 0010 1001` | `SH2shlr16` `:2232` | `Rn >>= 16` (logical) | unchanged |
| `ROTL Rn` | `0100 nnnn 0000 0100` | `SH2rotl` `:2029` | `T = bit31; Rn <<= 1; bit0 = T` | old bit 31 |
| `ROTR Rn` | `0100 nnnn 0000 0101` | `SH2rotr` `:2051` | `T = bit0; Rn >>= 1; bit31 = T` | old bit 0 |
| `ROTCL Rn` | `0100 nnnn 0010 0100` | `SH2rotcl` `:1973` | `temp = bit31; Rn <<= 1; bit0 = T_old; T = temp` | old bit 31 |
| `ROTCR Rn` | `0100 nnnn 0010 0101` | `SH2rotcr` `:2001` | `temp = bit0; Rn >>= 1; bit31 = T_old; T = temp` | old bit 0 |

All are 1 cycle.

### 9.7 Branch

`PC_br` below denotes the address of the branch instruction itself (which is the value of
`regs.PC` on entry to the handler).

| Mnemonic | Encoding | Handler | Target formula (exactly as coded) | Delay slot | Cycles |
|---|---|---|---|---|---|
| `BF disp` | `1000 1011 dddddddd` | `SH2bf` `:475` | if `T == 0`: `PC = PC_br + ((s32)(s8)disp << 1) + 4` else `PC += 2` | no | 3 taken / 1 not taken |
| `BF/S disp` | `1000 1111 dddddddd` | `SH2bfs` `:493` | if `T == 0`: `PC = PC_br + ((s32)(s8)disp << 1) + 4`, then delay slot at `PC_br + 2` | yes (only if taken) | 2 taken / 1 not taken |
| `BT disp` | `1000 1001 dddddddd` | `SH2bt` `:571` | if `T == 1`: `PC = PC_br + ((s32)(s8)disp << 1) + 4` | no | 3 / 1 |
| `BT/S disp` | `1000 1101 dddddddd` | `SH2bts` `:589` | if `T == 1`: `PC += ((s32)(s8)disp << 1) + 4`, then delay slot | yes (only if taken) | 2 / 1 |
| `BRA disp12` | `1010 dddddddddddd` | `SH2bra` `:514` | `disp = BCD; if (disp & 0x800) disp \|= 0xFFFFF000; PC = PC_br + (disp << 1) + 4` | yes | 2 |
| `BRAF Rm` | `0000 mmmm 0010 0011` | `SH2braf` `:530` | `PC = PC_br + Rm + 4` | yes | 2 |
| `BSR disp12` | `1011 dddddddddddd` | `SH2bsr` `:544` | `PR = PC_br + 4`; sign-extend `disp`; `PC = PC_br + (disp << 1) + 4` | yes | 2 |
| `BSRF Rm` | `0000 mmmm 0000 0011` | `SH2bsrf` `:560` | `PR = PC_br + 4`; `PC = PC_br + Rm + 4` | yes | 2 |
| `JMP @Rm` | `0100 mmmm 0010 1011` | `SH2jmp` `:1036` | `PC = Rm` | yes | 2 |
| `JSR @Rm` | `0100 mmmm 0000 1011` | `SH2jsr` `:1049` | `PR = PC_br + 4`; `PC = Rm` | yes | 2 |
| `RTS` | `0000 xxxx 0000 1011` | `SH2rts` `:2089` | `PC = PR` | yes | 2 |
| `RTE` | `0000 xxxx 0010 1011` | `SH2rte` `:2073` | `PC = [R15]; R15 += 4; SR.all = [R15] & 0x3F3; R15 += 4` (PC popped **first**, then SR) | yes | `4 + rcycle + wcycle` |

`SLEEP` (`0000 xxxx 0001 1011`, `SH2sleep` `:2632`):

```c
static void FASTCALL SH2sleep(SH2_struct * sh) { sh->cycles += 3; }
```

It does **not** advance PC and does **not** set `isSleeping`. The effect is that the CPU
re-fetches and re-executes the same `SLEEP` opcode, burning 3 cycles per iteration, until an
interrupt overwrites PC. This is functionally a 3-cycle busy loop, not a low-power state.

### 9.8 System control — LDC / LDS / STC / STS

| Mnemonic | Encoding | Handler | Operation | Cycles |
|---|---|---|---|---|
| `LDC Rm,SR` | `0100 mmmm 0000 1110` | `SH2ldcsr` `:1111` | `SR.all = Rm & 0x3F3`; **then calls `SH2HandleInterrupts`** | 1 |
| `LDC Rm,GBR` | `0100 mmmm 0001 1110` | `SH2ldcgbr` `:1063` | `GBR = Rm` | 1 |
| `LDC Rm,VBR` | `0100 mmmm 0010 1110` | `SH2ldcvbr` `:1121` | `VBR = Rm` | 1 |
| `LDC.L @Rm+,SR` | `0100 mmmm 0000 0111` | `SH2ldcmsr` `:1085` | `SR.all = [Rm] & 0x3F3; Rm += 4` (no interrupt re-check) | `3 + rcycle` |
| `LDC.L @Rm+,GBR` | `0100 mmmm 0001 0111` | `SH2ldcmgbr` `:1072` | `GBR = [Rm]; Rm += 4` | `3 + rcycle` |
| `LDC.L @Rm+,VBR` | `0100 mmmm 0010 0111` | `SH2ldcmvbr` `:1098` | `VBR = [Rm]; Rm += 4` | **`3` — `rcycle` computed and discarded (DEVIATION)** |
| `LDS Rm,MACH` | `0100 mmmm 0000 1010` | `SH2ldsmach` `:1132` | `MACH = Rm` | 1 |
| `LDS Rm,MACL` | `0100 mmmm 0001 1010` | `SH2ldsmacl` `:1141` | `MACL = Rm` | 1 |
| `LDS Rm,PR` | `0100 mmmm 0010 1010` | `SH2ldspr` `:1186` | `PR = Rm` | 1 |
| `LDS.L @Rm+,MACH` | `0100 mmmm 0000 0110` | `SH2ldsmmach` `:1150` | `MACH = [Rm]; Rm += 4` | `1 + rcycle` |
| `LDS.L @Rm+,MACL` | `0100 mmmm 0001 0110` | `SH2ldsmmacl` `:1162` | `MACL = [Rm]; Rm += 4` | **`1` — `rcycle` discarded (DEVIATION)** |
| `LDS.L @Rm+,PR` | `0100 mmmm 0010 0110` | `SH2ldsmpr` `:1174` | `PR = [Rm]; Rm += 4` | **`1` — `rcycle` discarded (DEVIATION)** |
| `STC SR,Rn` | `0000 nnnn 0000 0010` | `SH2stcsr` `:2288` | `Rn = SR.all` | 1 |
| `STC GBR,Rn` | `0000 nnnn 0001 0010` | `SH2stcgbr` `:2242` | `Rn = GBR` | 1 |
| `STC VBR,Rn` | `0000 nnnn 0010 0010` | `SH2stcvbr` `:2298` | `Rn = VBR` | 1 |
| `STC.L SR,@-Rn` | `0100 nnnn 0000 0011` | `SH2stcmsr` `:2264` | `Rn -= 4; [Rn] = SR.all` (decrement **first**) | `2 + cycle` |
| `STC.L GBR,@-Rn` | `0100 nnnn 0001 0011` | `SH2stcmgbr` `:2252` | `Rn -= 4; [Rn] = GBR` | `2 + cycle` |
| `STC.L VBR,@-Rn` | `0100 nnnn 0010 0011` | `SH2stcmvbr` `:2276` | `Rn -= 4; [Rn] = VBR` | `2 + cycle` |
| `STS MACH,Rn` | `0000 nnnn 0000 1010` | `SH2stsmach` `:2308` | `Rn = MACH` | 1 |
| `STS MACL,Rn` | `0000 nnnn 0001 1010` | `SH2stsmacl` `:2318` | `Rn = MACL` | 1 |
| `STS PR,Rn` | `0000 nnnn 0010 1010` | `SH2stspr` `:2364` | `Rn = PR` | 1 |
| `STS.L MACH,@-Rn` | `0100 nnnn 0000 0010` | `SH2stsmmach` `:2328` | `Rn -= 4; [Rn] = MACH` | `1 + cycle` |
| `STS.L MACL,@-Rn` | `0100 nnnn 0001 0010` | `SH2stsmmacl` `:2340` | `Rn -= 4; [Rn] = MACL` | `1 + cycle` |
| `STS.L PR,@-Rn` | `0100 nnnn 0010 0010` | `SH2stsmpr` `:2352` | `Rn -= 4; [Rn] = PR` | `1 + cycle` |

Note the asymmetry: the `@-Rn` store forms decrement `Rn` **before** the write, while the
`MOV.x Rm,@-Rn` forms write to `Rn - k` and then decrement (observationally identical unless
`Rm == Rn`, in which case `MOV.L Rn,@-Rn` stores the *un*decremented value — `sh2int.c:1634`).

### 9.9 System control — other

| Mnemonic | Encoding | Handler | Operation | Cycles |
|---|---|---|---|---|
| `NOP` | `0000 xxxx 0000 1001` | `SH2nop` `:1922` | nothing | 1 |
| `CLRT` | `0000 xxxx 0000 1000` | `SH2clrt` `:619` | `T = 0` | 1 |
| `SETT` | `0000 xxxx 0001 1000` | `SH2sett` `:2102` | `T = 1` | 1 |
| `CLRMAC` | `0000 xxxx 0010 1000` | `SH2clrmac` `:609` | `MACH = MACL = 0` | 1 |
| `SLEEP` | `0000 xxxx 0001 1011` | `SH2sleep` `:2632` | `cycles += 3`, PC unchanged | 3 |
| `TRAPA #imm` | `1100 0011 iiiiiiii` | `SH2trapa` `:2503` | see below | `8 + cycle + wcycle + wcycle2` |
| *(undecoded)* | — | `SH2undecoded` `:311` | see below | 1 |

#### `TRAPA #imm` (`sh2int.c:2503-2521`)

```c
R15 -= 4;  [R15] = SR.all;
R15 -= 4;  [R15] = PC + 2;          // return address = instruction after TRAPA
PC = [VBR + (imm << 2)];            // imm is the full 8-bit CD field, 0..255
cycles += 8 + <3 memory penalties>;
```

**`TRAPA` does not modify `SR.part.I`** — the interrupt mask is left as-is. (Contrast with
interrupt entry, §10.3, which does set it.)

#### `SH2undecoded` — illegal instruction (`sh2int.c:311-354`)

Reached for every encoding `decode()` does not recognise, and for `0xFFFF` returned by
`FetchInvalid`. Order of operations:

1. BIOS HLE hooks first: if `yabsys.extend_backup` is set and `PC == 0x0007D600`, call
   `BiosBUPInit(sh)` and return; if `extend_backup == 2` and `PC` is in `0x0380..0x03A8`,
   call `BiosHandleFunc(sh)` and return. If `yabsys.emulatebios`, try `BiosHandleFunc(sh)`
   and return if it handled the address. These are emulator HLE, **not hardware**.
2. Otherwise raise the exception:
   ```c
   YabSetError(YAB_ERR_SH2INVALIDOPCODE, sh);
   R15 -= 4;  [R15] = SR.all;
   R15 -= 4;  [R15] = PC + 2;
   vectnum = 4;                       // source comment: "4 for General Instructions,
                                      //  6 for delay slot" + "Fix me" — the delay-slot
                                      //  case is NOT distinguished
   PC = [VBR + (vectnum << 2)];
   cycles++;
   ```

**UNCLEAR / DEVIATION:** the code always uses vector 4 and carries an explicit `// Fix me`
comment saying vector 6 should be used for an illegal instruction in a delay slot. It also does
not modify `SR.part.I`.

### 9.10 Encodings that decode to `SH2undecoded`

From `decode()` (`sh2int.c:2639-2921`), these hole encodings are illegal:

- **A=0:** D ∈ {0, 1}; D=2 with C ≥ 3; D=3 with C ∈ {1} or C ≥ 3; D=8/9/10/11 with C ≥ 3.
- **A=2:** D = 3.
- **A=3:** D ∈ {1, 9}.
- **A=4:** D ∈ {12, 13}; D ∈ {0,1,2,3,5,6,7,8,9,10,11,14} with C ≥ 3; D=4 with C ∈ {1} or C ≥ 3.
- **A=8:** B ∈ {2, 3, 6, 7, 10, 12, 14} (i.e. anything other than 0,1,4,5,8,9,11,13,15).
- **A=15:** the entire group (`default: return &SH2undecoded`).

Groups A=6 and A=12 have a `case` for all 16 values of their selector nibble and therefore
contain no holes. Group A=1, 5, 7, 9, 10, 11, 13, 14 are single-instruction groups with no
holes.

### 9.11 Cross-check against the disassembler

`sh2d.c` carries two mnemonic tables (`tab[]` at `:190` for plain disassembly, `trace[]` at
`:44` for register-annotated disassembly) keyed by `(op & mask) == bits`. Every entry in those
tables maps to an instruction present in `decode()`, and vice versa — the two agree on every
opcode, operand nibble position, and displacement scale factor. Notable details confirmed by
the disassembler's `dat` field (`sh2d.c:154-186`, decode logic at `:459-533`):

- `MOV.B @(disp,Rm),R0` / `MOV.B R0,@(disp,Rn)` — displacement **× 1**.
- `MOV.W` forms of the same — displacement **× 2**.
- `MOV.L @(disp,Rm),Rn` / `MOV.L Rm,@(disp,Rn)` — displacement **× 4**.
- `@(disp,GBR)` byte/word/long — × 1 / × 2 / × 4.
- `MOVA` and `MOV.L @(disp,PC)` — × 4, and the disassembler prints the target as
  `(disp*4 + 4 + addr) & 0xFFFFFFFC` (matching the interpreter's alignment mask).
- `MOV.W @(disp,PC)` — × 2, and the disassembler prints `disp*2 + 4 + addr` **without** the
  alignment mask, matching `SH2movwi`.
- `BF/BT/BF-S/BT-S` — `(s8)disp * 2 + addr + 4`; `BRA/BSR` — `(s12)disp * 2 + addr + 4`.

The disassembler also flags `DT`, `BRAF`, `BSRF`, `DMULS.L`, `DMULU.L`, `MUL.L`, `MAC.L`,
`BF/S`, `BT/S` with `sh2 = 1`, i.e. "SH-2 only, not present on SH-1" — an SH-1/SH-2 distinction
that has no effect in the Saturn context but is preserved in the table.

---

## 10. Interrupt handling

### 10.1 Pending-interrupt queue

`interrupt_struct { u8 vector; u8 level; }` (`sh2core.h:315-319`), array
`SH2_struct.interrupts[MAX_INTERRUPTS]` with `MAX_INTERRUPTS = 50` (`sh2core.h:53`) and a
count `NumberOfInterrupts`.

`SH2InterpreterSendInterrupt(context, vector, level)` (`sh2int.c:3312-3349`):

1. **Deduplicate by vector only**: if any queued entry already has this `vector`, return
   immediately (the level of the existing entry is *not* upgraded).
2. Append `{level, vector}` at index `NumberOfInterrupts`, increment the count.
   **There is no bounds check against `MAX_INTERRUPTS`** — a 51st distinct pending vector
   overruns the array.
3. Bubble-sort the whole queue **ascending by level**. Therefore the **highest-priority
   interrupt ends up at index `NumberOfInterrupts - 1`**. Vector is not a tiebreaker; equal
   levels keep whatever order the sort leaves them in.

`SH2InterpreterRemoveInterrupt(context, vector, level)` (`sh2int.c:3351-3376`): finds the first
entry matching `vector` (level ignored), zeroes it, compacts the array, decrements the count.

### 10.2 Priority check

`SH2HandleInterrupts` (`sh2int.c:178-204`) inspects **only the last (highest-level) queue
entry**:

```c
if (NumberOfInterrupts != 0 &&
    interrupts[NumberOfInterrupts-1].level > regs.SR.part.I) { ...deliver... }
```

The comparison is strictly greater-than against the SR mask. Level 0 can therefore never be
delivered (I ≥ 0 always). Exactly **one** interrupt is delivered per call; the loop is not
re-entered.

Call sites: once at the top of `SH2InterpreterExec` (`sh2int.c:3154`) and
`SH2DebugInterpreterExec` (`:3064`), and inside `LDC Rm,SR` (`:1116`).

### 10.3 Delivery sequence

`sh2int.c:184-201`, in exactly this order:

```c
R15 -= 4;  MappedMemoryWriteLong(R15, SR.all);     // 1. push SR
R15 -= 4;  MappedMemoryWriteLong(R15, PC);         // 2. push PC (the NEXT instruction —
                                                   //    PC already points there)
level = interrupts[NumberOfInterrupts-1].level;
SR.part.I = (level == 0x10) ? 0xF : level;         // 3. raise the mask to the accepted level
PC = MappedMemoryReadLong(VBR + (vector << 2));    // 4. fetch the vector
NumberOfInterrupts--;                              // 5. pop the queue
isIdle = 0;  isSleeping = 0;
```

Details that matter:

- Both pushes use `MappedMemoryWriteLong(..., NULL)` — the memory wait-state penalty is
  discarded, and **no cycles at all are charged for taking an interrupt**.
- The pushed PC is the address of the instruction that *would have* executed next (no `+2`
  adjustment, unlike `TRAPA`/illegal-instruction which push `PC + 2`).
- **Level `0x10` is special-cased to an SR mask of `0xF`.** `0x10` is the level used by
  `SH2NMI` (`sh2core.c:264-268`), so this is how NMI, whose nominal level is above the
  4-bit field, gets clamped when written into `SR.part.I`.
- The delivered entry is removed from the queue at delivery time, not at `RTE` time.
- `RTE` restores PC then SR (`sh2int.c:2079-2082`), i.e. pops in the mirror order of the push.

### 10.4 NMI

`SH2NMI(context)` (`sh2core.c:264-268`):

```c
context->onchip.ICR |= 0x8000;                 // NMIL / NMI flag in ICR bit 15
SH2SendInterrupt(context, 0xB, 0x10);          // vector 11, level 16
```

### 10.5 User Break Controller interrupt

`SH2UBCInterrupt(context, flag)` (`sh2int.c:3016-3029`) — only reachable from
`SH2DebugInterpreterExec` under `#ifdef SH2_UBC`, which is **not defined** in the build:

```c
if (15 > SR.part.I) {              // UBC interrupts are always level 15
    R15 -= 4; [R15] = SR.all;
    R15 -= 4; [R15] = PC;
    SR.part.I = 15;
    PC = [VBR + (12 << 2)];        // vector 12
}
onchip.BRCR |= flag;               // the CMFCA/CMFCB match flag is set regardless
```

### 10.6 Complete list of interrupt sources raised inside these files

| Source | Vector expression | Level expression | Condition | Site |
|---|---|---|---|---|
| NMI | `0xB` (literal) | `0x10` (literal) | `SH2NMI()` called externally | `sh2core.c:267` |
| FRT output-compare A match | `VCRC & 0x7F` | `(IPRB & 0xF00) >> 8` | `TIER & 0x8` | `sh2core.c:2059` |
| FRT output-compare B match | `VCRC & 0x7F` | `(IPRB & 0xF00) >> 8` | `TIER & 0x4` | `sh2core.c:2077` |
| FRC overflow | `(VCRD >> 8) & 0x7F` | `(IPRB & 0xF00) >> 8` | `TIER & 0x2` | `sh2core.c:2088` |
| FRT input capture (MSH2) | `(VCRC >> 8) & 0x7F` | `(IPRB >> 8) & 0xF` | `TIER & 0x80` | `sh2core.c:2581` |
| FRT input capture (SSH2) | `(VCRC >> 8) & 0x7F` | `(IPRB >> 8) & 0xF` | `TIER & 0x80` | `sh2core.c:2612` |
| TIER write re-arming ICI | `(VCRC >> 8) & 0x7F` | `(IPRB >> 8) & 0xF` | `val & 0x80` and `FTCSR & 0x80` | `sh2core.c:1417-1423` |
| WDT interval-timer overflow | `(VCRWDT >> 8) & 0x7F` | `(IPRA >> 4) & 0xF` | interval mode, WTCNT overflow | `sh2core.c:2125` |
| DIVU overflow / divide-by-zero | `VCRDIV & 0x7F` | `(MSH2->onchip.IPRA >> 12) & 0xF` | `DVCR & 0x2` | `sh2core.c:1718`, `:1768`, `:1782`, `:1791` |
| DMAC channel transfer end | `*VCRDMA` (full value) | `(IPRA & 0xF00) >> 8` | `CHCR & 0x4` | `sh2core.c:2381`, `:2404`, `:2429`, `:2453`, `:2559` |
| UBC break | `12` (literal) | `15` (literal) | `#ifdef SH2_UBC` only | `sh2int.c:3025` |

**DEVIATION (SSH2 divider):** the DIVU interrupt level is read from `MSH2->onchip.IPRA` even
when `CurrentSH2 == SSH2`. Every other source uses `CurrentSH2`. Whether that is a bug or an
intentional workaround is not deducible from the code.

Note also that the FRT output-compare **B** interrupt uses `VCRC & 0x7F` — the *same* vector
field as output-compare A — while the input-capture interrupt uses the *other* byte,
`(VCRC >> 8) & 0x7F`. Whether hardware has a distinct OCIB vector is not deducible here.

Interrupts raised by the rest of the emulator (SCU, VDP, SMPC, …) enter through the same
`SH2SendInterrupt` API (`sh2core.c:252`) and are outside the scope of these files.

---

## 11. On-chip peripherals

All registers live in `Onchip_struct` (`sh2core.h:132-313`). "Offset" is `addr & 0x1FF`, i.e.
the label used in the `switch` statements; the absolute address is `0xFFFFFE00 + offset`.

### 11.1 Full on-chip register map with reset values

Reset values from `OnchipReset` (`sh2core.c:1076-1129`).

| Offset | Address | Register | Width | Reset | Module |
|---|---|---|---|---|---|
| `0x000` | `FFFFFE00` | `SMR` | 8 | `0x00` | SCI |
| `0x001` | `FFFFFE01` | `BRR` | 8 | `0xFF` | SCI |
| `0x002` | `FFFFFE02` | `SCR` | 8 | `0x00` | SCI |
| `0x003` | `FFFFFE03` | `TDR` | 8 | `0xFF` | SCI |
| `0x004` | `FFFFFE04` | `SSR` | 8 | `0x84` | SCI |
| `0x005` | `FFFFFE05` | `RDR` | 8 | `0x00` | SCI |
| `0x010` | `FFFFFE10` | `TIER` | 8 | `0x01` | FRT |
| `0x011` | `FFFFFE11` | `FTCSR` | 8 | `0x00` | FRT |
| `0x012`–`0x013` | `FFFFFE12` | `FRC` (H,L) | 16 | `0x0000` | FRT |
| `0x014`–`0x015` | `FFFFFE14` | `OCRA`/`OCRB` (selected by `TOCR` bit 4) | 16 | `0xFFFF` each | FRT |
| `0x016` | `FFFFFE16` | `TCR` | 8 | `0x00` | FRT |
| `0x017` | `FFFFFE17` | `TOCR` | 8 | `0xE0` | FRT |
| `0x018`–`0x019` | `FFFFFE18` | `FICR` | 16 | `0x0000` | FRT |
| `0x060` | `FFFFFE60` | `IPRB` | 16 | `0x0000` | INTC |
| `0x062` | `FFFFFE62` | `VCRA` | 16 | `0x0000` | INTC |
| `0x064` | `FFFFFE64` | `VCRB` | 16 | `0x0000` | INTC |
| `0x066` | `FFFFFE66` | `VCRC` | 16 | `0x0000` | INTC |
| `0x068` | `FFFFFE68` | `VCRD` | 16 | `0x0000` | INTC |
| `0x071` | `FFFFFE71` | `DRCR0` | 8 | `0x00` | DMAC |
| `0x072` | `FFFFFE72` | `DRCR1` | 8 | `0x00` | DMAC |
| `0x080`–`0x081` | `FFFFFE80` | `WTCSR` / `WTCNT` | 8 each | `0x18` / `0x00` | WDT |
| `0x083` | `FFFFFE83` | `RSTCSR` | 8 | `0x1F` | WDT |
| `0x091` | `FFFFFE91` | `SBYCR` | 8 | `0x60` | Power-down |
| `0x092` | `FFFFFE92` | `CCR` | 8 | `0x00` | Cache |
| `0x0E0` | `FFFFFEE0` | `ICR` | 16 | `0x0000` | INTC |
| `0x0E2` | `FFFFFEE2` | `IPRA` | 16 | `0x0000` | INTC |
| `0x0E4` | `FFFFFEE4` | `VCRWDT` | 16 | `0x0000` | INTC |
| `0x100` / `0x120` | `FFFFFF00` | `DVSR` | 32 | *(not reset)* | DIVU |
| `0x104` / `0x124` | `FFFFFF04` | `DVDNT` | 32 | *(not reset)* | DIVU |
| `0x108` / `0x128` | `FFFFFF08` | `DVCR` | 32 | `0x00000000` | DIVU |
| `0x10C` / `0x12C` | `FFFFFF0C` | `VCRDIV` | 32 | `0x00000000` | DIVU |
| `0x110` / `0x130` | `FFFFFF10` | `DVDNTH` | 32 | *(not reset)* | DIVU |
| `0x114` / `0x134` | `FFFFFF14` | `DVDNTL` | 32 | *(not reset)* | DIVU |
| `0x118` / `0x138` | `FFFFFF18` | `DVDNTUH` | 32 | *(not reset)* | DIVU |
| `0x11C` / `0x13C` | `FFFFFF1C` | `DVDNTUL` | 32 | *(not reset)* | DIVU |
| `0x140` | `FFFFFF40` | `BARA` | 32 | `0x00000000` | UBC |
| `0x144` | `FFFFFF44` | `BAMRA` | 32 | `0x00000000` | UBC |
| `0x148` | `FFFFFF48` | `BBRA` | 16 (stored `u32`) | `0x0000` | UBC |
| `0x160` | `FFFFFF60` | `BARB` | 32 | `0x00000000` | UBC |
| `0x164` | `FFFFFF64` | `BAMRB` | 32 | `0x00000000` | UBC |
| `0x168` | `FFFFFF68` | `BBRB` | 16 (stored `u32`) | `0x0000` | UBC |
| `0x170` | `FFFFFF70` | `BDRB` | 32 | `0x00000000` | UBC |
| `0x174` | `FFFFFF74` | `BDMRB` | 32 | `0x00000000` | UBC |
| `0x178` | `FFFFFF78` | `BRCR` | 32 | `0x0000` | UBC |
| `0x180` | `FFFFFF80` | `SAR0` | 32 | *(not reset)* | DMAC |
| `0x184` | `FFFFFF84` | `DAR0` | 32 | *(not reset)* | DMAC |
| `0x188` | `FFFFFF88` | `TCR0` | 32 | *(not reset)* | DMAC |
| `0x18C` | `FFFFFF8C` | `CHCR0` | 32 | `0x00000000` | DMAC |
| `0x190` | `FFFFFF90` | `SAR1` | 32 | *(not reset)* | DMAC |
| `0x194` | `FFFFFF94` | `DAR1` | 32 | *(not reset)* | DMAC |
| `0x198` | `FFFFFF98` | `TCR1` | 32 | *(not reset)* | DMAC |
| `0x19C` | `FFFFFF9C` | `CHCR1` | 32 | `0x00000000` | DMAC |
| `0x1A0` | `FFFFFFA0` | `VCRDMA0` | 32 | *(not reset)* | DMAC |
| `0x1A8` | `FFFFFFA8` | `VCRDMA1` | 32 | *(not reset)* | DMAC |
| `0x1B0` | `FFFFFFB0` | `DMAOR` | 32 | `0x00000000` | DMAC |
| `0x1E0` (long) / `0x1E2` (word) | `FFFFFFE0` | `BCR1` | 16 | `(preserved bit 15) \| 0x03F0` | BSC |
| `0x1E4` (long) / `0x1E6` (word) | `FFFFFFE4` | `BCR2` | 16 | `0x00FC` | BSC |
| `0x1E8` (long) / `0x1EA` (word) | `FFFFFFE8` | `WCR` | 16 | `0xAAFF` | BSC |
| `0x1EC` (long) / `0x1EE` (word) | `FFFFFFEC` | `MCR` | 16 | `0x0000` | BSC |
| `0x1F0` (long) / `0x1F2` (word) | `FFFFFFF0` | `RTCSR` | 16 | `0x0000` | BSC refresh |
| `0x1F4` (long) / `0x1F6` (word) | `FFFFFFF4` | `RTCNT` | 16 | `0x0000` | BSC refresh |
| `0x1F8` (long) / `0x1FA` (word) | `FFFFFFF8` | `RTCOR` | 16 | `0x0000` | BSC refresh |

The DIVU registers appear at **two** offsets each — `0x100`-block and `0x120`-block — because
`OnchipReadLong`/`OnchipWriteLong` list both `case` labels for every DIVU register
(`sh2core.c:1305-1328`, `:1691-1811`). This is a mirror, both aliases behave identically.

The BSC registers are reachable both as longs at the base offset and as words at base+2
(`OnchipReadWord` `sh2core.c:1273-1286`, whose comment reads *"real BCR1 register is located
at 0x1E2-0x1E3; Sega Rally OK"*). Writes to BSC registers only exist in the **long** path.

Registers **read** but never written by the handlers include `FICR` (written only by the input
capture hooks) and `RSTCSR` (written only through the `0x082` word-write magic path).

### 11.2 Serial Communication Interface (SCI)

Six 8-bit registers at offsets `0x000`–`0x005`. The SCI is **stubbed**: `SCIReceiveByte()`
returns 0 (`sh2core.c:2633`) and `SCITransmitByte()` does nothing (`sh2core.c:2639`).

| Reg | Read (`sh2core.c:1139-1171`) | Write (`sh2core.c:1379-1413`) |
|---|---|---|
| `SMR` `0x000` | returns stored value | stores raw value |
| `BRR` `0x001` | returns stored value | stores raw value |
| `SCR` `0x002` | returns stored value | **if `!(val & 0x20)` (transmitter being disabled): `SSR \|= 0x80` (set TDRE)**, then store |
| `TDR` `0x003` | returns stored value | stores raw value |
| `SSR` `0x004` | returns stored value; the receive/transmit auto-update logic is present but `#if 0`-commented out (`sh2core.c:1154-1167`) | if `SCR & 0x20` (transmitter enabled) and `!(val & 0x80)` (software clearing TDRE), call `SCITransmitByte(TDR)`. **`SSR` itself is never updated by a write.** |
| `RDR` `0x005` | returns stored value | not writable (falls through to "Unhandled") |

Bit meanings deducible from the code: `SCR` bit 5 = transmit enable, `SCR` bit 4 = receive
enable (referenced only in the commented-out block); `SSR` bit 7 = TDRE, bit 6 = RDRF
(referenced only in the commented-out block).

### 11.3 Free-Running Timer (FRT)

Registers `TIER`, `FTCSR`, `FRC`, `OCRA`, `OCRB`, `TCR`, `TOCR`, `FICR` at offsets
`0x010`–`0x019`. State: `SH2_struct.frc.shift` (log2 of the prescaler divisor) and
`frc.leftover` (fractional cycle accumulator).

**Register access behaviour**

| Reg | Read | Write |
|---|---|---|
| `TIER` `0x010` | raw | **`TIER = (val & 0x8E) \| 0x1`** — bits 6, 5, 4 and 0's complement are forced; bit 0 is always set. Additionally, if `val & 0x80` (ICI enable) and `FTCSR & 0x80` (ICF already pending), an input-capture interrupt is raised immediately (`sh2core.c:1416-1423`). Also writable as a **long** at offset `0x010` (`sh2core.c:1685-1687`). |
| `FTCSR` `0x011` | raw | **`FTCSR = (FTCSR & (val & 0xFE)) \| (val & 0x1)`** — bits 7..1 are write-**0**-to-clear (a bit survives only if it was already set *and* the written bit is 1); bit 0 (CCLRA) is directly assignable. |
| `FRC` `0x012`/`0x013` | `.part.H` / `.part.L`; word read at `0x012` returns `FRC.all` | byte writes set H/L halves independently |
| `OCRA`/`OCRB` `0x014`/`0x015` | if `!(TOCR & 0x10)` → OCRA else OCRB; word read at `0x014` likewise | same selector; byte writes merge into the selected register's high or low half |
| `TCR` `0x016` | raw | `TCR = val & 0x83`, and `val & 3` selects the prescaler: `0 → frc.shift = 3` (÷8), `1 → 5` (÷32), `2 → 7` (÷128), `3 → external clock, logged as not implemented and `frc.shift` left unchanged` |
| `TOCR` `0x017` | raw | `TOCR = 0xE0 \| (val & 0x13)` — bits 7..5 forced to 1; bit 4 = OCRA/OCRB select; bits 1,0 = output levels |
| `FICR` `0x018`/`0x019` | high/low byte | no write path |

**`FTCSR` bit meanings deducible from use:** bit 7 = ICF (input capture flag, set by the
capture hooks `sh2core.c:2572`, `:2603`), bit 3 = OCFA, bit 2 = OCFB, bit 1 = OVF, bit 0 =
CCLRA (clear FRC on OCRA match).

**`TIER` bit meanings deducible from use:** bit 7 = input-capture interrupt enable, bit 3 =
OCIA enable, bit 2 = OCIB enable, bit 1 = OVI enable. Bit 0 is forced to 1 on every write.

**`FRTExec(cycles)` (`sh2core.c:2041-2095`)** — called once per `SH2Exec` with the *requested*
cycle count:

```c
frcold = frctemp = FRC.all;
mask   = (1 << frc.shift) - 1;
frctemp    += ((cycles + frc.leftover) >> frc.shift);
frc.leftover = (cycles + frc.leftover) & mask;

if (frctemp >= OCRA && frcold < OCRA) {         // output compare A
    if (TIER & 0x8) SendInterrupt(VCRC & 0x7F, (IPRB & 0xF00) >> 8);
    if (FTCSR & 0x1) { frctemp = 0; frc.leftover = 0; }   // CCLRA
    FTCSR |= 0x8;                               // OCFA
}
if (frctemp >= OCRB && frcold < OCRB) {         // output compare B
    if (TIER & 0x4) SendInterrupt(VCRC & 0x7F, (IPRB & 0xF00) >> 8);
    FTCSR |= 0x4;                               // OCFB
}
if (frctemp > 0xFFFF) {                         // overflow
    if (TIER & 0x2) SendInterrupt((VCRD >> 8) & 0x7F, (IPRB & 0xF00) >> 8);
    FTCSR |= 2;                                 // OVF
}
FRC.all = frctemp;                              // truncates to 16 bits
```

**Notes on this as-coded behaviour:** the compare test is a *crossing* test
(`new >= OCR && old < OCR`), so a compare match can be missed if the counter jumps past OCR
in the same step in which it also wraps. The final `FRC.all = frctemp` truncates a `u32` into
the `u16` union — so after an overflow, the retained value is `frctemp & 0xFFFF`.

**Input capture** (`MSH2InputCaptureWriteWord` `sh2core.c:2569`, `SSH2InputCaptureWriteWord`
`:2600`) — these are memory-mapped write hooks installed elsewhere in the emulator, triggered
by an external event:

```c
FTCSR |= 0x80;             // ICF
FICR   = FRC.all;          // latch the counter
if (TIER & 0x80) SendInterrupt((VCRC >> 8) & 0x7F, (IPRB >> 8) & 0xF);
```

The SSH2 variant additionally runs the *other* CPU for 32 cycles (guarded by `depth < 4`), a
synchronisation hack with no hardware counterpart; the equivalent block in the MSH2 variant is
commented out.

### 11.4 Interrupt controller registers (INTC)

| Reg | Offset | Read behaviour | Write behaviour |
|---|---|---|---|
| `IPRA` | `0x0E2` | byte: high/low; word: full | byte `0x0E2`: `IPRA = (val << 8) \| (IPRA & 0x00FF)`; byte `0x0E3`: `IPRA = (IPRA & 0xFF00) \| (val & 0xF0)`; word: `IPRA = val & 0xFFF0` |
| `IPRB` | `0x060` | byte `0x060`: high byte; word: full | byte: `IPRB = (val << 8)` (**low byte destroyed**); byte `0x061`: ignored; word: `IPRB = val & 0xFF00`; long: `IPRB = val & 0xFF00` |
| `VCRA` | `0x062` | byte H/L, word full | byte: merge `val & 0x7F` into the selected half; word: `VCRA = val & 0x7F7F` |
| `VCRB` | `0x064` | same | same |
| `VCRC` | `0x066` | same | same |
| `VCRD` | `0x068` | byte `0x068` = high byte only; word full | byte `0x068`: `VCRD = (val & 0x7F) << 8` (**low byte cleared**); byte `0x069`: ignored; word: `VCRD = val & 0x7F7F` |
| `VCRWDT` | `0x0E4` | byte H/L, word full | byte: merge `val & 0x7F`; word (`0x0E4` **or** `0x0E5`): `VCRWDT = val & 0x7F7F` |
| `ICR` | `0x0E0` | byte H/L, word full | byte `0x0E0`: `ICR = ((val & 0x1) << 8) \| (ICR & 0xFEFF)`; byte `0x0E1`: `ICR = (ICR & 0xFFFE) \| (val & 0x1)`; word: `ICR = val & 0x0101` |

The vector fields are consistently masked to 7 bits (`0x7F`). Priority fields are 4 bits each.
The IPR field-to-source mapping is only observable indirectly, from how each interrupt source
extracts its level (see §10.6 table):

| IPR field | Extraction | Used by |
|---|---|---|
| `IPRA` bits 15–12 | `(IPRA >> 12) & 0xF` | DIVU |
| `IPRA` bits 11–8 | `(IPRA & 0xF00) >> 8` | DMAC |
| `IPRA` bits 7–4 | `(IPRA >> 4) & 0xF` | WDT |
| `IPRB` bits 11–8 | `(IPRB & 0xF00) >> 8` / `(IPRB >> 8) & 0xF` | FRT (all four sources) |

`ICR` bit 15 is set by `SH2NMI` (`sh2core.c:266`) — the NMI-level flag. Software can write only
bits 8 and 0.

### 11.5 Watchdog Timer (WDT)

Registers `WTCSR` (`0x080`), `WTCNT` (`0x081`), `RSTCSR` (`0x083`), plus the shadow `WTCSRM`.
State: `wdt.isenable`, `wdt.isinterval`, `wdt.shift`, `wdt.leftover`.

**Writes use the magic-value protocol** (`OnchipWriteWord` `sh2core.c:1577-1641`; the code's own
comment: *"This and RSTCSR have got to be the most wackiest register mappings I've ever seen"*).
A **word** write to offset `0x080`:

- **High byte `0xA5` → the low byte targets `WTCSR`:**
  - `val & 7` selects the clock divisor:
    | `val & 7` | `wdt.shift` |
    |---|---|
    | 0 | 1 |
    | 1 | 6 |
    | 2 | 7 |
    | 3 | 8 |
    | 4 | 9 |
    | 5 | 10 |
    | 6 | 12 |
    | 7 | 13 |
  - `wdt.isenable  = (val & 0x20)` — bit 5 = TME (timer enable)
  - `wdt.isinterval = (~val & 0x40)` — bit 6 = WT/**IT** mode select; **cleared = interval mode**
  - `WTCSR = (WTCSR & (WTCSRM | val) & 0x80) | (val & 0x67);` then `WTCSR &= ~0x80;`
    (bit 7 = OVF, write-0-to-clear via the `WTCSRM` shadow — but the following line
    unconditionally clears it anyway)
  - if `WTCSR & 0x20`: `SBYCR &= 0x7F`; else `WTCSR &= ~0x80` and `WTCNT = 0`
- **High byte `0x5A` → the low byte targets `WTCNT`:** `if (WTCSR & 0x20) WTCNT = (u8)val;`
  (i.e. WTCNT is only writable while the timer is enabled)

A **word** write to offset `0x082` targets `RSTCSR`:

- `val == 0xA500` exactly → `RSTCSR &= 0x7F` (clear WOVF, bit 7)
- high byte `0x5A` → `RSTCSR = (RSTCSR & 0x80) | (val & 0x60) | 0x1F`
  (bit 6 = RSTE, bit 5 = RSTS writable; bits 4–0 always read back as 1)

Reads: `WTCSR` at `0x080` and `WTCNT` at `0x081` are plain byte reads (`sh2core.c:1215-1219`).
There is **no read path for `RSTCSR`**.

**`WDTExec(cycles)` (`sh2core.c:2099-2136`)**:

```c
if (!wdt.isenable || (WTCSR & 0x80) || (RSTCSR & 0x80)) return;   // disabled, or OVF/WOVF set

wdttemp = WTCNT;
mask = (1 << wdt.shift) - 1;
wdttemp     += ((cycles + wdt.leftover) >> wdt.shift);
wdt.leftover = (cycles + wdt.leftover) & mask;

if (wdttemp > 0xFF) {
    if (wdt.isinterval) {
        WTCSR |= 0x80;                                     // OVF
        SendInterrupt((VCRWDT >> 8) & 0x7F, (IPRA >> 4) & 0xF);
    } else {
        // Watchdog Timer Mode: logged as "not implemented" — NOTHING HAPPENS
    }
}
WTCNT = (u8)wdttemp;      // truncated
```

**Watchdog mode (WT) is not implemented** — no reset is generated. Only interval-timer mode
does anything.

### 11.6 Division Unit (DIVU)

Registers at offsets `0x100`–`0x11C` (mirrored at `0x120`–`0x13C`).

| Register | Offset | Meaning |
|---|---|---|
| `DVSR` | `0x100`/`0x120` | Divisor (signed 32-bit) |
| `DVDNT` | `0x104`/`0x124` | **Write = trigger a 32÷32 division.** Read returns `DVDNTL`. |
| `DVCR` | `0x108`/`0x128` | Control/status. Word and long writes both apply `val & 0x3`. Bit 0 = OVF (overflow/divide-by-zero flag, set by the unit, never cleared by it), bit 1 = OVFIE (overflow interrupt enable). |
| `VCRDIV` | `0x10C`/`0x12C` | Interrupt vector, stored as `val & 0xFFFF`, used as `VCRDIV & 0x7F` |
| `DVDNTH` | `0x110`/`0x130` | Dividend high / remainder |
| `DVDNTL` | `0x114`/`0x134` | Dividend low. **Write = trigger a 64÷32 division.** |
| `DVDNTUH` | `0x118`/`0x138` | Shadow copy of `DVDNTH` after a division. Independently writable. |
| `DVDNTUL` | `0x11C`/`0x13C` | Shadow copy of `DVDNTL` after a division. Independently writable. |

**There is no multi-step / cycle-accurate division algorithm.** Both divisions are computed
*instantaneously* using the host's `/` and `%` operators at the moment of the triggering write.
No cycle cost is charged and no "division in progress" state exists.

#### 32 ÷ 32 — write to `DVDNT` (`sh2core.c:1695-1730`)

```c
s32 divisor = (s32)DVSR;
if (divisor == 0) {
    // Regardless of what DVDNTL is set to, the top 3 bits of the
    // written value are used to create the new DVDNTH value  (source comment)
    if (val & 0x80000000) { DVDNTL = 0x80000000; DVDNTH = 0xFFFFFFFC | ((val >> 29) & 0x3); }
    else                  { DVDNTL = 0x7FFFFFFF; DVDNTH = 0 | (val >> 29); }
    DVDNTUL = DVDNTL;  DVDNTUH = DVDNTH;
    DVCR |= 1;                                     // OVF
    if (DVCR & 0x2) SendInterrupt(VCRDIV & 0x7F, (MSH2->onchip.IPRA >> 12) & 0xF);
} else {
    DVDNTL = DVDNTUL = ((s32)val) / divisor;       // quotient
    DVDNTH = DVDNTUH = ((s32)val) % divisor;       // remainder
}
```

Note: in the non-zero-divisor path **no overflow check is performed at all** — the pathological
case `0x80000000 / -1` is left to the host's signed-division semantics (undefined behaviour in
C).

#### 64 ÷ 32 — write to `DVDNTL` (`sh2core.c:1744-1803`)

```c
s32 divisor  = (s32)DVSR;
s64 dividend = ((s64)DVDNTH << 32) | val;

if (divisor == 0) {
    if (DVDNTH & 0x80000000) { DVDNTL = 0x80000000; DVDNTH = DVDNTH << 3; /* "fix me" */ }
    else                     { DVDNTL = 0x7FFFFFFF; DVDNTH = DVDNTH << 3; /* "fix me" */ }
    DVDNTUL = DVDNTL;  DVDNTUH = DVDNTH;
    DVCR |= 1;
    if (DVCR & 0x2) SendInterrupt(VCRDIV & 0x7F, (MSH2->onchip.IPRA >> 12) & 0xF);
} else {
    s64 quotient  = dividend / divisor;
    s32 remainder = dividend % divisor;

    if (quotient > 0x7FFFFFFF) {                   // positive overflow
        DVCR |= 1;  DVDNTL = 0x7FFFFFFF;  DVDNTH = 0xFFFFFFFE;  /* "fix me" */
        if (DVCR & 0x2) SendInterrupt(...);
    } else if ((s32)(quotient >> 32) < -1) {       // negative overflow
        DVCR |= 1;  DVDNTL = 0x80000000;  DVDNTH = 0xFFFFFFFE;  /* "fix me" */
        if (DVCR & 0x2) SendInterrupt(...);
    } else {
        DVDNTL = (u32)quotient;
        DVDNTH = (u32)remainder;
    }
    DVDNTUL = DVDNTL;  DVDNTUH = DVDNTH;
}
```

**UNCLEAR:** the three `// fix me` comments mark values the author knew to be wrong: the
`DVDNTH` value produced on divide-by-zero (`DVDNTH << 3`) and on quotient overflow
(`0xFFFFFFFE`). What real hardware leaves in `DVDNTH` in those cases is **not** deducible from
this source. Also note the negative-overflow test `(s32)(quotient >> 32) < -1` rather than a
comparison against `0x80000000`.

A source comment at `sh2core.c:1323-1327` states that `DVDNTUH`/`DVDNTUL` *"act as a separate
register, but [are] set to the same value as DVDNTH/DVDNTL after division"* — hence they are
written on every division but can also be written independently.

### 11.7 Cache control register (CCR)

Offset `0x092` (`0xFFFFFE92`), 8-bit. Readable as byte (`sh2core.c:1220`) and as word
(`:1287`). Written identically by the byte path (`:1522-1533`) and the word path (`:1642-1653`):

```c
CCR = val & 0xCF;
if (val & 0x10) cache_clear(&cache);          // CP — cache purge
if (CCR & 0x01) cache_enable(&cache);         // CE — cache enable
else            cache_disable(&cache);
```

Bit usage deducible from the code:

| Bit | Mask | Name/use per the code |
|---|---|---|
| 0 | `0x01` | **CE** — cache enable. Drives `cache_enable`/`cache_disable`. |
| 1 | `0x02` | writable (mask `0xCF` permits it) but never read by any code in these files |
| 2 | `0x04` | writable, never read |
| 3 | `0x08` | **two-way mode** — read by `select_way_to_replace` (`sh2cache.c:117`) |
| 4 | `0x10` | **CP** — cache purge (one-shot). Note it is tested on the *written value*, and the bit is also stored into `CCR` because the `0xCF` mask keeps it. |
| 5 | `0x20` | **masked off** by `& 0xCF` — always reads back 0 |
| 6–7 | `0xC0` | **way select for the address array** — read by `AddressArrayReadLong` as `(CCR >> 6) & 3` (`sh2core.c:1934`) |

Note the ordering: `cache_clear` sets `ca->enable = 0` (`sh2cache.c:54`), so a write with both
CP and CE set purges first and re-enables afterwards.

### 11.8 DMA Controller (DMAC)

Two channels. Registers `SAR0/DAR0/TCR0/CHCR0/VCRDMA0` and `SAR1/DAR1/TCR1/CHCR1/VCRDMA1`, plus
the shared `DMAOR` and the per-channel request-control registers `DRCR0`/`DRCR1`.

| Register | Offset | Write masking | Notes |
|---|---|---|---|
| `SAR0` | `0x180` | none | source address |
| `DAR0` | `0x184` | none | destination address |
| `TCR0` | `0x188` | `val & 0xFFFFFF` | 24-bit transfer count |
| `CHCR0` | `0x18C` | see below | channel control |
| `SAR1`/`DAR1`/`TCR1`/`CHCR1` | `0x190`/`0x194`/`0x198`/`0x19C` | same | channel 1 |
| `VCRDMA0` | `0x1A0` | `val & 0xFFFF` | completion interrupt vector, used **unmasked** when sending |
| `VCRDMA1` | `0x1A8` | `val & 0xFFFF` | |
| `DMAOR` | `0x1B0` | `val & 0xF` | bit 0 = DME, bit 1 = NMIF, bit 2 = AE, bit 3 = round-robin priority |
| `DRCR0` | `0x071` | `val & 0x3` | stored, **never read by any code in these files** |
| `DRCR1` | `0x072` | `val & 0x3` | ditto |

**`CHCR` write path** (`sh2core.c:1827-1851` ch0, `:1861-1885` ch1):

```c
if (TCRn != 0) DMAProc(0x7FFFFFFF);        // flush any in-flight transfer first
CHCRn = val & 0xFFFF;
CHCRn = (val & ~2) | (CHCRn & (val | CHCRnM) & 2);   // TE (bit 1) is write-0-to-clear,
                                                     // gated through the CHCRnM shadow
if ((DMAOR & 7) == 1 && <DE set, TE clear>) { dma_chN.copy_clock = 0; DMAExec(); }
```

The `(DMAOR & 7) == 1` test means: DME set **and** NMIF clear **and** AE clear. Note the
channel-0 arm test uses the raw `val & 0x3` while channel 1 uses `CHCR1 & 0x3` — a small
inconsistency between the two otherwise-identical blocks.

Reading `CHCR0`/`CHCR1` (`sh2core.c:1335-1346`) clears the corresponding `CHCRnM` shadow to 0
as a side effect.

**`CHCR` bit fields deducible from `DMATransferCycles` (`sh2core.c:2338-2466`):**

| Bits | Mask | Meaning |
|---|---|---|
| 0 | `0x0001` | DE — channel enable |
| 1 | `0x0002` | TE — transfer end (set by the DMAC on completion) |
| 2 | `0x0004` | IE — interrupt on completion |
| 3 | `0x0008` | Read in `DMAProc` as *"Dual Channel"*: if clear, the cycle budget is doubled (`cycles <<= 1`) |
| 11–10 | `0x0C00` | Transfer size: `0` = byte, `1` = word, `2` = longword, `3` = 16-byte burst (implemented as longword transfers with a quarter cost, `copy_clock -= eat >> 2`) |
| 13–12 | `0x3000` | Source address mode: `0x0000` fixed, `0x1000` increment, `0x2000` decrement, `0x3000` treated as fixed |
| 15–14 | `0xC000` | Destination address mode: `0x0000` fixed, `0x4000` increment, `0x8000` decrement, `0xC000` treated as fixed |

**Transfer engine.** `DMAExec()` (`sh2core.c:2140`) calls `DMAProc(200)`; `DMAProc(cycles)`
(`:2200-2240`) checks `DMAOR & 0x6` (AE/NMIF abort), picks channel(s) per `DMAOR & 0x8`
(round-robin vs. channel-0 priority), applies the dual-channel cycle doubling, and calls
`DMATransferCycles(dmac, cycles)`.

`DMATransferCycles` is a **budgeted** engine: it accumulates `copy_clock += cycles` and
transfers one unit per `eat` cycles, where `eat = getEatClock(SAR, DAR)` (`sh2core.c:2243-2336`)
is a hard-coded source-region × destination-region latency table:

| Source region | Destination region | `eat` |
|---|---|---|
| `0x05800000` (CS2) | any | 1 |
| `0x05E00000` (VDP2 RAM) | High WRAM `0x06…` | 44 |
| | Low WRAM `0x002…` | 50 |
| | Sound RAM/regs, VDP1 RAM/regs | 427 |
| | VDP2 RAM | 1 |
| | VDP2 regs | 50 |
| | other | 44 |
| `0x05C00000` (VDP1 RAM) | High/Low WRAM, Sound, VDP2 regs | 50 |
| | VDP1 RAM / VDP1 regs | 570 |
| | VDP2 RAM | 225 |
| | other | 44 |
| WRAM / anything else | High/Low WRAM, VDP1 RAM, VDP2 regs | 14 |
| | Sound RAM/regs | 20 |
| | VDP1 regs | 30 |
| | VDP2 RAM | 82 |
| | other | 14 |

On completion (`*TCR <= 0`): if `CHCR & 0x4`, send `SH2SendInterrupt(CurrentSH2, *VCRDMA,
(IPRA & 0xF00) >> 8)`; set `CHCR |= 0x2` (TE) and `CHCRM |= 0x2`; call `SH2WriteNotify` over
the written range.

All DMA memory access goes through the `…Nocache` accessors
(`MappedMemoryRead/WriteByte/Word/LongNocache`), so **DMA bypasses the emulated cache entirely
in both directions** — it neither hits nor invalidates cache lines.

A second, legacy, instantaneous engine `DMATransfer()` (`sh2core.c:2470-2563`) exists but is
only reachable when `OLD_DMA` is set to 1 (`sh2core.c:69` defines it as 0). Its size-3 case
does 16-byte bursts with an explicit 4-longword buffer and masks addresses with `0x07FFFFFC`.

### 11.9 Bus State Controller (BSC) and refresh

| Register | Long offset | Word offset | Write masking |
|---|---|---|---|
| `BCR1` | `0x1E0` | `0x1E2` (read only) | `BCR1 &= 0x8000; BCR1 \|= val & 0x1FF7;` — bit 15 (MASTER) preserved, bit 3 not writable |
| `BCR2` | `0x1E4` | `0x1E6` (read only) | `val & 0xFC` |
| `WCR` | `0x1E8` | `0x1EA` (read only) | stored raw (no mask) |
| `MCR` | `0x1EC` | `0x1EE` (read only) | `val & 0xFEFC` |
| `RTCSR` | `0x1F0` | `0x1F2` (read only) | `val & 0xF8` |
| `RTCNT` | `0x1F4` | `0x1F6` (read only) | **no write path** |
| `RTCOR` | `0x1F8` | `0x1FA` (read only) | `val & 0xFF` |

These registers are **pure storage**. Nothing in `sh2core.c` reads `WCR`, `MCR`, `BCR2`,
`RTCSR`, `RTCNT`, or `RTCOR` to change behaviour: **no wait states are actually inserted, and
the refresh counter never counts.** The only functional bit is `BCR1` bit 15, which identifies
master vs. slave. `RTCNT` in particular can be read but never written or incremented.

### 11.10 Standby control (SBYCR)

Offset `0x091` (`0xFFFFFE91`), 8-bit, reset `0x60`. Write: `SBYCR = val & 0xDF` (bit 5 masked
off). **There is no read path.** The only functional use is inside the WDT write handler:
enabling the watchdog (`WTCSR & 0x20`) clears `SBYCR` bit 7 (`sh2core.c:1620`). Nothing else
reads it, so no standby/module-stop behaviour is emulated.

### 11.11 User Break Controller (UBC)

Registers `BARA`/`BAMRA`/`BBRA` (channel A) and `BARB`/`BAMRB`/`BBRB`/`BDRB`/`BDMRB`
(channel B), plus `BRCR`.

Writable paths that exist:

| Register | Offset | Write |
|---|---|---|
| `BARA` | `0x140` (long) | raw 32-bit |
| `BAMRA` | `0x144` (long) | raw 32-bit |
| `BBRA` | `0x148` (word) | `val & 0xFF` |
| `BRCR` | `0x178` (word) | `val & 0xF4DC` |

`BARB`, `BAMRB`, `BBRB`, `BDRB`, `BDMRB` exist in the struct with documented addresses
(`sh2core.h:224-289`) but have **no read or write handler** — accesses fall through to the
"Unhandled Onchip …" log. There is also no read path for `BARA`/`BAMRA`/`BBRA`/`BRCR`.

Bit constants are defined in `sh2core.h:62-90`:

| Constant | Value | Field |
|---|---|---|
| `BBR_CPA_NONE` / `_CPU` / `_PER` | `0<<6` / `1<<6` / `2<<6` | BBR bits 7–6: CPU-cycle / peripheral-cycle select |
| `BBR_IDA_NONE` / `_INST` / `_DATA` | `0<<4` / `1<<4` / `2<<4` | BBR bits 5–4: instruction / data access select |
| `BBR_RWA_NONE` / `_READ` / `_WRITE` | `0<<2` / `1<<2` / `2<<2` | BBR bits 3–2: read / write select |
| `BBR_SZA_NONE` / `_BYTE` / `_WORD` / `_LONGWORD` | `0` / `1` / `2` / `3` | BBR bits 1–0: operand-size select |
| `BRCR_CMFCA` | `1<<15` | channel A instruction-fetch match flag |
| `BRCR_CMFPA` | `1<<14` | channel A data match flag |
| `BRCR_EBBA` | `1<<13` | |
| `BRCR_UMD` | `1<<12` | |
| `BRCR_PCBA` | `1<<10` | channel A: break **after** instruction fetch (0 = before) |
| `BRCR_CMFCB` | `1<<7` | channel B instruction-fetch match flag |
| `BRCR_CMFPB` | `1<<6` | channel B data match flag |
| `BRCR_SEQ` | `1<<4` | |
| `BRCR_DBEB` | `1<<3` | |
| `BRCR_PCBB` | `1<<2` | channel B: break after instruction fetch |

The matching logic exists only in `SH2DebugInterpreterExec` under `#ifdef SH2_UBC`
(`sh2int.c:3077-3114`), which is not defined in the build. When compiled, it matches
`BARx.all == (PC & ~BAMRx.all)` for a break condition of
`BBR_CPA_CPU | BBR_IDA_INST | BBR_RWA_READ`, and only for **instruction fetch** — data
breakpoints are not implemented. **Semantics for `EBBA`, `UMD`, `SEQ`, `DBEB` are not
implemented anywhere and are therefore not deducible from this source.**

---

## 12. Cache emulation

`sh2cache.c` / `sh2cache.h`. Enabled by the CMake option `YAB_WANT_SH2_CACHE`
(`yabause/src/CMakeLists.txt:135-138`), which defines `CACHE_ENABLE=1` and
`EXEC_FROM_CACHE=1`. **The option defaults to OFF**, so in a stock build the cache functions
exist but nothing calls them except `cache_clear` / `cache_enable` / `cache_disable`
(which are called unconditionally from `SH2Reset` and the `CCR` write handler).

> **Code detail worth reproducing carefully:** `sh2int.c` and `memory.c` test the option with
> `#if CACHE_ENABLE`, while `sh2core.c` tests it with `#ifdef CACHE_ENABLE`
> (`sh2core.c:1933`, `:1948`, `:1962`, …). With `-DCACHE_ENABLE=1` these agree; with
> `-DCACHE_ENABLE=0` they would **disagree** (the `#ifdef` sites would take the cache path
> while the `#if` sites would not). The commented-out line `//#define CACHE_ENABLE 0` at
> `memory.h:50` is the trap this would spring.

### 12.1 Geometry and address decomposition

`cache_enty` (`sh2cache.h:30-34`):

```c
typedef struct _cache_line { u32 tag; int v; u8 data[16]; } cache_line;
typedef struct _cache_enty { u32 enable; u32 lru[64]; cache_line way[4][64]; } cache_enty;
```

- **4 ways × 64 entries × 16 bytes = 4 KiB total**, 16-byte lines, 4-way set-associative.
- Per-entry 6-bit pseudo-LRU state (`lru[64]`), shared by all 4 ways of that entry.
- Per-line valid bit `v`. **There is no dirty bit** — the cache is write-through (§12.3).

Address decomposition (`sh2cache.c:39-43`):

| Macro | Value | Field |
|---|---|---|
| `AREA_MASK` | `0xE0000000` | region selector (top 3 bits) |
| `TAG_MASK` | `0x1FFFFC00` | tag = bits 28–10 |
| `ENTRY_MASK` / `ENTRY_SHIFT` | `0x000003F0` / `4` | entry index = bits 9–4 (0–63) |
| `LINE_MASK` | `0x0000000F` | byte offset within the line |

Region constants (`sh2cache.c:45-50`):

| Constant | Value | Behaviour |
|---|---|---|
| `CACHE_USE` | `0x00000000` | normal cacheable access |
| `CACHE_THROUGH` | `0x20000000` | bypass — direct `…Nocache` access |
| `CACHE_PURGE` | `0x40000000` | associative purge (only honoured on a **longword write**) |
| `CACHE_ADDRES_ARRAY` | `0x60000000` | address array (handled in `sh2core.c`, §12.4) |
| `CACHE_DATA_ARRAY` | `0xC0000000` | data array (handled in `sh2core.c`, §12.5) |
| `CACHE_IO` | `0xE0000000` | on-chip IO |

`0x80000000` and `0xA0000000` have no constant and fall into the `default:` arm, which behaves
identically to `CACHE_THROUGH` (direct uncached access).

### 12.2 Read path (`cache_memory_read_b/w/l`, `sh2cache.c:303/358/414`)

For `CACHE_USE` addresses:

1. If `ca->enable == 0` → `MappedMemoryRead*Nocache(addr)` and return. **No lookup, no fill.**
2. Compute `tagaddr = addr & TAG_MASK`, `entry = (addr & ENTRY_MASK) >> 4`.
3. Probe ways 0, 1, 2, 3 **in that fixed order** for `way[i][entry].v && way[i][entry].tag ==
   tagaddr`. On the first match: `update_lru(i, &lru[entry])`, then return the bytes from
   `data[addr & 0xF]` (big-endian assembly for word/long).
4. On a miss:
   ```c
   lruway = select_way_to_replace(lru[entry]);
   update_lru(lruway, &lru[entry]);
   way[lruway][entry].tag = tagaddr;
   for (i = 0; i < 16; i += 4)
       <fill 4 bytes from> ReadLongList[(addr >> 16) & 0xFFF]((addr & 0xFFFFFFF0) + i);
   way[lruway][entry].v = 1;
   return <bytes from the newly filled line>;
   ```
   The fill is 4 longword reads of the aligned 16-byte line, stored big-endian
   (`data[i] = val >> 24`, …). **The fill calls `ReadLongList[...]` directly, bypassing
   `MappedMemoryReadLong`** — so it does not re-enter the cache and does not produce a wait-state
   value. The victim line is overwritten with no write-back (there is nothing to write back).

For `CACHE_THROUGH` and everything else → `MappedMemoryRead*Nocache(addr)`.

Word and longword reads index `data[(addr & 0xF) + 1..3]` **without a bounds check**, so a
misaligned access crossing the end of a line reads past `data[15]` into the adjacent struct
fields. No alignment enforcement exists anywhere.

### 12.3 Write path (`cache_memory_write_b/w/l`, `sh2cache.c:140/181/229`)

For `CACHE_USE` addresses:

1. If `ca->enable == 0` → `MappedMemoryWrite*Nocache(addr, val)` and return.
2. Probe ways 0–3 for a valid tag match; **if hit**, update the bytes in that line and
   `update_lru(way, …)`.
3. **Unconditionally** `MappedMemoryWrite*Nocache(addr, val)` — i.e. **write-through**.
4. On a miss, **nothing is allocated** — no write-allocate. The line stays absent.

The byte-write function is missing the explicit `// write through` comment that the word and
long versions carry, but does the same thing.

### 12.3.1 Associative purge (`CACHE_PURGE`)

Only `cache_memory_write_l` handles `CACHE_PURGE` (`sh2cache.c:232-247`):

```c
u32 tagaddr = addr & TAG_MASK;
u32 entry   = (addr & ENTRY_MASK) >> ENTRY_SHIFT;
for (i = 0; i < 3; i++) {              // <-- ways 0,1,2 only
    if (ca->way[i][entry].tag == tagaddr) {   // <-- v is NOT checked
        ca->way[i][entry].v = 0;              // only v is cleared, data is retained
        break;
    }
}
```

**Two DEVIATIONs, both plainly visible in the code:**

1. The loop bound is `i < 3`, so **way 3 is never purged.** A line resident in way 3 survives
   an associative purge.
2. The tag comparison does not check `v`, so an *invalid* line whose stale tag happens to match
   will consume the `break` and prevent a lower-priority valid way from being purged.

Byte and word writes to the purge region fall into `default:` and are treated as ordinary
uncached writes. Reads from the purge region never reach `sh2cache.c` at all — `memory.c`
returns `0xFFFFFFFF` for `addr >> 29 == 2` (§6).

### 12.3.2 Full purge / enable / disable

| Function | Line | Behaviour |
|---|---|---|
| `cache_clear(ca)` | `sh2cache.c:52-71` | Sets `enable = 0`, then for all 64 entries: `lru = 0`, and for all 4 ways: `tag = 0`, all 16 data bytes = 0, `v = 0`. |
| `cache_enable(ca)` | `:73-76` | `enable = 1`. Carries the explicit comment *"cache enable does not clear the cache"*. |
| `cache_disable(ca)` | `:78-80` | `enable = 0`. Contents are preserved, so re-enabling exposes the old lines again. |

`cache_clear` is called from `SH2Reset` (`sh2core.c:210`) and from the `CCR` CP-bit write
(§11.7).

### 12.3.3 Pseudo-LRU

`update_lru(way, u32 *lru)` (`sh2cache.c:87-113`) — called on a read hit, a write hit, and on
replacement after a miss:

| Way | Operation on the 6-bit `lru` |
|---|---|
| 3 | `lru \|= 0xB` (set bits 3, 1, 0) |
| 2 | `lru &= 0x3E` (clear bit 0); `lru \|= 0x14` (set bits 4, 2) |
| 1 | `lru \|= 0x20` (set bit 5); `lru &= 0x39` (clear bits 2, 1) |
| 0 | `lru &= 0x07` (clear bits 5, 4, 3) |

`select_way_to_replace(u32 lru)` (`sh2cache.c:115-138`):

```c
if (CCR & (1 << 3)) {                      // two-way mode
    return ((lru & 1) == 1) ? 2 : 3;       // only ways 2 and 3 are ever replaced
}
if      ((lru & 0x38) == 0x38) return 0;   // bits 5,4,3 all set
else if ((lru & 0x26) == 0x06) return 1;   // bit 5 clear, bits 2,1 set
else if ((lru & 0x15) == 0x01) return 2;   // bits 4,2 clear, bit 0 set
else if ((lru & 0x0B) == 0x00) return 3;   // bits 3,1,0 clear
return 0;                                  // "should not happen"
```

Note that **two-way mode only affects replacement**: lookups still probe all four ways, and
`update_lru` is unchanged. So enabling two-way mode does not make ways 0 and 1 inaccessible —
it only stops new lines from being allocated into them.

### 12.4 Cache address array (region `0x60000000`)

`AddressArrayReadLong` / `AddressArrayWriteLong` (`sh2core.c:1932-1957`). Note these are in
`sh2core.c`, not `sh2cache.c`, and are guarded by `#ifdef CACHE_ENABLE`.

**Read:**

```c
int way   = (CCR >> 6) & 3;          // way selected by CCR bits 7-6
int entry = (addr & 0x3FC) >> 4;     // NOTE the mask/shift combination
u32 data  = cache.way[way][entry].tag;      // bits 28-10
    data |= cache.lru[entry] << 4;          // bits 9-4
    data |= cache.way[way][entry].v << 2;   // bit 2
return data;
```

**Write:**

```c
int way   = (CCR >> 6) & 3;
int entry = (addr & 0x3FC) >> 4;
cache.way[way][entry].tag = addr & 0x1FFFFC00;   // <-- tag comes from the ADDRESS, not `val`
cache.way[way][entry].v   = (addr >> 2) & 1;     // <-- V comes from the ADDRESS too
cache.lru[entry]          = (val >> 4) & 0x3F;   // only LRU comes from the written value
```

**UNCLEAR:** the entry index uses `(addr & 0x3FC) >> 4`, which yields values 0–63 but skips
values (bits 3–2 of the address contribute nothing after the shift, and bit 9 is masked off by
`0x3FC`). Whether this indexing is correct for hardware is not deducible from the code. It is
also inconsistent with the data-array indexing (§12.5), which uses `(addr >> 4) & 0x3F`.

When `CACHE_ENABLE` is not defined, these functions instead index the flat
`CurrentSH2->AddressArray[(addr & 0x3FC) >> 2]`.

### 12.5 Cache data array (region `0xC0000000`)

`DataArrayReadByte/Word/Long` and `DataArrayWriteByte/Word/Long` (`sh2core.c:1961-2037`):

```c
int way   = (addr >> 10) & 3;    // bits 11-10 select the way
int entry = (addr >> 4) & 0x3F;  // bits 9-4 select the entry
// byte offset within the line: addr & 0xF, big-endian assembly for word/long
```

This region is what `EXEC_FROM_CACHE` (§5.2) executes from. When `CACHE_ENABLE` is not defined,
the same functions index the flat `CurrentSH2->DataArray[addr & 0xFFF]` (4 KiB), which is the
same total size and therefore still usable as scratch RAM — which is why `EXEC_FROM_CACHE`
being unconditionally on in `sh2int.c` does not break a non-cache build.

### 12.6 Save-state interaction

`SH2SaveState` / `SH2LoadState` (`sh2core.c:2644-2757`) serialise the whole `Onchip_struct`
— which includes the entire `cache_enty` (`sh2core.h:310`) — plus the separate flat
`AddressArray[0x100]` and `DataArray[0x1000]`. The struct-size-based versioning at
`sh2core.c:2714-2720` accounts for `CHCR0M` and `WTCSRM` being appended in later versions.

---

## 13. Summary of implementation deviations and open questions

Collected here so a re-implementation can decide, per item, whether to match Yabause's
behaviour (which is what real BIOS/game code was validated against) or the nominal
architecture.

**Cycle-count irregularities** (each is a missing or discarded term relative to its siblings):

| Instruction | Handler | Coded cycles | Sibling instructions use |
|---|---|---|---|
| `MOV.B @(R0,Rm),Rn` | `sh2int.c:1449` | `rcycle` | `1 + cycle` |
| `MOV.L Rm,@Rn` | `sh2int.c:1664` | `cycle` | `1 + cycle` |
| `LDC.L @Rm+,VBR` | `sh2int.c:1106` | `3` (rcycle discarded) | `3 + rcycle` |
| `LDS.L @Rm+,MACL` | `sh2int.c:1169` | `1` (rcycle discarded) | `1 + rcycle` |
| `LDS.L @Rm+,PR` | `sh2int.c:1181` | `1` (rcycle discarded) | `1 + rcycle` |
| `OR.B #imm,@(R0,GBR)` | `sh2int.c:1968` | `3` (both discarded) | `3 + rcycle + wcycle` |
| interrupt entry | `sh2int.c:186-197` | **0** | — |

**Semantic deviations / known-incomplete areas:**

1. `MAC.W` with `S == 0` overwrites `MACH` rather than accumulating into the full 64-bit
   `MACH:MACL` (§9.4).
2. `MAC.W` with `S == 1` sets `MACH |= 1` on saturation — a flag-like side effect on the high
   word.
3. Illegal-instruction exception always uses vector 4; the source's own `// Fix me` says
   vector 6 is required for the delay-slot case (§9.9).
4. `SLEEP` does not advance PC and does not set `isSleeping`; it is a 3-cycle spin (§9.7).
5. Watchdog-timer mode (as opposed to interval-timer mode) generates no reset — logged as
   "not implemented" (§11.5).
6. DIVU is instantaneous, charges no cycles, and has three `// fix me` values for the
   `DVDNTH` result on divide-by-zero and quotient overflow (§11.6).
7. DIVU interrupt level always comes from `MSH2->onchip.IPRA`, even on SSH2 (§10.6).
8. FRT output-compare B uses the same vector field (`VCRC & 0x7F`) as output-compare A
   (§10.6).
9. `FRTExec`/`WDTExec`/`DMAProc` advance by the *requested* cycle count, not the retired one
   (§7).
10. `SH2SendInterrupt` has no `MAX_INTERRUPTS` bounds check and does not upgrade the level of
    an already-queued vector (§10.1).
11. Associative purge skips way 3 and ignores the valid bit (§12.3.1).
12. Cache address-array indexing (`(addr & 0x3FC) >> 4`) is inconsistent with data-array
    indexing (`(addr >> 4) & 0x3F`) (§12.4).
13. BSC wait-state registers (`WCR`, `MCR`, `BCR2`) and the refresh counter (`RTCSR`/`RTCNT`/
    `RTCOR`) are storage only — no timing effect, no counting (§11.9).
14. `SBYCR` has no read path and no standby behaviour (§11.10).
15. UBC channel B registers have no read or write handlers, and UBC matching is compiled out
    (§11.11).
16. SCI is fully stubbed (§11.2).
17. DMA bypasses the cache in both directions (§11.8).
18. `SH2Reset` does not reset `R15` (the loop bound is `i < 15`) and does not set `PC`
    (§4).
19. The `SH2_struct.delay` field is dead (§2.1).
20. PC-relative instructions in a delay slot compute their address from `target - 2` (§8.2).

**Explicitly not deducible from these sources** (do not guess): the true hardware `DVDNTH`
value after a divide-by-zero or overflow; the hardware behaviour of a PC-relative load in a
delay slot; whether OCIB has its own vector field; the semantics of the UBC `EBBA`, `UMD`,
`SEQ`, and `DBEB` bits; the real illegal-instruction vector selection between 4 and 6; and any
actual bus wait-state timing.
