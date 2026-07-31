# SCU — System Control Unit (DMA, DSP, Interrupt Controller, Timers)

**Source of truth.** Everything in this document is derived *exclusively* from the Yabause
(YabaSanshiro fork) C source:

- `yabause/src/scu.c` (3566 lines)
- `yabause/src/scu.h` (364 lines)

Two facts that `scu.c` cannot supply on its own are marked inline and cited to their own file:
the SCU register block's base address (`yabause/src/memory.c:711`) and the argument passed to
`ScuExec` (`yabause/src/yabause.c:829`).

**No outside Saturn documentation was used.** Where the code is ambiguous, contradicts itself,
or is plainly buggy, that is stated explicitly rather than smoothed over — those notes are
tagged **[QUIRK]** (deliberate emulator shortcut / unimplemented hardware behaviour) or
**[BUG]** (a defect in the C source: precedence error, dead code, wrong index). Anything Yabause
does not implement is called out as *not implemented in this source* rather than guessed at.

Line citations are of the form `yabause/src/scu.c:1399`. They point at the code that
establishes the claim.

---

## 0. Structural overview

Yabause models the SCU with three separate allocations (`ScuInit`, `yabause/src/scu.c:78`):

| Global | Type | Contents |
|---|---|---|
| `ScuRegs` | `Scu` (`scu.h:77-147`) | All memory-mapped registers + DMA engine state + interrupt queue |
| `ScuDsp` | `scudspregs_struct` (`scu.h:159-297`) | DSP program RAM, data RAM, all DSP registers |
| `ScuBP` | `scubp_struct` (`scu.h:151-157`) | Debugger code breakpoints (not hardware) |

Entry points from the rest of the emulator:

| Function | Line | Role |
|---|---|---|
| `ScuInit` / `ScuDeInit` / `ScuReset` | 78 / 105 / 121 | Lifecycle |
| `ScuReadByte/Word/Long` | 2673 / 2689 / 2698 | CPU register reads |
| `ScuWriteByte/Word/Long` | 2750 / 2769 / 2776 | CPU register writes |
| `ScuExec(u32 timing)` | 1308 | Timer 1 tick, DMA time-slice, DSP instruction loop |
| `ScuSend*` (28 functions) | 3236-3481 | Interrupt sources raised by other subsystems |
| `ScuSaveState` / `ScuLoadState` | 3485 / 3506 | Savestates |

`ScuExec` is driven from the main loop as `ScuExec(sh2cycles >> 1)`
(`yabause/src/yabause.c:829`), i.e. the `timing` argument is *half* the SH-2 cycle count
elapsed. Everything in this file that consumes `timing` is scaled to that unit.

### 0.1 Reset state

`ScuReset` (`yabause/src/scu.c:121-158`):

| Register | Reset value | Line |
|---|---|---|
| `D0AD`, `D1AD`, `D2AD` | `0x101` | 122 |
| `D0EN`, `D1EN`, `D2EN` | `0x0` | 123 |
| `D0MD`, `D1MD`, `D2MD` | `0x7` | 124 |
| `DSTP` | `0x0` | 125 |
| `DSTA` | `0x0` | 126 |
| DSP `ProgControlPort.all` | `0` | 128 |
| `PDA` | `0x0` | 129 |
| `T1MD` | `0x0` | 131 |
| `IMS` | `0xBFFF` | 133 |
| `IST` | `0x0` | 134 |
| `AIACK` | `0x0` | 136 |
| `ASR0`, `ASR1` | `0x0` | 137 |
| `AREF` | `0x0` | 138 |
| `RSEL` | `0x0` | 140 |
| `VER` | `0x04` | 141 (comment: *"Looks like all consumer saturn's used at least version 4"*) |
| `timer0`, `timer1` | `0` | 143-144 |
| `dma0_time`, `dma1_time`, `dma2_time` | `0` | 146-148 |
| `interrupts[30]`, `NumberOfInterrupts` | zeroed | 151-152 |
| `dma0`, `dma1`, `dma2` (`scudmainfo_struct`) | zeroed | 154-156 |

`IMS = 0xBFFF` masks every bit except bit 14 — and no interrupt source in this file uses mask
bit 14, so at reset **all** interrupts are effectively masked. Note that `ScuReset` does *not*
clear `ScuDsp->ProgramRam`, `MD[][]`, `PC`, `CT[]`, `RA0/WA0`, or `jmpaddr`; only
`ProgControlPort.all` is zeroed.

### 0.2 The `OLD_DMA` switch

`#define OLD_DMA 0` at `yabause/src/scu.c:75`. With `OLD_DMA == 0` (the shipped configuration),
the **old** synchronous DMA path — `DoDMA` (`:221`) and `ScuDMA` (`:359`) — is compiled but
never executed, and the **new** time-sliced path (`ScuSetAddValue` `:1016`, `SucDmaExec`
`:1078`, `SucDmaCheck` `:1239`, `ScuDmaProc` `:1298`) is used instead. The old path is
described in §2.7 only because it is the clearest statement of the indirect-descriptor format;
**behaviour should be taken from the new path.**

The `#ifdef OPTIMIZED_DMA` block (`:162-219`, `DMAMemoryType[]` / `DMAMemoryPointer()`) is a
pointer fast-path table that is **not referenced by any live code** in this file — `DoDMA` and
`SucDmaExec` both go through `MappedMemoryRead*/Write*`. It is dead weight, listed here only so
its presence is not mistaken for a behavioural rule.

---

## 1. Register map

**Base address:** the SCU register block is installed over the single 64 KiB page `0x5FE`,
i.e. `0x05FE0000-0x05FEFFFF` (`FillMemoryArea(0x5FE, 0x5FE, &ScuReadByte, …)`,
`yabause/src/memory.c:711`). Every accessor masks the address with `0xFF`
(`scu.c:2674, 2690, 2699, 2751, 2770, 2777`), so **the 0x100-byte register file mirrors 256
times across the whole 64 KiB page**, and bits 15:8 of the address are ignored. Through the
SH-2 cache-through window this is `0x25FE0000`.

### 1.1 Complete offset table

Legend: **R** = handled by `ScuReadLong`; **W** = handled by `ScuWriteLong`; **—** = falls into
the `default:` branch, which only logs `"Unhandled SCU Register long read/write"` and returns 0
on read (`scu.c:2742-2744`, `2980-2982`).

| Offset | Name | Struct field | Long R | Long W | Notes / line |
|---|---|---|---|---|---|
| `0x00` | D0R | `D0R` | R `:2702` | W `:2781` | DMA0 read (source) address |
| `0x04` | D0W | `D0W` | R `:2704` | W `:2784` | DMA0 write (dest) address, or indirect table address |
| `0x08` | D0C | `D0C` | R `:2706` | W `:2787` | DMA0 transfer byte count |
| `0x0C` | D0AD | `D0AD` | — | W `:2790` | DMA0 address-add control |
| `0x10` | D0EN | `D0EN` | — | W `:2793-2813` | DMA0 enable/go — **has side effects** (§2.2) |
| `0x14` | D0MD | `D0MD` | — | W `:2814` | DMA0 mode / start factor |
| `0x18`-`0x1C` | — | — | — | — | Not decoded |
| `0x20` | D1R | `D1R` | R `:2708` | W `:2817` | DMA1 read address |
| `0x24` | D1W | `D1W` | R `:2710` | W `:2820` | DMA1 write address |
| `0x28` | D1C | `D1C` | R `:2712` | W `:2823` | DMA1 count |
| `0x2C` | D1AD | `D1AD` | — | W `:2826` | DMA1 address-add |
| `0x30` | D1EN | `D1EN` | — | W `:2829-2852` | DMA1 enable/go |
| `0x34` | D1MD | `D1MD` | — | W `:2853` | DMA1 mode |
| `0x38`-`0x3C` | — | — | — | — | Not decoded |
| `0x40` | D2R | `D2R` | R `:2714` | W `:2856` | DMA2 read address |
| `0x44` | D2W | `D2W` | R `:2716` | W `:2859` | DMA2 write address |
| `0x48` | D2C | `D2C` | R `:2718` | W `:2862` | DMA2 count |
| `0x4C` | D2AD | `D2AD` | — | W `:2865` | DMA2 address-add |
| `0x50` | D2EN | `D2EN` | — | W `:2868-2891` | DMA2 enable/go |
| `0x54` | D2MD | `D2MD` | — | W `:2892` | DMA2 mode |
| `0x58`-`0x5C` | — | — | — | — | Not decoded |
| `0x60` | DSTP | `DSTP` | — | W `:2895` | DMA force-stop. **Stored and never read by any logic.** |
| `0x64`-`0x78` | — | — | — | — | Not decoded |
| `0x7C` | DSTA | `DSTA` | R `:2720-2725` | W `:2898` | DMA status (§1.3) |
| `0x80` | PPAF | DSP `ProgControlPort` | R `:2726` | W `:2901` | DSP Program Control Port (§3.9) |
| `0x84` | PPD | DSP `ProgramRam[]` | — | W `:2922` | DSP Program RAM Data Port (§3.9) |
| `0x88` | PDA | DSP `DataRamPage/Address` | — | W `:2928` | DSP Data RAM Address Port (§3.9) |
| `0x8C` | PDD | DSP `MD[][]` | R `:2728` | W `:2933` | DSP Data RAM Data Port (§3.9) |
| `0x90` | T0C | `T0C` | — | W `:2940` | Timer 0 compare (§5) |
| `0x94` | T1S | `T1S` | — | W `:2943-2947` | Timer 1 set data — **has side effects** (§5) |
| `0x98` | T1MD | `T1MD` | — | W `:2948` | Timer 1 mode / global timer enable (§5) |
| `0x9C` | — | — | — | — | Not decoded |
| `0xA0` | IMS | `IMS` | — | W `:2951-2955` | Interrupt mask — write triggers `ScuTestInterruptMask()` |
| `0xA4` | IST | `IST` | R `:2733` | W `:2956-2963` | Interrupt status — write **ANDs** (§4.4) |
| `0xA7` | IST (low byte) | `IST` | byte R `:2677` | byte W `:2753-2760` | Only byte-accessible register (§1.4) |
| `0xA8` | AIACK | `AIACK` | R `:2736` | W `:2964-2967` | A-Bus interrupt acknowledge (§4.5) |
| `0xAC` | — | — | — | — | Not decoded |
| `0xB0` | ASR0 | `ASR0` | — | W `:2968` | A-Bus Set register 0. **Stored, never used.** |
| `0xB4` | ASR1 | `ASR1` | — | W `:2971` | A-Bus Set register 1. **Stored, never used.** |
| `0xB8` | AREF | `AREF` | — | W `:2974` | A-Bus refresh. **Stored, never used.** |
| `0xBC`-`0xC0` | — | — | — | — | Not decoded |
| `0xC4` | RSEL | `RSEL` | R `:2738` | W `:2977` | SDRAM select. **Stored, never used.** |
| `0xC8` | VER | `VER` | R `:2740` | — | Version register; fixed at `0x04` (`:141`). Writes unhandled. |
| `0xCC`-`0xFF` | — | — | — | — | Not decoded |

**[QUIRK] `IMS` (0xA0) is write-only in this implementation.** `ScuReadLong` has no case for
`0xA0`, so reading the interrupt mask returns 0 and logs "Unhandled". The same is true of every
`DnAD`/`DnEN`/`DnMD` register, `DSTP`, `T0C`, `T1S`, `T1MD`, `ASR0/1`, and `AREF`.

**[QUIRK] Word accesses do nothing.** `ScuReadWord` (`:2689`) always returns 0 and
`ScuWriteWord` (`:2769`) always discards, for every offset, with only a log line.

### 1.2 DMA register bit fields

The DMA registers are only ever interpreted through five fields. Bits not listed are read/write
storage with no effect anywhere in `scu.c`.

**`DnR` (0x00 / 0x20 / 0x40) — read address.** Whole 32-bit value copied into
`dmainfo->ReadAddress` (`:2800, 2837, 2877`). Masked to `0x0FFFFFFF` at each access inside
`SucDmaExec` (`:1125, 1144, 1160, 1186, 1202, 1219`).

**`DnW` (0x04 / 0x24 / 0x44) — write address.** Copied into `dmainfo->WriteAddress`
(`:2801, 2838, 2878`). In indirect mode this is not a destination at all — it is the address of
the descriptor table (§2.5). Never masked in the new path.

**`DnC` (0x08 / 0x28 / 0x48) — transfer count, in bytes.** Copied into
`dmainfo->TransferNumber` (`:2802, 2839, 2879`), then clamped by `ScuSetAddValue` (`:1061-1069`):

| Level | Clamp | Count of 0 means | Line |
|---|---|---|---|
| 0 | none (full 32 bits kept) | `0x100000` (1 MiB) | 1066-1069 |
| 1, 2 | `&= 0xFFF` (12 bits) | `0x1000` (4 KiB) | 1061-1065 |

The clamp is skipped entirely in indirect mode — the count then comes from the descriptor
(`:1052-1058`).

**`DnAD` (0x0C / 0x2C / 0x4C) — address add value.**

| Bits | Field | Meaning | Line |
|---|---|---|---|
| 31:9 | — | Unused | |
| 8 | Read add enable | `1` → `ReadAdd = 4`; `0` → `ReadAdd = 0` | 1018-1021 |
| 7:3 | — | Unused | |
| 2:0 | Write add select | see table below | 1023-1051 |

| `DnAD[2:0]` | `WriteAdd` (bytes) |
|---|---|
| 0 | 0 |
| 1 | 2 |
| 2 | 4 |
| 3 | 8 |
| 4 | 16 |
| 5 | 32 |
| 6 | 64 |
| 7 | 128 |

`ReadAdd == 0` does not mean "hold the source pointer" in the ordinary sense — it switches the
engine into **fill mode** (§2.4). The reset value `0x101` therefore means "read add 4, write
add 2".

**`DnEN` (0x10 / 0x30 / 0x50) — enable.**

| Bit | Meaning | Line |
|---|---|---|
| 0 | **Go**: writing 1 starts the transfer *immediately*, but only if `DnMD[2:0] == 7` | 2794, 2830, 2869 |
| 8 | **Armed**: transfer will start when the start factor selected by `DnMD[2:0]` fires | 3133, 3151, 3170 |

The whole written value is stored to `DnEN` after the go-check (`:2812, 2851, 2890`).
`ScuChekIntrruptDMA` clears `DnEN` to 0 after a factor-triggered start (`:3149, 3168, 3186`),
so an armed transfer is one-shot.

**`DnMD` (0x14 / 0x34 / 0x54) — mode.**

| Bits | Field | Meaning | Line |
|---|---|---|---|
| 31:25 | — | Unused |  |
| 24 | **Indirect mode** (`0x1000000`) | 1 = indirect (descriptor list), 0 = direct | 1052, 398 |
| 23:3 | — | Unused |  |
| 2:0 | **Start factor** | see §2.3 | 2794, 3133 |

No other bit of `DnMD` is examined anywhere in `scu.c`.

### 1.3 `DSTA` (0x7C) — DMA status

Read (`:2720-2725`) recomputes three bits from live engine state before returning the stored
value:

| Bit | Mask | Condition |
|---|---|---|
| 4 | `0x0010` | set iff `dma0.TransferNumber > 0` (DMA0 in progress) |
| 8 | `0x0100` | set iff `dma1.TransferNumber > 0` |
| 12 | `0x1000` | set iff `dma2.TransferNumber > 0` |

All other bits of `DSTA` are whatever was last written (`:2898-2900`), and nothing else in
`scu.c` inspects them.

### 1.4 Byte-access window

`ScuReadByte` / `ScuWriteByte` decode exactly one offset:

- **`0xA7` read** → `ScuRegs->IST & 0xFF` (`:2677-2678`), i.e. the low 8 interrupt status bits.
- **`0xA7` write** → `IST = IST & (0xFFFFFF00 | val)`, followed by
  `ScuRemoveInterruptByCPU(pre, after)` and `ScuTestInterruptMask()` (`:2753-2760`).
  So a byte write can only *clear* status bits, and only within the low byte.

Every other byte offset logs "Unhandled" and returns 0 / discards.

---

## 2. DMA engine

### 2.1 Engine state (`scudmainfo_struct`, `scu.h:64-75`)

Each of the three levels owns one of these, held in `ScuRegs->dma0/1/2` (`scu.h:143-145`).
It is a *working copy*: the memory-mapped `DnR/DnW/DnC/DnAD/DnMD` registers are snapshotted
into it at trigger time and then mutated as the transfer proceeds. The visible registers are
never written back.

| Field | Meaning |
|---|---|
| `mode` | Level number 0/1/2 (**not** a hardware mode field — it is set to the literal 0, 1 or 2 at `:2799, 2836, 2876` and used only to pick which DMA-end interrupt to raise) |
| `ReadAddress` | Current source pointer |
| `WriteAddress` | Current destination pointer |
| `TransferNumber` | Remaining bytes; **`> 0` is the "busy" predicate** (`:1241`, `:2721`) |
| `AddValue` | Snapshot of `DnAD` |
| `ModeAddressUpdate` | Snapshot of `DnMD` |
| `ReadAdd` | Decoded source step (0 or 4) |
| `WriteAdd` | Decoded destination step (0/2/4/8/16/32/64/128) |
| `InDirectAdress` | Pointer to the *next* indirect descriptor |

### 2.2 Trigger paths

There are exactly two ways a transfer starts, and both funnel into the same six-line setup
followed by `ScuSetAddValue()` + `ScuDmaProc(ScuRegs, 128)`:

**(a) Immediate start** — CPU writes `DnEN` with bit 0 set *and* `DnMD[2:0] == 7`
(`:2794` / `:2830` / `:2869`):

```c
if ((val & 0x1) && ((ScuRegs->D0MD & 0x7) == 0x7)) {
   if (ScuRegs->dma0.TransferNumber != 0) { ScuDmaProc(ScuRegs, 0x7FFFFFFF); }  // flush
   ScuRegs->dma0.mode = 0;
   ScuRegs->dma0.ReadAddress       = ScuRegs->D0R;
   ScuRegs->dma0.WriteAddress      = ScuRegs->D0W;
   ScuRegs->dma0.TransferNumber    = ScuRegs->D0C;
   ScuRegs->dma0.AddValue          = ScuRegs->D0AD;
   ScuRegs->dma0.ModeAddressUpdate = ScuRegs->D0MD;
   ScuSetAddValue(&ScuRegs->dma0);
   ScuDmaProc(ScuRegs, 128);
}
```

**(b) Factor start** — `ScuChekIntrruptDMA(id)` (`:3131-3188`), called from the interrupt
senders. For each level independently: `if ((DnEN & 0x100) && (DnMD & 0x07) == id)` → same
setup, then `DnEN = 0`.

**Re-trigger while busy:** if the target level still has `TransferNumber != 0`, the code first
calls `ScuDmaProc(ScuRegs, 0x7FFFFFFF)` — a *flush of all three levels to completion* — before
overwriting the state (`:2796-2798, 2832-2834, 2872-2874, 3134-3136, 3152-3154, 3171-3173`).
Note this flushes **every** level, not just the one being re-triggered.

### 2.3 Start-factor codes

The `id` argument to `ScuChekIntrruptDMA` is the value `DnMD[2:0]` must equal:

| `DnMD[2:0]` | Start factor | Raised by | Line |
|---|---|---|---|
| 0 | V-Blank IN | `ScuSendVBlankIN` | 3240 |
| 1 | V-Blank OUT | `ScuSendVBlankOUT` | 3265 |
| 2 | H-Blank IN | `ScuSendHBlankIN` | 3303 |
| 3 | Timer 0 | `ScuSendTimer0` | 3310 |
| 4 | Timer 1 | `ScuSendTimer1` | 3317 |
| 5 | Sound request | `ScuSendSoundRequest` | 3341 |
| 6 | Sprite draw end | `ScuSendDrawEnd` | 3384 |
| 7 | Immediate (CPU write to `DnEN` bit 0) | `ScuWriteLong` | 2794/2830/2869 |

`ScuSendDSPEnd` (`:3333`), `ScuSendSystemManager` (`:3346`), `ScuSendPadInterrupt` (`:3352`),
the three DMA-end senders, `ScuSendDMAIllegal`, and all 16 external senders do **not** call
`ScuChekIntrruptDMA` — they cannot start a DMA in this implementation.

The factor check runs *after* the corresponding interrupt is dispatched inside each `ScuSend*`
function, and it is unconditional on the interrupt mask: a masked V-Blank IN still starts a
DMA armed on factor 0.

### 2.4 Direct mode — transfer semantics (`SucDmaExec`, `:1078-1236`)

The engine splits on `ReadAdd == 0` (i.e. `DnAD` bit 8 clear):

#### Fill mode (`ReadAdd == 0`, `:1082-1175`)

A "constant source" test decides whether the source long-word is read once or re-read each
iteration (`:1086-1089`):

```c
int constant_source = ((ReadAddress & 0x1FF00000) == 0x00200000)   // Low Work RAM
                   || ((ReadAddress & 0x1E000000) == 0x06000000)   // High Work RAM
                   || ((ReadAddress & 0x1FF00000) == 0x05A00000)   // Sound RAM
                   || ((ReadAddress & 0x1DF00000) == 0x05C00000);  // VDP1/VDP2 RAM
```

If the source is **not** one of those regions it is treated as a register that can change
between reads, so it is re-read every iteration. `ReadAddress` is still advanced by `ReadAdd`
each iteration (`:1148, 1162`) — which is 0 in fill mode, so it stays put.

| Destination | Unit | Per iteration | Line |
|---|---|---|---|
| B-Bus (`0x05A00000 ≤ dst & 0x1FFFFFFF < 0x05FF0000`) | two 16-bit writes (high half then low half) | `dst += WriteAdd` **twice**, `count -= 4` | 1091-1139 |
| anything else | one 32-bit write | `dst += WriteAdd`, `count -= 4` | 1140-1173 |

The comment at `:1093-1095` gives the reason for the 16-bit split: avoiding misaligned 32-bit
accesses on hosts (e.g. PSP) that fault on them — i.e. it is a *host* constraint, not a
statement about Saturn hardware, although it coincides with the B-Bus being 16-bit.

#### Copy mode (`ReadAdd != 0`, `:1177-1233`)

| Case | Unit | Source step | Dest step | Count step | Line |
|---|---|---|---|---|---|
| Destination on B-Bus | 16-bit | `src += 2` | `dst += WriteAdd` | `-= 2` | 1180-1197 |
| Source on B-Bus | 16-bit | `src += 2` | `dst += (WriteAdd >> 1)` | `-= 2` | 1198-1213 |
| Neither | 32-bit | `src += 4` | `dst += WriteAdd` | `-= 4` | 1214-1231 |

**[QUIRK] In copy mode `ReadAdd` is ignored for the actual stride** — the source always advances
by the natural access width (2 or 4), never by `ReadAdd`. `ReadAdd` only functions as the
boolean fill/copy selector.

**B-Bus range constant:** the literal in the code is `0x5A00000`/`0x5FF0000`, i.e.
`0x05A00000 ≤ addr < 0x05FF0000`, applied to `addr & 0x1FFFFFFF`.

After every write burst the engine calls `SH2WriteNotify(start, WriteAddress - start)`
(`:1115, 1119, 1133, …`) so the SH-2 core can invalidate caches/recompiled blocks if the write
landed in main RAM.

### 2.5 Indirect mode

Selected by `DnMD` bit 24 (`ModeAddressUpdate & 0x1000000`, `:1052`, and `:398` in the legacy
path). In indirect mode `DnW` is not a destination — it is the address of a **descriptor
table**.

**Descriptor format — 12 (`0xC`) bytes, big-endian long-words** (`ScuSetAddValue:1052-1058`,
`SucDmaCheck:1265-1268`, legacy `ScuDMA:402-404`):

| Offset | Field |
|---|---|
| `+0x0` | Transfer byte count |
| `+0x4` | Write (destination) address |
| `+0x8` | Read (source) address; **bit 31 set = last descriptor in the list** |

```
descriptor N:  [ +0: count ][ +4: dst ][ +8: src | (0x80000000 if last) ]
descriptor N+1 begins at +0xC
```

**Walk protocol** (`SucDmaCheck:1242-1273`):

1. `ScuSetAddValue` loads descriptor 0 from `WriteAddress`, then sets
   `InDirectAdress = WriteAddress + 0xC` (`:1053-1057`).
2. Run `SucDmaExec` until `TransferNumber <= 0`.
3. **After** that descriptor completes, test `ReadAddress & 0x80000000`:
   - set → raise the level's DMA-end interrupt, force `TransferNumber = 0`, stop (`:1246-1263`).
   - clear → load the next three long-words from `InDirectAdress`, advance
     `InDirectAdress += 0xC`, loop (`:1264-1269`).
4. The whole walk is inside `while (atime > 0)`, so a long chain is spread over several
   `ScuExec` calls.

The end-of-list bit is **not** stripped from `ReadAddress` before use in the new path — instead
every access masks with `0x0FFFFFFF` (`:1125, 1144, 1160, 1186, 1202, 1219`). The legacy path
strips it explicitly with `& 0x7FFFFFFF` (`:407`).

The indirect count is **not** subject to the 12-bit / 20-bit clamps of §1.2 — `ScuSetAddValue`
takes the indirect branch and skips the clamp entirely (`:1052-1070`).

**Note the pointer aliasing:** the descriptor's destination goes into `dma->WriteAddress`, which
is the same field `ScuSetAddValue` originally read the descriptor table pointer from. The table
pointer survives only in `InDirectAdress`.

### 2.6 Scheduling, "priority", and bus arbitration

`ScuDmaProc(Scu *scu, int time)` (`:1298-1305`):

```c
SucDmaCheck(&scu->dma0, time);
SucDmaCheck(&scu->dma1, time);
SucDmaCheck(&scu->dma2, time);
```

- Each level gets its **own private copy** of the same time budget (`int atime = time;` at
  `:1240`), so all three levels are serviced in the same call and none starves the others.
- **There is no priority arbitration between levels.** The only ordering is the textual order
  0 → 1 → 2 in `ScuDmaProc`. Real-hardware level priority is not modelled here.
- **There is no bus locking of any kind.** Transfers execute as ordinary
  `MappedMemoryReadLong/WriteLong/ReadWord/WriteWord` calls with no notion of holding a bus,
  no stalling of the SH-2s, no CPU cycle-stealing, and no A-Bus/B-Bus contention.
  The only cross-component signalling is `SH2WriteNotify` (cache invalidation).
- **Cost model:** one loop iteration (2 or 4 bytes depending on path) costs one unit of `time`
  (`*time -= 1`, e.g. `:1108, 1124, 1146, 1185, 1201, 1218`). The budget per frame slice is
  `timing << 4` from `ScuExec` (`:1347`); a freshly triggered transfer gets an extra 128 units
  immediately (`:2809, 2846, 2886, 3147, 3166, 3184`); a re-trigger while busy uses
  `0x7FFFFFFF`, i.e. run to completion.
- `DSTP` (0x60), the DMA-stop register, is stored but **never consulted** — a DMA cannot be
  aborted in this implementation.
- The `dma0_time` / `dma1_time` / `dma2_time` fields are written only by the legacy `ScuDMA`
  path (`:420, 428, 436, 465, 473, 481`) and consumed only inside `#if OLD_DMA` (`:1322-1345`).
  They are inert in the shipped build.

### 2.7 Legacy path (`OLD_DMA == 1`, not compiled in)

For reference only. `ScuDMA` (`:359-489`) decodes `AddValue` identically, then either walks the
indirect list synchronously in a `for(;;)` (`:401-415`) or performs one direct transfer, and
raises the DMA-end interrupt inline — except that transfers larger than 1024 bytes defer the
interrupt by stashing the size in `dmaN_time` (`:419-441, 464-486`). `DoDMA` (`:221-354`) is the
synchronous equivalent of `SucDmaExec` with the same fill/copy and B-Bus splits.

### 2.8 DMA completion interrupts

Raised from `SucDmaCheck` at `:1250/1254/1258` (indirect) and `:1280/1284/1288` (direct):

| Level | Function | Vector | Level | Mask/status bit |
|---|---|---|---|---|
| 0 | `ScuSendLevel0DMAEnd` (`:3370`) | `0x4B` | 5 | `0x0800` |
| 1 | `ScuSendLevel1DMAEnd` (`:3364`) | `0x4A` | 6 | `0x0400` |
| 2 | `ScuSendLevel2DMAEnd` (`:3358`) | `0x49` | 6 | `0x0200` |

`ScuSendDMAIllegal` (vector `0x4C`, `:3376`) exists but **no code path in `scu.c` ever calls
it** — illegal-DMA detection is not implemented.

---

## 3. SCU DSP

### 3.1 Storage

| Object | C declaration | Size | Notes |
|---|---|---|---|
| Program RAM | `u32 ProgramRam[256]` (`scu.h:160`) | 256 × 32-bit | Addressed by 8-bit `PC` |
| Data RAM | `u32 MD[4][64]` (`scu.h:161`) | 4 pages × 64 × 32-bit | Pages MD0-MD3 |
| Data pointers | `u8 CT[4]` (`scu.h:212`) | 4 × 6-bit used | Always masked `& 0x3F` on use |
| `PC` | `u8` (`scu.h:205`) | 8-bit | Wraps at 256 |
| `TOP` | `u8` (`scu.h:206`) | 8-bit | Loop top address |
| `LOP` | `u16` (`scu.h:207`) | 12 bits used | Loop counter |
| `RX`, `RY` | `s32` (`scu.h:213-214`) | 32-bit | Multiplier inputs |
| `RA0`, `WA0` | `u32` (`scu.h:215-216`) | 25 bits used | DMA addresses in **long-word units** |
| `RA0M`, `WA0M` | `u32` (`scu.h:294-295`) | working copies used during a DSP DMA |
| `AC`, `P`, `ALU`, `MUL` | 48-bit unions (`scu.h:218-290`) | `part.L` = bits 31:0, `part.H` = bits 47:32, `part.unused` = 63:48 |
| `jmpaddr`, `delayed` | `s32`, `int` (`scu.h:208-209`) | delayed-branch machinery |
| `DataRamPage`, `DataRamReadAddress` | `u8`, `u8` (`scu.h:210-211`) | host-port cursor |
| `dsp_dma_instruction`, `dsp_dma_wait`, `dsp_dma_size` | (`scu.h:291-293`) | deferred DSP DMA state |

Physical byte address for DSP DMA is always `RA0 << 2` / `WA0 << 2`
(`:687, 691, 718, 730, 763`), i.e. these registers count long-words.

**`MUL` is vestigial.** `MUL.all` is copied by `ScuDspGetRegisters`/`SetRegisters`
(`:2554, 2582`) but never written by the interpreter — the line that would update it is
commented out (`:1947`), and `MOV MUL, P` computes `RX * RY` directly (`:1650`).

### 3.2 Instruction classes

Top-level dispatch is `switch (instruction >> 30)` (`:1645`):

| Bits 31:30 | Class | Handler |
|---|---|---|
| `00` | Operation Commands (ALU + X/Y/D1 bus moves) | `:1646-1703` |
| `01` | **Invalid** — logs `"Invalid DSP opcode"` (`:1943`) | none |
| `10` | Load Immediate Commands (MVI) | `:1704-1758` |
| `11` | "Other" — DMA / JMP / LPS-BTM / END | `:1759-1941` |

Within class `11`, `switch ((instruction >> 28) & 0xF)` (`:1761`) selects on bits 29:28:

| Bits 29:28 | `(insn>>28)&0xF` | Command |
|---|---|---|
| `00` | `0xC` | DMA (`:1762`) |
| `01` | `0xD` | JMP (`:1808`) |
| `10` | `0xE` | LPS / BTM (`:1902`) |
| `11` | `0xF` | END / ENDI (`:1925`) |

`NOP` is simply instruction word `0x00000000`: class `00`, ALU op `0`, and all bus fields zero.
The disassembler special-cases it (`:2108-2112`).

### 3.3 ALU field (bits 29:26) — only valid in class `00`

The ALU switch is `switch (instruction >> 26)` (`:1399`), executed **before** the class switch
and unguarded. Because classes `01`/`10`/`11` produce values ≥ `0x10`, the switch falls to
`default` for them, so in practice ALU ops only fire for Operation Commands.

`ALU.all` is preloaded with `AC.all` at `:1389` before the switch, so ops that only assign
`ALU.part.L` leave `ALU.part.H` equal to `AC.part.H` (comment at `:1406`).

| Op | Mnemonic | Operation | Z | S | C | V | Line |
|---|---|---|---|---|---|---|---|
| `0x0` | NOP | `ALU = AC` (via the preload) | — | — | — | — | 1401 |
| `0x1` | AND | `ALU.L = (u32)AC.L & (u32)P.L` | `ALU.L == 0` | `(s64)ALU.L < 0` | **cleared to 0** | — | 1405 |
| `0x2` | OR | `ALU.L = AC.L \| P.L` | same | same | **cleared to 0** | — | 1421 |
| `0x3` | XOR | `ALU.L = AC.L ^ P.L` | same | same | **cleared to 0** | — | 1436 |
| `0x4` | ADD | `ALU.L = (s32)AC.L + (s32)P.L` | `ALU.L == 0` | `(s32)ALU.L < 0` | bit 32 of `(u64)P.L + (u64)AC.L` | **not implemented** (code commented out `:1477-1480`) | 1451 |
| `0x5` | SUB | `ALU.L = (s32)AC.L - (s32)P.L` | `ALU.L == 0` | `(s64)ALU.L < 0` | bit 32 of `(u64)AC.L - (u64)P.L` (borrow) | **not implemented** (`:1518-1521`) | 1482 |
| `0x6` | AD2 | `ALU.all = (s64)AC.all + (s64)P.all` (48-bit) | `ALU.all == 0` | `ALU.all & 0x800000000000` | carry out of bit 47: `((AC.all & 0xFFFFFFFFFFFF) + (P.all & 0xFFFFFFFFFFFF)) & 0x1000000000000` | **not implemented** (`:1548-1551`) | 1524 |
| `0x7` | — | **no case — unimplemented** | | | | | |
| `0x8` | SR | `C = AC.L & 1`; `ALU.L = (AC.L & 0x80000000) \| (AC.L >> 1)` (arithmetic, sign preserved) | `ALU.L == 0` | `ALU.L & 0x80000000` | as shown | — | 1554 |
| `0x9` | RR | `C = AC.L & 1`; `ALU.L = (C << 31) \| ((u32)AC.L >> 1)` | `ALU.L == 0` | `ALU.L & 0x80000000` | as shown | — | 1571 |
| `0xA` | SL | `C = (AC.L >> 31) & 1`; `ALU.L = (u32)(AC.L << 1)` | `ALU.L == 0` | `ALU.L & 0x80000000` | as shown | — | 1587 |
| `0xB` | RL | `C = (AC.L >> 31) & 1`; `ALU.L = (AC.L << 1) \| C` | `ALU.L == 0` | `ALU.L & 0x80000000` | as shown | — | 1602 |
| `0xC` | — | **no case — unimplemented** | | | | | |
| `0xD` | — | **no case — unimplemented** | | | | | |
| `0xE` | — | **no case — unimplemented** | | | | | |
| `0xF` | RL8 | `C = (AC.L >> 24) & 1`; `ALU.L = (AC.L << 8) \| ((AC.L >> 24) & 0xFF)` | `ALU.L == 0` | `ALU.L & 0x80000000` | as shown (bit **24**, not bit 31) | — | 1620 |

**[QUIRK] The V (overflow) flag is never set by any ALU operation.** All three code paths that
would set it are commented out. `V` is only reachable via the Program Control Port.

**[QUIRK] `RR` uses the *new* carry, not the old one.** Line 1572 assigns `C` from `AC.L & 1`
*before* line 1573 shifts that same `C` into bit 31 — so `RR` is a plain 32-bit rotate right,
not a 33-bit rotate through carry. `RL` (`:1604-1606`) is symmetric.

### 3.4 Class `00` — Operation Commands: full bit layout

```
 31 30 | 29 28 27 26 | 25 | 24 23 | 22 21 20 | 19 | 18 17 | 16 15 14 | 13 12 | 11 10 9 8 | 7 6 5 4 3 2 1 0
  0  0 |   ALU op    | XL |  P-op |  X src   | YL | A-op  |  Y src   | D1-op |  D1 dest  |   imm8 / (3:0 = D1 src)
```

| Field | Bits | Values | Line |
|---|---|---|---|
| Class | 31:30 | `00` | 1645 |
| ALU op | 29:26 | §3.3 | 1399 |
| `XL` — X-bus load | 25 | `1` → `RX = readgensrc(X src)` | 1659-1663 |
| P-op | 24:23 | `0`,`1` = nothing; `2` = `P = (s64)RX * (s32)RY`; `3` = `P = (s64)(s32)readgensrc(X src)` | 1647-1657 |
| X src | 22:20 | X-bus source select, §3.5 | 1654, 1662 |
| `YL` — Y-bus load | 19 | `1` → `RY = readgensrc(Y src)` | 1666-1670 |
| A-op | 18:17 | `0` = nothing; `1` = `CLR A` (`AC.all = 0`); `2` = `MOV ALU,A` (`AC.all = ALU.all`); `3` = `MOV [s],A` (`AC.all = (s64)(s32)readgensrc(Y src)`) | 1671-1684 |
| Y src | 16:14 | Y-bus source select, §3.5 | 1669, 1681 |
| D1-op | 13:12 | `0`,`2` = nothing; `1` = `MOV SImm,[d]`; `3` = `MOV [s],[d]` | 1688-1701 |
| D1 dest | 11:8 | D1-bus destination select, §3.6 | 1695, 1698 |
| D1 imm | 7:0 | Signed 8-bit immediate, used when D1-op == 1: `(u32)(signed char)(instruction & 0xFF)` | 1695 |
| D1 src | 3:0 | Source select (4 bits, §3.5) used when D1-op == 3 | 1698 |

Note that **P-op cases 2 and 3 share the X-src field** (bits 22:20) with the `XL` load — both
`MOV [s],P` and `MOV [s],X` read from `(instruction >> 20) & 0x7`. Likewise A-op case 3 shares
bits 16:14 with the `YL` load.

**Execution order within one instruction** (matters because sources have side effects):
ALU (`:1399`) → P-op (`:1647`) → X load (`:1659`) → Y load (`:1666`) → A-op (`:1671`) →
D1-bus (`:1688`) → pending `CT` increments (`:1949-1952`) → `PC++` (`:1954`).

### 3.5 Source select — `readgensrc(u8 num)` (`:493-546`)

Used for X src (3 bits), Y src (3 bits), and D1 src (4 bits). The 3-bit fields can only reach
values 0-7.

| `num` | Mnemonic (`disd1bussrc`, `:1975`) | Returns | `CT` post-increment |
|---|---|---|---|
| `0x0` | `M0` | `MD[0][CT[0] & 0x3F]` | no |
| `0x1` | `M1` | `MD[1][CT[1] & 0x3F]` | no |
| `0x2` | `M2` | `MD[2][CT[2] & 0x3F]` | no |
| `0x3` | `M3` | `MD[3][CT[3] & 0x3F]` | no |
| `0x4` | `MC0` | `MD[0][CT[0] & 0x3F]` | **yes** |
| `0x5` | `MC1` | `MD[1][CT[1] & 0x3F]` | **yes** |
| `0x6` | `MC2` | `MD[2][CT[2] & 0x3F]` | **yes** |
| `0x7` | `MC3` | `MD[3][CT[3] & 0x3F]` | **yes** |
| `0x8` | `??` | `0xFFFFFFFF` (unimplemented) | — |
| `0x9` | `ALL` | `(u32)ALU.part.L` (bits 31:0 of ALU) | — |
| `0xA` | `ALH` | `(u32)(ALU.all >> 16)` (bits 47:16 of ALU) | — |
| `0xB`-`0xF` | `??` | `0xFFFFFFFF` | — |

The increment is implemented as a deferred flag:
`incFlg[num & 3] |= (num >> 2) & 1` (`:498`). The flags are cleared at the top of every
instruction (`:1384-1387`) and applied after the instruction body (`:1949-1952`), or early — at
`:1691-1694` — in the `MOV SImm,[d]` case, so that a same-instruction immediate write lands at
the *incremented* pointer.

`readgensrc` also **force-completes a pending DSP DMA** before returning
(`:500-503`) — see §3.8.

### 3.6 D1-bus destination — `writed1busdest(u8 num, u32 val)` (`:550-613`)

| `num` | Mnemonic (`disd1busdest`, `:2006`) | Effect |
|---|---|---|
| `0x0` | `MC0` | `MD[0][CT[0] & 0x3F] = val`; set `incFlg[0]` |
| `0x1` | `MC1` | `MD[1][CT[1] & 0x3F] = val`; set `incFlg[1]` |
| `0x2` | `MC2` | `MD[2][CT[2] & 0x3F] = val`; set `incFlg[2]` |
| `0x3` | `MC3` | `MD[3][CT[3] & 0x3F] = val`; set `incFlg[3]` |
| `0x4` | `RX` | `RX = val` |
| `0x5` | `PL` | `P.all = (signed)val` (sign-extended into 48/64 bits) |
| `0x6` | `RA0` | `RA0 = val` (**no mask**) |
| `0x7` | `WA0` | `WA0 = val` (**no mask**) |
| `0x8`-`0x9` | `??` | no effect |
| `0xA` | `LOP` | `LOP = (u16)val` |
| `0xB` | `TOP` | `TOP = (u8)val` |
| `0xC` | `CT0` | `CT[0] = (u8)val`; **clears** `incFlg[0]` |
| `0xD` | `CT1` | `CT[1] = (u8)val`; clears `incFlg[1]` |
| `0xE` | `CT2` | `CT[2] = (u8)val`; clears `incFlg[2]` |
| `0xF` | `CT3` | `CT[3] = (u8)val`; clears `incFlg[3]` |

Also force-completes a pending DSP DMA on entry (`:555-558`).

Writing `CT[n]` stores the full 8 bits without masking; every *read* of `CT[n]` masks with
`0x3F`, so the upper two bits are inert.

### 3.7 Class `10` — Load Immediate Commands (MVI)

```
 31 30 | 29 28 27 26 | 25 | 24 23 22 21 20 19 |  18 ................. 0
  1  0 |    dest     | CD |     condition     |  immediate
```

| Field | Bits | Line |
|---|---|---|
| Class | 31:30 = `10` | 1645 |
| Destination | 29:26 (`(instruction >> 26) & 0xF`), §3.7.1 | 1710 |
| `CD` — conditional flag | 25 (`(instruction >> 25) & 1`) | 1705 |
| Condition code | 24:19 (`(instruction >> 19) & 0x3F`) — only when `CD == 1` | 1707 |
| Immediate | 18:0 when `CD == 1`; 24:0 when `CD == 0` | 1710 / 1754 |

**Unconditional form (`CD == 0`, `:1751-1757`):**

```c
int value = (instruction & 0x1FFFFFF);              // 25-bit
if (value & 0x1000000) value |= 0xfe000000;         // sign-extend from bit 24
writeloadimdest((instruction >> 26) & 0xF, value);
```

**Conditional form (`CD == 1`, `:1705-1750`):** immediate is
`(instruction & 0x7FFFF) | ((instruction & 0x40000) ? 0xFFF80000 : 0)` — a 19-bit field
sign-extended from bit 18.

| Condition code (bits 24:19) | Mnemonic | Test | Line |
|---|---|---|---|
| `0x01` | `NZ` | `!Z` | 1708 |
| `0x02` | `NS` | `!S` | 1712 |
| `0x03` | `NZS` | `Z == 0 && S == 0` | 1716 |
| `0x04` | `NC` | `!C` | 1720 |
| `0x08` | `NT0` | `!T0` | 1724 |
| `0x21` | `Z` | `Z` | 1728 |
| `0x22` | `S` | `S` | 1732 |
| `0x23` | `ZS` | `Z \|\| S` | 1736 |
| `0x24` | `C` | `C` | 1740 |
| `0x28` | `T0` | `T0` | 1744 |
| anything else | — | **no operation** (silent `default: break`) | 1748 |

Structure of the condition field: bit 5 (`0x20`) = polarity (1 = test-true, 0 = test-false),
bits 3:0 = flag selector (`1` = Z, `2` = S, `3` = Z-or-S, `4` = C, `8` = T0). Bit 4 (`0x10`) is
unused. Note the asymmetry the code actually implements: `NZS` is `!Z && !S` but `ZS` is
`Z || S`.

#### 3.7.1 `writeloadimdest(u8 num, u32 val)` (`:617-672`)

This is a **different** destination map from `writed1busdest`:

| `num` | Mnemonic (`disloadimdest`, `:2045`) | Effect |
|---|---|---|
| `0x0`-`0x3` | `MC0`-`MC3` | `MD[n][CT[n] & 0x3F] = val`; set `incFlg[n]` |
| `0x4` | `RX` | `RX = val` |
| `0x5` | `PL` | `P.all = (s32)val` |
| `0x6` | `RA0` | `RA0 = val & 0x1FFFFFF` (**masked to 25 bits**) |
| `0x7` | `WA0` | `WA0 = val & 0x1FFFFFF` (**masked to 25 bits**) |
| `0x8`, `0x9` | `??` | logs `"writeloadimdest BAD NUM"` |
| `0xA` | `LOP` | `LOP = (u16)(val & 0x0FFF)` (**12 bits**) |
| `0xB` | `??` | logs BAD NUM — **no `TOP` destination here** (unlike D1-bus) |
| `0xC` | `PC` | `TOP = PC + 1; jmpaddr = val; delayed = 0` — i.e. this both sets the loop-top register and takes a jump |
| `0xD`-`0xF` | `??` | logs BAD NUM — **no `CT` destinations here** |

Also force-completes a pending DSP DMA on entry (`:620-623`).

`MVI …,PC` is therefore the canonical way to enter a `BTM` loop: it records the loop top
(`PC+1`) and branches in one instruction.

**[QUIRK] The disassembler disagrees with the interpreter about the unconditional immediate.**
`ScuDspDisasm` prints `(instruction & 0x1FFFFFF) << 2` (`:2311`) — the value shifted left 2 as
if it were a byte address — whereas the interpreter sign-extends and does not shift (`:1754`).
Trust the interpreter.

### 3.8 Class `11`, bits 29:28 = `00` — DSP DMA

This is the most intricate part of the encoding. Two stages: `ScuExec` latches the instruction
and computes the transfer count (`:1762-1806`); `step_dsp_dma` later dispatches to one of eight
handlers (`:954-998`).

#### 3.8.1 Bit layout

```
 31 30 | 29 28 | 27 ....... 18 | 17 16 15 | 14 | 13 | 12 | 11 | 10 9 8 | 7 ......... 0
  1  1 |  0  0 |    unused     |   add    |  H | CS | DIR|  0 | RAMsel |  count imm / (2:0 = count src)
```

| Field | Bits | Meaning |
|---|---|---|
| `add` | 17:15 | Address increment selector (§3.8.4) |
| `H` (hold) | 14 | `1` → the address register (`RA0` or `WA0`) is **restored** to its pre-transfer value afterwards (`DMAH` in the disassembler, `:2364-2368`) |
| `CS` (count source) | 13 | `0` → count is the 8-bit immediate in bits 7:0; `1` → count is read from Data RAM via bits 2:0 |
| `DIR` | 12 | `0` → **read**: D0-bus → DSP RAM; `1` → **write**: DSP RAM → D0-bus |
| — | 11 | Must be 0 for any handler to match |
| `RAMsel` | 10:8 | `0`-`3` = MD0-MD3, `4` = Program RAM (`disdmaram`, `:2076-2094`) |
| count imm | 7:0 | Transfer count when `CS == 0` |
| count src | 2:0 | Data RAM selector for the count when `CS == 1` |

#### 3.8.2 All eight DMA addressing-mode variants

Dispatch is an if-else chain in `step_dsp_dma` (`:961-992`). Reproduced exactly:

| Handler | Line | Dispatch test | `H` (b14) | `CS` (b13) | `DIR` (b12) | b11 | b10 | Disasm form |
|---|---|---|---|---|---|---|---|---|
| `dsp_dma01` | 674 | `((i>>10)&0x1F) == 0x00` | 0 | 0 imm | 0 read | 0 | **0** | `DMA<add> D0, MCn, #$imm` |
| `dsp_dma02` | 790 | `((i>>10)&0x1F) == 0x04` | 0 | 0 imm | 1 write | 0 | **0** | `DMA<add> MCn, D0, #$imm` |
| `dsp_dma03` | 814 | `((i>>11)&0x0F) == 0x04` | 0 | 1 RAM | 0 read | 0 | any | `DMA<add> D0, {MCn\|PRG}, [s]` |
| `dsp_dma04` | 898 | `((i>>10)&0x1F) == 0x0C` | 0 | 1 RAM | 1 write | 0 | **0** | `DMA<add> MCn, D0, [s]` |
| `dsp_dma05` | 924 | `((i>>11)&0x0F) == 0x08` | 1 | 0 imm | 0 read | 0 | any | `DMAH<add> D0, MCn, #$imm` |
| `dsp_dma06` | 931 | `((i>>10)&0x1F) == 0x14` | 1 | 0 imm | 1 write | 0 | **0** | `DMAH<add> MCn, D0, #$imm` |
| `dsp_dma07` | 938 | `((i>>11)&0x0F) == 0x0C` | 1 | 1 RAM | 0 read | 0 | any | `DMAH<add> D0, {MCn\|PRG}, [s]` |
| `dsp_dma08` | 946 | `((i>>10)&0x1F) == 0x1C` | 1 | 1 RAM | 1 write | 0 | **0** | `DMAH<add> MCn, D0, [s]` |

The four hold variants are literal wrappers:

```c
void dsp_dma05(scudspregs_struct *sc, u32 inst) { u32 saveRa0 = sc->RA0M; dsp_dma01(sc, inst); sc->RA0 = saveRa0; }  // :924
void dsp_dma06(...)                             { u32 saveWa0 = sc->WA0M; dsp_dma02(sc, inst); sc->WA0 = saveWa0; }  // :931
void dsp_dma07(...)                             { u32 saveRa0 = sc->RA0M; dsp_dma03(sc, inst); sc->RA0 = saveRa0; }  // :938
void dsp_dma08(...)                             { u32 saveWa0 = sc->WA0M; dsp_dma04(sc, inst); sc->WA0 = saveWa0; }  // :946
```

Since `RA0M`/`WA0M` are seeded from `RA0`/`WA0` when the DMA instruction issues (`:1803-1804`),
"hold" means the architectural register is rewound to its pre-transfer value while the transfer
itself still walked forward.

**[QUIRK] Encodings that fall through the chain are silently dropped.** Bit 11 must be 0.
Bit 10 must be 0 for variants 01/02/04/06/08. So e.g. a non-hold, immediate-count *read into
Program RAM* (`H=0, CS=0, DIR=0, RAMsel=4` → bits 14:10 = `00001`) matches none of the eight
tests: no handler runs, and `step_dsp_dma` still clears `T0`, `dsp_dma_instruction` and
`dsp_dma_wait` at `:994-996` as though it had. Program RAM can only be loaded by DSP DMA
through `dsp_dma03` / `dsp_dma07` (the count-from-RAM read forms).

**[QUIRK] `dsp_dma05` accepts `RAMsel == 4` but forwards to `dsp_dma01`,** which computes
`sel = (inst >> 8) & 0x03` (`:677`) — so `PRG` degrades silently to `MD0`.

#### 3.8.3 Transfer count

Computed in `ScuExec` at issue time (`:1773-1801`), *not* in the handlers:

- **`CS == 0`** (variants 01/02/05/06): `Counter = instruction & 0xFF`. A count of 0 performs
  **zero** transfers — the loops are `for (i = 0; i < imm; i++)` (`:689, 699, 727, …`); there is
  no "0 means 256" rule in this code.
- **`CS == 1`** (variants 03/04/07/08): `switch (instruction & 0x7)` (`:1787-1797`):

| bits 2:0 | Count value | `CT` post-increment |
|---|---|---|
| `0` | `MD[0][CT[0] & 0x3F]` | no |
| `1` | `MD[1][CT[1] & 0x3F]` | no |
| `2` | `MD[2][CT[2] & 0x3F]` | no |
| `3` | `MD[3][CT[3] & 0x3F]` | no |
| `4` | `MD[0][CT[0] & 0x3F]` | **yes** (`CT[0]++ & 0x3F`) |
| `5` | `MD[1][CT[1] & 0x3F]` | **yes** |
| `6` | `MD[2][CT[2] & 0x3F]` | **yes** |
| `7` | `MD[3][CT[3] & 0x3F]` | **yes** |

The result is stored in `dsp_dma_size` (`:1801`) and consumed by `dsp_dma03`/`dsp_dma04`
(`:816, 900`). Variants 01/02 re-derive the immediate from the instruction themselves and
ignore `dsp_dma_size`.

#### 3.8.4 Address increment (`add`, bits 17:15) — read vs. write differ

**Read side** (`dsp_dma01` `:681-682`, `dsp_dma03` `:823-824`):

```c
const u32 mode = (inst >> 15) & 0x7;
const u32 add  = (1 << (mode & 0x2)) & ~1;   // 0 or 4
...
sc->RA0M += (add >> 2);                       // 0 or 1 long-word
```

`mode & 0x2` is **instruction bit 16**. So the read pointer advances by 1 long-word if bit 16
is set, else not at all. Bits 17 and 15 are ignored on the read side.

**[QUIRK] The disassembler contradicts this.** `ScuDspDisasm` (`:2321-2324`) uses bit **15**
for reads (`addressAdd = (instruction >> 15) & 0x1` when bit 12 is clear) and the full 3-bit
field for writes. The interpreter uses bit **16** for reads. They cannot both be right; the
interpreter is what actually moves data.

**Write side** (`dsp_dma02` `:798-808`, `dsp_dma04` `:906-916`) — full 3-bit table, in
long-word units:

| bits 17:15 | `add` (long-words) | bytes |
|---|---|---|
| 0 | 0 | 0 |
| 1 | 1 | 4 |
| 2 | 2 | 8 |
| 3 | 4 | 16 |
| 4 | 8 | 32 |
| 5 | 16 | 64 |
| 6 | 32 | 128 |
| 7 | 64 | 256 |

#### 3.8.5 Read transfers — `dsp_dma01` (`:674-711`) and `dsp_dma03` (`:814-896`)

Bus classification (`:687-688`, `:828-829`):

```c
u32 abus_check = ((sc->RA0M << 2) & 0x0FF00000);
if (abus_check >= 0x02000000 && abus_check < 0x05900000) { /* A-Bus */ } else { /* other */ }
```

Note the mask keeps only address bits 27:20, so this is a coarse region test.

`dsp_dma01` body (identical in both branches — the A-Bus test has **no behavioural effect** here):

```c
sc->MD[sel][sc->CT[sel] & 0x3F] = MappedMemoryReadLong((sc->RA0M << 2), NULL);
sc->CT[sel]++;  sc->CT[sel] &= 0x3F;
sc->RA0M += (add >> 2);
```
with `sel = (inst >> 8) & 0x03` (`:677`). Afterwards: `T0 = 0`, `RA0 = RA0M` (`:709-710`).

`dsp_dma03` body: `sel = (inst >> 8) & 0x7` (`:820`); when `sel == 0x04` the destination is
`ProgramRam[index]` with a **local `index` starting at 0** (`:832-836`, `:851-854`) — i.e. a
DSP-DMA program load always writes from program address 0 and ignores `PC`. Otherwise it writes
`MD[sel][CT[sel] & 0x3F]` with `CT` post-increment. Afterwards: `T0 = 0` (`:895`).

**[QUIRK] `dsp_dma03` only writes back `RA0` in the non-A-Bus branch** (`:864`, inside the
`else`). An A-Bus source read via `dsp_dma03` leaves `RA0` at its old value even without the
hold bit.

#### 3.8.6 Write transfers — `dsp_dma_write_d0bus` (`:715-788`)

Shared by `dsp_dma02` and `dsp_dma04`. Signature `(sc, int sel, int add, int count)`;
`Adr = (sc->WA0M << 2) & 0x0FFFFFFF` (`:718`). Three destination classes:

| Class | Range test | `add` fixup | Per-iteration behaviour | Line |
|---|---|---|---|---|
| **A-Bus** | `0x02000000 ≤ Adr < 0x05A00000` | `if (add > 1) add = 1;` (clamped) | `Adr = WA0M << 2`; `MappedMemoryWriteLong(Adr, MD[sel][CT[sel]&0x3F])`; `CT[sel]++`; `WA0M += add` | 723-736 |
| **B-Bus** | `0x05A00000 ≤ Adr < 0x06000000` | `if (add == 0) add = 1;` | two 16-bit writes: `WriteWord(Adr, Val>>16)`, `WriteWord(Adr+2, Val)`; `CT[sel]++`; `Adr += (add << 2)`. **`WA0M += add * count` once at the end** | 739-753 |
| **CPU bus** (everything else) | — | `if (add == 0) add = 1;` | `T2WriteLong(HighWram, (WA0M << 2) & 0xFFFFC, Val)`; `CT[sel]++`; `WA0M += 1` if `add == 1`, else `WA0M += (add >> 1)` | 755-783 |

Afterwards: `WA0 = WA0M`, `T0 = 0` (`:785-786`).

**[QUIRK] The CPU-bus path writes straight into `HighWram`** via `T2WriteLong` with a
`0xFFFFC` mask, bypassing `MappedMemoryWriteLong` entirely. Any destination outside the A-Bus
and B-Bus ranges — including Low Work RAM at `0x00200000` — is redirected into High Work RAM.

**[QUIRK] The CPU-bus `add >> 1` stride** (`:779`) has no counterpart in the A-Bus or B-Bus
paths and no explanation in the source.

#### 3.8.7 DSP DMA timing and the `T0` flag

At issue (`:1762-1806`):

1. If a previous DSP DMA is still pending (`dsp_dma_wait > 0`), it is force-completed first
   (`:1765-1768`).
2. `dsp_dma_instruction = instruction`; `ProgControlPort.part.T0 = 1` (`:1770-1771`).
3. Count computed (§3.8.3) into `dsp_dma_size`.
4. `dsp_dma_wait = 2` (`:1802`) — the transfer is deferred by two `step_dsp_dma` calls.
5. `WA0M = WA0; RA0M = RA0` (`:1803-1804`).

`step_dsp_dma` (`:954-998`):

```c
if (sc->ProgControlPort.part.T0 == 0) return;
sc->dsp_dma_wait--;
if (sc->dsp_dma_wait > 0) return;
/* ... dispatch to dsp_dma01..08 ... */
sc->ProgControlPort.part.T0 = 0;
sc->dsp_dma_instruction = 0;
sc->dsp_dma_wait = 0;
```

It is called from three places:

- Once per DSP instruction, *before* the fetch, if `T0 != 0` (`:1378-1380`).
- From `readgensrc` (`:500-503`), `writed1busdest` (`:555-558`) and `writeloadimdest`
  (`:620-623`), each of which sets `dsp_dma_wait = 0` first so the `> 0` test fails and the
  transfer completes immediately. This models "any DSP RAM access while a DMA is in flight
  forces the DMA to finish".

`T0` is therefore the **"D0-bus DMA in progress"** flag (`scu.h:196`: *"D0 bus use DMA execute
flag"*). It is testable by `JMP T0/NT0` and `MVI …,T0/NT0`, and readable through the Program
Control Port (bit 23).

### 3.9 CPU-side DSP ports

| Offset | Direction | Behaviour | Line |
|---|---|---|---|
| `0x80` | read | `return ScuDsp->ProgControlPort.all & 0x00FD00FF` | 2726-2727 |
| `0x80` | write | `all = (all & 0x00FC0000) \| (val & 0x060380FF)`; then if `LE` → `PC = P`; if `val & 0x10000` (EX) → `jmpaddr = 0xFFFFFFFF` | 2901-2921 |
| `0x84` | write | `ProgramRam[PC] = val; PC++; ProgControlPort.part.P = PC` | 2922-2927 |
| `0x84` | read | **unhandled** (returns 0) | 2742 |
| `0x88` | write | `DataRamPage = (val >> 6) & 3; DataRamReadAddress = val & 0x3F` | 2928-2932 |
| `0x88` | read | **unhandled** | 2742 |
| `0x8C` | read | `if (!EX) return MD[DataRamPage][DataRamReadAddress++]; else return 0;` | 2728-2732 |
| `0x8C` | write | `if (!EX) { MD[DataRamPage][DataRamReadAddress] = val; DataRamReadAddress++; }` | 2933-2939 |

#### 3.9.1 Program Control Port bit layout (`scu.h:184-203`)

| Bit | Name | Meaning (comment in `scu.h`) | Readable via 0x80? | Writable via 0x80? |
|---|---|---|---|---|
| 7:0 | `P` | Program RAM address | yes (`0xFF`) | yes (`0xFF`) |
| 14:8 | `unused3` | — | no | no |
| 15 | `LE` | Program counter load enable | **no** | yes (`0x8000`) |
| 16 | `EX` | Program execute control | yes | yes (`0x10000`) |
| 17 | `ES` | Program step execute control | **no** | yes (`0x20000`) |
| 18 | `E` | Program end interrupt flag | yes | no (preserved) |
| 19 | `V` | Overflow flag | yes | no (preserved) |
| 20 | `C` | Carry flag | yes | no (preserved) |
| 21 | `Z` | Zero flag | yes | no (preserved) |
| 22 | `S` | Sign flag (`scu.h` says "Sine flag") | yes | no (preserved) |
| 23 | `T0` | D0-bus DMA execute flag | yes | no (preserved) |
| 24 | `unused2` | — | no | no |
| 25 | `EP` | Temporary stop execution flag | no | yes (`0x2000000`) |
| 26 | `PR` | Pause cancel flag | no | yes (`0x4000000`) |
| 31:27 | `unused1` | — | no | no |

Read mask `0x00FD00FF` = bits 23,22,21,20,19,18,16 and 7:0.
Write preserve mask `0x00FC0000` = bits 23:18 (the flags). Write accept mask `0x060380FF` =
bits 26,25,17,16,15 and 7:0.

**[QUIRK] `EP` and `PR` are storage only.** Nothing in `scu.c` reads `part.EP` or `part.PR`;
pause/step control is not implemented. `ES` is likewise never tested — single-stepping is done
by the debugger calling `ScuDspStep()` → `ScuExec(1)` (`:2457-2460`).

#### 3.9.2 Program load protocol

1. Write the Program Control Port with `LE = 1` (bit 15) and `P` = desired start address →
   `PC` is set from `P` (`:2905-2909`).
2. Write each 32-bit instruction word to `0x84`; `PC` auto-increments and is mirrored back into
   `ProgControlPort.part.P` (`:2924-2926`).
3. Write the Program Control Port again with `LE = 1`, `P` = entry point, `EX = 1` to run.
   Setting `EX` also clears `jmpaddr` to `0xFFFFFFFF` (`:2912-2915`).

**[QUIRK] The program-load cursor and the execution PC are the same register.** There is no
separate program-write pointer, so loading a program leaves `PC` pointing past the end.

#### 3.9.3 Data RAM port protocol

1. Write `0x88` with `(page << 6) | offset` — page in bits 7:6, offset in bits 5:0.
2. Read or write `0x8C` repeatedly; `DataRamReadAddress` post-increments on both.
3. **Both directions are gated on `!EX`** — while the DSP is executing, reads return 0 and
   writes are dropped.

**[QUIRK] `DataRamReadAddress` is never re-masked to 6 bits** after the initial
`val & 0x3F`. It is a `u8` that increments freely up to 255, and the index expression is
`MD[DataRamPage][DataRamReadAddress]` on a `u32 MD[4][64]` — so sequential access past offset
63 walks into the following pages and, with `DataRamPage == 3`, past the end of the array
entirely. Real hardware presumably wraps within the 64-word page; this code does not.

### 3.10 Class `11`, bits 29:28 = `01` — JMP (`:1808-1901`)

```
 31 30 | 29 28 | 27 26 | 25 24 23 22 21 20 19 | 18 ... 8 | 7 ... 0
  1  1 |  0  1 |   -   |     condition (7)    |  unused  | target
```

- Target address: `instruction & 0xFF` → `jmpaddr` (8-bit, into the 256-word program RAM).
- Condition field: `(instruction >> 19) & 0x7F` (bits 25:19).
- **Guard:** `if (ScuDsp->jmpaddr != 0xffffffff) break;` (`:1809-1811`) — if a branch is already
  pending, the jump instruction is ignored entirely.

| Condition code | Mnemonic | Test | Line |
|---|---|---|---|
| `0x00` | `JMP Imm` (unconditional) | always | 1813 |
| `0x41` | `JMP NZ` | `!Z` | 1817 |
| `0x42` | `JMP NS` | `!S` | 1824 |
| `0x43` | `JMP NZS` | `Z == 0 && S == 0` | 1833 |
| `0x44` | `JMP NC` | `!C` | 1842 |
| `0x48` | `JMP NT0` | `!T0` | 1849 |
| `0x61` | `JMP Z` | `Z` | 1858 |
| `0x62` | `JMP S` | `S` | 1865 |
| `0x63` | `JMP ZS` | `Z \|\| S` | 1874 |
| `0x64` | `JMP C` | `C` | 1883 |
| `0x68` | `JMP T0` | `T0` | 1890 |
| anything else | — | logs `"Unknown JMP instruction not implemented"` | 1897 |

Field structure: bit 6 of the condition (= instruction bit 25) is the "conditional" flag,
bit 5 (= instruction bit 24) is polarity, bits 3:0 select the flag — exactly the same encoding
as MVI's condition field shifted by one bit position, which is why MVI uses 6 bits and JMP 7.

**Delay slot** (`:1956-1967`), applied after `PC++`:

```c
if (ScuDsp->jmpaddr != 0xFFFFFFFF) {
   if (ScuDsp->delayed) {
      ScuDsp->PC = (unsigned char)ScuDsp->jmpaddr;
      ScuDsp->jmpaddr = 0xFFFFFFFF;
      dsp_counter += 1;                 // "hold clock" — the branch itself is free
   } else
      ScuDsp->delayed = 1;
}
```

So every taken branch executes **one delay-slot instruction** before the PC actually changes.
`jmpaddr == 0xFFFFFFFF` is the "no branch pending" sentinel; it is initialised only when the
CPU sets `EX` (`:2914`).

### 3.11 Class `11`, bits 29:28 = `10` — LPS / BTM (`:1902-1924`)

Bit 27 (`instruction & 0x8000000`) selects:

| Bit 27 | Mnemonic | Action |
|---|---|---|
| 1 | `LPS` | `if (LOP != 0) { jmpaddr = PC; delayed = 0; LOP--; }` |
| 0 | `BTM` | `if (LOP != 0) { jmpaddr = TOP; delayed = 0; LOP--; }` |

`PC` here is the address of the `LPS` instruction itself (the increment happens later at
`:1954`), so `LPS` re-executes itself plus its delay-slot instruction. `BTM` branches to `TOP`,
which is loaded either by `MOV …,TOP` (D1-bus destination `0xB`) or implicitly by
`MVI …,PC` (`TOP = PC + 1`, `:664`).

`LOP` is a 12-bit counter when loaded by MVI (`:661`) and a full 16-bit one when loaded over the
D1-bus (`:590`). Both loop forms decrement it by one per iteration and stop at 0.

### 3.12 Class `11`, bits 29:28 = `11` — END / ENDI (`:1925-1937`)

```c
ScuDsp->ProgControlPort.part.EX = 0;
if (instruction & 0x8000000) {        // bit 27 → ENDI
   ScuDsp->ProgControlPort.part.E = 1;
   ScuSendDSPEnd();                    // vector 0x45
}
ScuDsp->ProgControlPort.part.P = ScuDsp->PC + 1;
dsp_counter = 1;                       // then decremented at :1968 → loop exits
```

`END` stops execution silently; `ENDI` additionally sets the `E` flag and raises the DSP End
interrupt. Note `E` is never cleared by `scu.c` — the Program Control Port write mask preserves
bit 18 (`:2903`), so the flag is sticky until a savestate load or `ScuReset`.

### 3.13 DSP execution loop (`ScuExec`, `:1351-1970`)

```c
if (ScuDsp->ProgControlPort.part.EX) {
   s32 dsp_counter = (s32)timing;
   while (dsp_counter > 0) {
      /* breakpoint scan (:1370-1376) */
      if (ScuDsp->ProgControlPort.part.T0 != 0) step_dsp_dma(ScuDsp);   // :1378
      instruction = ScuDsp->ProgramRam[ScuDsp->PC];                     // :1382
      incFlg[0..3] = 0;                                                 // :1384
      ScuDsp->ALU.all = ScuDsp->AC.all;                                 // :1389
      /* ALU switch, class switch */
      /* apply incFlg → CT[n]++ & 0x3f  (:1949-1952) */
      ScuDsp->PC++;                                                     // :1954
      /* delayed branch resolution (:1957-1967) */
      dsp_counter--;
   }
}
```

One instruction per unit of `timing`, except that a resolved branch refunds one unit
(`dsp_counter += 1`, `:1963`). `ES` (single-step) is not consulted.

---

## 4. Interrupt controller

### 4.1 Complete interrupt source table

Every source is a `ScuSend*` function that calls
`SendInterrupt(vector, level, mask, statusbit)` (`:3101`). Extracted verbatim from
`:3236-3481`:

| Source | Function (line) | Vector | Level | `IMS` mask bit | `IST` status bit | Starts DMA factor |
|---|---|---|---|---|---|---|
| V-Blank IN | `ScuSendVBlankIN` (3236) | `0x40` | `0xF` (15) | `0x0001` (b0) | `0x00000001` | 0 |
| V-Blank OUT | `ScuSendVBlankOUT` (3250) | `0x41` | `0xE` (14) | `0x0002` (b1) | `0x00000002` | 1 |
| H-Blank IN | `ScuSendHBlankIN` (3281) | `0x42` | `0xD` (13) | `0x0004` (b2) | `0x00000004` | 2 |
| Timer 0 | `ScuSendTimer0` (3308) | `0x43` | `0xC` (12) | `0x0008` (b3) | `0x00000008` | 3 |
| Timer 1 | `ScuSendTimer1` (3315) | `0x44` | `0xB` (11) | `0x0010` (b4) | `0x00000010` | 4 |
| DSP End | `ScuSendDSPEnd` (3333) | `0x45` | `0xA` (10) | `0x0020` (b5) | `0x00000020` | — |
| Sound Request | `ScuSendSoundRequest` (3339) | `0x46` | `0x9` (9) | `0x0040` (b6) | `0x00000040` | 5 |
| System Manager (SMPC) | `ScuSendSystemManager` (3346) | `0x47` | `0x8` (8) | `0x0080` (b7) | `0x00000080` | — |
| Pad Interrupt | `ScuSendPadInterrupt` (3352) | `0x48` | `0x8` (8) | `0x0100` (b8) | `0x00000100` | — |
| Level 2 DMA End | `ScuSendLevel2DMAEnd` (3358) | `0x49` | `0x6` (6) | `0x0200` (b9) | `0x00000200` | — |
| Level 1 DMA End | `ScuSendLevel1DMAEnd` (3364) | `0x4A` | `0x6` (6) | `0x0400` (b10) | `0x00000400` | — |
| Level 0 DMA End | `ScuSendLevel0DMAEnd` (3370) | `0x4B` | `0x5` (5) | `0x0800` (b11) | `0x00000800` | — |
| DMA Illegal | `ScuSendDMAIllegal` (3376) | `0x4C` | `0x3` (3) | `0x1000` (b12) | `0x00001000` | — (**never called**) |
| Sprite Draw End | `ScuSendDrawEnd` (3382) | `0x4D` | `0x2` (2) | `0x2000` (b13) | `0x00002000` | 6 |
| External 00 | (3389) | `0x50` | `0x7` (7) | `0x8000` (b15) | `0x00010000` (b16) | — |
| External 01 | (3395) | `0x51` | `0x7` | `0x8000` | `0x00020000` | — |
| External 02 | (3401) | `0x52` | `0x7` | `0x8000` | `0x00040000` | — |
| External 03 | (3407) | `0x53` | `0x7` | `0x8000` | `0x00080000` | — |
| External 04 | (3413) | `0x54` | `0x4` (4) | `0x8000` | `0x00100000` | — |
| External 05 | (3419) | `0x55` | `0x4` | `0x8000` | `0x00200000` | — |
| External 06 | (3425) | `0x56` | `0x4` | `0x8000` | `0x00400000` | — |
| External 07 | (3431) | `0x57` | `0x4` | `0x8000` | `0x00800000` | — |
| External 08 | (3437) | `0x58` | `0x1` (1) | `0x8000` | `0x01000000` | — |
| External 09 | (3443) | `0x59` | `0x1` | `0x8000` | `0x02000000` | — |
| External 10 | (3449) | `0x5A` | `0x1` | `0x8000` | `0x04000000` | — |
| External 11 | (3455) | `0x5B` | `0x1` | `0x8000` | `0x08000000` | — |
| External 12 | (3461) | `0x5C` | `0x1` | `0x8000` | `0x10000000` | — |
| External 13 | (3467) | `0x5D` | `0x1` | `0x8000` | `0x20000000` | — |
| External 14 | (3473) | `0x5E` | `0x1` | `0x8000` | `0x40000000` | — |
| External 15 | (3479) | `0x5F` | `0x1` | `0x8000` | `0x80000000` | — |

**`IMS` bits 14 and 0x4000** are used by nothing. `IST` bits 14-15 are used by nothing.
The 16 external (A-Bus) interrupts all share the single mask bit 15 (`0x8000`) but have
distinct status bits 16-31 and four distinct priority levels (7, 4, 1, 1).

`ScuGetVectorString` (`:3198-3233`) names only a subset for logging: `0x40` VBlankIN,
`0x41` VBlankOUT, `0x42` HBlankIN, `0x43` Timer0, `0x44` Timer1, `0x45` DSP End,
`0x47` SmpcINTBACK, `0x49` DMA2 End, `0x4A` DMA1 End, `0x4B` DMA0 End, `0x4D` DrawEnd.

### 4.2 Dispatch — `SendInterrupt` (`:3101-3128`)

```c
if (mask & 0x8000) {                    // A-Bus / external
   if (ScuRegs->AIACK) {
      ScuRegs->AIACK = 0;
      if (!(ScuRegs->IMS & 0x8000)) SH2SendInterrupt(MSH2, vector, level);
   }
} else if (!(ScuRegs->IMS & mask)) {    // unmasked → deliver now
   SH2SendInterrupt(MSH2, vector, level);
} else {                                // masked → queue and latch status
   ScuQueueInterrupt(vector, level, mask, statusbit);
   ScuRegs->IST |= statusbit;
}
if (yabsys.IsSSH2Running) {
   if (vector == 0x42) SH2SendInterrupt(SSH2, 0x41, 1);   // HBlankIN mirrored to slave
   if (vector == 0x40) SH2SendInterrupt(SSH2, 0x43, 2);   // VBlankIN mirrored to slave
}
```

Three consequences worth stating plainly:

1. **`IST` only ever latches *masked* interrupts.** An interrupt delivered immediately never
   sets its status bit. `IST` is a "pending because masked" register in this implementation,
   not a general "this happened" register.
2. **External interrupts are dropped when `AIACK` is 0.** They are not queued and do not set
   `IST` — they simply vanish.
3. **The slave SH-2 gets fixed re-mapped vectors** for V-Blank IN and H-Blank IN only
   (`0x43` level 2, `0x41` level 1), and this happens regardless of mask state.

### 4.3 The pending queue

`ScuQueueInterrupt` (`:3066-3097`) appends to `ScuRegs->interrupts[30]`:

- **Dedupe by vector** — if the same vector is already queued, the call returns without
  re-queuing (`:3072-3076`), so `IST |= statusbit` in `SendInterrupt` still happens but the
  queue entry is not duplicated.
- After appending, the whole array is bubble-sorted **ascending by `level`** (`:3085-3096`).
- No bounds check against 30; `NumberOfInterrupts` can in principle run past the array.

`ScuTestInterruptMask` (`:3014-3063`) is the drain, called on writes to `IMS` (`:2954`),
`IST` (`:2961`, `:2758`) and `AIACK` (`:2966`). It walks the sorted array **from the end**
(index `NumberOfInterrupts - 1 - i`), i.e. **highest level first**:

- For an A-Bus entry (`mask & 0x8000`): if `AIACK` is set → clear `AIACK`, and if `IMS` bit 15
  is clear, deliver to MSH2, clear the `IST` bit, compact the array, `NumberOfInterrupts--`.
  **No `break`** — the loop continues with indices that the compaction has invalidated.
- Otherwise, if `!(IMS & mask)`:
  - If the entry's `IST` bit has already been cleared by the CPU → skip it silently
    (`:3042-3046`) — the entry stays in the queue.
  - Else deliver to MSH2, clear the `IST` bit, compact the array, `NumberOfInterrupts--`,
    and **`break`** (`:3059`) — only one interrupt is delivered per call through this path.

So unmasking `IMS` releases at most one queued non-external interrupt per register write.

### 4.4 Acknowledging / clearing

| Action | Effect |
|---|---|
| Long write to `IST` (`0xA4`) | `IST = IST & val` — write 0s to the bits you want to clear (`:2957-2959`), then `ScuRemoveInterruptByCPU` and `ScuTestInterruptMask` |
| Byte write to `IST` low byte (`0xA7`) | `IST = IST & (0xFFFFFF00 \| val)` — same AND semantics, low 8 bits only (`:2755-2757`) |
| Delivery via `ScuTestInterruptMask` | `IST &= ~statusbit` (`:3029`, `:3052`) |
| Delivery via `SendInterrupt` unmasked path | `IST` untouched (it was never set) |

**[BUG] `ScuRemoveInterruptByCPU` is dead code** (`:2988-3012`). Its guard reads

```c
if (((pre >> i) & 0x01) && ((after >> i) & 0x01 == 0)) {
```

C precedence makes `0x01 == 0` evaluate first (to `0`), so the second operand is
`(after >> i) & 0` = `0`, and the condition is **always false**. The function therefore never
removes anything from the pending queue. A second defect inside the (unreachable) body uses the
outer loop variable `i` where `ii` was intended: `ScuRegs->interrupts[i].statusbit == (1 << i)`
(`:2994`). Clearing an `IST` bit that has a queued entry consequently leaves the entry in the
queue; `ScuTestInterruptMask` then skips it forever at `:3042` without removing it.

### 4.5 `AIACK` (0xA8) — A-Bus interrupt acknowledge

A one-shot gate for all 16 external interrupts. Written by the CPU (`:2964-2967`, which also
runs `ScuTestInterruptMask`), consumed and cleared by `SendInterrupt` (`:3105-3110`) and
`ScuTestInterruptMask` (`:3025-3037`). Readable at `0xA8` (`:2736`). If `AIACK == 0` when an
external interrupt arrives, that interrupt is lost.

### 4.6 What is *not* modelled

- No interrupt is ever withdrawn from the SH-2: `ScuRemoveVBlankIN/Out`, `ScuRemoveHBlankIN`,
  `ScuRemoveTimer0/1` (`:3243, 3268, 3275, 3320, 3326`) all have their bodies commented out,
  and `ScuRemoveInterrupt(u8, u8)` is declared at `:3190` but never defined or called in this
  file.
- Only the **master** SH-2 receives SCU interrupts through the normal path; the slave gets the
  two hard-wired mirrors described in §4.2.

---

## 5. Timers 0 and 1

### 5.1 Registers

| Offset | Name | Written to | Notes |
|---|---|---|---|
| `0x90` | `T0C` | `ScuRegs->T0C` (`:2941`) | Timer 0 compare value — a **scanline number** |
| `0x94` | `T1S` | `ScuRegs->T1S`, plus `timer1_set = 1` and `timer1_preset = val` (`:2943-2947`) | Timer 1 reload value |
| `0x98` | `T1MD` | `ScuRegs->T1MD` (`:2949`) | Mode |

`T1MD` bits actually used:

| Bit | Meaning | Line |
|---|---|---|
| 0 | **Global timer enable.** All timer 0 comparisons and the timer 1 reload are gated on it | 1311, 3254, 3285 |
| 7 | Timer 1 mode: `0` = fire every line; `1` = fire only when timer 0 also matched (`timer0_set == 1`) | 1006-1011 |

No other `T1MD` bit is read anywhere.

Internal state (`scu.h:130-137`): `timer0` (current line counter), `timer1`,
`timer1_counter` (down-counter), `timer0_set`, `timer1_set`, `timer1_preset`.

### 5.2 Timer 0 — counts scanlines, compares against `T0C`

- **Counted event:** H-Blank IN. `ScuSendHBlankIN` does `ScuRegs->timer0++` (`:3284`).
- **Reset:** V-Blank OUT sets `ScuRegs->timer0 = 0` (`:3253`).
- **Compare (H-Blank path, `:3285-3296`)**, only when `T1MD & 1`:
  ```c
  if (ScuRegs->timer0 == ScuRegs->T0C) { ScuSendTimer0(); ScuRegs->timer0_set = 1; }
  else                                 { ScuRegs->timer0_set = 0; }
  ```
- **Compare (V-Blank OUT path, `:3254-3264`)**: the same test is run immediately after
  `timer0 = 0`, so `T0C == 0` fires Timer 0 at V-Blank OUT.
- **On match:** `ScuSendTimer0()` → vector `0x43`, level `0xC`, mask `0x0008`
  (`:3308-3311`), and `ScuChekIntrruptDMA(3)` — any DMA level armed on start factor 3 begins.
- `timer0_set` is the "timer 0 matched on this line" flag consumed by timer 1 mode 1.

### 5.3 Timer 1 — down-counter reloaded each line

- **Reload:** in `ScuSendHBlankIN` (`:3297-3301`), when `T1MD & 1` and `timer1_set == 1`:
  `timer1_set = 0; timer1_counter = timer1_preset;`. `timer1_preset` comes from the last write
  to `T1S`.
- **Count down:** `ScuTimer1Exec(timing)` (`:1001-1014`):
  ```c
  if (ScuRegs->timer1_counter > 0) {
     ScuRegs->timer1_counter -= (timing >> 1);
     if (ScuRegs->timer1_counter <= 0) {
        ScuRegs->timer1_set = 1;
        if ((ScuRegs->T1MD & 0x80) == 0)      ScuSendTimer1();
        else if (ScuRegs->timer0_set == 1)    ScuSendTimer1();
     }
  }
  ```
  The decrement is `timing >> 1`, and `timing` is already `sh2cycles >> 1`
  (`yabause/src/yabause.c:829`) — so `timer1_counter` ticks at roughly one unit per four SH-2
  cycles in this model.
- **On expiry:** `timer1_set = 1` (which arms the reload at the next H-Blank IN) and, subject
  to `T1MD` bit 7, `ScuSendTimer1()` → vector `0x44`, level `0xB`, mask `0x0010`
  (`:3315-3318`), plus `ScuChekIntrruptDMA(4)`.

### 5.4 The gate in `ScuExec`

```c
if (ScuRegs->T1MD & 0x1) {
   if (ScuRegs->T1MD & 0x80 == 0) {                                     // :1312
      ScuTimer1Exec(timing);
   } else {
      if (yabsys.LineCount == ScuRegs->T0C || ScuRegs->T0C > 500) {     // :1316
         ScuTimer1Exec(timing);
      }
   }
}
```

**[BUG] `ScuRegs->T1MD & 0x80 == 0` is a precedence error.** `0x80 == 0` evaluates to `0`
first, so the expression is `T1MD & 0` = `0` — always false. The `if` branch is unreachable and
`ScuTimer1Exec` is **only ever called from the `else` branch**, i.e. only on the scanline where
`yabsys.LineCount == T0C`, or unconditionally when `T0C > 500`. The intended reading was
presumably "if T1MD bit 7 is clear, tick every time". Note this is a *second, independent* gate
on top of the `T1MD & 0x80` test inside `ScuTimer1Exec` itself (`:1006`), which is written
correctly.

---

## 6. Savestates

`ScuSaveState` (`:3485-3502`) writes the tag `"SCU "` version 4, then the raw `Scu` struct, the
raw `scudspregs_struct`, and the four `incFlg` ints. `ScuLoadState` (`:3506-3564`) handles three
legacy layouts: versions `< 3` omit the three `scudmainfo_struct`s and the six trailing DSP DMA
fields; version 3 has the DMA structs but not the DSP DMA fields; version `>= 4` is the full
layout. `incFlg` is only present from version 2 onward. This is relevant to Mimas only as
evidence of which fields are considered *architectural* state: everything in `Scu` and
`scudspregs_struct`, plus the four `incFlg` deferred-increment flags.

---

## 7. Summary of divergences to be aware of when porting

| # | Item | Where |
|---|---|---|
| 1 | DSP read-DMA address increment uses instruction **bit 16**; the disassembler uses bit 15 | §3.8.4 |
| 2 | DSP DMA encodings with bit 11 set, or bit 10 set on variants 01/02/04/06/08, silently do nothing | §3.8.2 |
| 3 | DSP DMA to Program RAM only works through `dsp_dma03`/`dsp_dma07`; `dsp_dma05` degrades PRG → MD0 | §3.8.2 |
| 4 | DSP DMA CPU-bus writes go directly into `HighWram`, whatever the address | §3.8.6 |
| 5 | `dsp_dma03` does not write back `RA0` on the A-Bus path | §3.8.5 |
| 6 | Data RAM host port never re-masks its 6-bit offset; runs off the end of the page | §3.9.3 |
| 7 | DSP `V` (overflow) flag is never set by any ALU op | §3.3 |
| 8 | DSP `RR`/`RL` are plain rotates, not rotate-through-carry | §3.3 |
| 9 | ALU ops `0x7`, `0xC`, `0xD`, `0xE` have no implementation | §3.3 |
| 10 | `EP`, `PR`, `ES` control bits are stored but never acted on | §3.9.1 |
| 11 | DMA copy mode ignores `ReadAdd` for the source stride | §2.4 |
| 12 | No DMA priority arbitration and no bus locking whatsoever | §2.6 |
| 13 | `DSTP` (DMA stop) is inert | §2.6 |
| 14 | `ScuSendDMAIllegal` (vector `0x4C`) is never raised | §2.8 |
| 15 | `IST` latches only *masked* interrupts | §4.2 |
| 16 | External interrupts with `AIACK == 0` are dropped, not queued | §4.2 |
| 17 | `ScuRemoveInterruptByCPU` is dead code (precedence bug) and leaks stale queue entries | §4.4 |
| 18 | Timer 1 gate in `ScuExec` is inverted by a precedence bug | §5.4 |
| 19 | `IMS`, `DnAD`, `DnEN`, `DnMD`, `DSTP`, `T0C`, `T1S`, `T1MD`, `ASR0/1`, `AREF` have no read handlers | §1.1 |
| 20 | All 16-bit accesses to the SCU register block are no-ops | §1.1 |
