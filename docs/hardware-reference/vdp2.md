# VDP2 — Background / Scroll-Plane Compositor

**Provenance.** Every statement in this document is derived exclusively from the following
Yabause / YabaSanshiro source files. Nothing here comes from external Saturn documentation or
from general knowledge. Where the code is ambiguous, self-contradictory, or clearly a
workaround rather than hardware behaviour, that is called out explicitly instead of guessed at.

| File | Role |
|---|---|
| `yabause/src/vdp2.h` | Register struct + offsets, `Vdp2External`, per-line register snapshot types |
| `yabause/src/vdp2.cpp` | Register read/write dispatch, VRAM/CRAM access, timing, VRAM cycle-pattern analysis |
| `yabause/src/vidsoft.c` | Software rasteriser: per-pixel format decode, scroll/rotation math, window tests |
| `yabause/src/vidshared.h` | Register-field decode helpers (`Read*Data`), rotation math, sprite-type decode |
| `yabause/src/vidshared.c` | Plane-address computation, rotation parameter table reader, coefficient reader |
| `yabause/src/titan/titan.c`, `titan.h` | Priority resolution and colour-calculation blending |
| `yabause/src/vdp2debug.c` | Debug string dump — used only as a **cross-check** on field meanings |
| `yabause/src/memory.c` | `getVramCycle()` — consumer of the cycle-pattern CPU-penalty result |

`vidogl.c` was deliberately **not** read, per the project's own guidance, with one exception: a
`grep` for `AC_VRAM` was used to establish which VRAM timeslot codes any renderer actually
distinguishes. Those four grep hits are cited where used and nothing else from that file is
relied on.

Line numbers are approximate (±2) and refer to the tree as checked out.

---

## 0. Address map and memory model

| Region | Base | Size | Source |
|---|---|---|---|
| VDP2 VRAM | `0x25E00000` (also `0x05E00000`) | `0x80000` (512 KiB) | `vdp2.cpp:379`, `vdp2debug.c:166` |
| VDP2 Colour RAM (CRAM) | `0x25F00000` | `0x1000` (4 KiB) | `vdp2.cpp:382`, `vdp2.cpp:2538` |
| VDP2 registers | `0x25F80000` | `0x200` (offsets `0x000`–`0x11E`) | `vdp2.h:85`, `vdp2.cpp:1457` |

* VRAM accesses mask the address with `& 0x7FFFF` (`vdp2.cpp:174`, `180`, `187`, `198`, `228`, `258`).
* CRAM accesses mask with `& 0xFFF` (`vdp2.cpp:288`, `294`, `301`, `309`, `317`, `343`).
* Register accesses mask with `& 0x1FF` (`vdp2.cpp:1450`, `1457`, `1920`, `1928`, `1934`).
* **Byte and long register access are not implemented as such.** `Vdp2ReadByte` and
  `Vdp2ReadLong` unconditionally return `0` (`vdp2.cpp:1448`, `1918`); `Vdp2WriteByte` is a no-op
  (`vdp2.cpp:1926`); `Vdp2WriteLong` is decomposed into two word writes, high half first
  (`vdp2.cpp:2388`).
* VRAM writes set one of four "bank dirty" flags — `A0_Updated` for `0x00000–0x1FFFF`,
  `A1_Updated` for `0x20000–0x3FFFF`, `B0_Updated` for `0x40000–0x5FFFF`, `B1_Updated` for
  `0x60000–0x7FFFF` (`vdp2.cpp:208–219`). Note this is a fixed physical quartering of VRAM and
  does **not** consult `RAMCTL`'s bank-partition bits.

### 0.1 Colour RAM modes

`Vdp2Internal.ColorMode` is set from `RAMCTL` bits 12–13 on every register write to `0x00E`
(`vdp2.cpp:1962–1967`). It selects how a colour index is turned into a pixel
(`Vdp2ColorRamGetColorSoft`, `vidsoft.c:206–235`):

| `ColorMode` | Entry size | Address computation | Decode |
|---|---|---|---|
| 0 | 16-bit | `(index << 1) & 0xFFF` | `(t&0x1F)<<3 \| (t&0x3E0)<<6 \| (t&0x7C00)<<9 \| (t&0x8000)<<16` |
| 1 | 16-bit | `(index << 1) & 0xFFF` | identical code path to mode 0 |
| 2 | 32-bit | `(index << 2) & 0xFFF` | `T2ReadLong` returned verbatim |

* The 5-bit channels are expanded to 8 bits by left-shifting, **without** replicating the high
  bits into the low bits — so the maximum value produced is `0xF8`, not `0xFF`.
* CRAM bit 15 is preserved by relocating it to bit 31 of the returned pixel. This is the flag
  consumed by special colour-calculation mode 3 (`vidsoft.c:215`, `746`).
* In `ColorMode == 0` only, CRAM word/long writes below `0x800` are **mirrored** to `addr+0x800`
  (`vdp2.cpp:325–330`, `353–358`), i.e. mode 0 behaves as a 1024-entry table aliased across the
  full 4 KiB.
* Modes 0 and 1 have byte-for-byte identical decode code in `vidsoft.c`. The distinction between
  them (1024 vs 2048 entries) is expressed *only* through the mode-0 mirroring in the write path.

### 0.2 Internal pixel format

Layers are rasterised into an intermediate 32-bit pixel (`COLSAT2YAB16` / `COLSAT2YAB32`,
`vidsoft.c:54–56`, little-endian variants):

```
COLSAT2YAB16(a, t) = (a << 24) | ((t & 0x1F) << 3) | ((t & 0x3E0) << 6) | ((t & 0x7C00) << 9)
COLSAT2YAB32(a, t) = (a << 24) | (t & 0xFFFFFF)
```

So the word is `0xAA_BB_GG_RR` — byte 0 holds the channel that came from CRAM bits 0–4. The
final buffer is uploaded as `GL_RGBA/GL_UNSIGNED_BYTE` (`vidsoft.c:3880`), so byte 0 is red.

> **Naming inconsistency (harmless).** `titan.c:100–103` defines `TitanGetRed(p) = (p>>16)&0xFF`
> and `TitanGetBlue(p) = p & 0xFF`, i.e. the reverse of the above. All blending operations in
> `titan.c` are channel-symmetric, so the mislabelling has no visible effect, but a
> reimplementation should not copy the naming.

The `a` field is a **6-bit alpha in bits 24–29**, plus a flag in bit 31 (see §B.11). Bit 31 set
is what `TitanTransBit` tests (`titan.c:289`).

---

# Section A — Registers

## A.0 Complete register index

All registers are 16-bit. "Offset" is from `0x25F80000`. R/W column reflects what
`Vdp2ReadWord`/`Vdp2WriteWord` actually do (`vdp2.cpp:1456–1914`, `1933–2384`).

| Offset | Name | R/W | Purpose |
|---|---|---|---|
| `0x000` | TVMD | R/W | Display on, border mode, interlace, V-res, H-res |
| `0x002` | EXTEN | R/W | External signal enable; **read has the side effect of latching V-counter** |
| `0x004` | TVSTAT | R only | PAL/NTSC, field, H/V blank, external latch flags; **read clears bits 8–9** |
| `0x006` | VRSIZE | R/W | VRAM size (bit 15) + version field |
| `0x008` | HCNT | R only | H counter |
| `0x00A` | VCNT | R only | V counter |
| `0x00C` | — | — | Reserved; reads `0`, writes ignored (`vdp2.cpp:1492`, `1957`) |
| `0x00E` | RAMCTL | R/W | CRAM mode, VRAM bank partitioning, per-bank usage, coefficient-in-CRAM |
| `0x010` | CYCA0L | R/W | VRAM-A0 access timeslots T0–T3 |
| `0x012` | CYCA0U | R/W | VRAM-A0 access timeslots T4–T7 |
| `0x014` | CYCA1L | R/W | VRAM-A1 timeslots T0–T3 |
| `0x016` | CYCA1U | R/W | VRAM-A1 timeslots T4–T7 |
| `0x018` | CYCB0L | R/W | VRAM-B0 timeslots T0–T3 |
| `0x01A` | CYCB0U | R/W | VRAM-B0 timeslots T4–T7 |
| `0x01C` | CYCB1L | R/W | VRAM-B1 timeslots T0–T3 |
| `0x01E` | CYCB1U | R/W | VRAM-B1 timeslots T4–T7 |
| `0x020` | BGON | R/W | Layer enable + per-layer transparency-code disable |
| `0x022` | MZCTL | R/W | Mosaic enable per layer + mosaic cell size |
| `0x024` | SFSEL | R/W | Per-layer select of which SFCODE byte to use |
| `0x026` | SFCODE | R/W | Two 8-bit special-function colour-code masks |
| `0x028` | CHCTLA | R/W | NBG0/NBG1 character size, bitmap enable/size, colour format |
| `0x02A` | CHCTLB | R/W | NBG2/NBG3/RBG0 character size, bitmap enable/size, colour format |
| `0x02C` | BMPNA | R/W | NBG0/NBG1 bitmap palette number + special bits |
| `0x02E` | BMPNB | R/W | RBG0 bitmap palette number + special bits |
| `0x030` | PNCN0 | R/W | NBG0 pattern-name control |
| `0x032` | PNCN1 | R/W | NBG1 pattern-name control |
| `0x034` | PNCN2 | R/W | NBG2 pattern-name control |
| `0x036` | PNCN3 | R/W | NBG3 pattern-name control |
| `0x038` | PNCR | R/W | RBG0 pattern-name control |
| `0x03A` | PLSZ | R/W | Plane size per layer + rotation screen-over mode |
| `0x03C` | MPOFN | R/W | Map offset (upper plane-address bits) for NBG0–3 |
| `0x03E` | MPOFR | R/W | Map offset for rotation parameters A and B |
| `0x040` | MPABN0 | R/W | NBG0 planes A,B |
| `0x042` | MPCDN0 | R/W | NBG0 planes C,D |
| `0x044` | MPABN1 | R/W | NBG1 planes A,B |
| `0x046` | MPCDN1 | R/W | NBG1 planes C,D |
| `0x048` | MPABN2 | R/W | NBG2 planes A,B |
| `0x04A` | MPCDN2 | R/W | NBG2 planes C,D |
| `0x04C` | MPABN3 | R/W | NBG3 planes A,B |
| `0x04E` | MPCDN3 | R/W | NBG3 planes C,D |
| `0x050`–`0x05E` | MPABRA … MPOPRA | R/W | Rotation parameter A planes A–P (8 registers, 2 planes each) |
| `0x060`–`0x06E` | MPABRB … MPOPRB | R/W | Rotation parameter B planes A–P |
| `0x070` | SCXIN0 | R/W | NBG0 horizontal scroll, integer part |
| `0x072` | SCXDN0 | R/W | NBG0 horizontal scroll, fractional part |
| `0x074` | SCYIN0 | R/W | NBG0 vertical scroll, integer part |
| `0x076` | SCYDN0 | R/W | NBG0 vertical scroll, fractional part |
| `0x078` | ZMXIN0 | R/W | NBG0 horizontal coordinate increment, integer (`ZMXN0.part.I`) |
| `0x07A` | ZMXDN0 | R/W | NBG0 horizontal coordinate increment, fraction (`ZMXN0.part.D`) |
| `0x07C` | ZMYIN0 | R/W | NBG0 vertical coordinate increment, integer |
| `0x07E` | ZMYDN0 | R/W | NBG0 vertical coordinate increment, fraction |
| `0x080`–`0x08E` | SCXIN1 … ZMYDN1 | R/W | Same eight registers for NBG1 |
| `0x090` | SCXN2 | R/W | NBG2 horizontal scroll (integer only) |
| `0x092` | SCYN2 | R/W | NBG2 vertical scroll |
| `0x094` | SCXN3 | R/W | NBG3 horizontal scroll |
| `0x096` | SCYN3 | R/W | NBG3 vertical scroll |
| `0x098` | ZMCTL | R/W | Reduction enable for NBG0/NBG1 |
| `0x09A` | SCRCTL | R/W | Line-scroll / line-zoom / vertical-cell-scroll enables and interval |
| `0x09C` | VCSTAU | R/W | Vertical cell scroll table address, upper (`VCSTA.part.U`) |
| `0x09E` | VCSTAL | R/W | …lower |
| `0x0A0` | LSTA0U | R/W | NBG0 line-scroll table address, upper |
| `0x0A2` | LSTA0L | R/W | …lower |
| `0x0A4` | LSTA1U | R/W | NBG1 line-scroll table address, upper |
| `0x0A6` | LSTA1L | R/W | …lower |
| `0x0A8` | LCTAU | R/W | Line colour screen table address, upper + per-line flag |
| `0x0AA` | LCTAL | R/W | …lower |
| `0x0AC` | BKTAU | R/W | Back screen table address, upper + per-line flag |
| `0x0AE` | BKTAL | R/W | …lower |
| `0x0B0` | RPMD | R/W | Rotation parameter mode (A / B / coefficient-switched / window-switched) |
| `0x0B2` | RPRCTL | R/W | Rotation parameter read control — **never read by any renderer** |
| `0x0B4` | KTCTL | R/W | Coefficient table enable, data size, mode, line-colour enable, per parameter |
| `0x0B6` | KTAOF | R/W | Coefficient table address offset, per parameter |
| `0x0B8` | OVPNRA | R/W | Parameter A screen-over pattern name — **never read by any renderer** |
| `0x0BA` | OVPNRB | R/W | Parameter B screen-over pattern name — **never read** |
| `0x0BC` | RPTAU | R/W | Rotation parameter table address, upper |
| `0x0BE` | RPTAL | R/W | …lower |
| `0x0C0` | WPSX0 | R/W | Window 0 horizontal start |
| `0x0C2` | WPSY0 | R/W | Window 0 vertical start |
| `0x0C4` | WPEX0 | R/W | Window 0 horizontal end |
| `0x0C6` | WPEY0 | R/W | Window 0 vertical end |
| `0x0C8`–`0x0CE` | WPSX1 … WPEY1 | R/W | Window 1 coordinates |
| `0x0D0` | WCTLA | R/W | Window control: NBG0 (low byte), NBG1 (high byte) |
| `0x0D2` | WCTLB | R/W | Window control: NBG2 (low), NBG3 (high) |
| `0x0D4` | WCTLC | R/W | Window control: RBG0 (low), sprite (high) |
| `0x0D6` | WCTLD | R/W | Rotation-parameter window (low), colour-calculation window (high) |
| `0x0D8` | LWTA0U | R/W | Line window 0 table address, upper + enable (bit 31 of the pair) |
| `0x0DA` | LWTA0L | R/W | …lower |
| `0x0DC` | LWTA1U | R/W | Line window 1 table address, upper + enable |
| `0x0DE` | LWTA1L | R/W | …lower |
| `0x0E0` | SPCTL | R/W | Sprite type, sprite window enable, framebuffer format, CC condition |
| `0x0E2` | SDCTL | R/W | Shadow enable per layer + transparent-shadow enable |
| `0x0E4` | CRAOFA | R/W | CRAM address offset for NBG0–3 |
| `0x0E6` | CRAOFB | R/W | CRAM address offset for RBG0 and sprite |
| `0x0E8` | LNCLEN | R/W | Line colour screen enable per layer |
| `0x0EA` | SFPRMD | R/W | Special priority mode, 2 bits per layer |
| `0x0EC` | CCCTL | R/W | Colour calculation enable per layer + global blend mode |
| `0x0EE` | SFCCMD | R/W | Special colour calculation mode, 2 bits per layer |
| `0x0F0` | PRISA | R/W | Sprite priority 0 (low byte) and 1 (high byte) |
| `0x0F2` | PRISB | R/W | Sprite priority 2, 3 |
| `0x0F4` | PRISC | R/W | Sprite priority 4, 5 |
| `0x0F6` | PRISD | R/W | Sprite priority 6, 7 |
| `0x0F8` | PRINA | R/W | NBG0 priority (low), NBG1 priority (high) |
| `0x0FA` | PRINB | R/W | NBG2 priority (low), NBG3 priority (high) |
| `0x0FC` | PRIR | R/W | RBG0 priority |
| `0x0FE` | — | — | Reserved; reads `0`, writes ignored (`vdp2.cpp:1853`, `2327`) |
| `0x100` | CCRSA | R/W | Sprite colour-calc ratio 0, 1 |
| `0x102` | CCRSB | R/W | Sprite colour-calc ratio 2, 3 |
| `0x104` | CCRSC | R/W | Sprite colour-calc ratio 4, 5 |
| `0x106` | CCRSD | R/W | Sprite colour-calc ratio 6, 7 |
| `0x108` | CCRNA | R/W | NBG0 ratio (low), NBG1 ratio (high) |
| `0x10A` | CCRNB | R/W | NBG2 ratio (low), NBG3 ratio (high) |
| `0x10C` | CCRR | R/W | RBG0 colour-calc ratio |
| `0x10E` | CCRLB | R/W | Line colour screen / back screen colour-calc ratio |
| `0x110` | CLOFEN | R/W | Colour offset enable per layer |
| `0x112` | CLOFSL | R/W | Colour offset select (A or B) per layer |
| `0x114` | COAR | R/W | Colour offset A, red |
| `0x116` | COAG | R/W | Colour offset A, green |
| `0x118` | COAB | R/W | Colour offset A, blue |
| `0x11A` | COBR | R/W | Colour offset B, red |
| `0x11C` | COBG | R/W | Colour offset B, green |
| `0x11E` | COBB | R/W | Colour offset B, blue |

Any offset outside this table falls through to a `LOG("Unhandled VDP2 word write")` and returns
`0` / does nothing (`vdp2.cpp:1905`, `2378`).

---

## A.1 Display mode and status

### TVMD — `0x000`

| Bits | Field | Behaviour per code |
|---|---|---|
| 0–2 | HRESO | Horizontal resolution, see table below (`vidsoft.c:4040–4070`) |
| 1–2 | (reused) | `(TVMD >> 1) & 3` selects the window-coordinate scaling mode (`vidshared.h:595`) |
| 4–5 | VRESO | `0`→224, `1`→240, `2`→256, `3` leaves the previous value (`vidsoft.c:4078–4090`) |
| 6–7 | LSMD | `3` = double-density interlace; `0`, `2` and default = non-interlace (`vidsoft.c:4093–4104`) |
| 8 | BDCLMD | Border colour: 0 = black, 1 = back screen (`vidsoft.c:1452`, `vdp2debug.c:1462`) |
| 15 | DISP | Master display enable |

Horizontal resolution decode (`vidsoft.c:4040`):

| HRESO | `vdp2width` | `rbg0width` | `vdp2_x_hires` |
|---|---|---|---|
| 0, 4 | 320 | 320 | 0 |
| 1, 5 | 352 | 352 | 0 |
| 2, 6 | 640 | 320 | 1 |
| 3, 7 | 704 | 352 | 1 |

Note that HRESO bit 2 (the "exclusive monitor" bit) does not change the pixel counts in this
implementation — codes 4–7 duplicate 0–3. Rotation layers are always rasterised at
`rbg0width` and doubled horizontally when `vdp2_x_hires` is set (`vidsoft.c:1090–1096`).

Double-density interlace doubles `vdp2height` (`vidsoft.c:4096`). Note `rbg0height` is *not*
doubled, so RBG0 covers only the top half of an interlaced field buffer.

Side effects of a write to TVMD (`vdp2.cpp:1938–1941`):

```
yabsys.VBlankLineCount = 225 + (val & 0x30);
```

giving 225 / 241 / 257 active lines for VRESO 0 / 1 / 2.

`DISP` is also polled at frame level: `Vdp2DrawScreens` is only invoked when `TVMD & 0x8000`
(`vdp2.cpp:1271`), and the sprite layer read-out is skipped when it is clear
(`vidsoft.c:3566`).

### EXTEN — `0x002`

| Bits | Field | Behaviour |
|---|---|---|
| 8 | EXSYEN | External sync enable — only surfaced in the debug dump (`vdp2debug.c:1534`) |
| 9 | EXLTEN | External latch enable |

**Reading EXTEN has a side effect.** If bit 9 is *clear*, a read latches the V counter and sets
the external-latch flag (`vdp2.cpp:1463–1471`):

```
Vdp2Regs->VCNT   = yabsys.LineCount;
Vdp2Regs->TVSTAT |= 0x200;
```

`HCNT` is explicitly left untouched here (there is a commented-out `Vdp2Regs->HCNT = ?`).

If bit 9 is *set*, latching instead happens at VBLANK-OUT from the SMPC peripheral port, gated
additionally on `SmpcRegs->EXLE & 0x1` (`vdp2.cpp:1429–1434`), calling
`Vdp2SendExternalLatch(hcnt, vcnt)` which does (`vdp2.cpp:1439–1444`):

```
HCNT = hcnt << 1;   VCNT = vcnt;   TVSTAT |= 0x200;
```

The comment at `vdp2.cpp:1429` notes this is not cycle-accurate — it should fire on the line the
external event occurs, not once per frame.

### TVSTAT — `0x004` (read-only)

| Bit | Field | Set by |
|---|---|---|
| 0 | PAL | Not written by VDP2 code; preserved across reset (`vdp2.cpp:440`: `TVSTAT & 0x1`) |
| 1 | ODD | Field parity, updated at VBLANK-OUT (`vdp2.cpp:1425`) |
| 2 | HBLANK | Set in `Vdp2HBlankIN` (`vdp2.cpp:820`), cleared in `Vdp2HBlankOUT` (`vdp2.cpp:835`) |
| 3 | VBLANK | Set in VBLANK-IN (`vdp2.cpp:755`), cleared at VBLANK-OUT (`vdp2.cpp:1425`) |
| 8 | EXSYFG | Cleared on read |
| 9 | EXLTFG | Set by external latch; cleared on read |

Read behaviour (`vdp2.cpp:1473–1485`):

```
tvstat = TVSTAT;
TVSTAT &= 0xFCFF;                       // clear bits 8 and 9
return (TVMD & 0x8000) ? tvstat : (tvstat | 0x8);
```

i.e. **when DISP is clear, reads always report VBLANK set**, regardless of the internal state.

Field parity update at VBLANK-OUT (`vdp2.cpp:1416–1425`): if `(TVMD >> 6) & 3 == 0`
(non-interlace), `vdp2_is_odd_frame` is forced to 1 every frame; otherwise it toggles. Then
`TVSTAT = (TVSTAT & ~0x8 & ~0x2) | (vdp2_is_odd_frame << 1)`.

`vdp2_is_odd_frame` also drives which scanline parity is rendered in interlace mode
(`vidsoft.c:805–825`).

### VRSIZE — `0x006`

| Bits | Field | Behaviour |
|---|---|---|
| 0–3 | VER | Version. `Vdp2Reset` sets the whole register to 0 with the comment `// fix me(version should be set)` (`vdp2.cpp:441`) |
| 15 | VRAMSZ | 0 = 4 Mbit, 1 = 8 Mbit |

Bit 15 is consulted in four distinct places, and its effect differs in each:

1. **Character address width** (`vidsoft.c:313`): if clear, `charaddr &= 0x3FFF` before the
   `× 0x20` scale, capping character data at 512 KiB/32 = the low half of the address space.
2. **Plane address computation** (`vidshared.h:397`): selects a different mask on the map value
   for the 1-word-pattern / 16×16-cell case only (`tmp` unmasked vs `tmp & 0xFF`). All other
   combinations are identical between the two branches.
3. **Back screen address** (`vidsoft.c:1467`): `BKTAU & 0x7` vs `BKTAU & 0x3`.
4. **Line colour screen address** (`vidsoft.c:1508`, `1284`): `LCTA & 0x7FFFF` vs `& 0x3FFFF`.

Note `CalcPlaneAddr` reads the **global** `Vdp2Regs->VRSIZE` rather than the `regs` parameter it
is given (`vidshared.h:397`), so it ignores the per-line register snapshot and the threaded
register copy. A reimplementation should use the snapshot.

### HCNT — `0x008`, VCNT — `0x00A` (read-only)

Writes are explicitly discarded (`vdp2.cpp:1951–1956`). Both are only ever written by the latch
paths described under EXTEN. There is no free-running counter emulation in this code — `VCNT`
takes the value of `yabsys.LineCount` at latch time and `HCNT` is only ever set from the
external-latch path.

---

## A.2 RAMCTL — `0x00E`

| Bits | Field | Meaning as used by the code |
|---|---|---|
| 0–1 | VRAM-A0 usage | `1` = coefficient table (`vidshared.c:363`, `376`) |
| 2–3 | VRAM-A1 usage | `1` = coefficient table (`vidshared.c:366`) |
| 4–5 | VRAM-B0 usage | `1` = coefficient table (`vidshared.c:369`) |
| 6–7 | VRAM-B1 usage | `1` = coefficient table (`vidshared.c:372`) |
| 8 | VRAMD | VRAM-A partitioned into A0/A1 (`vdp2.cpp:620`) |
| 9 | VRBMD | VRAM-B partitioned into B0/B1 (`vdp2.cpp:681`) |
| 12–13 | CRMD | Colour RAM mode → `Vdp2Internal.ColorMode` (`vdp2.cpp:1962`) |
| 15 | CRKTE | Coefficient table lives in CRAM rather than VRAM (`vidshared.c:351`) |

Only the value `1` is ever compared against in the per-bank usage fields; the other three
encodings are not distinguished anywhere in the read code. `CheckBanks(regs, 1)`
(`vidsoft.c:1112–1124`) returns 1 when **no** bank is set to `1`, i.e. no bank is designated as
the coefficient-table bank.

Writing RAMCTL re-decodes every CRAM entry if the colour mode changed (`vdp2.cpp:1962–1967`).

> **Contradiction in the source.** `Vdp2GetBank` (`vidshared.c:156–213`) decides whether the
> upper half of VRAM-A is bank A1 by testing `RAMCTL & 0x10`, and the upper half of VRAM-B by
> `RAMCTL & 0x20` — bits 4 and 5. But `VDP2genVRamCyclePattern` (`vdp2.cpp:620`, `681`) and
> `Rbg0CheckRam` (`vidsoft.c:1128`) test bits 8 and 9 for the same question, and bits 4–5 are
> simultaneously used as VRAM-B0's usage field (`vidshared.c:369`). These cannot both be right.
> The bit-8/9 reading is used by three call sites and is consistent with bits 0–7 being four
> 2-bit usage fields; `Vdp2GetBank`'s bit-4/5 reading appears to be the outlier. `Vdp2GetBank`
> also assumes an address space twice as large in its 8 Mbit branch (banks at `0x40000`,
> `0x80000`, `0xC0000`) than the `0x80000` VRAM actually allocated, which suggests that branch
> is untested. Flagging rather than resolving: the code does not make the true layout clear.

`Vdp2GetBank` is only called from `Vdp2ReadRotationTable` (the float variant), which the
software renderer does not use — the software renderer calls `Vdp2ReadRotationTableFP`, which
does not consult bank information at all.

---

## A.3 VRAM access cycle pattern — CYCA0L/U, CYCA1L/U, CYCB0L/U, CYCB1L/U (`0x010`–`0x01E`)

Each bank has two registers giving eight 4-bit timeslot codes. Nibble ordering is
**most-significant nibble = earliest timeslot** (`vdp2.cpp:600–607`):

| Register | Bits 15–12 | Bits 11–8 | Bits 7–4 | Bits 3–0 |
|---|---|---|---|---|
| `CYCxnL` | T0 | T1 | T2 | T3 |
| `CYCxnU` | T4 | T5 | T6 | T7 |

Decoded into `Vdp2External.AC_VRAM[bank][slot]`, `u8[4][8]` (`vdp2.h:422`), with bank indices
`0 = A0`, `1 = A1`, `2 = B0`, `3 = B1`.

### Timeslot codes actually distinguished by the code

| Code | Meaning | Evidence |
|---|---|---|
| `0x0`–`0x3` | Pattern-name data read for NBG0–NBG3 | `vidogl.c:6570` tests `== 0x00` for NBG0 pattern-name access; symmetric tests at `6985` (NBG1), `7376` (NBG2), `7537` (NBG3) |
| `0x4`–`0x7` | Character-pattern (or bitmap) data read for NBG0–NBG3 | `vdp2.cpp:613–614` maps code `n` to `BGON` bit `n-4`; `vidogl.c:6566` tests `== 0x04` for NBG0 character access |
| `0x8`–`0xD` | **Not distinguished anywhere in the code read.** On hardware these encode vertical-cell-scroll and coefficient-table fetches, but neither `vdp2.cpp` nor `vidsoft.c` nor the `AC_VRAM` consumers ever test for them. | — |
| `0xE` | CPU access slot | `vdp2.cpp:610`, `632` |
| `0xF` | No access | `vdp2.cpp:642` |

### The only thing the code computes from the pattern: a CPU stall factor

`VDP2genVRamCyclePattern` (`vdp2.cpp:595–740`) is called once per frame, at HBLANK-OUT of line 1
(`vdp2.cpp:926–928`). It produces two numbers, `Vdp2External.cpu_cycle_a` and `cpu_cycle_b`.
For VRAM-A the algorithm is (VRAM-B is identical with `CYCB*` and bit 9):

```
cpu_cycle_a = 0
for slot in 0..7 of A0:
    if code >= 0x0E:                       cpu_cycle_a++      // 0xE or 0xF: free
    elif 4 <= code <= 7 and (BGON & (1 << (code-4))) == 0:
                                           cpu_cycle_a++      // slot reserved for a disabled layer
if RAMCTL & 0x100:                          // VRAM-A is split into A0/A1
    decode A1 slots
    fcnt = 0
    for slot in 0..7:
        if A0[slot] == 0x0E:
            if A1[slot] != 0x0E:            cpu_cycle_a--
            elif fcnt == 0:                 cpu_cycle_a--
        if A1[slot] == 0x0F:                fcnt++
    if fcnt == 0:  cpu_cycle_a = 0
    if cpu_cycle_a < 0: cpu_cycle_a = 0
else:
    A1 slots := copy of A0 slots
```

The count is then mapped to a memory-access cost:

```
if (cpu_cycle_a == 0)                     Vdp2External.cpu_cycle_a = 200;
else if (Vdp2External.cpu_cycle_a == 1)   Vdp2External.cpu_cycle_a = 24;
else                                      Vdp2External.cpu_cycle_a = 2;
```

> **Bug in the source, reproduced verbatim above.** Both the A and B mapping blocks test
> `Vdp2External.cpu_cycle_a` (the previous frame's *output* for A, already overwritten by the
> time the B block runs) in the `== 1` branch, instead of the freshly computed local
> `cpu_cycle_a` / `cpu_cycle_b` (`vdp2.cpp:724`, `734`). Since the output values are only ever
> 200, 24 or 2, `Vdp2External.cpu_cycle_a == 1` is never true after the first frame, so the
> `24`-cycle case is effectively dead. A reimplementation should decide deliberately whether to
> reproduce this.

`memory.c:735–747` consumes the result:

```
if (LineCount >= VBlankLineCount)           return 2;      // vblank: no contention
if ((addr & 0x000F0000) < 0x00040000)       return cpu_cycle_a;
else                                        return cpu_cycle_b;
```

so the A/B split for CPU stalls is a fixed address bisection at `0x40000`, independent of
`RAMCTL`.

### What is *not* implemented

Neither `vidsoft.c` nor `vdp2.cpp` uses the cycle pattern to decide which VRAM bank a layer may
read from, to gate a layer's rendering, or to sequence fetches. `AC_VRAM` is only consumed by
`vidogl.c` (four call sites, all deciding a per-bank boolean "does this layer read pattern-name
or character data from this bank"), never by the software renderer.

The software renderer's *only* cycle-pattern-derived behaviour is a hard-coded signature match
(`vidsoft.c:3522–3535`):

```c
// dracula x bad cycle setting
if (CYCA0L == 0x5566 && CYCA0U == 0x47ff && CYCA1L == 0xffff && CYCA1U == 0xffff &&
    CYCB0L == 0x12ff && CYCB0U == 0x03ff && CYCB1L == 0xffff && CYCB1U == 0xffff)
   bad_cycle_setting[TITAN_NBG3] = 1;
else
   bad_cycle_setting[TITAN_NBG3] = 0;
```

When set, NBG3 renders each cell using the *previous* cell's pattern data (see §B.5), simulating
a one-cell fetch delay. This is a game-specific workaround, not a general model of what happens
when a layer's access pattern is invalid. **The general behaviour on conflicting or insufficient
timeslot allocation is not modelled by this code and cannot be derived from it.**

---

## A.4 Layer enable and special-function selection

### BGON — `0x020`

| Bit | Field | Effect |
|---|---|---|
| 0 | N0ON | NBG0 enable (`vidsoft.c:1595`) |
| 1 | N1ON | NBG1 enable (`vidsoft.c:1710`) |
| 2 | N2ON | NBG2 enable (`vidsoft.c:1816`) |
| 3 | N3ON | NBG3 enable (`vidsoft.c:1888`) |
| 4 | R0ON | RBG0 enable (`vidsoft.c:1964`) |
| 5 | R1ON | RBG1 enable — **takes over the NBG0 slot** (`vidsoft.c:1564`) |
| 8 | N0TPON | NBG0 transparency-code *disable*: `transparencyenable = !(BGON & 0x100)` (`vidsoft.c:1631`) |
| 9 | N1TPON | NBG1 (`vidsoft.c:1711`) |
| 10 | N2TPON | NBG2 (`vidsoft.c:1817`) |
| 11 | N3TPON | NBG3 (`vidsoft.c:1889`) |
| 12 | R0TPON | RBG0 (`vidsoft.c:1968`) |

The bit assignments are independently confirmed by the on-hardware test-dump snippet embedded as
a comment in `vdp2.cpp:2542–2547` (`N0ON 0x01 … R1ON 0x20`).

When `transparencyenable` is 0, `Vdp2FetchPixel` never returns "transparent" — colour index 0 /
a clear MSB is drawn as an ordinary colour (`vidsoft.c:392`, `400`, `408`, `416`, `424`).

`BGON` bits 0–4 are also used per-line: at HBLANK-OUT the code compares each line's `BGON` layer
bit against line 0's and flags a per-line-alpha change if they differ (`vdp2.cpp:856–889`).

`Vdp2External.disptoggle` (`vdp2.h:415`, default `0xFF`, toggled by `ToggleNBG0`…`ToggleRBG0` at
`vdp2.cpp:2448–2479`) is ANDed with the enable bit as a debug mask (`vidsoft.c:1655`, `1764`,
`1850`, `1923`, `1966`).

### MZCTL — `0x022`

| Bits | Field | Decode |
|---|---|---|
| 0 | N0MZE | Mosaic enable NBG0 |
| 1 | N1MZE | NBG1 |
| 2 | N2MZE | NBG2 |
| 3 | N3MZE | NBG3 |
| 4 | R0MZE | RBG0 |
| 8–11 | MZSZH | Mosaic width − 1 |
| 12–15 | MZSZV | Mosaic height − 1 |

`ReadMosaicData` (`vidshared.h:542–554`): if the layer's enable bit is set,
`mosaicxmask = ((MZCTL >> 8) & 0xF) + 1` and `mosaicymask = (MZCTL >> 12) + 1`; otherwise both
are 1. Note the Y field is read without a mask — correct only because the register is 16-bit.
Cross-checked by `vdp2debug.c:89`.

### SFSEL — `0x024` and SFCODE — `0x026`

`SFCODE` holds two independent 8-bit codes: **code A in bits 0–7**, **code B in bits 8–15**.
`SFSEL` picks which one each layer uses (`vidsoft.c:1643–1646`, `1748`, `1835`, `1908`, `2040`):

| SFSEL bit | Layer | Selection |
|---|---|---|
| 0 | NBG0 | set → `SFCODE >> 8`; clear → `SFCODE & 0xFF` |
| 1 | NBG1 | ditto |
| 2 | NBG2 | ditto |
| 3 | NBG3 | ditto |
| 4 | RBG0 | ditto |

The selected byte becomes `info.specialcode`. Each of its 8 bits enables a *pair* of colour-code
values, matched against the low 4 bits of the raw dot value
(`PixelIsSpecialPriority`, `vidsoft.c:755–801`):

| specialcode bit | Matches dot & 0xF |
|---|---|
| 0 | 0, 1 |
| 1 | 2, 3 |
| 2 | 4, 5 |
| 3 | 6, 7 |
| 4 | 8, 9 |
| 5 | A, B |
| 6 | C, D |
| 7 | E, F |

The same value is used by special colour-calculation mode 2, but with a different (and
inconsistent) test — `GetAlpha` uses `specialcode & (1 << ((dot & 0xF) >> 1))`
(`vidsoft.c:743`), which is the same pair mapping expressed as a shift.

---

## A.5 Character and bitmap control

### CHCTLA — `0x028` (NBG0, NBG1)

| Bits | Field | Decode |
|---|---|---|
| 0 | N0CHSZ | Character size: 0 = 1×1 cell (8×8 px), 1 = 2×2 cells (16×16 px) — passed to `ReadPatternData` (`vidsoft.c:1588`, `1623`) |
| 1 | N0BMEN | Bitmap enable (`vidsoft.c:1572`, `1600`) |
| 2–3 | N0BMSZ | Bitmap size — `ReadBitmapSize(&info, CHCTLA >> 2, 0x3)` (`vidsoft.c:1575`, `1603`) |
| 4–6 | N0CHCN | Colour format, 3 bits (`vidsoft.c:1634`) |
| 8 | N1CHSZ | NBG1 character size (`vidsoft.c:1738`) |
| 9 | N1BMEN | NBG1 bitmap enable (`vidsoft.c:1716`) |
| 10–11 | N1BMSZ | NBG1 bitmap size — `ReadBitmapSize(&info, CHCTLA >> 10, 0x3)` (`vidsoft.c:1718`) |
| 12–13 | N1CHCN | NBG1 colour format, only 2 bits (`vidsoft.c:1714`) |

Bit 7 is not read anywhere.

### CHCTLB — `0x02A` (NBG2, NBG3, RBG0)

| Bits | Field | Decode |
|---|---|---|
| 0 | N2CHSZ | NBG2 character size (`vidsoft.c:1826`) |
| 1 | N2CHCN | NBG2 colour format, 1 bit (`vidsoft.c:1820`) |
| 4 | N3CHSZ | NBG3 character size (`vidsoft.c:1899`) |
| 5 | N3CHCN | NBG3 colour format, 1 bit (`vidsoft.c:1892`) |
| 8 | R0CHSZ | RBG0 character size (`vidsoft.c:2030`) |
| 9 | R0BMEN | RBG0 bitmap enable (`vidsoft.c:2001`) |
| 10 | R0BMSZ | RBG0 bitmap size — `ReadBitmapSize(&info, CHCTLB >> 10, 0x1)`, **1-bit mask** so RBG0 bitmaps are only 512×256 or 512×512 (`vidsoft.c:2004`) |
| 12–14 | R0CHCN | RBG0 colour format, 3 bits (`vidsoft.c:1971`) |

### Colour format (`colornumber`) encoding

Common to `N0CHCN`, `N1CHCN`, `N2CHCN`, `N3CHCN`, `R0CHCN`. Decode per `Vdp2FetchPixel`
(`vidsoft.c:385–433`), names cross-checked against `vdp2debug.c:56–81`:

| Value | Format | Bits/pixel | Transparency test |
|---|---|---|---|
| 0 | Palette, 16 colours | 4 | `(dot & 0xF) == 0` |
| 1 | Palette, 256 colours | 8 | `(dot & 0xFF) == 0` |
| 2 | Palette, 2048 colours | 16 | `dot == 0` |
| 3 | RGB 5:5:5, 32768 colours | 16 | `!(dot & 0x8000)` |
| 4 | RGB direct, 16.7 M colours | 32 | `!(dot & 0x80000000)` |

Layers with narrower `CHCN` fields can only reach the low values: NBG1 and RBG0 by field width
(2 and 3 bits), NBG2/NBG3 to 0–1 only.

**Bandwidth exclusion rules** — higher-colour NBG0/NBG1 modes suppress other layers entirely:

| Suppressed layer | Condition | Source |
|---|---|---|
| NBG1 | NBG0 enabled and `N0CHCN == 4` | `vidsoft.c:1765` |
| NBG2 | NBG0 enabled and `N0CHCN >= 2` | `vidsoft.c:1851` |
| NBG3 | (NBG0 enabled and `N0CHCN == 4`) or (NBG1 enabled and `N1CHCN >= 2`) | `vidsoft.c:1924–1925` |

The layer simply returns without drawing anything.

### Bitmap sizes — `ReadBitmapSize` (`vidshared.h:435–452`)

| Value | `cellw` × `cellh` |
|---|---|
| 0 | 512 × 256 |
| 1 | 512 × 512 |
| 2 | 1024 × 256 |
| 3 | 1024 × 512 |

`cellw` doubles as the row stride in `Vdp2FetchPixel`, so bitmap pixel addressing is
`charaddr + (y * cellw + x) * bytes_per_pixel`.

### BMPNA — `0x02C`, BMPNB — `0x02E`

| Register | Bits | Field | Use |
|---|---|---|---|
| BMPNA | 0–2 | N0BMP | NBG0 bitmap palette number → `paladdr = (BMPNA & 0x7) << 8` (`vidsoft.c:1578`, `1609`) |
| BMPNA | 4 | N0BMPR | NBG0 bitmap special colour calculation → `specialcolorfunction` (`vidsoft.c:1581`, `1612`) |
| BMPNA | 5 | N0BMPR (pri) | Bitmap special priority — only surfaced in the debug dump (`vdp2debug.c:124`) |
| BMPNA | 8–10 | N1BMP | NBG1 bitmap palette → `paladdr = BMPNA & 0x700` (`vidsoft.c:1724`) |
| BMPNA | 12 | N1BMCC | NBG1 bitmap special colour calc (`vidsoft.c:1727`) |
| BMPNB | 0–2 | R0BMP | RBG0/RBG1 bitmap palette → `(BMPNB & 0x7) << 8` (`vidsoft.c:2013`) |
| BMPNB | 4 | R0BMCC | RBG0 bitmap special colour calc (`vidsoft.c:2016`) |
| BMPNB | 5 | R0BMPR | Bitmap special priority — debug dump only |

> **Discrepancy.** `vidsoft.c` scales the bitmap palette number by `<< 8`
> (`1578`, `1609`, `2013`), while `vdp2debug.c:135` prints it as `(palnum & 0x7) << 4`. The two
> disagree by a factor of 16. `<< 8` is self-consistent with `Vdp2FetchPixel`'s use of
> `paladdr | (dot & 0xFF)` for 8 bpp bitmaps (a 256-entry-aligned base), which is why it is
> presented as the primary reading here — but the code does not settle the question, and a
> reimplementation should verify against hardware.

---

## A.6 Pattern name control and plane size

### PNCN0–PNCN3 (`0x030`–`0x036`), PNCR (`0x038`)

Decoded by `ReadPatternData` (`vidshared.h:508–538`) and consumed by `Vdp2PatternAddr`
(`vidsoft.c:239–320`).

| Bits | Field | Meaning |
|---|---|---|
| 0–4 | Supplementary character number | Merged into the character address; exact merge depends on `auxmode` and character size |
| 5–7 | Supplementary palette number | Used only for 16-colour, 1-word pattern data |
| 8 | Special colour calculation bit | Becomes `specialcolorfunction` for 1-word pattern data |
| 9 | Special priority bit | Becomes `specialfunction` for 1-word pattern data |
| 14 | N*CNSM (auxmode) | Character-number supplement mode: 0 = 10-bit char number + flip bits, 1 = 12-bit char number, no flip |
| 15 | N*PNB | Pattern name data size: **1 = one word, 0 = two words** |

`ReadPatternData` also sets, from the register pair:

* `patterndatasize` = 1 or 2 words, `patterndatasize_bits` = 0 or 1
* `patternwh` = 1 or 2 (cells per side) from the CHCTL character-size bit, `patternwh_bits` = 0/1
* `pagewh = 64 >> patternwh_bits` — a page is 64×64 cells of 8×8, or 32×32 patterns of 16×16
* `cellw = cellh = 8` (always, for tile mode)
* `supplementdata = pnc & 0x3FF`
* `auxmode = (pnc & 0x4000) >> 14`

Field meanings cross-checked against `vdp2debug.c:749–753`.

### PLSZ — `0x03A`

| Bits | Field | Consumer |
|---|---|---|
| 0–1 | N0PLSZ | `ReadPlaneSize(&info, PLSZ)` (`vidsoft.c:1619`) |
| 2–3 | N1PLSZ | `ReadPlaneSize(&info, PLSZ >> 2)` (`vidsoft.c:1733`) |
| 4–5 | N2PLSZ | `PLSZ >> 4` (`vidsoft.c:1823`) |
| 6–7 | N3PLSZ | `PLSZ >> 6` (`vidsoft.c:1896`) |
| 8–9 | RAPLSZ | Rotation parameter A plane size, `PLSZ >> 8` (`vidsoft.c:2025`) |
| 10–11 | RAOVR | Parameter A screen-over mode (`vidshared.c:468`) |
| 12–13 | RBPLSZ | Parameter B plane size, `PLSZ >> 12` (`vidsoft.c:1587`, `2028`) |
| 14–15 | RBOVR | Parameter B screen-over mode (`vidshared.c:475`) |

`ReadPlaneSize` (`vidshared.h:483–504`):

| Value | `planew` × `planeh` |
|---|---|
| 0 | 1 × 1 |
| 1 | 2 × 1 |
| 2 | 1 × 1 — *"Not sure what 0x2 does, though a few games seem to use it"* |
| 3 | 2 × 2 |

The comment at `vidshared.h:499` is the code's own admission that encoding 2 is guesswork.

Screen-over mode (used only by rotation layers, `vidsoft.c:1356–1371`, `1386–1401`):

| Value | Behaviour in `Vdp2DrawRotationFP` |
|---|---|
| 0 | Repeat: `x &= xmask; y &= ymask` |
| 1 | Logged as `"Screen-over mode 1 not implemented"`, then falls through to the same masking as 0. On hardware this is the "display a single pattern named by OVPNRA/OVPNRB" mode; **OVPNRA/OVPNRB are never read by any renderer** |
| 2 | Transparent outside: `if (x > xmask \|\| y > ymask) continue` |
| 3 | Clamp to 512×512: `if (x > 512 \|\| y > 512) continue` |

Note modes 2 and 3 use `>` rather than `>=`, so the boundary pixel is included; and neither
tests for negative coordinates, which cannot occur because `GenerateRotatedXPosFP` returns a
`u16` (see §B.8).

---

## A.7 Map / plane address registers

### MPOFN — `0x03C`, MPOFR — `0x03E`

Each 3-bit field supplies the upper bits of the plane address, always scaled to `N << 6`:

| Register | Bits | Layer | Expression |
|---|---|---|---|
| MPOFN | 0–2 | NBG0 | `(MPOFN & 0x7) << 6` (`vidshared.c:55`) |
| MPOFN | 4–6 | NBG1 | `(MPOFN & 0x70) << 2` (`vidshared.c:81`) |
| MPOFN | 8–10 | NBG2 | `(MPOFN & 0x700) >> 2` (`vidshared.c:107`) |
| MPOFN | 12–14 | NBG3 | `(MPOFN & 0x7000) >> 6` (`vidshared.c:133`) |
| MPOFR | 0–2 | Rot. param A | `(MPOFR & 0x7) << 6` (`vidshared.c:620`) |
| MPOFR | 4–6 | Rot. param B | `(MPOFR & 0x70) << 2` (`vidshared.c:682`) |

The same fields double as the **bitmap base address** when the layer is in bitmap mode, with a
completely different scale:

| Layer | Bitmap base |
|---|---|
| NBG0 | `(MPOFN & 0x7) * 0x20000` (`vidsoft.c:1608`) |
| NBG1 | `((MPOFN & 0x70) >> 4) * 0x20000` (`vidsoft.c:1723`) |
| RBG0, param A | `(MPOFR & 0x7) * 0x20000` (`vidsoft.c:2008`) |
| RBG0, param B | `(MPOFR & 0x70) * 0x2000` (`vidsoft.c:2011`) |
| RBG1 | `(MPOFR & 0x70) * 0x2000` (`vidsoft.c:1577`) |

(`(x & 0x70) * 0x2000` and `((x & 0x70) >> 4) * 0x20000` are the same value.)

### Per-plane map registers

NBG layers have 4 planes (A–D) in 2 registers; rotation parameters have 16 planes (A–P) in 8
registers. Each register holds two 8-bit plane numbers, **low byte first**:

| Register | Plane index 2n | Plane index 2n+1 |
|---|---|---|
| MPABN0 `0x040` | A (`& 0xFF`) | B (`>> 8`) |
| MPCDN0 `0x042` | C | D |
| MPABN1 `0x044` | A | B |
| MPCDN1 `0x046` | C | D |
| MPABN2 `0x048` | A | B |
| MPCDN2 `0x04A` | C | D |
| MPABN3 `0x04C` | A | B |
| MPCDN3 `0x04E` | C | D |
| MPABRA `0x050` | A | B |
| MPCDRA `0x052` | C | D |
| MPEFRA `0x054` | E | F |
| MPGHRA `0x056` | G | H |
| MPIJRA `0x058` | I | J |
| MPKLRA `0x05A` | K | L |
| MPMNRA `0x05C` | M | N |
| MPOPRA `0x05E` | O | P |
| MPABRB `0x060` … MPOPRB `0x06E` | A, C, E, G, I, K, M, O | B, D, F, H, J, L, N, P |

Source: `vidshared.c:58–72` (NBG0), `623–673` (param A), `686–736` (param B).

### `CalcPlaneAddr` — turning a plane number into a VRAM byte address

`vidshared.h:392–431`. Input `tmp = map_offset | plane_byte` (note bits 6–7 of the offset and
the plane byte **overlap and are ORed**, they are not concatenated).

```
deca  = planeh + planew - 2;      // 0 for 1x1, 1 for 2x1, 2 for 2x2
multi = planeh * planew;          // 1, 2 or 4
```

| VRSIZE bit 15 | pattern data size | pattern W/H | Address |
|---|---|---|---|
| 1 (8 Mbit) | 1 word | 1×1 cell | `((tmp & 0x3F) >> deca) * (multi * 0x2000)` |
| 1 | 1 word | 2×2 cells | `(tmp >> deca) * (multi * 0x800)` |
| 1 | 2 words | 1×1 | `((tmp & 0x1F) >> deca) * (multi * 0x4000)` |
| 1 | 2 words | 2×2 | `((tmp & 0x7F) >> deca) * (multi * 0x1000)` |
| 0 (4 Mbit) | 1 word | 1×1 | `((tmp & 0x3F) >> deca) * (multi * 0x2000)` |
| 0 | 1 word | 2×2 | `((tmp & 0xFF) >> deca) * (multi * 0x800)` |
| 0 | 2 words | 1×1 | `((tmp & 0x1F) >> deca) * (multi * 0x4000)` |
| 0 | 2 words | 2×2 | `((tmp & 0x7F) >> deca) * (multi * 0x1000)` |

The only difference between the 4 Mbit and 8 Mbit branches is the mask in the
1-word / 2×2-cell row (`tmp` vs `tmp & 0xFF`) — and since `tmp` is at most 9 bits wide, that
distinction only matters when the map offset reaches bit 8. The identical algorithm is
independently duplicated in `vdp2debug.c:259–272`, which is a useful confirmation that the
constants are intentional.

The per-plane addresses are precomputed into `sinfo->planetbl[16]` by `GeneratePlaneAddrTable`
(`vidsoft.c:558–567`), which iterates `mapwh * mapwh` entries — 4 for NBG layers
(`mapwh = 2`) and 16 for rotation layers (`mapwh = 4`).

---

## A.8 Scroll, zoom and line-scroll registers

### NBG0 / NBG1 scroll and zoom

| Offset | Register | Read as |
|---|---|---|
| `0x070` / `0x080` | SCXIN0 / SCXIN1 | `info.x = SCXIN & 0x7FF` (`vidsoft.c:1605`, `1621`, `1720`, `1735`) |
| `0x072` / `0x082` | SCXDN0 / SCXDN1 | **never read by the software renderer** |
| `0x074` / `0x084` | SCYIN0 / SCYIN1 | `info.y = SCYIN & 0x7FF` |
| `0x076` / `0x086` | SCYDN0 / SCYDN1 | **never read** |
| `0x078`+`0x07A` | ZMXN0 (I,D) | `coordincx = (ZMXN0.all & 0x7FF00) / 65536.0` (`vidsoft.c:1626`) |
| `0x07C`+`0x07E` | ZMYN0 (I,D) | `coordincy = (ZMYN0.all & 0x7FF00) / 65536.0` (`vidsoft.c:1627`) |
| `0x088`+`0x08A` | ZMXN1 (I,D) | `coordincx = (ZMXN1.all & 0x7FF00) / 65536.0` (`vidsoft.c:1758`) |
| `0x08C`+`0x08E` | ZMYN1 (I,D) | `coordincy` (`vidsoft.c:1759`) |

The `ZM*` registers are declared as a union of two 16-bit halves over a 32-bit `all`, with the
`I` half at the **lower** offset and therefore the **upper** 16 bits of `all` on little-endian
(`vdp2.h:162–176`). The mask `0x7FF00` therefore selects:

* bits 16–18 of `all` = `ZMXIN` bits 0–2 → **3 integer bits**
* bits 8–15 of `all` = `ZMXDN` bits 8–15 → **8 fractional bits**

`coordincx` is a *coordinate increment*, i.e. a value > 1.0 reduces the layer. It is applied as
`x = info->x + mosaic_x[i] * coordincx` (`vidsoft.c:1020`). `vdp2debug.c:869` prints its
reciprocal and labels it "Coordinate Increments", confirming the direction.

**NBG2 and NBG3 have no zoom at all**: `info.coordincx = info.coordincy = 1` unconditionally
(`vidsoft.c:1845`, `1918`). RBG0 likewise (`vidsoft.c:2051`) — rotation layers get their scale
from `kx`/`ky` instead.

### SCXN2/SCYN2 (`0x090`/`0x092`), SCXN3/SCYN3 (`0x094`/`0x096`)

Single registers, no fractional counterpart. `info.x = SCXN2 & 0x7FF` etc.
(`vidsoft.c:1824–1825`, `1897–1898`).

### ZMCTL — `0x098`

**Never read by `vidsoft.c` or `vidshared.*`.** Only `vdp2debug.c:872`, `1089` decodes it, as a
2-bit reduction field per layer (bits 0–1 NBG0, bits 8–9 NBG1) with `1` = ½ and `2`/`3` = ¼
horizontal reduction. The software renderer's zoom comes entirely from `ZMXN`/`ZMYN`.

### SCRCTL — `0x09A`

Two identical 8-bit halves: NBG0 in bits 0–7, NBG1 in bits 8–15.

| Bit (within half) | Field | Effect |
|---|---|---|
| 0 | N*VCSC | Vertical cell scroll enable (`vidsoft.c:1660`, `1770`) |
| 1 | N*LSCX | Line scroll, horizontal |
| 2 | N*LSCY | Line scroll, vertical |
| 3 | N*LZMX | Line zoom (X only) |
| 4–5 | N*LSS | Line scroll interval |

`ReadLineScrollData(info, mask, tbl)` (`vidshared.h:558–571`):

```
if (mask & 0xE) {                       // any of bits 1,2,3 set
   islinescroll   = (mask >> 1) & 0x7;  // bit0=H, bit1=V, bit2=zoom
   linescrolltbl  = (tbl & 0x7FFFE) << 1;
   lineinc        = 1 << ((mask >> 4) & 0x03);
}
```

so the interval is 1, 2, 4 or 8 lines. Called with `(SCRCTL & 0xFF, LSTA0.all)` for NBG0
(`vidsoft.c:1659`) and `(SCRCTL >> 8, LSTA1.all)` for NBG1 (`vidsoft.c:1769`). Interval decode
cross-checked at `vdp2debug.c:904–918`.

Vertical cell scroll table setup differs between the two layers because they share one table
(`vidsoft.c:1660–1670`, `1770–1783`):

| Layer | Condition | `verticalscrolltbl` | `verticalscrollinc` |
|---|---|---|---|
| NBG0 | `SCRCTL & 1` | `(VCSTA.all & 0x7FFFE) << 1` | 8 if NBG1 also enabled (`SCRCTL & 0x100`), else 4 |
| NBG1 | `SCRCTL & 0x100`, NBG0 also on | `4 + ((VCSTA.all & 0x7FFFE) << 1)` | 8 |
| NBG1 | `SCRCTL & 0x100`, NBG0 off | `(VCSTA.all & 0x7FFFE) << 1` | 4 |

`verticalscrollinc` is stored but **never used** — the actual read goes through the per-line
`cell_scroll_data` snapshot instead (see §B.7).

### VCSTA (`0x09C`/`0x09E`), LSTA0 (`0x0A0`/`0x0A2`), LSTA1 (`0x0A4`/`0x0A6`)

32-bit address pairs, upper word at the lower offset. All are used as
`(reg.all & 0x7FFFE) << 1`, i.e. a word-granular VRAM address expressed in half-words with the
LSB forced to 0 (`vdp2.cpp:836`, `vidsoft.c:1663`, `vidshared.h:563`).

### LCTA — `0x0A8`/`0x0AA`

| Field | Location | Use |
|---|---|---|
| Per-line flag | `LCTA.part.U & 0x8000` (bit 31 of the pair) | 1 = one colour per line, 0 = single colour (`vidsoft.c:1515`) |
| Address | `LCTA.all & 0x7FFFF` (8 Mbit) or `& 0x3FFFF` (4 Mbit), then `<< 1` | `vidsoft.c:1508–1511` |

In the rotation path the same register is re-read with a different increment:
`lineInc = (LCTA.part.U & 0x8000) ? 2 : 0` (`vidsoft.c:1289`) — i.e. the address advances by one
word per line in per-line mode and stays put otherwise.

---

## A.9 Back screen — BKTAU `0x0AC`, BKTAL `0x0AE`

| Field | Location | Use |
|---|---|---|
| Per-line flag | `BKTAU & 0x8000` | 1 = one colour per line (`vidsoft.c:1472`) |
| Address | `((BKTAU & 0x7) << 16 \| BKTAL) * 2` (8 Mbit) or `((BKTAU & 0x3) << 16 \| BKTAL) * 2` (4 Mbit) | `vidsoft.c:1467–1470` |

The back screen value is a raw RGB 5:5:5 word read from VRAM and expanded with `COLSAT2YAB16`
at full alpha `0x3F`, then passed through the colour-offset stage with `CLOFEN` bit 5
(`vidsoft.c:1465`, `1480`, `1489`).

When `DISP == 0` **and** `BDCLMD == 0`, the back screen is forced to black
(`vidsoft.c:1452–1457`) — note this means the back screen is still drawn from VRAM when
`DISP == 0` but `BDCLMD == 1`.

---

## A.10 Rotation registers and the rotation parameter table

### RPMD — `0x0B0`

Bits 0–1 (`vidsoft.c:1974–1997`):

| Value | Mode | Effect |
|---|---|---|
| 0 | Parameter A only | `rotatenum = 0`, `PlaneAddr = Vdp2ParameterAPlaneAddr` |
| 1 | Parameter B only | `rotatenum = 1`, `PlaneAddr = Vdp2ParameterBPlaneAddr` |
| 2 | A/B switched per pixel by the coefficient MSB | `rotatenum = 0`, `rotatemode = 1` |
| 3 | A/B switched by the rotation parameter window | `rotatenum = 0`, `rotatemode = 2` |

In modes 2 and 3, `Vdp2DrawRotationFP` loads a second parameter set
`p2 = &parameter[1 - rotatenum]` (`vidsoft.c:1234–1243`). `rotatemode` is stored but the actual
dispatch inside the drawing loop keys off `regs->RPMD & 3` directly.

`Vdp2ReadRotationTable` (the float variant, used only by the debug dump) additionally forces
`deltaKAx = 0` for parameter B when `RPMD == 0x02` (`vidshared.c:420`), citing the hardware
documentation page in a comment. The FP variant used by the renderer does **not** do this.

### RPRCTL — `0x0B2`

Not read by any renderer. `vdp2debug.c:824–849` decodes three bits, all in the context of RBG1:

| Bit | Meaning per the debug string |
|---|---|
| 8 | Read Xst parameter |
| 9 | Read Yst parameter |
| 10 | Read KAst parameter |

By symmetry there is presumably a parameter-B set in the upper bits, but the code does not show
it.

### KTCTL — `0x0B4`

| Bits | Field | Use |
|---|---|---|
| 0 | RA coefficient table enable | `parameter->coefenab = KTCTL & 0x1` (`vidshared.c:467`) |
| 1 | RA coefficient data size | `coefdatasize = (KTCTL & 0x2) ? 2 : 4` bytes (`vidshared.c:587`) |
| 2–3 | RA coefficient mode | `coefmode = (KTCTL >> 2) & 0x3` (`vidshared.c:589`) |
| 4 | RA line colour screen enable | `if (rotatenum == 0 && (KTCTL & 0x10)) linescreen = 2` (`vidsoft.c:1280`); also `use_coef_for_linecolor` (`vidshared.c:401`) |
| 8 | RB coefficient table enable | `coefenab = KTCTL & 0x100` (`vidshared.c:474`) |
| 9 | RB coefficient data size | `coefdatasize = (KTCTL & 0x200) ? 2 : 4` (`vidshared.c:593`) |
| 10–11 | RB coefficient mode | `(KTCTL >> 10) & 0x3` (`vidshared.c:595`) |
| 12 | RB line colour screen enable | `else if (KTCTL & 0x1000) linescreen = 3` (`vidsoft.c:1282`) |

Coefficient mode (`Vdp2ReadCoefficientFP`, `vidshared.h:784–818`):

| Mode | Effect |
|---|---|
| 0 | Coefficient sets **both** `kx` and `ky` |
| 1 | Coefficient sets `kx` only |
| 2 | Coefficient sets `ky` only |
| 3 | Coefficient sets `Xp` (screen X translation), with a different fixed-point scale |

### KTAOF — `0x0B6`

Bits 0–2 for parameter A, bits 8–10 for parameter B. Used as a 64 K-entry page selector on the
coefficient table (`vidshared.c:588`, `594`):

```
coeftbladdr = (KTAOF_field * 0x10000 + touint(KAst)) * coefdatasize;
```

Note `touint(KAst)` is `(u16)(KAst >> 16)` — the integer part of the fixed-point `KAst`,
**truncated to 16 bits**.

### OVPNRA — `0x0B8`, OVPNRB — `0x0BA`

Never read. These hold the pattern name used by screen-over mode 1, which the renderer logs as
unimplemented (`vidsoft.c:1362`, `1392`).

### RPTA — `0x0BC`/`0x0BE`

Base address of the rotation parameter table. `addr = RPTA.all << 1`, then
(`vidshared.c:461–476`):

| Parameter | Address |
|---|---|
| A | `addr & 0x000FFF7C` |
| B | `(addr & 0x000FFFFC) \| 0x00000080` |

Parameter A's mask clears bit 7, and parameter B forces it set — so the two tables are 128 bytes
apart and the register selects a 128-byte-aligned pair.

### Rotation parameter table layout in VRAM

Read by `Vdp2ReadRotationTableFP` (`vidshared.c:456–614`). Offsets are from the parameter's base
address. Every field is masked, sign-extended from the indicated bit, and interpreted as
signed 16.16 fixed point unless noted.

| Offset | Size | Field | Value mask | Sign bit | Notes |
|---|---|---|---|---|---|
| `+0x00` | 4 | Xst | `0x1FFFFFC0` | `0x10000000` | Screen start X |
| `+0x04` | 4 | Yst | `0x1FFFFFC0` | `0x10000000` | |
| `+0x08` | 4 | Zst | `0x1FFFFFC0` | `0x10000000` | |
| `+0x0C` | 4 | ΔXst | `0x0007FFC0` | `0x00040000` | Per-line X start increment |
| `+0x10` | 4 | ΔYst | `0x0007FFC0` | `0x00040000` | |
| `+0x14` | 4 | ΔX | `0x0007FFC0` | `0x00040000` | Per-pixel X increment |
| `+0x18` | 4 | ΔY | `0x0007FFC0` | `0x00040000` | |
| `+0x1C` | 4 | A | `0x000FFFC0` | `0x00080000` | Matrix |
| `+0x20` | 4 | B | `0x000FFFC0` | `0x00080000` | |
| `+0x24` | 4 | C | `0x000FFFC0` | `0x00080000` | |
| `+0x28` | 4 | D | `0x000FFFC0` | `0x00080000` | |
| `+0x2C` | 4 | E | `0x000FFFC0` | `0x00080000` | |
| `+0x30` | 4 | F | `0x000FFFC0` | `0x00080000` | |
| `+0x34` | 2 | Px | `0x3FFF` | `0x2000` | Integer, converted with `tofixed()` |
| `+0x36` | 2 | Py | `0x3FFF` | `0x2000` | |
| `+0x38` | 2 | Pz | `0x3FFF` | `0x2000` | `+0x3A` skipped (reader advances by 4) |
| `+0x3C` | 2 | Cx | `0x3FFF` | `0x2000` | |
| `+0x3E` | 2 | Cy | `0x3FFF` | `0x2000` | |
| `+0x40` | 2 | Cz | `0x3FFF` | `0x2000` | `+0x42` skipped |
| `+0x44` | 4 | Mx | `0x3FFFFFC0` | `0x20000000` | |
| `+0x48` | 4 | My | `0x3FFFFFC0` | `0x20000000` | |
| `+0x4C` | 4 | kx | `0x00FFFFFF` | `0x00800000` | Scale factor X |
| `+0x50` | 4 | ky | `0x00FFFFFF` | `0x00800000` | |
| `+0x54` | 4 | KAst | `0xFFFFFFC0` | — (unsigned) | Only read when `coefenab` |
| `+0x58` | 4 | ΔKAst | `0x03FFFFC0` | `0x02000000` | Per-line coefficient address increment |
| `+0x5C` | 4 | ΔKAx | `0x03FFFFC0` | `0x02000000` | Per-pixel coefficient address increment |

Total 0x60 bytes; parameter B sits 0x80 bytes after parameter A.

The masks all clear the low 6 bits, so every fixed-point field has a 10-bit fraction at most
even though it is stored as 16.16.

> **Sign-extension inconsistency.** The float reader `Vdp2ReadRotationTable` sign-extends
> Px/Py/Pz/Cx/Cy/Cz with `0xFFFFC000` (`vidshared.c:294`, `298`, `302`, `306`, `310`, `314`),
> which is correct for a 14-bit field. The fixed-point reader used by the software renderer uses
> `0xFFF80000` for the same fields (`vidshared.c:531`, `535`, `539`, `543`, `547`, `551`), which
> leaves bits 14–18 clear and therefore produces a wrong (large positive) value for negative
> inputs. This looks like a copy-paste error from the A–F fields, but the code does not say so.

### Coefficient table entry format

`Vdp2ReadCoefficientMode0_2FP` (`vidshared.c:765–784`), for modes 0–2:

| `coefdatasize` | Address mask | Layout |
|---|---|---|
| 2 bytes | `addr & 0x7FFFE` | bit 15 = `msb`; value = sign-extended `i & 0x7FFF` (sign bit `0x4000`) × 64, i.e. a 10-bit fraction |
| 4 bytes | `addr & 0x7FFFC` | bit 31 = `msb`; bits 24–30 = `linescreen` (7 bits); value = sign-extended `i & 0x00FFFFFF` (sign bit `0x00800000`) used directly as 16.16 |

For mode 3 (`Xp`), `Vdp2ReadCoefficientFP` (`vidshared.h:797–816`):

| `coefdatasize` | Layout |
|---|---|
| 2 bytes | bit 15 = `msb`; `Xp` = sign-extended `i & 0x7FFF` (sign `0x4000`) × 16384 |
| 4 bytes | bit 31 = `msb`; bits 24–30 = `linescreen`; `Xp` = sign-extended `i & 0x007FFFFF` (sign `0x00800000`) × 256 |

`msb` is the per-pixel transparency / parameter-switch flag: when set, the pixel is skipped
(single-parameter mode) or the other parameter is used (RPMD 2). See §B.8.

---

## A.11 Window registers

### Coordinates — WPSX0 `0x0C0`, WPSY0 `0x0C2`, WPEX0 `0x0C4`, WPEY0 `0x0C6`, and WPSX1…WPEY1 `0x0C8`–`0x0CE`

`ReadWindowCoordinates` (`vidshared.h:576–614`):

```
xstart = WPSXn;              ystart = WPSYn & 0x1FF;
xend   = WPEXn;              yend   = WPEYn & 0x1FF;
```

The X values are then rescaled according to `(TVMD >> 1) & 0x3`:

| `(TVMD>>1)&3` | Mode | X transform |
|---|---|---|
| 0 | Normal | `(x >> 1) & 0x1FF` |
| 1 | Hi-Res | `x & 0x3FF` |
| 2 | Exclusive Normal | `x & 0x1FF` |
| 3 | Exclusive Hi-Res | `(x & 0x3FF) >> 1` |

Y is masked to 9 bits and never rescaled.

### Line window tables — LWTA0 `0x0D8`/`0x0DA`, LWTA1 `0x0DC`/`0x0DE`

`ReadLineWindowData` (`vidshared.h:641–655`):

```
if ((wctl & 0x2) && (LWTA0.all & 0x80000000)) { islinewindow |= 0x1; addr0 = (LWTA0.all & 0x7FFFE) << 1; }
if ((wctl & 0x8) && (LWTA1.all & 0x80000000)) { islinewindow |= 0x2; addr1 = (LWTA1.all & 0x7FFFE) << 1; }
```

So bit 31 of the 32-bit pair is the line-window enable, **and it only takes effect if the
corresponding window is enabled in that layer's WCTL byte.**

Each line window entry is two words — start X then end X — consumed sequentially per line
(`ReadOneLineWindowClip`, `vidshared.h:659–698`). Special case: if the end value reads exactly
`0xFFFF`, both start and end are forced to 0, disabling the window for that line. The comment at
`vidshared.h:666–668` names 3D Baseball and Panzer Dragoon Saga as the games requiring this.
After that check, both values are masked to `0x3FF` and then rescaled by the same
`(TVMD >> 1) & 3` table as the static coordinates.

Line windows only supply **X** bounds; `ystart`/`yend` keep whatever the static path left in
them (which is `0` when the static path never ran).

### WCTLA `0x0D0`, WCTLB `0x0D2`, WCTLC `0x0D4`, WCTLD `0x0D6`

Eight-bit control byte per consumer:

| Register | Low byte | High byte |
|---|---|---|
| WCTLA | NBG0 (`vidsoft.c:1671`) | NBG1 (`vidsoft.c:1786`) |
| WCTLB | NBG2 (`vidsoft.c:1857`) | NBG3 (`vidsoft.c:1931`) |
| WCTLC | RBG0 (`vidsoft.c:2056`) | Sprite (`vidsoft.c:3597`) |
| WCTLD | Rotation parameter window (`vidsoft.c:1238`, `1349`) | Colour calculation window (`vidsoft.c:861`, `1076`, `3605`) |

Byte layout (`vidsoft.c:437–554`, cross-checked `vdp2debug.c:154–228`):

| Bit | Field | Meaning |
|---|---|---|
| 0 | W0 area | 1 = draw **inside** window 0, 0 = draw **outside** |
| 1 | W0 enable | |
| 2 | W1 area | 1 = inside, 0 = outside |
| 3 | W1 enable | |
| 4 | Sprite window area | 1 = inside, 0 = outside |
| 5 | Sprite window enable | |
| 6 | — | not read |
| 7 | Logic | 1 = AND, 0 = OR |

When no window is enabled (`wctl & 0x2A == 0`), bit 7 alone decides: AND → nothing is drawn,
OR → everything is drawn (`vidsoft.c:515–521`). `vdp2debug.c:218–227` describes this as "window
enabled/disabled whole screen".

---

## A.12 Sprite and shadow control

### SPCTL — `0x0E0`

| Bits | Field | Use |
|---|---|---|
| 0–3 | SPTYPE | Sprite data type 0–F, decoded by `Vdp1GetSpritePixelInfo` (`vidsoft.c:3593`) |
| 4 | SPWINEN | Sprite window enable (`vidsoft.c:3556`, `3643`, `3927`; `titan.c:350`) |
| 5 | SPCLMD | Framebuffer may contain RGB data as well as palette indices (`vidsoft.c:3547`, `3689`) |
| 8–10 | SPCCN | Colour-calculation condition number (`vidsoft.c:3569`) |
| 12–13 | SPCCCS | Colour-calculation condition select (`vidsoft.c:3568`) |

`SPCCCS` semantics (`vidsoft.c:3727–3744`, cross-checked `vdp2debug.c:1616–1631`):

| Value | Condition for the sprite pixel to be colour-calculated |
|---|---|
| 0 | `priority <= SPCCN` |
| 1 | `priority == SPCCN` |
| 2 | `priority >= SPCCN` |
| 3 | Colour data MSB set (`dot & 0x80000000`, i.e. the preserved CRAM bit 15) |

Sprite type decode (16 types) is in `Vdp1GetSpritePixelInfo` (`vidshared.h:829–981`). Each type
partitions the framebuffer word into shadow bit / priority bits / colour-calc bits / colour
data, and defines a "normal shadow" colour value equal to the all-ones colour value minus one:

| Type | Shadow | Priority bits | Colour-calc bits | Colour data | Normal-shadow value |
|---|---|---|---|---|---|
| 0 | — | 2 (`>>14`) | 3 (`>>11`) | 11 | `0x7FE` |
| 1 | — | 3 (`>>13`) | 2 (`>>11`) | 11 | `0x7FE` |
| 2 | bit 15 | 1 (`>>14`) | 3 (`>>11`) | 11 | `0x7FE` |
| 3 | bit 15 | 2 (`>>13`) | 2 (`>>11`) | 11 | `0x7FE` |
| 4 | bit 15 | 2 (`>>13`) | 3 (`>>10`) | 10 | `0x3FE` |
| 5 | bit 15 | 3 (`>>12`) | 1 (`>>11`) | 11 | `0x7FE` |
| 6 | bit 15 | 3 (`>>12`) | 2 (`>>10`) | 10 | `0x3FE` |
| 7 | bit 15 | 3 (`>>12`) | 3 (`>>9`) | 9 | `0x1FE` |
| 8 | — | 1 (`>>7`) | — | 7 | `0x7E` |
| 9 | — | 1 (`>>7`) | 1 (`>>6`) | 6 | `0x3E` |
| A | — | 2 (`>>6`) | — | 6 | `0x3E` |
| B | — | — | 2 (`>>6`) | 6 | `0x3E` |
| C | — | 1 (`>>7`) | — | 8 (bit 7 shared) | `0xFE` |
| D | — | 1 (`>>7`) | 1 (`>>6`) | 8 (bits 6–7 shared) | `0xFE` |
| E | — | 2 (`>>6`) | — | 8 (bits 6–7 shared) | `0xFE` |
| F | — | — | 2 (`>>6`) | 8 (bits 6–7 shared) | `0xFE` |

Types 0 and 1 have no shadow bit; types 8–F are the 8-bit framebuffer formats.

Additional type-dependent rule in the 16-bit read-out path (`vidsoft.c:3698–3703`): a
framebuffer word of exactly `0x8000` is only drawn if `SPTYPE < 2`, or `SPTYPE < 8` with the
sprite window disabled.

### SDCTL — `0x0E2`

| Bits | Field | Use |
|---|---|---|
| 0 | N0SDEN | `info.titan_shadow_enabled = (SDCTL >> 0) & 1` for NBG0 (`vidsoft.c:1559`) |
| 1 | N1SDEN | NBG1 (`vidsoft.c:1708`) |
| 2 | N2SDEN | NBG2 (`vidsoft.c:1814`) |
| 3 | N3SDEN | NBG3 (`vidsoft.c:1886`) |
| 4 | R0SDEN | RBG0 (`vidsoft.c:1959`) |
| 8 | TPSDSL | Transparent shadow enable — debug dump only (`vdp2debug.c:1587`) |

`titan.c:648` tests `SDCTL & 0x13F` (bits 0–5 and 8) when deciding whether the simplified
compositor may be used, implying bit 5 (a back-screen shadow enable) is also meaningful; nothing
reads bit 5 for rendering.

`titan_shadow_enabled` is per-layer, stored on every written pixel, and consulted on the pixel
*beneath* a shadow pixel (`titan.c:332`, `355`) — i.e. it means "this layer accepts being
shadowed", not "this layer casts a shadow".

---

## A.13 Colour RAM offsets, line screen enable, special priority mode

### CRAOFA — `0x0E4`, CRAOFB — `0x0E6`

Each 3-bit field is scaled to a multiple of 256 CRAM colour indices:

| Register | Bits | Layer | Expression | Source |
|---|---|---|---|---|
| CRAOFA | 0–2 | NBG0 | `(CRAOFA & 0x7) << 8` | `vidsoft.c:1651` |
| CRAOFA | 4–6 | NBG1 | `(CRAOFA & 0x70) << 4` | `vidsoft.c:1756` |
| CRAOFA | 8–10 | NBG2 | `CRAOFA & 0x700` | `vidsoft.c:1843` |
| CRAOFA | 12–14 | NBG3 | `(CRAOFA & 0x7000) >> 4` | `vidsoft.c:1916` |
| CRAOFB | 0–2 | RBG0 | `(CRAOFB & 0x7) << 8` | `vidsoft.c:2048` |
| CRAOFB | 4–6 | Sprite | `(CRAOFB & 0x70) << 4` | `vidsoft.c:3592` |

All five expressions reduce to `field × 0x100`. The result is added to the palette index *before*
the CRAM address scaling in `Vdp2ColorRamGetColorSoft`, so it is an offset in colour entries, not
bytes (`vidsoft.c:395`, `403`, `411`).

### LNCLEN — `0x0E8`

| Bit | Layer | Source |
|---|---|---|
| 0 | NBG0 | `vidsoft.c:1648` |
| 1 | NBG1 | `vidsoft.c:1753` |
| 2 | NBG2 | `vidsoft.c:1840` |
| 3 | NBG3 | `vidsoft.c:1913` |
| 4 | RBG0 | `vidsoft.c:2045` |
| 5 | Sprite | `(LNCLEN >> 5) & 1` (`vidsoft.c:3612`) |

Setting a bit makes `info.linescreen = 1` for that layer, which selects `linescreen[1]` — the
buffer filled by `Vdp2DrawLineScreen`. Values 2 and 3 are only reachable via the rotation
coefficient path (`vidsoft.c:1280–1283`) and select two additional per-line colour buffers.

`Vdp2DrawLineScreen` returns immediately if `LNCLEN == 0` (`vidsoft.c:1505`), and `TitanRender`
disables simplified compositing when `LNCLEN & 0x1F` is nonzero (`titan.c:644`).

### SFPRMD — `0x0EA`

Two bits per layer: NBG0 bits 0–1, NBG1 2–3, NBG2 4–5, NBG3 6–7, RBG0 8–9
(`vidsoft.c:1546`, `1696`, `1802`, `1874`, `1948`).

| Value | Mode | Effect |
|---|---|---|
| 0 | Off | Priority is the register value verbatim |
| 1 | Per tile | `priority = (priority & 0xE) \| (specialfunction & 1)` — the pattern-name special-priority bit replaces bit 0 (`vidsoft.c:317–319`) |
| 2 | Per pixel | `priority = priority & 0xE`, then bit 0 is set if `specialfunction & 1` **and** `PixelIsSpecialPriority(specialcode, dot)` (`vidsoft.c:1055–1066`) |
| 3 | Undocumented | `vdp2debug.c:372` labels it "(undocumented)"; the renderer treats it as neither 1 nor 2, i.e. as mode 0 |

Mode 1 is applied inside `Vdp2PatternAddr`, so it affects tile layers only. Mode 2 is applied in
`Vdp2DrawScroll` only — **`Vdp2DrawRotationFP` never applies special priority**, it passes
`info->priority` straight to `Rbg0PutPixel`.

`SFPRMD & 0x3FF` nonzero also (a) disables simplified compositing (`titan.c:640`) and (b) forces
a layer to be drawn even when its priority register reads 0 (`vidsoft.c:3958–3965`,
`3897`).

---

## A.14 Priority and colour calculation

### PRINA `0x0F8`, PRINB `0x0FA`, PRIR `0x0FC`

| Register | Bits 0–2 | Bits 8–10 |
|---|---|---|
| PRINA | NBG0 priority | NBG1 priority |
| PRINB | NBG2 priority | NBG3 priority |
| PRIR | RBG0 priority | — |

Source: `vidsoft.c:1653`, `1761`, `1847`, `1920`, `1965`; duplicated in `titan.c:651–655` and
`vidsoft.c:3950–3954`.

**Priority 0 means "do not display".** `TitanPutPixel` returns immediately for priority 0
(`titan.c:501`), and `TitanDigPixel` only scans priorities 7 down to 1 (`titan.c:303`).

### PRISA `0x0F0`, PRISB `0x0F2`, PRISC `0x0F4`, PRISD `0x0F6`

Eight 3-bit sprite priority registers, low byte then high byte
(`vidsoft.c:3575–3582`):

```
prioritytable[0] = PRISA & 0x7;        prioritytable[1] = (PRISA >> 8) & 0x7;
prioritytable[2] = PRISB & 0x7;        prioritytable[3] = (PRISB >> 8) & 0x7;
prioritytable[4] = PRISC & 0x7;        prioritytable[5] = (PRISC >> 8) & 0x7;
prioritytable[6] = PRISD & 0x7;        prioritytable[7] = (PRISD >> 8) & 0x7;
```

Indexed by the sprite pixel's decoded priority field, whose width depends on the sprite type
(1–3 bits), so narrow types only reach the first 2, 4 or 8 entries.

### CCCTL — `0x0EC`

| Bits | Field | Use in the renderer |
|---|---|---|
| 0 | NBG0 colour calculation enable | `CCCTL & 0x201`, `& 0x101` (`vidsoft.c:1636–1641`) |
| 1 | NBG1 | `& 0x202`, `& 0x102` (`vidsoft.c:1741–1746`) |
| 2 | NBG2 | `& 0x204`, `& 0x104` (`vidsoft.c:1828–1833`) |
| 3 | NBG3 | `& 0x208`, `& 0x108` (`vidsoft.c:1901–1906`) |
| 4 | RBG0 | `& 0x210`, `& 0x110` (`vidsoft.c:2033–2038`) |
| 6 | Sprite colour calculation enable | `CCCTL & 0x40` (`vidsoft.c:3693`, `3722`, `3811`) |
| 8 | Global: additive blending | `CCCTL & 0x100` → `TITAN_BLEND_ADD` (`vidsoft.c:3515`) |
| 9 | Global: "bottom" ratio mode | `CCCTL & 0x200` → `TITAN_BLEND_BOTTOM` (`vidsoft.c:3516`) |

Bit 8 takes precedence over bit 9 in `VIDSoftVdp2DrawStart`. When neither is set the mode is
`TITAN_BLEND_TOP`.

`titan.c:636` tests `CCCTL & 0x807F` when deciding whether simplified compositing is safe,
implying bits 0–6 and bit 15 are all meaningful.

> **Conflicting reading in `vdp2debug.c`.** The debug dump treats bit 15 as "gradation
> calculation enable", bits 8–10 as a "gradation screen number", and bit 10 as "extended colour
> calculation" (`vdp2debug.c:288–295`, `1605–1611`). The renderer instead treats bit 8 as
> additive blending and bit 9 as the bottom-ratio mode. Bits 8–9 cannot mean both things. The
> renderer's reading is what actually affects output and is presented as primary here; the
> discrepancy is unresolved by the source.

### SFCCMD — `0x0EE`

Two bits per layer: NBG0 bits 0–1, NBG1 2–3, NBG2 4–5, NBG3 6–7, RBG0 8–9
(`vidsoft.c:1642`, `1747`, `1834`, `1907`, `2039`). Interpreted by `GetAlpha`
(`vidsoft.c:736–751`):

| Value | Mode | Behaviour |
|---|---|---|
| 0 | Off | Colour calculation applies unconditionally |
| 1 | Per pattern | Colour calculation only when `specialcolorfunction & 1` (from the pattern-name / bitmap register bit); otherwise alpha is forced to `0x3F` (opaque) |
| 2 | Per colour code | Requires `specialcolorfunction & 1` **and** the SFCODE bit for the pixel's colour-code pair |
| 3 | Per colour data MSB | Colour calculation only when the preserved CRAM bit 15 (pixel bit 31) is set |

### Ratio registers CCRSA–CCRSD `0x100`–`0x106`, CCRNA `0x108`, CCRNB `0x10A`, CCRR `0x10C`, CCRLB `0x10E`

Each 5-bit ratio field is converted to a 6-bit alpha by **inverting and doubling**:

| Layer | Expression | Source |
|---|---|---|
| NBG0 | `((~CCRNA & 0x1F) << 1) + 1` | `vidsoft.c:1637` |
| NBG1 | `((~CCRNA & 0x1F00) >> 7) + 1` | `vidsoft.c:1742` |
| NBG2 | `((~CCRNB & 0x1F) << 1) + 1` | `vidsoft.c:1829` |
| NBG3 | `((~CCRNB & 0x1F00) >> 7) + 1` | `vidsoft.c:1902` |
| RBG0 | `((~CCRR & 0x1F) << 1) + 1` | `vidsoft.c:2034` |
| Sprite 0–7 | `((~CCRSx & 0x1F) << 1) + 1` and `((~CCRSx >> 7) & 0x3E) + 1` | `vidsoft.c:3583–3590` |

So a register value of 0 yields alpha `0x3F` (fully opaque) and 31 yields `0x01` (nearly
transparent). `vdp2debug.c:301` prints the same field as the ratio pair `31-r : 1+r`,
confirming the direction.

`CCRLB` is the odd one out — the line colour screen uses
`alpha = (CCRLB & 0x1F) << 1` (`vidsoft.c:1513`): **not inverted and with no `+1`**, so it
produces even values 0–62 and behaves in the opposite direction to every other ratio register.
Whether this is deliberate or a bug is not determinable from the code.

---

## A.15 Colour offset

### CLOFEN `0x110`, CLOFSL `0x112`

| Bit | Consumer | Source |
|---|---|---|
| 0 | NBG0 | `ReadVdp2ColorOffset(regs, &info, 0x1, 0x1)` (`vidsoft.c:1652`) |
| 1 | NBG1 | `0x2` (`vidsoft.c:1757`) |
| 2 | NBG2 | `0x4` (`vidsoft.c:1844`) |
| 3 | NBG3 | `0x8` (`vidsoft.c:1917`) |
| 4 | RBG0 | `0x10` (`vidsoft.c:2050`) |
| 5 | Back screen | `1 << 5` (`vidsoft.c:1465`) |
| 6 | Sprite | `0x40` (`vidsoft.c:3595`) |

`CLOFEN` enables the offset; `CLOFSL` selects set B (1) or set A (0) for that layer
(`vidsoft.c:340–381`).

### COAR/COAG/COAB `0x114`–`0x118`, COBR/COBG/COBB `0x11A`–`0x11E`

Each is a 9-bit signed value: bits 0–7 are the magnitude and **bit 8 is the sign**, applied by
explicit sign extension (`vidsoft.c:348–373`):

```
cor = COxR & 0xFF;  if (COxR & 0x100) cor |= 0xFFFFFF00;
```

Applied per pixel by `DoColorOffset` → `COLOR_ADD` (`vidsoft.c:60–72`, `331–336`), which adds the
offset to each 8-bit channel with signed saturation to `[0, 0xFF]` and leaves the alpha byte
untouched.

When `CLOFEN` is clear for a layer, `PostPixelFetchCalc` is set to `DoNothing`
(`vidsoft.c:379`), so the whole stage is a function pointer chosen once per layer per line.

---

## A.16 Per-line register latching

VDP2 registers are snapshotted **once per scanline**. At HBLANK-OUT, for every line below
`VBlankLineCount` (`vdp2.cpp:831–853`):

1. The whole `Vdp2` register struct is copied into `Vdp2Lines[LineCount]` (`vdp2.cpp:837`).
2. 88 longwords are copied from `(VCSTA.all & 0x7FFFE) << 1` into
   `cell_scroll_data[LineCount].data[]`, byte-swapped (`vdp2.cpp:841–853`). 88 = (352/8) × 2,
   i.e. one entry per 8-pixel cell column for two layers (`vdp2.h:406–409`). A bounds check
   falls back to word-wise reads if the copy would run past the end of VRAM.

`Vdp2Lines` is 270 entries (`vdp2.h:404`) and `Vdp2RestoreRegs(line, lines)` returns `NULL` for
`line > 270` (`vdp2.cpp:1019`).

Each layer's `LoadLineParams*` callback re-reads a small subset from that snapshot at the start
of every rasterised line (`vidsoft.c:1539–1549`, `1689–1699`, `1795–1805`, `1867–1877`,
`1941–1949`):

| Layer | Re-read per line |
|---|---|
| NBG0 | Colour offset, `SFPRMD` bits 0–1, `BGON` bits 0 and 5, **and the entire plane address table is regenerated** (comment: "sonic 2, 2 player mode") |
| NBG1 | Colour offset, `SFPRMD` bits 2–3, `BGON` bit 1, plane table |
| NBG2 | Colour offset, `SFPRMD` bits 4–5, `BGON` bit 2, plane table |
| NBG3 | Colour offset, `SFPRMD` bits 6–7, `BGON` bit 3, plane table |
| RBG0 | Colour offset, `SFPRMD` bits 8–9 only — **no plane table, no enable** |
| Sprite | Colour offset only |

Everything else (scroll, zoom, colour format, priority, windows, alpha) is sampled **once per
frame** by this renderer, even though the per-line snapshot contains it.

A separate per-line change detector runs at HBLANK-OUT and ORs bits into
`*Vdp2External.perline_alpha` when a line's registers differ from line 0's
(`vdp2.cpp:856–923`): bit 0 for NBG0 (`BGON` bit 0 or `CCRNA` low byte), bit 1 for NBG1, bit 2
for NBG2, bit 3 for NBG3, bit 4 for RBG0, `CLOFEN`'s value when `COBR`/`COAR`/`CLOFSL` change,
bit 6 for `PRISA`, bit 7 for NBG3 scroll, bit 8 for NBG2 scroll. The double-buffered pair is
swapped at VBLANK-OUT (`vdp2.cpp:1394–1403`). **The software renderer never reads
`perline_alpha`** — it is produced for the GL renderer only.

---

# Section B — Rendering and compositing

## B.1 Frame pipeline

Per frame, driven from `vdp2VBlankOUT` (`vdp2.cpp:1141`):

1. `VIDCore->Vdp2DrawStart()` → `VIDSoftVdp2DrawStart` (`vidsoft.c:3512`)
   * choose the global blend mode from `CCCTL` bits 8/9
   * draw the back screen into `tt_context.backscreen`
   * draw the line colour screen into `tt_context.linescreen[1]`
   * evaluate the Castlevania cycle-pattern signature
2. VDP1 framebuffer erase / swap / draw as required.
3. If `TVMD & 0x8000`: `VIDCore->Vdp2DrawScreens()` → `VIDSoftVdp2DrawScreens`
   (`vidsoft.c:3943`)
   * `VIDSoftVdp2SetResolution(TVMD)` → sets `vdp2width`, `vdp2height`, `rbg0width`,
     `rbg0height`, `vdp2_x_hires`, `vdp2_interlace`, and `TitanSetResolution`
   * read the five layer priorities and the `SFPRMD` "draw even at priority 0" flags
   * `TitanErase()` — zeroes all six layer buffers for `vdp2width × height` pixels
   * snapshot registers/VRAM/CRAM/cell-scroll into `vidsoft_thread_context` if threading
   * draw the sprite layer, then NBG0, RBG0, NBG1, NBG2, NBG3
4. At VBLANK-IN, `VIDCore->Vdp2DrawEnd()` → `VIDSoftVdp2DrawEnd` (`vidsoft.c:3860`)
   * join layer threads, then `TitanRender(dispbuffer)`

Each of the six layers writes into its own full-screen `struct PixelData` buffer
(`titan.c:35–42`): `{ u32 pixel; u8 priority; u8 linescreen; u8 shadow_type; u8 shadow_enabled; }`.
Layers never see each other; all resolution happens in `TitanDigPixel`.

Layer buffer indices (`titan.h:30–36`): `NBG3=0, NBG2=1, NBG1=2, NBG0=3, RBG0=4, SPRITE=5`,
with `BACK = -1` used as a sentinel in the simplified path only.

When `vidsoft_num_layer_threads > 0`, each layer runs on its own thread against a private copy
of VRAM, CRAM, the register file and the per-line snapshot (`vidsoft.c:3967–3996`). Layers whose
priority is 0 and whose `SFPRMD` field is 0 are skipped entirely (`vidsoft.c:3897`). The sprite
layer can only be threaded when the sprite window is unused by every layer
(`CanUseSpriteThread`, `vidsoft.c:3924–3941`).

## B.2 Character pattern decode

`Vdp2PatternAddr` (`vidsoft.c:239–320`) reads one pattern-name entry from `info->addr` and
advances it.

### One-word pattern data (`patterndatasize == 1`)

```
tmp = T1ReadWord(ram, addr);  addr += 2;
specialfunction      = (supplementdata >> 9) & 1;
specialcolorfunction = (supplementdata >> 8) & 1;
```

Palette address:

| Colour format | `paladdr` |
|---|---|
| 0 (16 colours) | `((tmp & 0xF000) >> 8) \| ((supplementdata & 0xE0) << 3)` |
| 1–4 | `(tmp & 0x7000) >> 4` |

Character address, by `auxmode`:

| auxmode | patternwh | `flipfunction` | `charaddr` |
|---|---|---|---|
| 0 | 1 (8×8) | `(tmp & 0xC00) >> 10` | `(tmp & 0x3FF) \| ((supplementdata & 0x1F) << 10)` |
| 0 | 2 (16×16) | `(tmp & 0xC00) >> 10` | `((tmp & 0x3FF) << 2) \| (supplementdata & 0x3) \| ((supplementdata & 0x1C) << 10)` |
| 1 | 1 | `0` (no flip) | `(tmp & 0xFFF) \| ((supplementdata & 0x1C) << 10)` |
| 1 | 2 | `0` | `((tmp & 0xFFF) << 2) \| (supplementdata & 0x3) \| ((supplementdata & 0x10) << 10)` |

The identical table appears in the commented-out block at `vdp2debug.c:564–588`, which is a
useful independent confirmation of the constants.

### Two-word pattern data (`patterndatasize == 2`)

```
tmp1 = word at addr;  tmp2 = word at addr+2;  addr += 4;
charaddr             = tmp2 & 0x7FFF;
flipfunction         = (tmp1 & 0xC000) >> 14;
paladdr              = (colornumber == 0) ? ((tmp1 & 0x7F) << 4) : ((tmp1 & 0x70) << 4);
specialfunction      = (tmp1 & 0x2000) >> 13;
specialcolorfunction = (tmp1 & 0x1000) >> 12;
```

### Common tail

```
if (!(VRSIZE & 0x8000))  charaddr &= 0x3FFF;
charaddr *= 0x20;                                  // 32 bytes per 8x8 4bpp cell
if (specialprimode == 1)  priority = (priority & 0xE) | (specialfunction & 1);
```

`flipfunction` bit 0 = horizontal flip, bit 1 = vertical flip.

## B.3 Plane / page / cell addressing — `Vdp2MapCalcXY`

`vidsoft.c:571–684`. Inputs are screen-space `x`, `y` already masked to the virtual screen size.

Derived geometry (`SetupScreenVars`, `vidsoft.c:688–732`), tile mode:

| Quantity | Value |
|---|---|
| `pagepixelwh` | 512 (= 64 cells × 8 px), `_bits` = 9, `_mask` = 511 |
| `planepixelwidth` | `planew × 512`, `_bits` = `8 + planew` |
| `planepixelheight` | `planeh × 512`, `_bits` = `8 + planeh` |
| `screenwidth` | `mapwh × planepixelwidth` |
| `screenheight` | `mapwh × planepixelheight` |
| `xmask`, `ymask` | `screenwidth - 1`, `screenheight - 1` |

Note `planepixelwidth_bits = 8 + planew` gives 9 for `planew==1` and 10 for `planew==2` —
correct only because `planew` is exactly 1 or 2.

For bitmap mode all of the above are zeroed and `xmask = cellw - 1`, `ymask = cellh - 1`.

Per pixel:

```
cellwh = 2 + patternwh;                      // 3 for 8x8, 4 for 16x16
check  = ((y >> cellwh) << 16) | (x >> cellwh);
if (check != sinfo->oldcellcheck) {          // cell boundary crossed
   planenum = ((y >> planepixelheight_bits) * mapwh) + (x >> planepixelwidth_bits);
   x &= planepixelwidth_mask;
   y &= planepixelheight_mask;
   info->addr = sinfo->planetbl[planenum]
              + (( ((y >> pagepixelwh_bits) << pagesize_bits) << planew_bits)
              +  ( (x >> pagepixelwh_bits) << pagesize_bits)
              +  ((((y & pagepixelwh_mask) >> cellwh) << pagewh_bits))
              +  (  (x & pagepixelwh_mask) >> cellwh)
                ) << (patterndatasize_bits + 1);
   Vdp2PatternAddr(info, regs, ram);
   info->pipe[0] = info->pipe[1];            // shift the one-cell pipeline
   info->pipe[1] = { paladdr, charaddr, flipfunction };
}
```

with `pagesize_bits = pagewh_bits * 2`. The pattern name is therefore fetched **once per cell**,
not per pixel — the `oldcellcheck` comparison is the cache.

The final `<< (patterndatasize_bits + 1)` converts a pattern index to a byte offset: `<<1` for
one-word entries, `<<2` for two-word entries.

### Flip application

For 8×8 cells (`patternwh == 1`), after `x &= 7; y &= 7`:

| `flipfunction & 3` | Transform |
|---|---|
| 0 | none |
| 1 | `x = 7 - x` |
| 2 | `y = 7 - y` |
| 3 | both |

For 16×16 cells the four 8×8 sub-cells are stored consecutively, so the flip is expressed as an
adjustment to `y` that selects the sub-cell (`vidsoft.c:644–682`):

```
y &= 15;
if (flipfunction & 2) {                      // vertical
   y = (y & 8) ? (15 - y) : (7 - y + 16);
} else if (y & 8) {
   y += 8;
}
if (flipfunction & 1) {                      // horizontal
   if (!(x & 8)) y += 8;
   x = 7 - (x & 7);
} else if (x & 8) {
   y += 8;  x &= 7;
} else {
   x &= 7;
}
```

The unflipped case is the `else` branch at `vidsoft.c:673–682`: `y += 8` for the lower row and
another `y += 8` for the right column, i.e. sub-cell order is TL, TR, BL, BR laid out as four
consecutive 8×8 blocks in `y`.

## B.4 Pixel fetch — `Vdp2FetchPixel`

`vidsoft.c:385–433`. `info->cellw` is the row stride: 8 for tiles, the bitmap width for bitmaps.
All addresses are masked with `& 0x7FFFF`.

| `colornumber` | Read | Sub-byte handling | Transparent when | Colour |
|---|---|---|---|---|
| 0 (4 bpp) | byte at `charaddr + (y*cellw + x)/2` | `if (!(x & 1)) dot >>= 4` — **even x is the high nibble** | `(dot & 0xF) == 0` | `CRAM[coloroffset + (paladdr \| (dot & 0xF))]` |
| 1 (8 bpp) | byte at `charaddr + y*cellw + x` | — | `(dot & 0xFF) == 0` | `CRAM[coloroffset + (paladdr \| (dot & 0xFF))]` |
| 2 (16 bpp palette) | word at `charaddr + (y*cellw + x)*2` | — | `dot == 0` | `CRAM[coloroffset + dot]` — note `paladdr` is **not** applied |
| 3 (16 bpp RGB) | word at `charaddr + (y*cellw + x)*2` | — | `!(dot & 0x8000)` | `COLSAT2YAB16(0, dot)` — direct, no CRAM |
| 4 (32 bpp RGB) | long at `charaddr + (y*cellw + x)*4` | — | `!(dot & 0x80000000)` | `COLSAT2YAB32(0, dot)` — direct, `dot & 0xFFFFFF` |

All transparency tests are gated on `info->transparencyenable` (from `BGON` bits 8–12); when
disabled, the pixel is always drawn.

The raw `dot` value is returned separately from the colour and is what special priority and
special colour calculation mode 2 examine — so those features operate on the **palette index**,
not the resolved colour, and are meaningless in modes 3 and 4.

> **Channel-order observation.** Mode 3 unpacks a 15-bit value with the same shifts as the CRAM
> reader, so it is consistent with palette layers. Mode 4 passes the low 24 bits through
> unchanged, which places the byte at bits 0–7 in the same slot that mode 3 fills from the
> 15-bit value's *low* 5 bits. Whether the 32-bit direct format is byte-order-compatible with
> that is not established by this code.

## B.5 The `bad_cycle` pipeline

`Vdp2MapCalcXY` maintains `info->pipe[0]` and `info->pipe[1]` — the previous and current cell's
`paladdr`, `charaddr` and `flipfunction`. When `bad_cycle_setting[layer]` is set,
`Vdp2DrawScroll` uses `pipe[0]` instead of the freshly fetched values
(`vidsoft.c:611–618`, `1036–1045`), i.e. every cell renders with the pattern data of the cell to
its left. As noted in §A.3, this is enabled only by an exact eight-register signature match for
NBG3, and is not a general model of cycle-pattern violations.

## B.6 NBG scroll rendering — `Vdp2DrawScroll`

`vidsoft.c:829–1086`. Called for NBG0 (when not in RBG1 mode), NBG1, NBG2, NBG3, and as the
fallback tail of `Vdp2DrawRotationFP`.

### Setup

1. `SetupScreenVars` → geometry and the plane address table.
2. `scrolly = info->y` is saved (used as the base for vertical line scroll).
3. `ReadWindowData(info->wctl, clip, regs)` and `ReadLineWindowData` for the layer's own windows.
4. `ReadWindowData(regs->WCTLD >> 8, colorcalcwindow, regs)` for the colour-calculation window.
   The comment at `vidsoft.c:860` states the polarity: **inside → no colour calculation,
   outside → colour calculation**.
5. A static 16×1024 mosaic lookup table is built once: `mosaic_table[i][j] = j / (i+1) * (i+1)`
   (`vidsoft.c:862–877`).
6. `Vdp2GetInterlaceInfo` → `start_line` = 0 or 1, `line_increment` = 1 or 2.
7. `num_vertical_cell_scroll_enabled` counts `SCRCTL & 1` and `SCRCTL & 0x100`.

### Line scroll table pre-pass

`vidsoft.c:886–919`. Iterates **every** line from `start_line` to `vdp2height` (not by
`line_increment`) and, for each enabled component in order H, V, zoom, reads from
`info->linescrolltbl` and advances it by 4 bytes when
`need_increment = (j != 0) && (((j + 1) % info->lineinc) == 0)`:

| Component | Value read |
|---|---|
| Horizontal (`islinescroll & 1`) | `(T1ReadLong(ram, tbl) >> 16) & 0x7FF` |
| Vertical (`islinescroll & 2`) | `(T1ReadWord(ram, tbl) & 0x7FF) + scrolly` |
| Zoom (`islinescroll & 4`) | `(T1ReadLong(ram, tbl) & 0x7FF00) / 65536.0` |

So enabled components are interleaved as consecutive 4-byte entries, and only the integer part
of horizontal scroll is used.

### Per-line loop

```
for (j = start_line; j < vdp2height; j += line_increment) {
   if (islinescroll & 1)  linescrollx = linescrollx_table[j];
   if (islinescroll & 2)  y = info->y = linescrolly_table[j];
   else                   y = info->y + info->coordincy * mosaic_y[j];
   if (islinescroll & 4)  info->coordincx = lineszoom_table[j];

   if (vdp2_interlace) { linewnd0addr = base0 + j*4;  linewnd1addr = base1 + j*4; }
   ReadLineWindowClip(...);
   y &= sinfo.ymask;

   ... vertical cell scroll ...
   Y = y;
   info->LoadLineParams(info, &sinfo, vdp2_interlace ? j/2 : j, lines);
   if (!info->enable) continue;

   for (i = 0; i < vdp2width; i++) { ... }
   output_y++;
}
```

Note the vertical mosaic is applied to the *line index* before scaling by `coordincy`, and the
vertical line-scroll path bypasses mosaic and zoom entirely.

In interlace mode the line-window address is recomputed absolutely from `j` each line rather than
advanced incrementally — because `ReadLineWindowClip` advances the pointer by 4 per call and the
loop skips every other line.

### Vertical cell scroll

`vidsoft.c:965–993`. Skipped entirely when `vdp2_x_hires` ("seems to be ignored in hi res").

```
y_value = vdp2_interlace ? j/2 : j;
if (num_vertical_cell_scroll_enabled == 1)  scroll = cell_data[y_value].data[0] >> 16;
else if (layer == NBG0)                     scroll = cell_data[y_value].data[0] >> 16;
else if (layer == NBG1)                     scroll = cell_data[y_value].data[1] >> 16;
y += scroll;   y &= 0x1FF;
```

The comment at `vidsoft.c:967–970` explicitly states this is wrong: hardware applies a different
value **per cell column**, advancing the table pointer by `verticalscrollinc` at every cell
boundary and resetting at end of line. This implementation applies a single value per line. The
`verticalscrollinc` field computed in §A.8 is consequently dead.

### Per-pixel loop

```
if (!TestBothWindow(info->wctl, clip, i, j))  continue;
x = info->x + mosaic_x[i] * info->coordincx;
x &= sinfo.xmask;
if (linescrollx) { x += linescrollx;  x &= 0x3FF; }

if (!info->isbitmap) { y = Y;  Vdp2MapCalcXY(info, &x, &y, &sinfo, regs, ram, bad_cycle); }

charaddr = bad_cycle ? info->pipe[0].charaddr : info->charaddr;
paladdr  = bad_cycle ? info->pipe[0].paladdr  : info->paladdr;

if (!Vdp2FetchPixel(info, x, y, &color, &dot, ram, charaddr, paladdr, color_ram))  continue;

priority = info->priority;
if (info->specialprimode == 2) {
   priority = info->priority & 0xE;
   if ((info->specialfunction & 1) && PixelIsSpecialPriority(info->specialcode, dot))
      priority |= 1;
}

alpha = TestBothWindow(regs->WCTLD >> 8, colorcalcwindow, i, j)
      ? GetAlpha(info, color, dot)
      : 0x3F;

TitanPutPixel(priority, i, output_y, info->PostPixelFetchCalc(info, COLSAT2YAB32(alpha, color)),
              info->linescreen, info);
```

Two things worth noting:

* The `x &= 0x3FF` after line scroll uses a fixed 1024 mask rather than `sinfo.xmask`, so line
  scroll can push `x` outside the virtual screen on narrow planes.
* The colour-calculation window test is inverted relative to the drawing windows — a *false*
  result from `TestBothWindow` (i.e. outside the drawn region) means "force opaque".

## B.7 RBG rendering — `Vdp2DrawRotationFP`

`vidsoft.c:1139–1443`. Three distinct paths.

### Path 1 — no coefficient table, screen not rotated

`!p->coefenab && IsScreenRotatedFP(p)` (`vidshared.h:732–744`, which tests
`deltaXst==0, deltaYst==1, deltaX==1, deltaY==0, A==1, B==0, C==0, D==0, E==1, F==0`).

The layer degenerates to a plain scroll layer (`vidsoft.c:1164–1167`):

```
info->x = touint(mulfixed(p->kx, (p->Xst - p->Px)) + p->Px + p->Mx);
info->y = touint(mulfixed(p->ky, (p->Yst - p->Py)) + p->Py + p->My);
info->coordincx = tofloat(p->kx);
info->coordincy = tofloat(p->ky);
```

and control falls out of the `if` to `Vdp2DrawScroll` at `vidsoft.c:1442`.

### Path 2 — no coefficient table, screen rotated

`vidsoft.c:1169–1213`. Per-line the rotation origin advances; per-pixel the matrix is applied.

```
GenerateRotatedVarFP(p, &xmul, &ymul, &C, &F);      // xmul = Xst-Px, ymul = Yst-Py,
                                                    // C = C*(Zst-Pz), F = F*(Zst-Pz)
CalculateRotationValuesFP(p);                       // Xp, Yp, dX, dY
for (j = 0; j < vdp2height; j++) {
   info->LoadLineParams(...);  ReadLineWindowClip(...);
   for (i = 0; i < rbg0width; i++) {
      if (!TestBothWindow(info->wctl, clip, i, j))  continue;
      x = GenerateRotatedXPosFP(p, i, xmul, ymul, C) & sinfo.xmask;
      y = GenerateRotatedYPosFP(p, i, xmul, ymul, F) & sinfo.ymask;
      if (!info->isbitmap)  Vdp2MapCalcXY(info, &x, &y, &sinfo, regs, ram, 0);
      if (!Vdp2FetchPixel(...))  continue;
      Rbg0PutPixel(info, color, dot, i, j);
   }
   xmul += p->deltaXst;
   ymul += p->deltaYst;
}
```

Note this path iterates `j < vdp2height` while path 3 iterates `j < rbg0height` — an
inconsistency in the source. `rbg0height` is not doubled for interlace, `vdp2height` is.

Also note that screen-over is **not** applied on this path; `x`/`y` are unconditionally masked.

### The rotation math

`vidshared.h:317–388`. Setup, once per frame:

```
Xp = A*(Px-Cx) + B*(Py-Cy) + C*(Pz-Cz) + Cx + Mx
Yp = D*(Px-Cx) + E*(Py-Cy) + F*(Pz-Cz) + Cy + My
dX = A*deltaX + B*deltaY
dY = D*deltaX + E*deltaY
```

Per line: `xmul = Xst - Px + n*deltaXst`, `ymul = Yst - Py + n*deltaYst`, plus the constants
`C*(Zst-Pz)` and `F*(Zst-Pz)`.

Per pixel at column `i`:

```
Xsp = A*xmul + B*ymul + C*(Zst-Pz)
Ysp = D*xmul + E*ymul + F*(Zst-Pz)
x   = touint( kx * (Xsp + dX*i) + Xp )
y   = touint( ky * (Ysp + dY*i) + Yp )
```

All arithmetic is 16.16 signed fixed point (`fixed32 = s32`, `FP_SIZE = 16`,
`mulfixed(a,b) = (s64)a*(s64)b >> 16`).

**`touint(v)` is `(u16)(v >> 16)`** (`vidshared.h:287`) — the result is truncated to 16 bits and
treated as unsigned. This is why the screen-over tests never check for negative coordinates, and
it means the rotated address space wraps at 65536 regardless of the plane geometry.

### Path 3 — coefficient table enabled

`vidsoft.c:1215–1440`. This is the full path.

**Second parameter setup** (`vidsoft.c:1234–1243`): for `RPMD == 2`, `p2` is the other
parameter, switched per pixel by `p->msb`. For `RPMD == 3`, `p2` is the other parameter switched
by the rotation-parameter window read from `WCTLD`'s low byte.

**`Rbg0CheckRam` workaround** (`vidsoft.c:1126–1137`, `1265–1276`): if both VRAM banks are
partitioned (`(RAMCTL >> 8) & 3 == 3`) and no bank is designated as the coefficient bank
(`CheckBanks(regs, 1)`), then `deltaKAx` is forced to 0 for any parameter in coefficient mode 0.
The comment names Sonic R and All-Star Baseball '97. This effectively demotes per-dot
coefficients to per-line when the bank assignment is inconsistent — the closest thing in this
codebase to modelling an invalid VRAM allocation, and it is a targeted workaround rather than a
general rule.

**Line colour screen from coefficients** (`vidsoft.c:1278–1290`, `1309–1315`): if the layer has
`LNCLEN` set and `KTCTL` bit 4 (parameter A) or bit 12 (parameter B) is set, `info->linescreen`
becomes 2 or 3, and each line writes

```
lineColorAddr = (T1ReadWord(ram, lineAddr) & 0x780) | p->linescreen;
TitanPutLineHLine(info->linescreen, j, COLSAT2YAB32(0x3F, CRAM[lineColorAddr]));
lineAddr += lineInc;                                  // 2 if LCTA per-line, else 0
```

where `p->linescreen` is the 7-bit field extracted from bits 24–30 of a 4-byte coefficient
entry. So the coefficient table supplies the low bits of a per-pixel line-colour index while
`LCTA` supplies the high bits.

**Coefficient fetch cadence**:

| Condition | Fetch |
|---|---|
| `deltaKAx == 0` | Once per line, at `coeftbladdr + (coefy + touint(rcoefy)) * coefdatasize` (`vidsoft.c:1294–1300`) |
| `deltaKAx != 0` | Once per pixel, at `coeftbladdr + (coefy + coefx + toint(rcoefx + rcoefy)) * coefdatasize`, then `coefx += toint(deltaKAx); rcoefx += decipart(deltaKAx)` (`vidsoft.c:1327–1335`) |

End of line (`vidsoft.c:1419–1437`): `xmul += deltaXst; ymul += deltaYst; coefx = 0; rcoefx = 0;
coefy += toint(deltaKAst); rcoefy += decipart(deltaKAst)` — and the same for `p2` when present.
The integer and fractional accumulators are kept separate so that fractional address increments
accumulate without drift.

**Per-pixel parameter selection** (`vidsoft.c:1346–1409`):

```
if (!TestBothWindow(info->wctl, clip, i, j))  continue;

if ((!userpwindow && p->msb) || (userpwindow && !TestBothWindow(WCTLD, rpwindow, i, j))) {
   if (p2 == NULL || (p2->coefenab && p2->msb))  continue;      // both parameters reject → skip
   x = GenerateRotatedXPosFP(p2, i, xmul2, ymul2, C2);
   y = GenerateRotatedYPosFP(p2, i, xmul2, ymul2, F2);
   apply p2->screenover;
   if (!isbitmap) Vdp2MapCalcXY(info, &x, &y, &sinfo2, regs, ram, 0);
}
else if (p->msb)  continue;                                     // parameter A rejects, no B
else {
   x = GenerateRotatedXPosFP(p, i, xmul, ymul, C);
   y = GenerateRotatedYPosFP(p, i, xmul, ymul, F);
   apply p->screenover;
   if (!isbitmap) Vdp2MapCalcXY(info, &x, &y, &sinfo, regs, ram, 0);
}

if (!Vdp2FetchPixel(info, x, y, &color, &dot, ram, info->charaddr, info->paladdr, color_ram))
   continue;
Rbg0PutPixel(info, color, dot, i, j);
```

So the coefficient MSB has two roles: in single-parameter mode it makes the pixel transparent;
in `RPMD == 2` it switches to the other parameter set.

Note that both parameter sets share one `info` (hence one `charaddr`/`paladdr`/`colornumber`) but
have separate `screeninfo_struct`s (`sinfo` and `sinfo2`) built from different `PlaneAddr`
functions (`vidsoft.c:1251`, `1260`).

**`Rbg0PutPixel`** (`vidsoft.c:1100–1108`): in hi-res, the pixel is written twice at
`x*2` and `x*2+1`; otherwise once. Rotation layers never apply special priority — they pass
`info->priority` directly.

### RBG1

When `BGON & 0x20` is set, NBG0's slot is taken over by RBG1 (`vidsoft.c:1564–1594`). RBG1 always
uses **rotation parameter B**: `rotatenum = 1`, plane size from `PLSZ >> 12`, plane addresses
from `Vdp2ParameterBPlaneAddr`, bitmap base from `MPOFR` bits 4–6. It still uses NBG0's
`PNCN0`, `CHCTLA`, `BMPNA`, `CRAOFA`, `PRINA`, `WCTLA`, `CCRNA` and `LNCLEN` bit 0. `info.enable`
becomes `0x20`, so the dispatch at `vidsoft.c:1675–1684` routes to `Vdp2DrawRotationFP` rather
than `Vdp2DrawScroll`.

`LoadLineParamsNBG0` sets `enable = (BGON & 0x1) || (BGON & 0x20)` (`vidsoft.c:1547`), i.e. the
NBG0 slot stays enabled per-line if either mode is active.

## B.8 Window logic

### Single window test — `TestWindow` (`vidsoft.c:437–462`)

```
if (wctl & enablemask) {
   if (wctl & inoutmask) {                      // draw inside
      if (x < xstart || x > xend || y < ystart || y > yend)  return 0;
   } else {                                     // draw outside
      if (x >= xstart && x <= xend && y >= ystart && y <= yend)  return 0;
      if (yend > vdp2height && x >= xstart && x <= xend)  return 0;   // "overflows vertically on hardware"
   }
   return 1;      // enabled, pixel passes
}
return 3;         // disabled
```

The return value is a two-bit code: **bit 0 = pixel passes, bit 1 = window disabled**. The extra
vertical-overflow rule in the "outside" branch (`vidsoft.c:455–457`) is an empirical hardware
observation recorded in a comment, with no further justification given.

Bounds are inclusive on both ends.

### Sprite window test — `TestSpriteWindow` (`vidsoft.c:466–492`)

Reads `sprite_window_mask[y*vdp2width + x]`, a `704×512` byte array populated during the sprite
pass (`vidsoft.c:3763`). `wctl & 0x20` enables it, `wctl & 0x10` selects inside/outside. Same
`0`/`1`/`3` encoding. Out-of-range addresses return 0 (fail).

### Combination — `TestBothWindow` (`vidsoft.c:508–554`)

```
w0  = TestWindow(wctl, 0x2, 0x1, &clip[0], x, y);
w1  = TestWindow(wctl, 0x8, 0x4, &clip[1], x, y);
spr = TestSpriteWindow(wctl, x, y);

if ((wctl & 0x2A) == 0)          return (wctl & 0x80) ? 0 : 1;   // nothing enabled
if ((w1 & 2) && (spr & 2))       return w0 & 1;                  // only W0
if ((w0 & 2) && (spr & 2))       return w1 & 1;                  // only W1
if (spr & 2)                     return WindowLogic(wctl, w0, w1);
if ((w1 & 2) && (w0 & 2))        return spr & 1;                 // only sprite
if ((wctl & 0x2A) == 0x22)       return WindowLogic(wctl, w0, spr);
if ((wctl & 0x2A) == 0x28)       return WindowLogic(wctl, w1, spr);
if ((wctl & 0x2A) == 0x2A)       return (wctl & 0x80) ? (w0 || w1 || spr) : (w0 && w1 && spr);
return 1;
```

`WindowLogic` (`vidsoft.c:496–504`):

```
(wctl & 0x80) ? (w0 || w1) : (w0 && w1)
```

> **The comments contradict the code.** `WindowLogic`'s comments say AND logic "returns 0 only if
> both the windows are active" and OR "returns 0 if one of the windows is active", but the
> implementation is `||` for the AND bit and `&&` for the OR bit. Since `TestWindow` returns 1
> for "pixel passes", `||` means "pass if either window passes" and `&&` means "pass only if
> both pass" — which is the reverse of the comment's phrasing but consistent with
> `vdp2debug.c:214`'s reading of bit 7 as an overlap logic selector. Treat the code as
> authoritative and the comments as noise.

The three-window case at `vidsoft.c:544–550` duplicates `WindowLogic`'s structure inline rather
than calling it, and the inline `//and logic` / `//or logic` comments are attached to the
opposite branches from `WindowLogic`'s. Same underlying behaviour, same comment confusion.

### Where windows are applied

| Consumer | wctl source | Purpose |
|---|---|---|
| Each NBG/RBG layer | `WCTLA`/`WCTLB`/`WCTLC` byte | Per-pixel draw/skip |
| Colour calculation | `WCTLD >> 8` | Inside → force alpha `0x3F`; outside → normal alpha |
| Rotation parameter switch | `WCTLD` low byte | RPMD 3 parameter selection |
| Sprite layer | `WCTLC >> 8` | Per-pixel draw/skip, but **only when the sprite window is disabled** (`vidsoft.c:3643`); when enabled, the test is deferred until after shadow processing (`vidsoft.c:3778–3784`) |

## B.9 Priority resolution — `TitanDigPixel`

`titan.c:294–374`. Runs once per output pixel.

```
for (priority = 7; priority > 0; priority--)
   for (which_layer = TITAN_SPRITE; which_layer >= 0; which_layer--)
      if (framebuffer[which_layer][pos].priority == priority) {
         pixel_stack[pixel_stack_pos++] = framebuffer[which_layer][pos];
         if (pixel_stack_pos == 2)  goto finished;
      }
pixel_stack[pixel_stack_pos] = backscreen[pos];
```

* Only the **top two** layers are ever needed, because colour calculation blends exactly two.
* Layers are scanned from highest index to lowest, so the tie-break order at equal priority is
  **sprite > RBG0 > NBG0 > NBG1 > NBG2 > NBG3** (`titan.h:30–35`).
* Priority 0 never appears because `TitanPutPixel` refuses to store it; a slot with priority 0 is
  either untouched (zeroed by `TitanErase`) or was a transparent pixel that was never written.
* If fewer than two layers have a pixel, the back screen fills the remaining slot. If two layers
  are found the back screen is skipped entirely (comment: "backscreen is unnecessary in this
  case").

**Note:** the search matches on `priority` alone, not on whether `pixel` is nonzero. A layer that
wrote a black-but-opaque pixel participates normally; a layer that never wrote has priority 0 and
is skipped. This is why `TitanErase` must zero the full `PixelData` struct each frame
(`titan.c:433`).

## B.10 Colour calculation and blending

After the two-pixel stack is built (`titan.c:324–371`):

```
if (pixel_stack[0].linescreen)
   pixel_stack[0].pixel = blend(pixel_stack[0].pixel, linescreen[pixel_stack[0].linescreen][y]);

... shadow handling (see B.11) ...

else if (trans(pixel_stack[0].pixel))
   pixel_stack[0].pixel = blend(pixel_stack[0].pixel, pixel_stack[1].pixel);
```

The `blend` and `trans` function pointers are chosen once per frame from `CCCTL`
(`titan.c:461–478`):

| Blend mode | `blend` | `trans` — "is this pixel a colour-calc source?" |
|---|---|---|
| `TITAN_BLEND_TOP` (default) | `TitanBlendPixelsTop` | `TitanTransAlpha`: `alpha < 0x3F` |
| `TITAN_BLEND_ADD` (`CCCTL & 0x100`) | `TitanBlendPixelsAdd` | `TitanTransBit`: `pixel & 0x80000000` |
| `TITAN_BLEND_BOTTOM` (`CCCTL & 0x200`) | `TitanBlendPixelsBottom` | `TitanTransBit` |

### The three blend functions

**Top** (`titan.c:230–246`) — the top pixel's own alpha is the mixing ratio:

```
alpha  = (alpha_of(top) << 2) + 3;            // 6-bit -> 8-bit, 0x3F -> 0xFF
ralpha = 0xFF - alpha;
out.ch = (ch(top) * alpha)/0xFF + (ch(bottom) * ralpha)/0xFF     for each of 3 channels
out.alpha = 0x3F
```

**Bottom** (`titan.c:248–266`) — the *bottom* pixel's alpha is the ratio, and the top pixel must
carry the bit-31 flag or it is passed through unblended:

```
if ((top & 0x80000000) == 0)  return top;
alpha  = (alpha_of(bottom) << 2) + 3;
ralpha = 0xFF - alpha;
out.ch    = (ch(top)*alpha)/0xFF + (ch(bottom)*ralpha)/0xFF
out.alpha = alpha_of(top)                                        // alpha is preserved, not forced
```

**Add** (`titan.c:268–282`) — per-channel saturating addition, alpha forced to `0x3F`.

### Where the alpha value comes from

Per layer, once per frame (§A.14): `alpha = ((~CCR & 0x1F) << 1) + 1` when the layer's `CCCTL`
enable bit or the global bit 9 is set, else `0x3F`. Then:

```
if ((CCCTL & 0x2xx) == 0x2xx)       alpha |= 0x80;      // bit 9 AND the layer bit
else if ((CCCTL & 0x1xx) == 0x1xx)  alpha |= 0x80;      // bit 8 AND the layer bit
```

The `0x80` bit lands at bit 31 of the composed pixel via `COLSAT2YAB32`, which is exactly what
`TitanTransBit` tests. So in ADD and BOTTOM modes, a layer only participates in colour
calculation if both the global mode bit and its own enable bit are set; in TOP mode, participation
is decided purely by whether the alpha is less than `0x3F`.

Per pixel, `GetAlpha` (`vidsoft.c:736–751`) can override the layer alpha back to `0x3F`
(opaque, no blending) according to `SFCCMD` — see §A.14 — and the colour-calculation window can
do the same (`vidsoft.c:1076–1079`).

### Final output conversion

`TitanFixAlpha` (`titan.c:97`, default build):

```
((pixel & 0x3F000000) << 2) + 0x03000000 | (pixel & 0x00FFFFFF)
```

expands the 6-bit alpha to 8 bits in place. Note the `+` rather than `|` means an alpha of
`0x3F` produces exactly `0xFF`. Bit 31's colour-calc flag is discarded by the `0x3F000000` mask.

### Simplified compositing path

`TitanRenderLinesSimplified` (`titan.c:116–199`) replaces `TitanDigPixel` when **all four** of
these hold (`titan.c:636–649`):

* `CCCTL & 0x807F == 0` (no colour calculation)
* `SFPRMD & 0x3FF == 0` (no special priority)
* `LNCLEN & 0x1F == 0` (no line screen)
* `SDCTL & 0x13F == 0` (no shadows)

It pre-sorts layers by priority once per band and picks the first non-transparent pixel, with the
sprite layer short-circuiting whenever `sprite.priority >= layer_priority[bg_layer]`. Behaviourally
equivalent to `TitanDigPixel` when the four conditions hold, but note it consults
`tt_context.layer_priority[]` — the *register* priority — rather than the per-pixel stored
priority, so it would be wrong under per-pixel special priority. That is exactly the case its
guard excludes.

## B.11 Shadows

Three shadow paths, all resolved in `TitanDigPixel` (`titan.c:329–363`):

**Transparent MSB shadow** — top pixel has `shadow_type == TITAN_MSB_SHADOW` and its RGB is 0:

```
if (pixel_stack[1].shadow_enabled)
   pixel_stack[0].pixel = TitanBlendPixelsTop(0x20000000, pixel_stack[1].pixel);
else
   pixel_stack[0].pixel = pixel_stack[1].pixel;
```

`0x20000000` is a pixel with alpha `0x20` and RGB 0 — i.e. blending the layer below with black at
roughly 50%.

**Self-shadow** — top pixel has `TITAN_MSB_SHADOW` with nonzero RGB: the normal colour
calculation runs first, then, **only if the sprite window is disabled**
(`!(Vdp2Regs->SPCTL & 0x10)`), the result is darkened by the same `0x20000000` blend
(`titan.c:341–352`).

**Normal shadow** — top pixel has `TITAN_NORMAL_SHADOW` (set when the sprite colour value equals
the type-specific normal-shadow constant, §A.12): identical handling to the transparent MSB
shadow.

In all cases `shadow_enabled` is read from the pixel **below** — it is the `SDCTL` bit of the
layer being shadowed, not the shadowing layer.

`TitanDigPixel` reads the **global** `Vdp2Regs->SPCTL` (`titan.c:350`) rather than a snapshot,
which is a thread-safety wart given layer rendering runs on worker threads against copies.

## B.12 Back screen and line colour screen

**Back screen** (`Vdp2DrawBackScreen`, `vidsoft.c:1447–1492`): writes one colour per line into
`tt_context.backscreen` via `TitanPutBackHLine`. Colour is a raw RGB 5:5:5 word read directly
from VRAM (not through CRAM), alpha forced to `0x3F`, then passed through the colour-offset
stage. Per-line mode advances the address by 2 per line.

**Line colour screen** (`Vdp2DrawLineScreen`, `vidsoft.c:1496–1535`): writes one colour per line
into `linescreen[1]`. The value read from VRAM is masked to `0x7FF` and used as a **CRAM index**,
unlike the back screen. Alpha is `(CCRLB & 0x1F) << 1` (see the note in §A.14). Skipped entirely
when `LNCLEN == 0`.

`linescreen[2]` and `linescreen[3]` are filled only by the rotation coefficient path
(§B.7). `linescreen[0]` is never allocated — `TitanPutLineHLine` returns immediately for index 0
(`titan.c:491`) and `TitanInit` allocates only indices 1–3 (`titan.c:390`).

## B.13 Sprite layer read-out

`VidsoftDrawSprite` (`vidsoft.c:3541–3858`) is a VDP2 function: it reads the VDP1 front
framebuffer and composites it into Titan layer 5 using VDP2's `SPCTL`, `PRISA`–`PRISD`,
`CCRSA`–`CCRSD`, `CRAOFB`, `WCTLC` and `WCTLD` registers.

Skipped entirely unless `Vdp1External.disptoggle && (TVMD & 0x8000)` (`vidsoft.c:3566`).

### Framebuffer sampling

If `Vdp1Regs->TVMR & 2` (VDP1 rotation mode), the read-out coordinates come from **VDP2 rotation
parameter A** (`vidsoft.c:3607–3608`, `3651–3653`):

```
x = touint(p.Xst + i * p.deltaX + i2 * p.deltaXst) & (vdp1width  - 1);
y = touint(p.Yst + i * p.deltaY + i2 * p.deltaYst) & (vdp1height - 1);
```

Otherwise a resolution-matching rule applies (`vidsoft.c:3657–3679`):

| VDP1 width | `vdp2_x_hires` | X step per output pixel |
|---|---|---|
| 1024 | 1 | 1.0 (1:1) |
| 512 | 1 | 0.5 (VDP1 pixel doubling) |
| 1024 | 0 | 2.0 (half-res read-out) |
| otherwise | — | `x = i` |

### 16-bit framebuffer

| Framebuffer word | Handling |
|---|---|
| `0x0000` | Skipped (transparent) |
| bit 15 set **and** `SPCTL & 0x20` | RGB direct: `COLSAT2YAB16(alpha, pixel)`, priority `prioritytable[0]`. Alpha is `colorcalctable[0]` only when `SPCCCS == 3`, the colour-calc window passes, and `CCCTL & 0x40`. The `0x8000`-exactly special case from §A.12 applies. |
| otherwise | Colour bank: decode via `Vdp1GetSpritePixelInfo`, look up `CRAM[vdp1coloroffset + pixel]` |

### 8-bit framebuffer

Only the colour-bank path exists; any nonzero byte is decoded the same way
(`vidsoft.c:3789–3851`).

### Sprite colour calculation

Gated on the colour-calculation window and `CCCTL & 0x40`. `SPCCCS` decides whether the pixel is
"transparent" (i.e. a colour-calc participant), per the table in §A.12. Then
(`vidsoft.c:3746–3758`):

```
if (CCCTL & 0x200) {                       // bottom mode
   alpha = colorcalctable[spi.colorcalc];
   if (transparent)  alpha |= 0x80;
} else if (transparent) {
   alpha = colorcalctable[spi.colorcalc];
   if (CCCTL & 0x100)  alpha |= 0x80;
}
```

The comment at `vidsoft.c:3746–3751` explains bottom mode: the alpha is set unconditionally
because a lower-priority layer will consume it, and bit 31 is only set when the sprite itself is
a colour-calc participant.

### Sprite window mask

When `SPCTL & 0x10` and the pixel carries `msbshadow`, `spr_window_mask[y*vdp2width + x] = 1`
(`vidsoft.c:3763`). The mask is cleared to zero at the start of the sprite pass
(`vidsoft.c:3559–3562`). Note the mask is indexed by the **framebuffer** coordinates `x`/`y`,
while `TestSpriteWindow` indexes it by **screen** coordinates — these coincide only when the
VDP1 and VDP2 resolutions match.

---

## B.14 Summary of things this code does not model

Collected here so a reimplementation knows where it cannot use Yabause as an oracle:

1. **VRAM access cycle patterns** are decoded but only used to compute a CPU stall factor and
   (in the GL renderer) a per-bank boolean. Timeslot codes `0x8`–`0xD` are never distinguished.
   Behaviour on conflicting or insufficient allocation is unmodelled except for one hard-coded
   game signature (§A.3) and one coefficient-bank workaround (§B.7).
2. **Screen-over mode 1** (display a single pattern from OVPNRA/OVPNRB) is logged as
   unimplemented; `OVPNRA`/`OVPNRB` are never read.
3. **`RPRCTL`** (rotation parameter read control) is never read.
4. **`ZMCTL`** (reduction enable) is never read by the software renderer.
5. **Fractional scroll** (`SCXDN*`, `SCYDN*`) is discarded; only the integer registers are used.
6. **Vertical cell scroll** is applied once per line rather than once per cell column, with an
   in-source comment acknowledging this is wrong (`vidsoft.c:967`).
7. **Special priority** is applied only in `Vdp2DrawScroll`, never in the rotation path.
8. **Per-line register changes** other than colour offset, `SFPRMD`, `BGON` and the plane table
   are ignored, despite a full per-line snapshot being captured.
9. **`HCNT`** is never driven by a free-running counter; it only ever holds an externally latched
   value.
10. **Byte and long register accesses** are unimplemented (reads return 0, byte writes are
    dropped).
11. `PLSZ` encoding `2` is guessed at (`vidshared.h:499`), and `SFPRMD` mode 3 is labelled
    undocumented (`vdp2debug.c:373`).
12. Several internal inconsistencies are documented above and should be resolved against
    hardware rather than copied: the `RAMCTL` bank-partition bit position (§A.2), the bitmap
    palette shift (§A.5), the rotation parameter P/C sign extension (§A.10), the `CCRLB` alpha
    derivation (§A.14), the `CCCTL` bits 8–10 meaning (§A.14), the `cpu_cycle` mapping bug
    (§A.3), and the `rbg0height`/`vdp2height` mismatch between rotation paths (§B.7).
