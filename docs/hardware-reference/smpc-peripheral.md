# SMPC — System Manager & Peripheral Control (+ the peripheral protocol)

**Source of truth.** Everything in this document is derived *exclusively* from the Yabause
(YabaSanshiro fork) C source:

- `yabause/src/smpc.c` (991 lines)
- `yabause/src/smpc.h` (116 lines)
- `yabause/src/peripheral.c` (1034 lines)
- `yabause/src/peripheral.h` (314 lines)

A handful of facts that these four files cannot supply on their own are marked inline and cited
to their own file: the SMPC register block's base address (`yabause/src/memory.c:615`), the
cadence and unit of the `SmpcExec` argument (`yabause/src/yabause.c:785, 833-835`), what
`YabauseStartSlave`/`YabauseStopSlave`/`M68KStart`/`M68KStop`/`SH2NMI`/`ScuSendSystemManager`
actually do (`yabause.c:942-995`, `scsp.c:5213-5229`, `sh2core.c:252-268`, `scu.c:3346-3348`),
the one consumer of `EXLE` (`vdp2.cpp:1428-1434`), and how a front end populates the port data
(`libretro/libretro.c:188-217`).

**No outside Saturn documentation was used.** Where the code is ambiguous, self-contradictory,
or plainly wrong, that is stated rather than smoothed over. Notes are tagged **[QUIRK]**
(deliberate emulator shortcut / hardware behaviour not modelled), **[BUG]** (a defect in the C
source), **[HACK]** (a game-specific or TAS-specific special case), or **[DEAD]** (code or
state that can never affect anything). Anything the source does not implement is called out as
*not implemented in this source* rather than guessed at.

Line citations are of the form `yabause/src/smpc.c:482`. They point at the code that
establishes the claim.

---

## 0. Structural overview

### 0.1 The three globals

| Global | Type | Declared | Contents |
|---|---|---|---|
| `SmpcRegs` | `Smpc *` | `smpc.c:65`, `smpc.h:40-52` | The 64-byte memory-mapped register file |
| `SmpcRegsT` | `u8 *` | `smpc.c:66` | The *same* allocation, viewed as a flat byte array — this is what the read/write accessors index |
| `SmpcInternalVars` | `SmpcInternal *` | `smpc.c:67`, `smpc.h:64-86` | All non-register state: latched control bits, RTC base, SMEM, the INTBACK state machine, and the two port-data snapshots |

Plus three file-scope variables (`smpc.c:68-70`):

| Variable | Role |
|---|---|
| `int intback_wait_for_line` | INTBACK peripheral fetch is additionally gated on reaching scanline 207 (§5.5). **Not part of `SmpcInternal`, so it is not saved in savestates.** |
| `u8 bustmp` | Last byte written to *any* SMPC address; supplies the non-SF bits when SF is read (§2.3) |
| `int syslngid` | Copy of `syslanguageid`, used once inside `SmpcReset` (`smpc.c:134-135`) |

`Smpc` (`smpc.h:40-52`) is a packed run of `u8` fields totalling exactly 64 bytes:

```c
typedef struct {
        u8 IREG[7];      // struct index  0.. 6
        u8 padding[8];   //               7..14
        u8 COMREG;       //              15
        u8 OREG[32];     //              16..47
        u8 SR;           //              48
        u8 SF;           //              49
        u8 padding2[8];  //              50..57
        u8 PDR[2];       //              58..59
        u8 DDR[2];       //              60..61
        u8 IOSEL;        //              62
        u8 EXLE;         //              63
} Smpc;
```

`SmpcInternal` (`smpc.h:64-86`):

| Field | Type | Meaning as used by the code |
|---|---|---|
| `dotsel` | `u8` | `0` = 320-dot / 26 MHz clock, `1` = 352-dot / 28 MHz. Set only by CKCHG320/CKCHG352 (`smpc.c:214, 237`), read back in OREG10 (`:348`) |
| `syslanguageid` | `int` | Front-end language selection, copied into `SMEM` at reset (`:134-135`) |
| `syslngid` | `int` | **[DEAD]** struct field never read or written anywhere; only the file-scope `syslngid` at `smpc.c:70` is used |
| `mshnmi` | `u8` | Reported in OREG10 bit 3 (`:348`). **Never set to anything but 0** (`:153`) |
| `sndres` | `u8` | Reported in OREG10 bit 0. **Never set to anything but 0** (`:155`) |
| `cdres` | `u8` | Reported in OREG11 bit 6. **Never set to anything but 0** (`:156`) |
| `sysres` | `u8` | Reported in OREG10 bit 1. **Never set to anything but 0** (`:154`) |
| `resb` | `u8` | **[DEAD]** zeroed at reset (`:159`), never read |
| `ste` | `u8` | **[DEAD]** zeroed at reset (`:158`), never read |
| `resd` | `u8` | Reset-button disable. `1` at reset (`:157`); RESENAB clears, RESDISA sets (`:539, 546`); gates `SmpcResetButton` (`:530`) and is reported in OREG0 bit 6 (`:267`) |
| `intback` | `u8` | "An INTBACK peripheral sequence is in progress" — the continuation state machine flag (§5) |
| `intbackIreg0` | `u8` | **[DEAD]** assigned at `:482`, never read |
| `firstPeri` | `u8` | "The next peripheral chunk is the first one" — selects SR `0xC0` vs `0x80` (`:367-372`) |
| `regionid` | `u8` | Effective region, reported in OREG9 (`:335`) |
| `regionsetting` | `u8` | Requested region; `REGION_AUTODETECT` triggers CD-based detection (`:116-125`) |
| `SMEM[4]` | `u8[4]` | The 4 bytes of SMPC persistent memory, reported in OREG12-15 and written by SETSMEM |
| `timing` | `s32` | Microseconds remaining until the pending COMREG command fires (§3.3) |
| `port1`, `port2` | `PortData_struct` | Snapshots of `PORTDATA1`/`PORTDATA2` taken at the start of an INTBACK peripheral sequence (§5.4) |
| `clocksync` | `u8` | `1` = derive RTC from frame count instead of host wall clock (`:271-275`) |
| `basetime` | `u32` | UNIX epoch base for the `clocksync` mode |

`PortData_struct` (`smpc.h:57-62`) is shared with `peripheral.c`:

```c
typedef struct {
   int offset;   // read cursor, used only by the INTBACK chunker
   int size;     // number of valid bytes in data[]
   u8 data[256]; // the raw INTBACK report bytes for this port
} PortData_struct;
```

### 0.2 Lifecycle entry points

| Function | Line | Role |
|---|---|---|
| `SmpcInit(regionid, syslanguageid, clocksync, basetime)` | `smpc.c:74` | `calloc`s both structs, stores the four settings. `basetime` of 0 means "use host `time(NULL)`" |
| `SmpcSetClockSync(clocksync, basetime)` | `smpc.c:92` | Re-arms the RTC source at runtime. **[BUG]** declared `int` but has no `return` on the success path (`:92-96`) |
| `SmpcDeInit` | `smpc.c:100` | Frees both allocations |
| `SmpcRecheckRegion` | `smpc.c:112` | If `regionsetting == REGION_AUTODETECT`, `regionid = Cs2GetRegionID()`, defaulting to `1` (Japan) if that returns 0; otherwise just calls `Cs2GetIP(0)`. Also called from the CD block (`cs2.c:739`) |
| `SmpcReset` | `smpc.c:132` | §0.4 |
| `SmpcExec(s32 t)` | `smpc.c:552` | Command timer + dispatch (§3.3) |
| `SmpcINTBACKEnd` | `smpc.c:504` | Force-terminates an INTBACK sequence; called at V-Blank IN (`yabause.c:802`) |
| `SmpcResetButton` | `smpc.c:528` | Front-end reset button (`yabause.c:574`) |
| `SmpcCKCHG320` / `SmpcCKCHG352` | `smpc.c:222 / 199` | Also called directly by the HLE BIOS (`bios.c:363-365`) |
| `SmpcSaveState` / `SmpcLoadState` | `smpc.c:924 / 952` | §8 |

### 0.3 Region constants

`smpc.h:30-38`. These are the values that end up in OREG9.

| Constant | Value |
|---|---|
| `REGION_AUTODETECT` | `0` |
| `REGION_JAPAN` | `1` |
| `REGION_ASIANTSC` | `2` |
| `REGION_NORTHAMERICA` | `4` |
| `REGION_CENTRALSOUTHAMERICANTSC` | `5` |
| `REGION_KOREA` | `6` |
| `REGION_ASIAPAL` | `10` (`0xA`) |
| `REGION_EUROPE` | `12` (`0xC`) |
| `REGION_CENTRALSOUTHAMERICAPAL` | `13` (`0xD`) |

The same list is repeated as a comment next to the OREG9 write (`smpc.c:328-334`).

### 0.4 Reset state

`SmpcReset` (`smpc.c:132-169`):

1. `memset(SmpcRegs, 0, sizeof(Smpc))` — **the whole 64-byte register file is zeroed**,
   including IREG, COMREG, all 32 OREGs, SR, SF, PDR, DDR, IOSEL, EXLE.
2. `syslngid = SmpcInternalVars->syslanguageid;`
   `memset(SMEM, syslngid, 4);` then `memset(SMEM, 0, 3);` — net effect:
   **`SMEM[0..2] = 0`, `SMEM[3] = syslanguageid`** (`:134-136`). The comment block at
   `:135` documents the language codes (`0`=English, `1`=Deutsch, `2`=French, `3`=Spanish,
   `4`=Italian, `5`=Japanese) and `:138-148` documents a 3-bit "Other Settings" encoding
   (Button Labels / Audio Stereo-Mono / Sound Effects) that the code itself never produces —
   it only ever writes `0`.
3. `SmpcRecheckRegion()`.
4. `dotsel = mshnmi = sysres = sndres = cdres = ste = resb = 0`, **`resd = 1`** (`:152-159`).
5. `intback = intbackIreg0 = firstPeri = 0`, `timing = 0` (`:161-165`).
6. Both `PortData_struct` snapshots zeroed (`:167-168`).

`SmpcReset` does **not** touch `PORTDATA1`/`PORTDATA2`; those are owned by the front end and
reset by `PerPortReset` (`peripheral.c:890`).

---

## 1. Register map

### 1.1 Base address and mirroring

`FillMemoryArea(0x010, 0x017, &SmpcReadByte, …)` (`yabause/src/memory.c:615-620`) installs the
SMPC accessors over the eight 64 KiB pages `0x010`-`0x017`, i.e. the physical range
**`0x00100000`-`0x0017FFFF`** (through the SH-2 cache-through window, `0x20100000`-`0x2017FFFF`).

Both accessors mask with `addr &= 0x7F` (`smpc.c:636, 755`) and then index
`SmpcRegsT[addr >> 1]`. Consequences:

- The register file is **64 bytes at odd byte addresses `0x01, 0x03, … 0x7F`**, mirroring
  every 0x80 bytes, i.e. **4096 times over the 512 KiB window**.
- Because the index is `addr >> 1` and *not* gated on `addr & 1`, **the even address `2n` is a
  full alias of the odd address `2n+1`** for storage purposes. It is *not* an alias for side
  effects: the write-side `switch` matches on the odd address only (§1.4). **[QUIRK]**

**Register address = `0x00100000 + 2*index + 1`.**

### 1.2 Complete offset table

Legend for **W-side effect**: what the `switch(addr)` in `SmpcWriteByte` (`smpc.c:759-905`)
does *in addition to* the unconditional store `SmpcRegsT[addr >> 1] = val` at `smpc.c:757`.

| Offset | Full address | Idx | Name | Written by SW | W-side effect | R behaviour |
|---|---|---|---|---|---|---|
| `0x01` | `0x00100001` | 0 | **IREG0** | yes | INTBACK continue/break decode (`:760-776`) | plain |
| `0x03` | `0x00100003` | 1 | **IREG1** | yes | none | plain |
| `0x05` | `0x00100005` | 2 | **IREG2** | yes | none | plain |
| `0x07` | `0x00100007` | 3 | **IREG3** | yes | none | plain |
| `0x09` | `0x00100009` | 4 | **IREG4** | yes | none | plain |
| `0x0B` | `0x0010000B` | 5 | **IREG5** | yes | none | plain |
| `0x0D` | `0x0010000D` | 6 | **IREG6** | yes | none | plain |
| `0x0F`-`0x1D` | | 7-14 | *(padding)* | yes | none | plain — 8 bytes of readable/writable scratch that models nothing |
| `0x1F` | `0x0010001F` | 15 | **COMREG** | yes | `SmpcSetTiming()` — **arms the command** (`:777-779`) | plain |
| `0x21` | `0x00100021` | 16 | **OREG0** | yes **[QUIRK]** | none | plain |
| `0x23`…`0x5F` | | 17-47 | **OREG1**…**OREG31** | yes **[QUIRK]** | none | plain |
| `0x61` | `0x00100061` | 48 | **SR** (status) | yes **[QUIRK]** | none | plain |
| `0x63` | `0x00100063` | 49 | **SF** (status flag / busy) | yes | `SmpcRegs->SF &= val` — **[DEAD]**, see §1.3 | **special**: returns `bustmp` with bit 0 replaced by `SF` (`:637-641`) |
| `0x65`-`0x73` | | 50-57 | *(padding2)* | yes | none | plain — 8 more bytes of inert scratch |
| `0x75` | `0x00100075` | 58 | **PDR1** (port 1 data) | yes | Peripheral direct-access response synthesis for port 1 (`:783-818`, §6.2) | plain |
| `0x77` | `0x00100077` | 59 | **PDR2** (port 2 data) | yes | Same for port 2, minus the TH-ID mode (`:819-849`, §6.2). Source comment mislabels it `// PDR1` | plain |
| `0x79` | `0x00100079` | 60 | **DDR1** (port 1 direction) | yes | Writes an **ID nibble into PDR1** based on what is plugged into port 1 (`:850-896`, §6.3) | plain |
| `0x7B` | `0x0010007B` | 61 | **DDR2** (port 2 direction) | yes | **none — no `case 0x7B` exists** **[QUIRK]** | plain |
| `0x7D` | `0x0010007D` | 62 | **IOSEL** | yes | `SmpcRegs->IOSEL = val` (redundant with the unconditional store) | plain. **Never read by any code in the tree — completely inert** |
| `0x7F` | `0x0010007F` | 63 | **EXLE** | yes | `SmpcRegs->EXLE = val` (redundant) | plain. Bit 0 is read once, by VDP2 (`vdp2.cpp:1432`, §6.4) |

> **Real-game confirmation:** `real-game-capture-appendix.md` has actual observed values for
> IREG0-2, COMREG, SF, DDR1/2, IOSEL, EXLE from a live play session of a real commercial game.
> Notably: COMREG's real command distribution (`0x10` dominant = INTBACK, but also `0x19`/`0x07`/
> `0x06`/`0x03`/`0x02`/`0x1A` at low frequency — the actual OTHER commands this real game issues),
> and independent confirmation that real code does write `0x7B` (DDR2) despite this file's own
> `[QUIRK]` note that Yabause has no case for it at all.

### 1.3 SF (status flag) — read and write paths

Write (`smpc.c:754-757, 780-782`):

```c
SmpcRegsT[addr >> 1] = val;     // addr 0x63 -> index 49 == SF, so SF = val
...
case 0x63:
   SmpcRegs->SF &= val;         // SF = val & val  ==  val
   return;
```

The unconditional store already assigned `SF = val`, so the masking `SF &= val` is a no-op.
**Net semantics: a byte write to `0x63` sets SF to the written value verbatim** — software can
both set and clear it. The `&= val` line is **[DEAD]**; it presumably intended "software may
only clear SF" and does not achieve that.

Read (`smpc.c:637-641`):

```c
if (addr == 0x063) {
   bustmp &= ~0x01;
   bustmp |= SmpcRegs->SF;
   return bustmp;
}
```

`bustmp` is set to the written value on **every** `SmpcWriteByte` call, whatever the address
(`smpc.c:756`). So reading SF returns `(last byte ever written to the SMPC block & 0xFE) | SF`.
This models the SMPC data bus keeping the last driven value in the bits it does not drive.
Note that `bustmp` is *modified in place* by the read (`bustmp &= ~0x01; bustmp |= SF`), so
successive reads of SF are self-consistent but the stale bus value decays only through bit 0.
`bustmp` is a file-scope global, **not saved in savestates**.

### 1.4 Access widths

| Accessor | Line | Behaviour |
|---|---|---|
| `SmpcReadByte` | `:635` | The only functional read |
| `SmpcReadWord` | `:647` | Logs `"SMPC register read word"` and **returns 0** |
| `SmpcReadLong` | `:655` | Logs and **returns 0** |
| `SmpcWriteByte` | `:754` | The only functional write |
| `SmpcWriteWord` | `:910` | Logs and does **nothing** |
| `SmpcWriteLong` | `:917` | Logs and does **nothing** |

All logging goes through `SMPCLOG`, which is `DebugPrintf(MainLog, …)` when
`DEBUG`-style logging is compiled in and an empty macro otherwise (`debug.h:126-128`).

---

## 2. Command handshake protocol

### 2.1 The sequence the master SH-2 performs, as this code supports it

1. Write the command's arguments to **IREG0..IREG6** (`0x01, 0x03, … 0x0D`). Only INTBACK and
   SETSMEM read any IREG at all (§4).
2. Optionally write `1` to **SF** (`0x63`) to mark the SMPC busy. Nothing in `smpc.c` requires
   this — the value is simply stored (§1.3).
3. Write the command code to **COMREG** (`0x1F`). This is the trigger: the write handler calls
   `SmpcSetTiming()` (`smpc.c:777-779`), which sets `SmpcInternalVars->timing` to a
   **microsecond** delay based on the command (§3.3).
4. Poll **SF** (`0x63`) until bit 0 reads 0, and/or wait for the SCU **System Manager**
   interrupt (INTBACK only).
5. Read results from **OREG0..OREG31** (`0x21`…`0x5F`) and **SR** (`0x61`).

### 2.2 What actually clears SF

`SmpcExec` clears SF **unconditionally after every dispatched command**, including commands
whose handler is a bare log line:

```c
switch (SmpcRegs->COMREG) { … }
SmpcRegs->SF = 0;                       // smpc.c:628
```

There are exactly two places SF is *set* to 1 by the SMPC itself:

| Line | Context |
|---|---|
| `smpc.c:472` | First statement of `SmpcINTBACK()` — **immediately undone**, because `SmpcExec` sets `SF = 0` at `:628` as soon as `SmpcINTBACK` returns. **[DEAD]** |
| `smpc.c:773` | INTBACK *continue* request (write of IREG0 with bit 7): `COMREG = 0x10; SmpcSetTiming(); SF = 1;` — this one survives until the next `SmpcExec` dispatch |

And one place it is cleared outside the normal path:

| Line | Context |
|---|---|
| `smpc.c:727` | `SmpcSetTiming` default branch: an **unrecognised COMREG clears SF immediately** and leaves `timing` untouched, so the command never dispatches |

**[QUIRK]** Therefore, for every command except an INTBACK continuation, SF is only 1 if the
game set it itself, and it is cleared `timing` microseconds later. A game that writes COMREG
and then polls SF without having set SF first will observe "not busy" instantly.

### 2.3 Completion signalling

- **SF cleared** (`smpc.c:628`) for every command.
- **OREG31 = command echo** for *some* commands only (§4.1). SSHON, SSHOFF, CKCHG352, CKCHG320,
  MSHON, CDON, CDOFF and SYSRES do **not** write OREG31. **[QUIRK]**
- **SCU System Manager interrupt** — `ScuSendSystemManager()` (`scu.c:3346-3348`: vector `0x47`,
  level `0x8`, mask bit `0x0080`) is raised by **INTBACK only**, at `smpc.c:476, 488, 497`.
  No other command interrupts the CPU. The SCU *Pad* interrupt (`ScuSendPadInterrupt`,
  vector `0x48`) is **never called from anywhere in the tree**. **[QUIRK]**

---

## 3. Command timing and dispatch

### 3.1 How `SmpcExec` is driven

`SmpcExec(smpc_accum_t)` is called **once per scanline**, at `DecilineCount == 10`
(`yabause.c:783-786`). `smpc_accum_t` accumulates
`yabsys.UsecFrac >> YABSYS_TIMING_BITS` once per deciline (`yabause.c:833-835`), where
`DecilineUsec` is one tenth of a line in microseconds (`yabause.c:178`). With
`DecilineMode == 1` (the default, `yabause.c:349`) that is ≈6.34 µs per deciline for NTSC, so
`SmpcExec` receives **≈63 µs per call, one call per scanline**.

**`SmpcInternalVars->timing` is therefore in microseconds.**

### 3.2 `SmpcExec` body

```c
void SmpcExec(s32 t) {                                  // smpc.c:552
   if (SmpcInternalVars->timing > 0) {
      if (intback_wait_for_line) {                      // :555
         if (yabsys.LineCount == 207) {                 // :557
            SmpcInternalVars->timing = -1;
            intback_wait_for_line = 0;
         }
      }
      SmpcInternalVars->timing -= t;                    // :564
      if (SmpcInternalVars->timing <= 0) {
         switch (SmpcRegs->COMREG) { … }                // :566-626
         SmpcRegs->SF = 0;                              // :628
      }
   }
}
```

Note the ordering: when `intback_wait_for_line` fires, `timing` is forced to `-1`, then
`t` is subtracted, then the command dispatches **in that same call**. Scanline 207 is
hard-coded; V-Blank IN is at line 225 by default (`vdp2.cpp:515`), so this is 18 lines before
V-Blank.

### 3.3 `SmpcSetTiming` — delay per command

`SmpcSetTiming` (`smpc.c:663-730`) runs on the COMREG write.

| COMREG | Name | `timing` (µs) | Line | Note |
|---|---|---|---|---|
| `0x00` | MSHON | 1 | `:665-668` | logs "not implemented" here too |
| `0x02` | SSHON | 1 | `:712-714` | |
| `0x03` | SSHOFF | 1 | `:715-717` | |
| `0x06` | SNDON | 1 | `:718-724` | |
| `0x07` | SNDOFF | 1 | `:718-724` | |
| `0x08` | CDON | 1 | `:669-672` | |
| `0x09` | CDOFF | 1 | `:673-676` | |
| `0x0D` | SYSRES | 1 | `:677-681` | source comment: *"this has to be tested on a real saturn"* |
| `0x0E` | CKCHG352 | 1 | `:677-681` | same comment |
| `0x0F` | CKCHG320 | 1 | `:677-681` | same comment |
| `0x10` | INTBACK | 250 / 16000 | `:682-708` | see below |
| `0x17` | SETSMEM | 1 | `:709-711` | |
| `0x18` | NMIREQ | 1 | `:718-724` | |
| `0x19` | RESENAB | 1 | `:718-724` | |
| `0x1A` | RESDISA | 1 | `:718-724` | |
| anything else | — | *(unchanged)* | `:725-728` | logs, sets `SF = 0`, command never dispatches |

INTBACK timing sub-cases (`smpc.c:682-708`):

| Condition | `timing` | `intback_wait_for_line` |
|---|---|---|
| `SmpcInternalVars->intback` already set (this is a *continue*) | `16000` | `1` |
| `IREG0 == 0x01 && (IREG1 & 0x08)` — status then peripherals | `250` | 0 |
| `IREG0 == 0x01 && !(IREG1 & 0x08)` — status only | `250` | 0 |
| `IREG0 == 0x00 && (IREG1 & 0x08)` — peripherals only | `16000` | `1` |
| **anything else** | **not assigned** | — |

**[BUG]** The last row: if `IREG0` is neither exactly `0x00` nor exactly `0x01`, `timing` keeps
whatever it had (normally `0`), so `SmpcExec`'s `if (timing > 0)` never becomes true and the
command **never runs**. If the game set SF itself it will spin forever. This is reachable
because `SmpcINTBACK` itself only tests `IREG[0] & 1` (`smpc.c:482`), so e.g. `IREG0 == 0x03`
is a well-formed status request as far as the *handler* is concerned, but is dropped by the
*timer*.

### 3.4 `SmpcExec` dispatch table

`smpc.c:566-626`. Every case not listed falls into `default:` which only logs
`"Command %02X not implemented"` (`:623-625`).

| COMREG | Name in source | Handler | Line |
|---|---|---|---|
| `0x00` | MSHON | *(log only)* | `:567-569` |
| `0x02` | SSHON | `SmpcSSHON` | `:570-573` |
| `0x03` | SSHOFF | `SmpcSSHOFF` | `:574-577` |
| `0x06` | SNDON | `SmpcSNDON` | `:578-581` |
| `0x07` | SNDOFF | `SmpcSNDOFF` | `:582-585` |
| `0x08` | CDON | *(log only)* | `:586-588` |
| `0x09` | CDOFF | *(log only)* | `:589-591` |
| `0x0D` | SYSRES | *(log only)* | `:592-594` |
| `0x0E` | CKCHG352 | `SmpcCKCHG352` | `:595-598` |
| `0x0F` | CKCHG320 | `SmpcCKCHG320` | `:599-602` |
| `0x10` | INTBACK | `SmpcINTBACK` | `:603-606` |
| `0x17` | SETSMEM | `SmpcSETSMEM` | `:607-610` |
| `0x18` | NMIREQ | `SmpcNMIREQ` | `:611-614` |
| `0x19` | RESENAB | `SmpcRESENAB` | `:615-618` |
| `0x1A` | RESDISA | `SmpcRESDISA` | `:619-622` |

**Command codes with no handler anywhere in this source:** `0x01`, `0x04`, `0x05`, `0x0A`,
`0x0B`, `0x0C`, `0x11`-`0x16`, `0x1B`-`0xFF`. In particular **there is no `0x16` / SETTIME
command and no RTC-write path of any kind** — the RTC is read-only and comes from the host
clock (§7.1).

---

## 4. Command reference

### 4.1 OREG31 command-echo values

| Command | OREG31 | Line |
|---|---|---|
| SNDON | `0x06` | `:187` |
| SNDOFF | `0x07` | `:194` |
| INTBACK (status) | `0x10` | `:358` |
| INTBACK (peripheral-only, first call) | `0x10` | `:496` — **overwrites peripheral data**, see §5.4 |
| SETSMEM | `0x17` | `:516` |
| NMIREQ | `0x18` | `:523` |
| RESENAB | `0x19` | `:540` |
| RESDISA | `0x1A` | `:547` |
| SSHON, SSHOFF, CKCHG320, CKCHG352, MSHON, CDON, CDOFF, SYSRES | *(not written)* | — |

### 4.2 `0x00` MSHON — Master SH-2 ON

| | |
|---|---|
| Inputs | none |
| Outputs | none |
| Effect | **Not implemented.** `SMPCLOG("smpc\t: MSHON not implemented\n")` at both `:568` and `:666`. `timing = 1`; SF is cleared after the 1 µs delay. |

### 4.3 `0x02` SSHON — Slave SH-2 ON

| | |
|---|---|
| Inputs | none |
| Outputs | none (no OREG31 echo) |
| Effect | `SmpcSSHON()` (`:173-175`) → `YabauseStartSlave()` |

`YabauseStartSlave` (`yabause.c:942-990`) has two paths:

- **HLE BIOS (`yabsys.emulatebios`)**: writes SSH2 on-chip registers (BCR1/BCR2/WCR/MCR, ICR,
  IPRA, the VCR bank, VCRDMA0/1, VCRDIV, TIER), then sets `SSH2.R15` from
  `Cs2GetSlaveStackAdress()` (overridden by `[0x060002AC]` if non-zero), `VBR = 0x06000400`,
  `PC = [0x06000250]`, `SR.I = 0`, and runs `SH2HandleInterrupts`.
- **Real BIOS**: `SH2PowerOn(SSH2)` then `PC = 0x20000200`.

Both then set `yabsys.IsSSH2Running = 1`, which is what actually gates `SH2Exec(SSH2, …)` in
the main loop (`yabause.c:560, 566, 570`).

### 4.4 `0x03` SSHOFF — Slave SH-2 OFF

| | |
|---|---|
| Inputs | none |
| Outputs | none (no OREG31 echo) |
| Effect | `SmpcSSHOFF()` (`:179-181`) → `YabauseStopSlave()` = `SH2Reset(SSH2); yabsys.IsSSH2Running = 0;` (`yabause.c:992-995`) |

The slave is **fully reset**, not merely halted; restarting it goes through the full
`YabauseStartSlave` path above.

### 4.5 `0x06` SNDON — Sound (68000) ON

| | |
|---|---|
| Inputs | none |
| Outputs | OREG31 = `0x06` (`:187`) |
| Effect | `M68KStart()` (`scsp.c:5213-5219`) = `M68K->Reset(); savedcycles = 0; IsM68KRunning = 1;` |

Note `ScspReset()` is commented out inside `M68KStart` (`scsp.c:5215`) — the SCSP register
state is *not* touched, only the 68000 core.

### 4.6 `0x07` SNDOFF — Sound (68000) OFF

| | |
|---|---|
| Inputs | none |
| Outputs | OREG31 = `0x07` (`:194`) |
| Effect | `M68KStop()` (`scsp.c:5224-5229`) = `M68K->Reset(); IsM68KRunning = 0;` |

Again `ScspReset()` is commented out. The 68000 is reset on both start *and* stop.

### 4.7 `0x08` CDON / `0x09` CDOFF

Not implemented; log only (`:587, 590`, `:670, 674`).

### 4.8 `0x0D` SYSRES — System Reset

Not implemented; log only (`:593`). `timing = 1` (`:677-681`). **A game requesting a full
system reset via SMPC gets nothing.**

### 4.9 `0x0E` CKCHG352 — Change clock to 352-dot mode

`SmpcCKCHG352` (`smpc.c:199-218`), in order:

1. `Vdp1Reset()`, `Vdp2Reset()`, `ScuReset()`, `ScspReset()`
2. *(comment "Clear VDP1/VDP2 ram" — **no such clear is performed**)*
3. `YabauseStopSlave()`
4. `YabauseChangeTiming(CLKTYPE_28MHZ)`
5. `SmpcInternalVars->dotsel = 1`
6. `SH2NMI(MSH2)`

`YabauseChangeTiming` (`yabause.c:162-180`) recomputes the SH-2 cycles-per-deciline from
`freq_base * freq_mult`, where `freq_base` is 28.4375 MHz (PAL) or 28.63636 MHz (NTSC) and
`freq_mult` is `1.0` for `CLKTYPE_28MHZ`. It also resets `DecilineCount` and `LineCount` to 0.

`SH2NMI` (`sh2core.c:264-268`) sets `ICR |= 0x8000` **and** raises vector `0x0B` at level
`0x10`.

### 4.10 `0x0F` CKCHG320 — Change clock to 320-dot mode

`SmpcCKCHG320` (`smpc.c:222-241`) is identical except `YabauseChangeTiming(CLKTYPE_26MHZ)`
(`freq_mult = 15/16`, i.e. 26.66 / 26.85 MHz) and `dotsel = 0`.

Both CKCHG entry points are also called directly by the HLE BIOS system-clock-change trap
(`bios.c:362-365`), selecting on `R4 == 0`.

### 4.11 `0x10` INTBACK — Interrupt Back

See §5 in full.

### 4.12 `0x17` SETSMEM — Set SMPC Memory

`SmpcSETSMEM` (`smpc.c:510-517`):

| | |
|---|---|
| Inputs | **IREG0..IREG3** → `SmpcInternalVars->SMEM[0..3]` |
| Outputs | OREG31 = `0x17` |

```c
for (i = 0; i < 4; i++)
   SmpcInternalVars->SMEM[i] = SmpcRegs->IREG[i];
```

There is **no length/validity check** and no persistence — SMEM lives only in
`SmpcInternalVars` and is written to savestates as part of the struct (§8). It is read back
through INTBACK status OREG12-15 (`:355-356`). It is initialised at reset to
`{0, 0, 0, syslanguageid}` (§0.4).

### 4.13 `0x18` NMIREQ — NMI Request

`SmpcNMIREQ` (`smpc.c:521-524`):

| | |
|---|---|
| Inputs | none |
| Outputs | OREG31 = `0x18` |
| Effect | `SH2SendInterrupt(MSH2, 0xB, 16)` |

**[QUIRK]** This calls `SH2SendInterrupt` directly rather than `SH2NMI`, so unlike CKCHG320/352
it does **not** set `ICR` bit 15 (`sh2core.c:266`). Vector `0x0B`, level `16` (`0x10`) is
otherwise the same as a real NMI.

### 4.14 `0x19` RESENAB — Reset Enable

`SmpcRESENAB` (`smpc.c:538-541`): `resd = 0`, OREG31 = `0x19`.

### 4.15 `0x1A` RESDISA — Reset Disable

`SmpcRESDISA` (`smpc.c:545-548`): `resd = 1`, OREG31 = `0x1A`.

### 4.16 Reset button (not a command)

`SmpcResetButton` (`smpc.c:528-534`) is called by the front end via `YabauseResetButton`
(`yabause.c:568-575`):

```c
if (SmpcInternalVars->resd)
   return;                              // reset disabled -> button does nothing
SH2SendInterrupt(MSH2, 0xB, 16);
```

Same **[QUIRK]** as NMIREQ: `ICR` bit 15 is not set. Because `resd = 1` at reset (`:157`), the
reset button is **inert until the game issues RESENAB**.

---

## 5. INTBACK in detail

INTBACK is the only stateful SMPC command. It returns two logically separate things — a
**status block** (RTC, region, system config, SMEM) and a **peripheral report** — and the
peripheral report can span multiple calls because it must be delivered 32 bytes at a time
through OREG0-31.

### 5.1 Input registers

| Register | Bit(s) | Read at | Meaning as implemented |
|---|---|---|---|
| **IREG0** | 0 | `:482` | `1` = return the status block on this call |
| **IREG0** | 6 | `:763` | On a *write* while a sequence is in progress: **break** |
| **IREG0** | 7 | `:769` | On a *write* while a sequence is in progress: **continue** |
| **IREG1** | 3 | `:485, 491` | `1` = the caller also wants peripheral data |
| **IREG1** | 7:4 | `:368, 370` | Echoed verbatim into SR bits 3:0 on every peripheral chunk |
| IREG2..IREG6 | — | — | **Never read** |

`SmpcSetTiming` additionally compares `IREG0` for exact equality with `0x00`/`0x01` (§3.3).

### 5.2 The state machine

`SmpcINTBACK` (`smpc.c:471-500`):

```c
SmpcRegs->SF = 1;                                        // :472  (immediately undone, §2.2)

if (SmpcInternalVars->intback) {                         // :474  CONTINUATION
   SmpcINTBACKPeripheral();
   ScuSendSystemManager();
   return;
}

if ((SmpcInternalVars->intbackIreg0 = (SmpcRegs->IREG[0] & 1))) {   // :482  STATUS
   SmpcInternalVars->firstPeri = 1;
   SmpcInternalVars->intback   = (SmpcRegs->IREG[1] & 0x8) >> 3;
   SmpcINTBACKStatus();
   SmpcRegs->SR = 0x4F | (SmpcInternalVars->intback << 5);          // :487
   ScuSendSystemManager();
   return;
}

if (SmpcRegs->IREG[1] & 0x8) {                           // :491  PERIPHERAL-ONLY
   SmpcInternalVars->firstPeri = 1;
   SmpcInternalVars->intback   = 1;
   SmpcRegs->SR = 0x40;                                  // :494  dead store
   SmpcINTBACKPeripheral();                              //       overwrites SR
   SmpcRegs->OREG[31] = 0x10;                            // :496  clobbers OREG31 data
   ScuSendSystemManager();
   return;
}
// IREG0 bit0 == 0 and IREG1 bit3 == 0: falls out doing nothing at all
```

Continue / break are handled **in the IREG0 write handler**, not in the command dispatcher
(`smpc.c:760-776`):

```c
case 0x01:                                    // write to IREG0
   if (SmpcInternalVars->intback) {
      if (SmpcRegs->IREG[0] & 0x40) {         // BREAK
         SmpcInternalVars->intback = 0;
         SmpcRegs->SR &= 0x0F;
         break;
      }
      else if (SmpcRegs->IREG[0] & 0x80) {    // CONTINUE
         SmpcRegs->COMREG = 0x10;
         SmpcSetTiming();                     // -> timing = 16000, wait_for_line = 1
         SmpcRegs->SF = 1;
      }
   }
   return;
```

Note the continue path **synthesises a COMREG write internally** — the game does not have to
re-write COMREG. Break is tested before continue, so `IREG0 = 0xC0` breaks.

Finally, `SmpcINTBACKEnd()` (`smpc.c:504-506`) sets `intback = 0` and is called
unconditionally at **V-Blank IN** (`yabause.c:802`). Any sequence still in progress at V-Blank
is silently abandoned.

### 5.3 SR (status register) encoding, as produced

| Path | SR value | Line |
|---|---|---|
| Status returned | `0x4F \| (intback << 5)` → `0x4F` (status only) or `0x6F` (peripherals follow) | `:487` |
| Peripheral chunk, first of the sequence | `0xC0 \| (IREG1 >> 4)` | `:368` |
| Peripheral chunk, subsequent | `0x80 \| (IREG1 >> 4)` | `:370` |
| Break acknowledged | `SR &= 0x0F` | `:766` |

The source comment at `:487` says *"the low nibble is undefined (or 0xF)"*. Bit 7 marks
"peripheral data present"; bit 6 distinguishes the first chunk from the rest. **There is no bit
that tells the game whether more chunks remain** — the code never signals exhaustion, so the
game must derive that from the port-status/size bytes it parses out of the OREGs. **[QUIRK]**

### 5.4 `SmpcINTBACKPeripheral` — the 32-byte chunker

`smpc.c:363-467`.

**Step 1 — SR** (`:367-372`), per §5.3, then `firstPeri = 0`.

**Step 2 — snapshot** (`:410-422`), only when *both* internal snapshots are drained:

```c
if (SmpcInternalVars->port1.size == 0 && SmpcInternalVars->port2.size == 0) {
   memcpy(&SmpcInternalVars->port1, &PORTDATA1, sizeof(PortData_struct));
   memcpy(&SmpcInternalVars->port2, &PORTDATA2, sizeof(PortData_struct));
   PerFlush(&PORTDATA1);
   PerFlush(&PORTDATA2);
   SmpcInternalVars->port1.offset = 0;
   SmpcInternalVars->port2.offset = 0;
   LagFrameFlag = 0;                        // [HACK] TAS lag-frame detection, movie.c:147
}
```

`PerFlush` (`peripheral.c:791-804`) zeroes the mouse motion deltas on the **live** port data so
they accumulate afresh until the next poll (§6.6). `LagFrameFlag = 0` marks this frame as
"input was polled" for the movie recorder (`movie.c:147, 166`) — pure TAS bookkeeping, not
hardware.

**Step 3 — copy port 1** (`:425-439`):

```c
if (port1.size > 0) {
   if ((port1.size - port1.offset) < 32) {
      memcpy(OREG, port1.data + port1.offset, port1.size - port1.offset);
      oregoffset += port1.size - port1.offset;
      port1.size = 0;                                // drained
   } else {
      memcpy(OREG, port1.data, 32);                  // <-- ignores port1.offset  [BUG]
      oregoffset += 32;
      port1.offset += 32;
   }
}
```

**Step 4 — copy port 2** into the remaining OREGs (`:441-453`), same shape, same bug:

```c
if (port2.size > 0 && oregoffset < 32) {
   if ((port2.size - port2.offset) < (32 - oregoffset)) {
      memcpy(OREG + oregoffset, port2.data + port2.offset, port2.size - port2.offset);
      port2.size = 0;
   } else {
      memcpy(OREG + oregoffset, port2.data, 32 - oregoffset);   // <-- ignores offset  [BUG]
      port2.offset += 32 - oregoffset;
   }
}
```

**[BUG]** Both multi-chunk branches read from `data` rather than `data + offset`, so the second
and later chunks re-send the *first* 32 bytes. This is only reachable when a port's report
exceeds 32 bytes, which requires a multi-tap full of large peripherals (six twin-sticks =
`1 + 6*10 = 61` bytes; six keyboards = `1 + 6*5 = 31` bytes, just under).

**[QUIRK]** In the port-2 `else` branch, `port2.size` is never cleared, so when
`size - offset` happens to equal `32 - oregoffset` exactly, an extra zero-byte continuation
round is required to drain it.

**[BUG]** Break (`:763-768`) and `SmpcINTBACKEnd` (`:504-506`) clear `intback` but **do not
reset `port1.size`/`port2.size`**. A partially-drained snapshot therefore survives into the
*next* INTBACK, which will resume from the stale, frame-old snapshot instead of re-sampling the
live pads. Only `SmpcReset` clears the snapshots (`:167-168`).

### 5.5 Peripheral report byte format in the OREGs

The OREG stream is simply the concatenation of `PORTDATA1.data[0 .. size-1]` followed by
`PORTDATA2.data[0 .. size-1]`, chunked 32 bytes at a time. The layout of that byte stream is
built entirely by `PerAddPeripheral` (`peripheral.c:621-779`) and is:

```
<port status byte>  <peripheral entry>  <peripheral entry> …
peripheral entry := <peripheral ID byte>  <N data bytes>,  where N = ID & 0x0F
```

**Port status byte** — `port->data[0]`, low nibble = number of peripheral slots that follow:

| Value | Set at | Meaning |
|---|---|---|
| `0xF0` | `peripheral.c:892, 894` | Nothing connected. The port contributes exactly **one** byte (`size = 1`) |
| `0xF1` | `peripheral.c:641` | One peripheral connected directly |
| `0xA0` | `peripheral.c:643` | A light gun is connected directly (low nibble 0 ⇒ **no** peripheral entries follow; `size` is forced to 1, `:729`) |
| `0x16` | `peripheral.c:656` | Multi-tap with 6 slots |

The source carries a comment block (`smpc.c:374-379`) listing the port-status values it
believes hardware uses — `0x04` Sega-tap, `0x16` multi-tap, `0x21`-`0x2F` clock-serial
peripheral, `0xF0` not connected/unknown, `0xF1` directly connected — but only `0xF0`, `0xF1`,
`0x16` and `0xA0` are ever produced.

**Peripheral ID byte** — `peripheral.h:41-48`:

| Constant | Value | Data bytes (`ID & 0xF`) |
|---|---|---|
| `PERPAD` | `0x02` | 2 |
| `PERWHEEL` | `0x13` | 3 |
| `PERMISSIONSTICK` | `0x15` | 5 |
| `PER3DPAD` | `0x16` | 6 |
| `PERTWINSTICKS` | `0x19` | 9 |
| `PERGUN` | `0x25` | 5 (but never emitted into the report — see `0xA0` above) |
| `PERKEYBOARD` | `0x34` | 4 |
| `PERMOUSE` | `0xE3` | 3 |
| *(empty tap slot)* | `0xFF` | 15 nominally, but written as a **single** `0xFF` byte meaning "slot empty" (`peripheral.c:653, 753`) |

`smpc.c:381-391` carries a second comment list of IDs it expects hardware to use (`0x02`
digital, `0x13` racing, `0x15` analog, `0x23` pointing/shooting, `0x34` keyboard, `0xE1`/`0xE2`
Mega Drive pads, `0xE3` Saturn mouse, `0xFF` not connected). Note the mismatch: the comment
says pointing/shooting is `0x23`, but the code defines `PERGUN = 0x25`; `0xE1`/`0xE2` Mega
Drive pads are not implemented at all.

**Worked examples** (byte streams as they land in OREG0…):

- Nothing on either port: `F0 | F0` — OREG0 = `0xF0`, OREG1 = `0xF0`.
- One digital pad on port 1, nothing on port 2:
  `F1 02 FF FF | F0` — OREG0 `0xF1`, OREG1 `0x02`, OREG2/3 the two button bytes, OREG4 `0xF0`.
  (`PORTDATA1.size == 4`.)
- Two digital pads on port 1 (multi-tap): `16 02 FF FF 02 FF FF FF FF FF FF`
  (`size == 11`) — status `0x16`, two 3-byte entries, four `0xFF` empty-slot bytes.
- A gun on port 1: `A0` only (`size == 1`). All gun state is read through PDR1 (§6.2) and the
  VDP2 external latch (§6.4).

`smpc.c:393-406` documents the intended disconnected-port convention (one `0xF0` byte, then the
next port's status in the following OREG) and `:455-466` contains a commented-out hand-written
example for a mouse. Both are documentation only.

### 5.6 INTBACK status block — OREG layout

`SmpcINTBACKStatus` (`smpc.c:260-359`):

| OREG | Value | Line |
|---|---|---|
| **0** | `0x80 \| (resd << 6)` | `:267` |
| **1** | Year, thousands and hundreds digits, BCD: `(y/1000 << 4) \| ((y%1000)/100)` | `:283-287` |
| **2** | Year, tens and units digits, BCD | `:285-288` |
| **3** | `(tm_wday << 4) \| (tm_mon + 1)` — weekday 0=Sunday in the high nibble, month 1-12 in the low nibble (**not BCD**, and the month overflows the nibble for months 10-12 only in the sense that `0xA`-`0xC` are used) | `:289` |
| **4** | Day of month, BCD | `:290` |
| **5** | Hour (24 h), BCD | `:291` |
| **6** | Minute, BCD | `:292` |
| **7** | Second, BCD | `:293` |
| **8** | Cartridge code — **hard-coded `0`**, source comment `// FIXME : random value` | `:324` |
| **9** | `SmpcInternalVars->regionid` (§0.3) | `:335` |
| **10** | `0x34 \| (dotsel << 6) \| (mshnmi << 3) \| (sysres << 1) \| sndres`. Bits 5, 4, 2 are hard-wired 1; bit 7 hard-wired 0 | `:337-348` |
| **11** | `cdres << 6` — always `0` in practice; source comment `// FIXME` | `:350-352` |
| **12-15** | `SMEM[0..3]` | `:355-356` |
| **16-30** | **Not written — stale contents of whatever was in the OREGs before** | — |
| **31** | `0x10` (command echo) | `:358` |

The OREG0 line has a commented-out alternative (`:268`):
`SmpcRegs->OREG[0] = 0x0 | (resd << 6); // goto setclock/setlanguage screen`. Bit 7 set means
"normal startup"; clear means the BIOS should show the clock/language setup screen.

**[QUIRK]** OREG16-30 are left untouched, so a game reading the full 32-byte status block gets
15 bytes of stale data where hardware would supply defined values.

---

## 6. The direct-access port mechanism: PDR1/PDR2, DDR1/DDR2, IOSEL, EXLE

This is the second, SMPC-bypassing way to talk to a controller: the game sets pin directions in
DDRn, drives output pins by writing PDRn, and reads input pins back from PDRn. Yabause does not
model pins at all; instead it **synthesises the value a real peripheral would drive, at the
moment the game writes PDRn or DDRn**, and stores it into `SmpcRegs->PDR[n]` so the game's
subsequent *read* of PDRn returns it.

### 6.1 Control-method selection

Every PDR/DDR write handler switches on **`SmpcRegs->DDR[n] & 0x7F`** — i.e. bit 7 of DDR is
masked off and the low 7 bits are treated as a mode selector rather than as per-pin direction
bits. Recognised values:

| `DDR & 0x7F` | Name in source | PDR1 handler | PDR2 handler | DDR1 handler |
|---|---|---|---|---|
| `0x00` | (all-input) | gun button read (`:786-789`) | gun button read (`:822-825`) | ID nibble (`:852-895`) |
| `0x40` | "th control mode (acquire id)" | `do_th_mode` (`:791-793`) | **absent** | ID nibble (same body as `0x00`) |
| `0x60` | "th tr control mode" | 4-phase nibble read (`:795-813`) | 4-phase nibble read (`:826-844`) | — |
| anything else | — | logs `"Peripheral Unknown Control Method not implemented"` (`:814-816`) | same (`:845-847`) | nothing |

Both PDR handlers carry the source comment `// FIX ME (should support other peripherals)`
(`:784, 820`).

### 6.2 PDR1 write (`0x75`) / PDR2 write (`0x77`)

**Mode `0x00`** (`:786-789` / `:822-825`) — only meaningful for a light gun:

```c
if (PORTDATA1.data[1] == PERGUN && (val & 0x7F) == 0x7F)
   SmpcRegs->PDR[0] = PORTDATA1.data[2];      // the gun button byte
```

i.e. when the game floats all seven lines high, the gun's trigger/start byte appears in PDR.
Note `PORTDATA1.data[1]` is only the gun's ID if the gun is the *first* slot on the port.

**Mode `0x40`** (PDR1 only) — `do_th_mode(val)` (`smpc.c:737-750`), commented
*"acquiring megadrive id / world heroes perfect wants to find a saturn pad / id = 0xb"*
**[HACK]**:

| `val & 0x40` | Returns |
|---|---|
| `0x40` (TH high) | `0x70 \| ((PORTDATA1.data[3] & 0xF) & 0xC)` → for an idle Saturn pad (`data[3] == 0xFF`) this is `0x7C` |
| `0x00` (TH low) | `0x30 \| ((PORTDATA1.data[2] >> 4) & 0xF)` → the Right/Left/Down/Up nibble, `0x3F` when idle |

The function ends with `return 0; // should not happen` (`:748-749`), unreachable because
`val & 0x40` has only two possible values.

**Mode `0x60`** — the classic four-phase TH/TR handshake. The two select bits `val & 0x60`
choose which nibble of the pad report to present; bit 7 of the written value is preserved and
bit 4 is forced:

| `val & 0x60` | Source comment | Resulting `PDR[n]` | Line (port 1 / port 2) |
|---|---|---|---|
| `0x60` | 1st Data | `(val & 0x80) \| 0x14 \| (data[3] & 0x08)` — bit 3 carries the **L trigger** | `:797-799` / `:828-830` |
| `0x20` | 2nd Data | `(val & 0x80) \| 0x10 \| ((data[2] >> 4) & 0xF)` — **Right, Left, Down, Up** | `:800-802` / `:831-833` |
| `0x40` | 3rd Data | `(val & 0x80) \| 0x10 \| (data[2] & 0xF)` — **Start, A, C, B** | `:803-805` / `:834-836` |
| `0x00` | 4th Data | `(val & 0x80) \| 0x10 \| ((data[3] >> 4) & 0xF)` — **R trigger, X, Y, Z** | `:806-808` / `:837-839` |

The `default: break;` arm (`:809`, `:840`) is unreachable — `val & 0x60` has exactly four
values, all four are cased.

Note the asymmetry: **PDR2 has no `0x40` (TH-ID) case**, so the Mega Drive ID handshake works
only on port 1.

### 6.3 DDR1 write (`0x79`)

`smpc.c:850-896`. Cases `0x00` and `0x40` of `DDR[0] & 0x7F` share one body, labelled
`// Low Nibble of Peripheral ID` / `// High Nibble of Peripheral ID`. It switches on the port-1
**status byte** and writes an identifying value straight into `PDR[0]`:

| `PORTDATA1.data[0]` | `PORTDATA1.data[1]` | `PDR[0]` ← | Line |
|---|---|---|---|
| `0xA0` (gun attached) | `PERGUN` | `0x7C` | `:856-861` |
| `0xA0` | anything else | *(unchanged)* | `:858-860` |
| `0xF0` (nothing connected) | — | `0x7F` | `:862-864` |
| `0xF1` | `PERPAD` | `0x7C` | `:869-871` |
| `0xF1` | `PER3DPAD`, `PERKEYBOARD` | `0x71` | `:872-874` |
| `0xF1` | `PERMOUSE` | `0x70` | `:875-877` |
| `0xF1` | `PERWHEEL`, `PERMISSIONSTICK`, `PERTWINSTICKS`, other | *(unchanged)* + log `"Peripheral TH Control Method not supported for peripherl id %02X"` | `:879-884` |
| anything else (incl. `0x16` multi-tap) | — | `0x71` | `:888-890` |

So the low nibble of PDR1 is the ID code: `0xC` = Saturn digital pad, `0x1` = analog pad /
keyboard / multi-tap, `0x0` = mouse, `0xF` = nothing connected.

**Formatting trap** at `:856-861`: the `break;` after the `if` is *not* inside the `if` body
despite its indentation, so case `0xA0` always breaks and never falls through into case `0xF0`.

**[QUIRK]** Writing DDR immediately mutates PDR. On hardware DDR only configures pin
directions; the value would appear on the next PDR *read*. Any Mimas implementation that models
real pin behaviour will diverge from this code path but should still produce the same
observable ID nibbles.

**[QUIRK]** There is **no `case 0x7B`**: writing DDR2 stores the byte and produces no ID for
port 2 at all.

### 6.4 IOSEL (`0x7D`) and EXLE (`0x7F`)

| Register | Consumers |
|---|---|
| **IOSEL** | **None.** Stored at `smpc.c:757` and `:898`, saved/loaded in savestates (`:939, 965`), and never read by any code in the tree. **[QUIRK]** Yabause behaves as if both the SMPC-managed path (INTBACK) and the direct-access path (PDR/DDR) are permanently enabled on both ports. |
| **EXLE** | Bit 0 only, in `Vdp2VBlankOUT` (`vdp2.cpp:1428-1434`): when `Vdp2Regs->EXTEN & 0x200` **and** `SmpcRegs->EXLE & 0x1`, it calls `Vdp2SendExternalLatch((PORTDATA1.data[3] << 8) \| PORTDATA1.data[4], (PORTDATA1.data[5] << 8) \| PORTDATA1.data[6])`, which sets `HCNT = hcnt << 1`, `VCNT = vcnt`, `TVSTAT \|= 0x200`. Those four bytes are exactly the light gun's `gunbits[1..4]` X/Y pair (§6.5). The VDP2 source carries the comment *"Should be revised for accuracy (should occur only on the line it happens at, etc.)"* — the latch fires once per frame at V-Blank OUT rather than at the beam position, and **only port 1 is ever latched**. |

### 6.5 Light gun position, end to end

`PerGunMove` (`peripheral.c:597-617`) integrates relative motion into an absolute position
stored big-endian across `gunbits[1..4]` (= `port->data[3..6]`), scaled by `/4` with Y
inverted, clamped to `0 ≤ x < 320` and `0 ≤ y < 224` — both bounds carry `// fix me` comments
(`:605, 610`) because they are hard-coded to one video mode.

Gun buttons live in `gunbits[0]` (= `port->data[2]`), active-low:

| Bit | Button | Pressed | Released |
|---|---|---|---|
| 4 | Trigger | `&= 0xEF` (`:571`) | `\|= 0x10` (`:578`) |
| 5 | Start | `&= 0xDF` (`:585`) | `\|= 0x20` (`:592`) |

Initial value `0x7C` (`:724`), which is also the value DDR1 mode `0x00`/`0x40` reports as the
gun's ID (`smpc.c:859`).

---

## 7. RTC, SMEM and clock synchronisation

### 7.1 RTC source

`SmpcINTBACKStatus` (`smpc.c:271-293`):

```c
if (SmpcInternalVars->clocksync)
   tmp = SmpcInternalVars->basetime + ((u64)yabsys.frame_count * 1001 / 60000);
else
   tmp = time(NULL);
localtime_r(&tmp, &times);      // or localtime()/internal_localtime_r() per platform
```

- **`clocksync == 0`**: the RTC is the **host wall clock**, re-read on every INTBACK. It is
  therefore not monotonic with emulated time and jumps if the host clock changes.
- **`clocksync == 1`**: deterministic — `basetime` (a UNIX epoch second, set at
  `SmpcInit`/`SmpcSetClockSync`, defaulting to `time(NULL)`) plus `frame_count * 1001/60000`
  seconds. **[QUIRK]** The `1001/60000` factor is NTSC-only; in PAL mode the emulated clock
  runs ~20 % slow. `basetime` is a `u32`, with the source comment
  *"Safe until early 2106. After that you're on your own (:"* (`smpc.h:85`).

**There is no way to set the RTC.** No SETTIME-equivalent command exists (§3.4), and `times`
is never written back to `basetime`.

### 7.2 Movie-mode RTC override **[HACK]**

`smpc.c:295-321`: when `Movie.Status` is `Recording` or `Playback`, OREG1-7 are recomputed from
a synthetic clock (`struct movietime`, `:243-256`) pinned to **1998-01-01, Sunday-indexed
weekday 4**, with the time of day derived from `framecounter/60 + 43200` (noon plus one second
per 60 frames). This exists purely to make TAS runs deterministic and has no hardware analogue.

### 7.3 SMEM

4 bytes (`smpc.h:80`). Written only by SETSMEM (§4.12), read only into OREG12-15 (`:355-356`),
initialised at reset to `{0, 0, 0, syslanguageid}` (§0.4). Persisted only inside savestates.

---

## 8. Savestates

`SmpcSaveState` (`smpc.c:924-948`) writes tag `"SMPC"` version **3**, then, in order:
`IREG[7]`, `COMREG`, `OREG[32]`, `SR`, `SF`, `PDR[2]`, `DDR[2]`, `IOSEL`, `EXLE`, then the raw
`SmpcInternal` struct.

`SmpcLoadState` (`smpc.c:952-988`) reads the same 48 register bytes, then handles three legacy
`SmpcInternal` layouts: version 1 discriminates on `size - 48` against
`sizeof(SmpcInternal) - 8` or `24`; version 2 always uses `sizeof(SmpcInternal) - 8`; version
≥ 3 uses the full struct. The `- 8` accounts for the `clocksync` + `basetime` fields added
later.

Both functions carry `// Write/Read ID's of currently emulated peripherals(fix me)`
(`:945, 985`) — **the set of connected peripherals is not part of the savestate**, so a state
loaded with a different controller configuration than it was saved with silently mismatches.

For Mimas the useful signal is which state is considered architectural: the 48 register bytes
plus every field of `SmpcInternal` (including both `PortData_struct` snapshots). Note what is
*missing*: the file-scope `intback_wait_for_line` and `bustmp` (`smpc.c:68-69`).

---

## 9. `peripheral.c` — the peripheral model

### 9.1 Data model

There is no per-peripheral object. `PerAddPeripheral` (`peripheral.c:621-779`) appends bytes
directly into `PORTDATA1.data` / `PORTDATA2.data` (the INTBACK report buffer) and returns a
**pointer into that buffer**, aimed at the peripheral's ID byte. The `PerPad_struct`,
`PerMouse_struct`, `PerAnalog_struct` and `PerGun_struct` types (`peripheral.h:142-146`,
`207-211`, `248-252`, `284-288`) are just typed views over those bytes:

```c
typedef struct { u8 perid; u8 padbits[2];    } PerPad_struct;     // 3 bytes
typedef struct { u8 perid; u8 mousebits[3];  } PerMouse_struct;   // 4 bytes
typedef struct { u8 perid; u8 analogbits[9]; } PerAnalog_struct;  // 10 bytes
typedef struct { u8 perid; u8 gunbits[5];    } PerGun_struct;     // 6 bytes
```

So "pressing a button" is a bit-clear directly inside the byte stream INTBACK will later copy
into the OREGs. `PerGetId(p)` (`:783-787`) just dereferences the first byte.

### 9.2 `PerAddPeripheral` — building the port report

```c
int pernum = port->data[0] & 0xF;   // slots currently in use
int peroffset = 1, current = 1;

if (pernum == 0xF) return NULL;                                   // [DEAD] see below
else if (perid == PERGUN && pernum == 1) return NULL;             // :632

if (pernum == 0) {                       // first peripheral on this port
   pernum = 1;
   port->data[0] = (perid != PERGUN) ? 0xF1 : 0xA0;               // :640-643
} else {                                 // promote the port to a 6-slot multi-tap
   if (pernum == 1) {                    // was direct: pad out 5 empty slots
      u8 tmp = 1 + (port->data[1] & 0xF) + 1;
      for (i = 0; i < 5; i++) port->data[tmp + i] = 0xFF;         // :649-653
   }
   pernum = 6;  port->data[0] = 0x16;                             // :655-656
   current = 0; size = port->data[peroffset] & 0xF;               // walk to a free slot
   while (current < pernum && size != 0xF) {
      peroffset += size + 1; current++; size = port->data[peroffset] & 0xF;
   }
   if (current == pernum) return NULL;                            // tap full
   current++;
}
port->data[peroffset++] = perid;
```

Then a per-type initialiser fills the data bytes and sets `port->size`, and finally
(`:747-756`) any remaining tap slots are filled with single `0xFF` bytes, each incrementing
`port->size`.

**[DEAD]** The `pernum == 0xF` guard can never fire: `data[0]` is only ever `0xF0`, `0xF1`,
`0xA0` or `0x16`, whose low nibbles are `0`, `1`, `0`, `6`.

**[BUG]** Adding a second peripheral to a port that already holds a gun: the gun sets
`data[0] = 0xA0`, so `pernum == 0`, so the new peripheral takes the "first peripheral on this
port" branch and **overwrites the gun's ID byte and status byte**.

**[BUG]** The `perid == PERGUN && pernum == 1` guard (source comment *"Gun doesn't work with
multi-tap"*) only rejects a gun when exactly one peripheral is directly connected. If the port
has already been promoted to a tap (`pernum == 6`), a gun *is* accepted, and its initialiser
sets `port->size = 1` (`:729`) — truncating the entire port's report to the status byte and
discarding every other peripheral on that tap.

### 9.3 Per-type initial data and report layout

`peripheral.c:680-745`. `peroffset` below is the index of the peripheral's **first data byte**
(one past its ID byte); `port->size = peroffset + (perid & 0xF)` in every case except the gun.

| Type | ID | Bytes | Initial values | Line |
|---|---|---|---|---|
| `PERPAD` | `0x02` | 2 | `FF FF` | `:682-686` |
| `PERWHEEL` | `0x13` | 3 | `FF FF 7F` | `:687-692` |
| `PERMISSIONSTICK` | `0x15` | 5 | `FF FF 7F 7F 7F` | `:693-700` |
| `PERTWINSTICKS` | `0x19` | 9 | `FF FF 7F 7F 7F 7F 7F 7F` — **only 8 of the 9 bytes are initialised** | `:701-713` |
| `PER3DPAD` | `0x16` | 6 | `FF FF 7F 7F 7F 7F` | `:714-722` |
| `PERGUN` | `0x25` | 5 | `7C FF FF FF FF`, then **`port->size = 1`** | `:723-730` |
| `PERKEYBOARD` | `0x34` | 4 | `FF F8 06 00` | `:731-737` |
| `PERMOUSE` | `0xE3` | 3 | `00 00 00` | `:738-743` |

**[BUG]** `PERTWINSTICKS` declares 9 data bytes (`0x19 & 0xF == 9`, and `PerAnalog_struct` has
`analogbits[9]`) but the initialiser writes only `analogbits[0..7]`. `analogbits[8]` — written
by `PerAxis7Value` (`:559-565`) — starts at whatever was in the buffer, i.e. `0x00`, rather
than the neutral `0x7F` every other axis gets. The initialiser's own comments compound this:
it labels `analogbits[5..7]` "left stick", while the accessor comments label
`analogbits[6..8]` "left stick" (`:542, 550, 558`).

### 9.4 Digital pad bit layout (`PERPAD`, and the two digital bytes of every analog device)

Both bytes are **active-low**: a bit is `1` when released, `0` when pressed. `padbits[0]` is
`port->data[peroffset]`, `padbits[1]` is `port->data[peroffset+1]`.

**`padbits[0]` (first data byte):**

| Bit | Button | Press | Release | Lines |
|---|---|---|---|---|
| 7 | Right | `&= 0x7F` | `\|= ~0x7F` | `:189-198` |
| 6 | Left | `&= 0xBF` | `\|= ~0xBF` | `:202-211` |
| 5 | Down | `&= 0xDF` | `\|= ~0xDF` | `:176-185` |
| 4 | Up | `&= 0xEF` | `\|= ~0xEF` | `:163-172` |
| 3 | Start | `&= 0xF7` | `\|= ~0xF7` | `:215-224` |
| 2 | A | `&= 0xFB` | `\|= ~0xFB` | `:228-237` |
| 1 | C | `&= 0xFD` | `\|= ~0xFD` | `:254-263` |
| 0 | B | `&= 0xFE` | `\|= ~0xFE` | `:241-250` |

**`padbits[1]` (second data byte):**

| Bit | Button | Press | Release | Lines |
|---|---|---|---|---|
| 7 | R trigger | `&= 0x7F` | `\|= ~0x7F` | `:306-315` |
| 6 | X | `&= 0xBF` | `\|= ~0xBF` | `:267-276` |
| 5 | Y | `&= 0xDF` | `\|= ~0xDF` | `:280-289` |
| 4 | Z | `&= 0xEF` | `\|= ~0xEF` | `:293-302` |
| 3 | L trigger | `&= 0xF7` | `\|= ~0xF7` | `:319-328` |
| 2:0 | *(unused)* | — | — | initialised to `1` (`:683-684`) and never touched |

The release form `*bits |= ~0xEF` relies on the `int`→`u8` truncation of `~0xEF == 0xFFFFFF10`
to yield `0x10`. Correct, but fragile.

### 9.5 Analog devices (`PERWHEEL`, `PERMISSIONSTICK`, `PER3DPAD`, `PERTWINSTICKS`)

`PerAnalog_struct.analogbits[0..1]` are the two digital bytes above; `analogbits[2..8]` are the
seven axes, one byte each, neutral `0x7F`:

| Setter | Target | Source comment | Line |
|---|---|---|---|
| `PerAxis1Value` | `analogbits[2]` | — | `:458` |
| `PerAxis2Value` | `analogbits[3]` | — | `:500` |
| `PerAxis3Value` | `analogbits[4]` | inverted (`-(s8)val`) for `PERMISSIONSTICK` | `:525-531` |
| `PerAxis4Value` | `analogbits[5]` | — | `:535` |
| `PerAxis5Value` | `analogbits[6]` | "left stick L/R" | `:543` |
| `PerAxis6Value` | `analogbits[7]` | "left stick U/D" | `:551` |
| `PerAxis7Value` | `analogbits[8]` | "left stick throttle"; inverted for `PERTWINSTICKS`, with a copy-pasted comment saying *"axis inverted on mission stick"* | `:559-565` |

Which axes are actually reported depends on the declared data length: wheel = axis 1 only;
mission stick = axes 1-3; 3D pad = axes 1-4; twin sticks = axes 1-7.

**Digital synthesis from analog axes** — the axis setters also drive the digital direction bits
in `analogbits[0]`, with hysteresis:

| Device | Axis | Press threshold | Release threshold | Bit affected | Line |
|---|---|---|---|---|---|
| `PERWHEEL` | 1 | `val <= 0x67` | `val >= 0x6F` | 6 (Left) | `:468-471` |
| `PERWHEEL` | 1 | `val >= 0x97` | `val <= 0x8F` | 7 (Right) | `:474-477` |
| `PERMISSIONSTICK`, `PERTWINSTICKS` | 1 | `val <= 0x56` | `val >= 0x6A` | 6 (Left) | `:485-488` |
| `PERMISSIONSTICK`, `PERTWINSTICKS` | 1 | `val >= 0xAB` | `val <= 0x95` | 7 (Right) | `:491-494` |
| `PERMISSIONSTICK`, `PERTWINSTICKS` | 2 | `val <= 0x65` | `val >= 0x6A` | 4 (Up) | `:510-513` |
| `PERMISSIONSTICK`, `PERTWINSTICKS` | 2 | `val >= 0xA9` | `val <= 0x94` | 5 (Down) | `:516-519` |

`PER3DPAD` gets **no** digital synthesis — its D-pad must be driven through the ordinary
`PerPad*` entry points.

### 9.6 Mouse (`PERMOUSE`, ID `0xE3`, 3 data bytes)

Unlike the pad, mouse buttons are **active-high**.

| Byte | Bit | Meaning | Set / clear | Line |
|---|---|---|---|---|
| `mousebits[0]` | 0 | Left button (named "A") | `\|= 1` / `&= 0xFE` | `:332-340` |
| | 1 | Right button ("B") | `\|= 2` / `&= 0xFD` | `:356-364` |
| | 2 | Middle button ("C") | `\|= 4` / `&= 0xFFFB` (truncates to `0xFB`) | `:344-352` |
| | 3 | Start | `\|= 8` / `&= 0xF7` | `:368-376` |
| | 4 | X sign (`negx`) | computed by `PerMouseMove` | `:449` |
| | 5 | Y sign (`negy`) | computed by `PerMouseMove` | `:449` |
| | 6 | X overflow | **read and preserved, never set** | `:387, 449` |
| | 7 | Y overflow | **read and preserved, never set** | `:388, 449` |
| `mousebits[1]` | 7:0 | X displacement | see below | `:450-451` |
| `mousebits[2]` | 7:0 | Y displacement | see below | `:452-453` |

`PerMouseMove(mouse, dispx, dispy)` (`:380-454`) accumulates relative motion. Magnitudes are
kept internally as unsigned bytes; when the sign bit is set the stored byte is the **one's
complement** of the magnitude (`mousebits[1] = ~diffx`, `:450`), and it is decoded back the
same way on the next call (`:390-393`). The accumulate logic handles sign flips by hand
(`:395-447`); there is no saturation and no overflow-flag generation, so a large enough
accumulated delta simply wraps the byte.

`PerFlush` (`:791-804`) is what ends a "mouse frame":

```c
u8 perid = port->data[1];
if (perid == 0xE3) {
   PerMouse_struct * mouse = (PerMouse_struct *)(port->data + 1);
   mouse->mousebits[0] &= 0x0F;   // clear sign + overflow, keep buttons
   mouse->mousebits[1] = 0;
   mouse->mousebits[2] = 0;
}
```

It is called only from `SmpcINTBACKPeripheral` (`smpc.c:417-418`), on both ports, immediately
after the snapshot. Its own comment (`:793-794`) admits the limitation: **it only flushes a
mouse in the port's first slot**, and it hard-codes `0xE3` instead of using `PERMOUSE`. A mouse
behind a multi-tap accumulates deltas forever.

### 9.7 Keyboard (`PERKEYBOARD`, ID `0x34`, 4 data bytes)

Initialised to `FF F8 06 00` (`:731-737`) and **that is all**. There is no
`case PERKEYBOARD:` in the `PerUpdateConfig` dispatch at `:759-777`, so no input callbacks are
ever registered for it and no code anywhere modifies those four bytes. The keyboard is
report-shape-only: it can be enumerated by INTBACK and identified through DDR1 (`PDR1 = 0x71`,
`smpc.c:872-874`), but no key can ever be pressed. **[QUIRK]**

### 9.8 Input binding layer

| Structure | Definition | Role |
|---|---|---|
| `PerBaseConfig_struct` | `:61-67` | `{ u8 name; Press; Release; SetAxisValue; MoveAxis }` — the per-control callback quad |
| `PerConfig_struct` | `:69-73` | `{ u32 key; PerBaseConfig_struct *base; void *controller }` — one live binding |
| `perpadbaseconfig[13]` | `:79-93` | The 13 pad buttons, indices `PERPAD_UP`(0) … `PERPAD_Z`(12) |
| `permousebaseconfig[5]` | `:95-101` | `PERMOUSE_LEFT`(13) … `PERMOUSE_AXIS`(17) |
| `peranalogbaseconfig[7]` | `:103-111` | `PERANALOG_AXIS1`(18) … `PERANALOG_AXIS7`(24) |
| `pergunbaseconfig[3]` | `:113-117` | `PERGUN_TRIGGER`(25), `PERGUN_START`(27), `PERGUN_AXIS`(28) |

The control-name constants are in `peripheral.h:126-138, 199-203, 240-246, 280-282`. Note
`PERGUN_START` is `27`, **skipping 26** — nothing occupies that slot.

`PerUpdateConfig` (`:905-919`) appends `nelems` entries to the global `perkeyconfig` array via
`realloc`, setting only `base` and `controller`:

```c
perkeyconfig = realloc(perkeyconfig, perkeyconfigsize * sizeof(PerConfig_struct));
for (i = oldsize; i < perkeyconfigsize; i++) {
   perkeyconfig[i].base = baseconfig + j;
   perkeyconfig[i].controller = controller;
   j++;
}
```

**[BUG]** `perkeyconfig[i].key` is left **uninitialised** (`realloc` does not zero). Until the
front end calls `PerSetKey` for that entry, `PerKeyDown`/`PerKeyUp`/`PerAxisValue`/`PerAxisMove`
(`:808-886`) compare the incoming key against garbage and can spuriously fire the entry's
callback.

Dispatch is a **linear scan of the whole array with no early exit** (`:812-819` etc.), so one
host key may legitimately be bound to several controllers. `PerSetKey` (`:840-852`) matches on
`(base->name, controller)` and assigns `key`.

`PerPortReset` (`:890-901`) resets both ports to `data[0] = 0xF0`, `size = 1` and frees
`perkeyconfig`.

Registration happens at the end of `PerAddPeripheral` (`:759-777`):

| Peripheral | Config sets registered |
|---|---|
| `PERPAD` | `perpadbaseconfig` |
| `PERWHEEL`, `PERMISSIONSTICK`, `PER3DPAD`, `PERTWINSTICKS` | `perpadbaseconfig` **and** `peranalogbaseconfig` |
| `PERGUN` | `pergunbaseconfig` |
| `PERMOUSE` | `permousebaseconfig` |
| `PERKEYBOARD` | **none** |

A representative front end (`libretro/libretro.c:188-217`) calls `PerPortReset()`, then
`PerPadAdd`/`Per3DPadAdd` per player, then `PerSetKey((player << 8) + control, control, ctrl)`
for every control — i.e. the "key" namespace is `(player << 8) | control_id`.

### 9.9 The peripheral core interface

`PerInterface_struct` (`peripheral.h:63-74`): `{ id, Name, Init, DeInit, HandleEvents, Scan,
canScan, Flush, KeyName }`. `PerInit(coreid)` (`:126-151`) scans the front-end-supplied
`PERCoreList[]` for a matching `id` (`PERCORE_DEFAULT == -1` means "take the first one") and
calls its `Init`. `PERCore->HandleEvents()` is the front end's per-frame poll, which is also
where `YabauseExec()` is driven from (see `PERDummyHandleEvents`, `:1008-1013`).

`PERDummy` (`:982-992`, id `PERCORE_DUMMY == 0`) is the null core: it registers no inputs and
its `HandleEvents` just runs the emulator.

`PERSF_*` scan flags (`peripheral.h:56-61`): `PERSF_KEY` `1<<0`, `PERSF_BUTTON` `1<<1`,
`PERSF_HAT` `1<<2`, `PERSF_AXIS` `1<<3`, `PERSF_MOUSEMOVE` `1<<4`, `PERSF_ALL` `0xFFFFFFFF`.
These are for input-configuration UIs, not emulation.

---

## 10. Known deviations / gaps in this implementation

### 10.1 Summary table

| # | Item | Kind | Where |
|---|---|---|---|
| 1 | MSHON (`0x00`), CDON (`0x08`), CDOFF (`0x09`), SYSRES (`0x0D`) are log-only stubs | QUIRK | §4.2, §4.7, §4.8 |
| 2 | No SETTIME or any other RTC-write command exists (`0x16` is unhandled); the RTC is read-only | QUIRK | §3.4, §7.1 |
| 3 | Command codes `0x01`, `0x04`, `0x05`, `0x0A`-`0x0C`, `0x11`-`0x16`, `0x1B`+ have no handler; they only log | QUIRK | §3.4 |
| 4 | `SmpcSetTiming` leaves `timing` unassigned when `IREG0 ∉ {0x00, 0x01}` on INTBACK → the command never dispatches and SF never clears | BUG | §3.3 |
| 5 | `SmpcINTBACK`'s `SF = 1` at `:472` is immediately undone by `SmpcExec`'s `SF = 0` at `:628` | DEAD | §2.2 |
| 6 | `SmpcRegs->SF &= val` in the write handler is a no-op — the preceding unconditional store already assigned SF | DEAD | §1.3 |
| 7 | Unrecognised COMREG clears SF instantly (`:727`) but leaves `timing` alone | QUIRK | §2.2 |
| 8 | SSHON, SSHOFF, CKCHG320/352, MSHON, CDON, CDOFF, SYSRES do not write the OREG31 command echo | QUIRK | §2.3 |
| 9 | Only INTBACK raises an interrupt; `ScuSendPadInterrupt` (vector `0x48`) is never called from anywhere | QUIRK | §2.3 |
| 10 | NMIREQ and the reset button use `SH2SendInterrupt` directly, so they do not set `ICR` bit 15 the way `SH2NMI` does | QUIRK | §4.13, §4.16 |
| 11 | CKCHG320/352 hard-reset VDP1, VDP2, SCU and SCSP, and stop the slave SH-2; the "Clear VDP1/VDP2 ram" comment is not implemented | QUIRK | §4.9 |
| 12 | CKCHG timing (`timing = 1`) is admittedly untested — source comment *"this has to be tested on a real saturn"* | QUIRK | §3.3 |
| 13 | `M68KStart`/`M68KStop` both reset the 68000 and neither resets the SCSP (`ScspReset()` commented out) | QUIRK | §4.5, §4.6 |
| 14 | `SmpcSetClockSync` (declared `int`) falls off the end without returning | BUG | §0.2 |
| 15 | INTBACK multi-chunk copies read from `data` instead of `data + offset`, so chunks 2+ re-send the first 32 bytes | BUG | §5.4 |
| 16 | `OREG[31] = 0x10` at `:496` overwrites the 32nd byte of peripheral data on the first peripheral-only INTBACK — and is applied inconsistently (not on continuations) | BUG | §5.2, §5.4 |
| 17 | INTBACK break and `SmpcINTBACKEnd` clear `intback` but not `port1.size`/`port2.size`, so a stale snapshot is resumed on the next INTBACK | BUG | §5.4 |
| 18 | `SmpcRegs->SR = 0x40` at `:494` is overwritten by `SmpcINTBACKPeripheral` on the next line | DEAD | §5.2 |
| 19 | SR never signals "this was the last chunk"; the game must infer exhaustion from the report contents | QUIRK | §5.3 |
| 20 | INTBACK status leaves OREG16-30 stale — 15 undefined bytes | QUIRK | §5.6 |
| 21 | OREG8 (cartridge code) is hard-coded `0`, marked `// FIXME : random value` | QUIRK | §5.6 |
| 22 | OREG11 (CDRES) is always `0`; `cdres`, `mshnmi`, `sysres`, `sndres` are never set to anything but 0 | QUIRK | §0.1, §5.6 |
| 23 | `intback_wait_for_line` pins the peripheral fetch to hard-coded scanline 207 | QUIRK | §3.2 |
| 24 | `clocksync` advances the RTC at `frame_count * 1001/60000` s — NTSC-only, ~20 % slow in PAL | QUIRK | §7.1 |
| 25 | Movie recording/playback replaces the RTC with a fixed 1998-01-01 synthetic clock | HACK | §7.2 |
| 26 | `LagFrameFlag = 0` inside `SmpcINTBACKPeripheral` is TAS bookkeeping in the middle of hardware emulation | HACK | §5.4 |
| 27 | `IOSEL` is stored and never read — both the SMPC path and the direct-access path are always live | QUIRK | §6.4 |
| 28 | No `case 0x7B` — writing DDR2 has no side effect, so port 2 has no ID handshake | QUIRK | §1.2, §6.3 |
| 29 | PDR2 has no `0x40` (TH-ID) control method; the Mega Drive ID handshake is port-1-only | QUIRK | §6.2 |
| 30 | `do_th_mode` exists solely to make *World Heroes Perfect* recognise a Saturn pad | HACK | §6.2 |
| 31 | Writing DDR immediately mutates PDR; DDR is treated as a 7-bit mode selector, not per-pin direction bits | QUIRK | §6.1, §6.3 |
| 32 | Control methods other than `0x00`, `0x40`, `0x60` only log "not implemented"; wheel, mission stick and twin sticks have no direct-access support at all | QUIRK | §6.1, §6.3 |
| 33 | The VDP2 external latch fires once per frame at V-Blank OUT, not at the beam position, and only from port 1 | QUIRK | §6.4 |
| 34 | Gun coordinates are clamped to a hard-coded 320×224 (`// fix me` in the source) | QUIRK | §6.5 |
| 35 | A gun's `port->size = 1` means it never appears in the INTBACK report at all | QUIRK | §5.5, §9.3 |
| 36 | Adding a second peripheral to a port holding a gun silently overwrites the gun | BUG | §9.2 |
| 37 | A gun *is* accepted into an already-promoted multi-tap, and then truncates the whole port report to 1 byte | BUG | §9.2 |
| 38 | `pernum == 0xF` guard in `PerAddPeripheral` is unreachable | DEAD | §9.2 |
| 39 | `PERTWINSTICKS` leaves its 9th data byte (`analogbits[8]`, axis 7) uninitialised at `0x00` instead of neutral `0x7F`; the init and accessor comments disagree about which bytes are the "left stick" | BUG | §9.3, §9.5 |
| 40 | Axis-3 inversion is `PERMISSIONSTICK`-only and axis-7 inversion is `PERTWINSTICKS`-only, but both carry the comment *"axis inverted on mission stick"* | QUIRK | §9.5 |
| 41 | Mouse overflow bits (6, 7) are read and preserved but never generated | QUIRK | §9.6 |
| 42 | `PerFlush` only clears a mouse in the port's **first** slot and hard-codes `0xE3`; the source flags this as a FIXME | QUIRK | §9.6 |
| 43 | `PerMouseMiddleReleased` masks with `0xFFFB` against a `u8` | QUIRK | §9.6 |
| 44 | `PERKEYBOARD` registers no input callbacks — it can be enumerated but no key can ever be pressed | QUIRK | §9.7 |
| 45 | `PerUpdateConfig` leaves `PerConfig_struct.key` uninitialised after `realloc`; unbound entries can fire on garbage key values | BUG | §9.8 |
| 46 | Only byte accesses work; word/long reads return 0 and word/long writes are silently dropped | QUIRK | §1.4 |
| 47 | Even byte addresses alias the odd register addresses for storage but not for side effects (e.g. writing COMREG at `0x1E` stores the value but never arms the command) | QUIRK | §1.1 |
| 48 | OREG0-31 and SR are freely writable by the CPU | QUIRK | §1.2 |
| 49 | 16 bytes of `padding`/`padding2` (`0x0F`-`0x1D`, `0x65`-`0x73`) are readable/writable scratch modelling nothing | QUIRK | §1.2 |
| 50 | `bustmp` and `intback_wait_for_line` are file-scope globals excluded from savestates; the connected-peripheral set is excluded too (`// fix me`) | QUIRK | §8 |
| 51 | `SmpcInternal` fields `ste`, `resb`, `intbackIreg0`, `syslngid` are written and never read | DEAD | §0.1 |
| 52 | The port-status and peripheral-ID comment blocks in `smpc.c` (`:374-406`, `:381-391`) describe values the code never produces (Sega-tap `0x04`, clock-serial `0x21`-`0x2F`, Mega Drive pads `0xE1`/`0xE2`, pointing device `0x23` vs. the defined `PERGUN = 0x25`) | QUIRK | §5.5 |

### 10.2 The larger structural gaps

Beyond the individual defects above, four things about this implementation are structurally
unlike the hardware and matter for how Mimas should be designed:

1. **There is no serial peripheral bus.** Real SMPC talks to controllers over a clocked serial
   protocol whose timing is what makes INTBACK slow. Here, a "peripheral" is a pre-formatted
   byte array that the front end mutates in place, and INTBACK is a `memcpy`. Peripheral
   *timing* is modelled only as two magic numbers (`250` and `16000` µs) plus a hard-coded
   scanline.

2. **The register file has no read/write discipline.** Every one of the 64 bytes is a plain
   RAM cell that both the CPU and the SMPC can write; direction is enforced nowhere. The only
   register with any read logic at all is SF.

3. **The direct-access port (PDR/DDR) is response synthesis, not pin emulation.** Values are
   computed at write time from a table of "what would peripheral X answer here", switched on a
   7-bit view of DDR that is treated as an enum. Any game whose access pattern is not exactly
   one of the three recognised patterns falls into a log-only default.

4. **State ownership is split awkwardly.** `PORTDATA1`/`PORTDATA2` are globals owned by
   `peripheral.c` and mutated by the front end at arbitrary times; the SMPC snapshots them
   under a condition (`both sizes == 0`) that can be left permanently unsatisfied by a
   mid-sequence break. Mimas should own peripheral state behind an explicit sample/latch step
   rather than replicating this.
