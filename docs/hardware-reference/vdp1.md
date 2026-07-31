# VDP1 — Sprite / Polygon Drawing Engine

**Provenance.** This document is derived *exclusively* by reading the Yabause / YabaSanshiro C and C++ source tree. No external Saturn documentation, no manufacturer manual, and no recollected general knowledge was used. Every factual claim below carries a `file:line` citation and can be checked against the source.

Files read to produce this document:

| File | Role |
| --- | --- |
| `yabause/src/vdp1.cpp` (1916 lines, read in full) | VDP1 front end: register file, memory handlers, command-list walker, command dispatch, debugger decoders |
| `yabause/src/vdp1.h` (220 lines, read in full) | Register struct, command-table struct, `VIDCore` renderer vtable, external-state struct |
| `yabause/src/vidsoft.c` (VDP1 section, lines ~2374–4170) | The software renderer that actually *implements* the drawing callbacks `vdp1.cpp` dispatches to |
| `yabause/src/vdp2.cpp` (lines ~930–1260, ~1400–1410) | Consumer of the VDP1 erase/swap flags; where frame change and Draw End interrupt actually happen |
| `yabause/src/vidshared.h` (lines ~820–1000) | `Vdp1GetSpritePixelInfo` — how VDP2 re-interprets VDP1 framebuffer pixels |
| `yabause/src/memory.c` (lines 675–692) | CPU address map for VDP1 |
| `yabause/src/scu.c` (line 3382) | The Draw End interrupt vector |
| `yabause/src/vidogl.c` (lines 1095–1130) | Cross-check of TVMR/FBCR decoding in the second renderer |

**Why more than `vdp1.cpp`.** `vdp1.cpp` contains *no drawing code at all*. It is a dispatcher: it walks the command list and calls function pointers on `VIDCore` (`vdp1.h:96-108`). All rasterisation, texture fetch, gouraud interpolation, clipping tests and framebuffer erase live in the renderer back ends. To answer "exactly what drawing algorithm the code performs", the software renderer `vidsoft.c` had to be read as well; it is cited separately and explicitly throughout so the two layers are never confused.

**A standing caveat.** Yabause is an emulator, not a hardware spec. Where Yabause's authors themselves left a comment saying they are guessing, working around a game bug, or unsure, that comment is reproduced here rather than being smoothed over into a confident statement. Sections marked **[Ambiguous]** are places where the code does not make the real hardware behaviour determinable.

---

## 1. Address map and storage

`memory.c:675-692` installs VDP1 into the SH-2 address space in three regions (the `FillMemoryArea` arguments are the top 12 bits of the address):

| CPU address range | Region | Handlers |
| --- | --- | --- |
| `0x05C00000`–`0x05C7FFFF` | VDP1 VRAM | `Vdp1RamReadByte/Word/Long`, `Vdp1RamWriteByte/Word/Long` (`memory.c:675`) |
| `0x05C80000`–`0x05CFFFFF` | VDP1 framebuffer (the *back* buffer, see §8) | `Vdp1FrameBufferRead*/Write*` (`memory.c:681`) |
| `0x05D00000`–`0x05D7FFFF` | VDP1 register file | `Vdp1Read*/Vdp1Write*` (`memory.c:687`) |

Allocation sizes, `vdp1.cpp:282-304`:

| Storage | Size | Notes |
| --- | --- | --- |
| `Vdp1Ram` | `0x80000` (512 KiB) | `vdp1.cpp:286` |
| `Vdp1FrameBuffer[0]`, `Vdp1FrameBuffer[1]` | `0x40000` (256 KiB) each | `vdp1.cpp:290-294` — "Allocate enough memory for two frames" |

Address masking, applied on every access:

* VRAM accesses mask with `0x7FFFF` (`vdp1.cpp:146,153,160,167,175,183`). VRAM therefore mirrors every 512 KiB across the whole `0x5C0xxxxx` window.
* Framebuffer accesses mask with `0x3FFFF` (`vdp1.cpp:194,208,222,236,251,266`). Mirrors every 256 KiB.
* Register accesses mask with `0xFF` (`vdp1.cpp:418,426,451,459,468,544`). The eleven registers therefore mirror every 256 bytes across the whole `0x5D0xxxxx` window.

Framebuffer *reads* through the CPU port go through the renderer if it provides a hook: `Vdp1FrameBufferReadWord` and `...ReadLong` call `VIDCore->Vdp1ReadFrameBuffer` under a VRAM lock when non-NULL, otherwise fall back to `T1Read*` on `Vdp1FrameBuffer[Vdp1External.current_frame]` (`vdp1.cpp:207-231`). `Vdp1FrameBufferReadByte` has its renderer path commented out and *always* reads the raw array (`vdp1.cpp:193-203`). Framebuffer *writes* do both: they call the renderer hook when present **and** write the raw array unconditionally (`vdp1.cpp:235-276`).

In `vidsoft.c` those hooks resolve to `VIDSoftVdp1ReadFrameBuffer` / `VIDSoftVdp1WriteFrameBuffer` (`vidsoft.c:3438-3501`), which operate on `vdp1backframebuffer` — i.e. the CPU port sees the buffer VDP1 is *drawing into*, not the one being displayed. Both byte-swap 16- and 32-bit accesses on little-endian hosts, and 32-bit accesses additionally swap the two halfwords (`vidsoft.c:3495`). 32-bit framebuffer *reads* are stubbed to return 0, behind a comment `#if 0 //enable when burning rangers is fixed` (`vidsoft.c:3458-3467`). **[Ambiguous]** — real hardware presumably returns data; this is an emulator workaround, not hardware behaviour.

### 1.1 VRAM write side effects

Every VRAM write resets a global timer: `vdp1_clock = 0` (`vdp1.cpp:170,178,188`). `Vdp1_onHblank()` increments it by 100 per HBlank (`vdp1.cpp:138`). `vdp2.cpp:931` gates the once-per-frame VOUT/frame-change event on `vdp1_clock > 0` with the comment "Delay if vdp1 ram was written". This is an emulator heuristic for "the CPU is still filling the command list, don't start the frame yet"; it does not correspond to a hardware register.

Yabause additionally maintains a 64-byte-page dirty bitmap over VRAM (`vdp1.cpp:84-121`), gated on `g_ygl_persistent_tile_cache_enabled`, described in its own comment as "Currently just counted for telemetry, not yet consumed for correctness". Not hardware.

---

## 2. Register file

The register struct is `Vdp1` in `vdp1.h:54-82`. It contains both the eleven real registers and a block of internal engine state (§2.4).

### 2.1 Register map

Offsets are from the base of the register region (`0x05D00000`). Byte and long accesses are **not implemented**: `Vdp1ReadByte` logs "trying to byte-read a Vdp1 register" and returns 0 (`vdp1.cpp:417-421`); `Vdp1ReadLong` returns 0 (`vdp1.cpp:450-454`); `Vdp1WriteByte` and `Vdp1WriteLong` are no-ops that only log (`vdp1.cpp:458-461`, `vdp1.cpp:543-546`). **All meaningful register access is 16-bit.**

| Offset | Name | Access | Handler line | Summary |
| --- | --- | --- | --- | --- |
| `0x00` | **TVMR** | Write-only | `vdp1.cpp:470-473` | TV mode / framebuffer geometry select, VBlank-erase enable |
| `0x02` | **FBCR** | Write-only | `vdp1.cpp:474-485` | Frame-buffer change/erase mode, interlace |
| `0x04` | **PTMR** | Write-only | `vdp1.cpp:486-520` | Plot trigger. Writing 1 starts drawing immediately |
| `0x06` | **EWDR** | Write-only | `vdp1.cpp:521-523` | Erase/write data (the fill value) |
| `0x08` | **EWLR** | Write-only | `vdp1.cpp:524-526` | Erase/write upper-left coordinate |
| `0x0A` | **EWRR** | Write-only | `vdp1.cpp:527-529` | Erase/write lower-right coordinate |
| `0x0C` | **ENDR** | Write-only | `vdp1.cpp:530-535` | Draw forced termination |
| `0x10` | **EDSR** | Read-only | `vdp1.cpp:428-430` | End status (BEF / CEF) |
| `0x12` | **LOPR** | Read-only | `vdp1.cpp:431-433` | Last operation command address |
| `0x14` | **COPR** | Read-only | `vdp1.cpp:434-436` | Current operation command address |
| `0x16` | **MODR** | Read-only, synthesised | `vdp1.cpp:437-441` | Mode status readback |

Offset `0x0E` is not decoded by either the read or the write switch: reading it falls into `default:` → logs "trying to read a Vdp1 write-only register" and returns 0 (`vdp1.cpp:442-445`); writing it falls into `default:` → logs and is discarded (`vdp1.cpp:536-538`). Offsets `0x18` and above likewise decode to nothing.

Writing to a read-only register (`0x10`–`0x16`) is silently discarded via the same `default:` (`vdp1.cpp:536-538`). Reading a write-only register (`0x00`–`0x0C`) returns 0 (`vdp1.cpp:445`).

### 2.2 Write-only registers, bit by bit

#### TVMR — offset `0x00` (`vdp1.cpp:470-473`)

The write handler stores the value verbatim; all decoding happens at draw-start time in the renderer (`vidsoft.c:2392-2415`) and in `vdp2.cpp`.

| Bits | Name | Meaning per the code |
| --- | --- | --- |
| 15–4 | — | Stored but never read by any decoder. Only bits 3–0 reach MODR (`vdp1.cpp:438`) |
| 3 | **VBE** | VBlank Erase. `vdp2.cpp:1405-1409`: `if (((Vdp1Regs->TVMR >> 3) & 0x01) == 1) Vdp1External.vbalnk_erase = 1; else = 0`, evaluated at VBlank-IN. Consumed at `vdp2.cpp:1221-1224` |
| 2 | **TVM2** | Stored, reaches MODR, but **no behavioural decode exists**. `vidsoft.c:2392-2415` tests only bits 0 and 1; `vidogl.c:1103-1117` switches on `TVMR & 7` but cases 4–7 all fall to `default:` with no distinct effect. **[Ambiguous]** — what TVM2 does on hardware is not determinable from this source |
| 1 | **TVM1** | Rotation mode. Combined with bit 0 to pick framebuffer geometry (below); also used by VDP2 readout at `vidsoft.c:3607` and `vidsoft.c:3651` to select rotation-parameter-driven framebuffer addressing |
| 0 | **TVM0** | 8-bit (1) vs 16-bit (0) framebuffer pixel size |

Framebuffer geometry decode, `vidsoft.c:2392-2415`:

| TVM1 | TVM0 | `vdp1width` | `vdp1height` | `vdp1pixelsize` | Comment in source |
| --- | --- | --- | --- | --- | --- |
| 0 | 0 | 512 | 256 | 2 | "Rotation/Normal 16-bit" (`vidsoft.c:2411-2414`) |
| 1 | 0 | 512 | 256 | 2 | same branch — bit 1 is only examined when bit 0 is set |
| 0 | 1 | 1024 | 256 | 1 | "Normal 8-bit" (`vidsoft.c:2400-2405`) |
| 1 | 1 | 512 | 512 | 1 | "Rotation 8-bit" (`vidsoft.c:2394-2399`) |

Note that all four geometries are exactly `0x40000` bytes, matching the framebuffer allocation.

When TVM1 is set, VDP2's framebuffer readout addresses the framebuffer through a rotation parameter table instead of linearly: `x = touint(p.Xst + i*p.deltaX + i2*p.deltaXst) & (vdp1width-1)`, likewise for `y` (`vidsoft.c:3651-3654`, table loaded at `vidsoft.c:3607-3608`).

#### FBCR — offset `0x02` (`vdp1.cpp:474-485`)

The write handler stores the value **and** has an immediate side effect on the two-bit `FCM:FCT` field:

```
Vdp1Regs->FBCR = val;
if ((Vdp1Regs->FBCR & 3) == 3)      Vdp1External.manualchange = 1;   // vdp1.cpp:477-480
else if ((Vdp1Regs->FBCR & 3) == 2) Vdp1External.manualerase  = 1;   // vdp1.cpp:481-484
```

| Bits | Name | Meaning per the code |
| --- | --- | --- |
| 15–5 | — | Stored; not decoded anywhere. Only bits 4–1 reach MODR (`vdp1.cpp:438`) |
| 4 | **EOS** | Reaches MODR (`(FBCR & 0x1E) << 3` covers bits 4..1) but has **no behavioural decode**. **[Ambiguous]** |
| 3 | **DIE** | Double-interlace enable. `vidsoft.c:2388-2391`: `vdp1interlace = (FBCR & 8) ? 2 : 1`. Cross-checked at `vidogl.c:1120-1126` |
| 2 | **DIL** | Double-interlace draw line select. Used only when `vdp1interlace == 2`: `CheckDil()` at `vidsoft.c:2653-2672` rejects even `y` when DIL=1 and odd `y` when DIL=0 |
| 1 | **FCM** | Frame change mode. `FCM=0` = "one cycle mode": erase and swap happen automatically every frame |
| 0 | **FCT** | Frame change trigger, only meaningful with FCM=1 |

FCM:FCT combinations as actually implemented:

| FCM | FCT | `FBCR & 3` | Behaviour |
| --- | --- | --- | --- |
| 0 | 0 | `0` | One-cycle mode. `vdp2.cpp:941-945` sets `swap_frame_buffer = 1` every frame; `vdp2.cpp:1221-1224` erases every VBlank; `vidsoft.c:4113` and `vidsoft.c:4135` both take the `(FBCR & 2) == 0` branch unconditionally |
| 0 | 1 | `1` | Also treated as one-cycle mode, with the explicit comment `0x01 is treated as one cyscle mode in Sonic R` (`vdp2.cpp:942`). **This is a game-specific workaround, not necessarily hardware behaviour.** [Ambiguous] |
| 1 | 0 | `2` | Manual erase. `Vdp1External.manualerase = 1` (`vdp1.cpp:483`); consumed just before the next frame change (`vdp2.cpp:1230-1233`) |
| 1 | 1 | `3` | Manual change. `Vdp1External.manualchange = 1` (`vdp1.cpp:479`); consumed at `vdp2.cpp:935-938` to request a buffer swap |

#### PTMR — offset `0x04` (`vdp1.cpp:486-520`)

Stored verbatim, then acted on. In the synchronous (non-`YAB_ASYNC_RENDERING`) build:

```
Vdp1Regs->PTMR = val;                       // vdp1.cpp:490
if (val == 1) {
    Vdp1Regs->EDSR >>= 1;                   // vdp1.cpp:511  (CEF -> BEF, CEF cleared)
    Vdp1Draw();                             // vdp1.cpp:512
    VIDCore->Vdp1DrawEnd();                 // vdp1.cpp:513
    yabsys.wait_line_count = (yabsys.LineCount + 50) % yabsys.MaxLineCount;   // vdp1.cpp:514-515
}
```

| Value written | Meaning per the code |
| --- | --- |
| `0` | Idle / no trigger. Nothing happens beyond storing the value |
| `1` | **Immediate plot trigger.** Shifts EDSR right by one, runs the whole command list synchronously, schedules the Draw End signal for 50 scanlines later |
| `2` | **Draw on frame change.** Not acted on at write time. `vdp2.cpp:947-954`: `if (Vdp1Regs->PTMR == 2) Vdp1External.frame_change_plot = 1; else = 0`. The draw is then kicked off from the frame-change path (`vdp2.cpp:1246-1252`) |

Only bit 1 of PTMR reaches MODR (`(PTMR & 2) << 7`, `vdp1.cpp:438`).

The `yabsys.wait_line_count` scheduling (50 lines) is an emulator timing approximation for how long drawing takes — VDP1 drawing in Yabause is instantaneous, and the 50-line delay exists purely so the Draw End interrupt does not fire on the same line the draw was requested. It is **not** a hardware timing figure. The line `//if (yabsys.wait_line_count == 2) { yabsys.wait_line_count = 3; } // it should not be the same line with render.` (`vdp1.cpp:516`) shows this was tuned empirically.

#### EWDR — offset `0x06` (`vdp1.cpp:521-523`)

Erase/Write Data. Stored verbatim; the entire 16-bit value is the fill pattern. Used by `VIDSoftVdp1EraseFrameBuffer`:

* 16-bit framebuffer: `((u16*)back_framebuffer)[...] = regs->EWDR` — the full word (`vidsoft.c:4145`).
* 8-bit framebuffer: `back_framebuffer[pos] = regs->EWDR & 0xFF` — the low byte only (`vidsoft.c:4159`). **[Ambiguous]** — hardware plausibly writes both bytes of each 16-bit unit; the code writes one byte per pixel.

#### EWLR — offset `0x08` (`vdp1.cpp:524-526`)

Erase/Write upper-left coordinate. Decoded at `vidsoft.c:4142` and `vidsoft.c:4152`:

| Bits | Field | Extraction | Resulting value |
| --- | --- | --- | --- |
| 15 | — | Discarded by the mask | Not used |
| 14–9 | X1 (in 8-pixel units) | `(EWLR >> 6) & 0x1F8` | `((EWLR >> 9) & 0x3F) * 8` — start X in pixels, 8-pixel granularity |
| 8–0 | Y1 | `EWLR & 0x1FF` | start Y in lines, 1-line granularity |

#### EWRR — offset `0x0A` (`vdp1.cpp:527-529`)

Erase/Write lower-right coordinate. Decoded at `vidsoft.c:4137-4140`:

| Bits | Field | Extraction | Resulting value |
| --- | --- | --- | --- |
| 15–9 | X3 (in 8-pixel units) | `((EWRR >> 6) & 0x3F8) + 8` | `(((EWRR >> 9) & 0x7F) * 8) + 8` — exclusive end X |
| 8–0 | Y3 | `(EWRR & 0x1FF) + 1` | exclusive end Y |

Both are clamped: `if (h > vdp1height) h = vdp1height; if (w > vdp1width) w = vdp1width;` (`vidsoft.c:4138,4140`).

**In 8-bit framebuffer mode the X computation is replaced entirely** (`vidsoft.c:4149-4150`): `w = regs->EWRR >> 9; w *= 16;` — i.e. `((EWRR >> 9) & 0x7F) * 16`, double the 16-bit-mode width, and notably computed *after* the clamp so it is not re-clamped against `vdp1width`. A bounds guard `if (pos < 0x3FFFF)` at `vidsoft.c:4157` catches the overflow instead. The doubling is consistent with 8-bit mode having 1024-pixel-wide lines; the missing re-clamp looks like an oversight.

#### ENDR — offset `0x0C` (`vdp1.cpp:530-535`)

Draw forced termination. Any write:

```
Vdp1Regs->ENDR = val;                        // vdp1.cpp:531
Vdp1External.status = VDP1_STATUS_IDLE;      // vdp1.cpp:532
yabsys.wait_line_count = -1;                 // vdp1.cpp:533
```

The written value is irrelevant — the register is a strobe. Setting `status = IDLE` makes the resumption paths at `vdp2.cpp:1246` and `vdp2.cpp:1256` stop re-entering `Vdp1Draw()`. Setting `wait_line_count = -1` cancels the pending Draw End signal, so **a forced termination suppresses the Draw End interrupt** (see §9). Note that ENDR does *not* set `EDSR` and does *not* modify `COPR`/`LOPR`.

### 2.3 Read-only registers

#### EDSR — offset `0x10` (`vdp1.cpp:428-430`)

Returned verbatim from `Vdp1Regs->EDSR`. Two bits are used:

| Bit | Name | Set by | Cleared by |
| --- | --- | --- | --- |
| 1 | **CEF** (current end flag) | `regs->EDSR \|= 2` on bad-command abort (`vdp1.cpp:619`); `Vdp1Regs->EDSR \|= 2` on normal draw completion (`vdp2.cpp:1002` / `vdp2.cpp:990`); `Vdp1Regs->EDSR \|= 2` in `Vdp1NoDraw` (`vdp1.cpp:853`) | The `>>= 1` shift below |
| 0 | **BEF** (before end flag) | Receives the old CEF via `EDSR >>= 1` | The `>>= 1` shift when CEF was 0 |

The shift `Vdp1Regs->EDSR >>= 1` — "BEF ← CEF; CEF ← 0" per the comment at `vdp1.cpp:816-818` — occurs at exactly two places in the synchronous build:

1. On a plot trigger write of PTMR=1 (`vdp1.cpp:511`).
2. On a framebuffer frame change (`vdp2.cpp:1240`, guarded by `#if !defined(YAB_ASYNC_RENDERING)`).

`Vdp1Draw()` itself has the shift commented out (`vdp1.cpp:819`) with the note "this should be done after a frame change or a plot trigger". `Vdp1NoDraw()` likewise (`vdp1.cpp:846`).

#### LOPR — offset `0x12` (`vdp1.cpp:431-433`)

Last-operation command address, as a *word-pair index*: `regs->LOPR = regs->addr >> 3`. Written **only on error paths**:

* Bad command opcode (12–15) in `Vdp1DrawCommands` (`vdp1.cpp:620`) and in `Vdp1FakeDrawCommands` (`vdp1.cpp:726`).
* The "force to quit internal command error" check, which fires when `EDSR & 0x02` is already set at the top of the loop body (`vdp1.cpp:629`). The comment says this technique is used by *Batsugun*.

LOPR is **never** written on normal successful completion in this code. **[Ambiguous]** — real hardware documents LOPR as tracking the last-processed command generally; this implementation only maintains it for aborts.

#### COPR — offset `0x14` (`vdp1.cpp:434-436`)

Current-operation command address, same `addr >> 3` word-pair encoding. Updated:

* At entry to `Vdp1DrawCommands` (`vdp1.cpp:554`).
* At the top of **every** iteration of the command loop, before dispatch (`vdp1.cpp:571`). This is the live "where am I in the list" value.
* On both abort paths (`vdp1.cpp:621`, `vdp1.cpp:630`).
* Reset to 0 when a frame begins with the engine idle (`vdp1.cpp:821`), in `Vdp1NoDraw` (`vdp1.cpp:848`), and on the frame-change-triggered restart (`vdp2.cpp:1250`).

Because `addr` is a byte address and command tables are `0x20` bytes apart, consecutive commands produce COPR values `0x20/8 = 4` apart.

#### MODR — offset `0x16` (`vdp1.cpp:437-441`)

Unlike the other three, MODR is **not a stored register** — it is synthesised on every read from the current contents of PTMR, FBCR and TVMR:

```c
u16 mode = 0x1000
         | ((Vdp1Regs->PTMR & 2) << 7)     // PTM1 -> bit 8
         | ((Vdp1Regs->FBCR & 0x1E) << 3)  // FBCR bits 4..1 -> bits 7..4
         | (Vdp1Regs->TVMR & 0xF);         // TVMR bits 3..0 -> bits 3..0
```

| Bits | Source | Content |
| --- | --- | --- |
| 15–12 | constant `0x1` | Version. `Vdp1Reset` also seeds `Vdp1Regs->MODR = 0x1000` with the comment `// VDP1 Version 1` (`vdp1.cpp:384`), but that stored field is never returned — the read handler always recomputes |
| 11–9 | always 0 | Not produced by the expression |
| 8 | PTMR bit 1 | PTM1 |
| 7 | FBCR bit 4 | EOS |
| 6 | FBCR bit 3 | DIE |
| 5 | FBCR bit 2 | DIL |
| 4 | FBCR bit 1 | FCM |
| 3 | TVMR bit 3 | VBE |
| 2 | TVMR bit 2 | TVM2 |
| 1 | TVMR bit 1 | TVM1 |
| 0 | TVMR bit 0 | TVM0 |

FBCR bit 0 (FCT) and PTMR bit 0 (PTM0) are **not** visible in MODR.

### 2.4 Internal engine state (in the same struct, not CPU-addressable)

`vdp1.h:67-81` puts the following alongside the registers. These are engine state, reachable only through commands or internal logic:

| Field | Type | Meaning | Set by |
| --- | --- | --- | --- |
| `addr` | `u32` | Byte address of the command table currently being processed | Reset to 0 at draw start (`vdp1.cpp:815`, `vdp2.cpp:1249`); advanced by the jump logic (`vdp1.cpp:637-677`) |
| `localX`, `localY` | `s16` | Local coordinate origin added to every vertex | Local Coordinate command (`vidsoft.c:3430-3434`) |
| `systemclipX1/Y1/X2/Y2` | `u16` | System clipping rectangle | System Clipping command (`vidsoft.c:3420-3426`) |
| `userclipX1/Y1/X2/Y2` | `u16` | User clipping rectangle | User Clipping command (`vidsoft.c:3410-3416`) |
| `disptoggle_dont_use_me` | `int` | Dead field; the live one is `Vdp1External.disptoggle` (`vdp1.h:68`) | — |

Non-saved external state lives in `Vdp1External_struct` (`vdp1.h:157-166`): `disptoggle`, `manualerase`, `manualchange`, `vbalnk_erase`, `frame_change_plot`, `swap_frame_buffer`, `current_frame`, `status`. `status` is `VDP1_STATUS_IDLE` (0) or `VDP1_STATUS_RUNNING` (1) (`vdp1.h:151-154`).

### 2.5 Reset

`Vdp1Reset()` (`vdp1.cpp:381-406`):

```c
memset(Vdp1Regs, 0, sizeof(Vdp1Regs));   // vdp1.cpp:382
Vdp1Regs->PTMR = 0;  Vdp1Regs->MODR = 0x1000;  Vdp1Regs->TVMR = 0;
Vdp1Regs->EWDR = 0;  Vdp1Regs->EWLR = 0;  Vdp1Regs->EWRR = 0;  Vdp1Regs->ENDR = 0;
VIDCore->Vdp1Reset();                     // vdp1.cpp:390
Vdp1Regs->userclipX1 = 0;   Vdp1Regs->userclipY1 = 0;
Vdp1Regs->userclipX2 = 1024; Vdp1Regs->userclipY2 = 1024;
Vdp1Regs->systemclipX1 = 0;  Vdp1Regs->systemclipY1 = 0;
Vdp1Regs->systemclipX2 = 1024; Vdp1Regs->systemclipY2 = 1024;
T1WriteWord(Vdp1Ram, 0x40000, 0x8000);    // vdp1.cpp:402
vdp1_clock = 0;
```

Three things worth flagging:

1. **`memset(Vdp1Regs, 0, sizeof(Vdp1Regs))` is an apparent bug.** `Vdp1Regs` is a `Vdp1 *`, so `sizeof` yields the pointer size (8 on 64-bit), not `sizeof(Vdp1)`. Only the first 8 bytes — `TVMR`, `FBCR`, `PTMR`, `EWDR` — are actually cleared. The explicit assignments that follow cover `EWLR`, `EWRR`, `ENDR`, but `EDSR`, `LOPR`, `COPR` and `addr` are **left with their pre-reset contents**. Do not treat this as hardware behaviour.
2. **Ordering:** `VIDCore->Vdp1Reset()` runs at line 390, *before* the clip assignments at 392–399. `VIDSoftVdp1Reset` sets the clip rectangles to `0,0,512,256` (`vidsoft.c:2376-2379`), and `vdp1.cpp` then overwrites them with `0,0,1024,1024`. The 1024×1024 values win. The two files disagree about the reset default; this is not resolvable from the source.
3. `T1WriteWord(Vdp1Ram, 0x40000, 0x8000)` writes a Draw End terminator at VRAM offset `0x40000`, commented `// Safe tarminator for Radient silvergun with no bios` (`vdp1.cpp:401`). A game-specific hack, not hardware.

---

## 3. Command table format

VDP1 draws by walking a linked list of command tables in VRAM. Each table is **16 words / 32 bytes** (`0x20`). `Vdp1ReadCommand` (`vdp1.cpp:859-876`) reads all 15 defined words with `T1ReadWord` after masking the base address with `0x7FFFF`:

| Word | Byte offset | Field | Type in `vdp1cmd_struct` (`vdp1.h:170-187`) | Read at |
| --- | --- | --- | --- | --- |
| 0 | `0x00` | **CMDCTRL** | `u16` | `vdp1.cpp:861` |
| 1 | `0x02` | **CMDLINK** | `u16` | `vdp1.cpp:862` |
| 2 | `0x04` | **CMDPMOD** | `u16` | `vdp1.cpp:863` |
| 3 | `0x06` | **CMDCOLR** | `u16` | `vdp1.cpp:864` |
| 4 | `0x08` | **CMDSRCA** | `u16` | `vdp1.cpp:865` |
| 5 | `0x0A` | **CMDSIZE** | `u16` | `vdp1.cpp:866` |
| 6 | `0x0C` | **CMDXA** | `s16` | `vdp1.cpp:867` |
| 7 | `0x0E` | **CMDYA** | `s16` | `vdp1.cpp:868` |
| 8 | `0x10` | **CMDXB** | `s16` | `vdp1.cpp:869` |
| 9 | `0x12` | **CMDYB** | `s16` | `vdp1.cpp:870` |
| 10 | `0x14` | **CMDXC** | `s16` | `vdp1.cpp:871` |
| 11 | `0x16` | **CMDYC** | `s16` | `vdp1.cpp:872` |
| 12 | `0x18` | **CMDXD** | `s16` | `vdp1.cpp:873` |
| 13 | `0x1A` | **CMDYD** | `s16` | `vdp1.cpp:874` |
| 14 | `0x1C` | **CMDGRDA** | `u16` | `vdp1.cpp:875` |
| 15 | `0x1E` | — | not read | reserved / unused |

The coordinate words are declared `s16` in the struct but assigned the result of `T1ReadWord` (a `u16`), so the conversion is an implementation-defined narrowing that in practice reinterprets the 16-bit pattern as two's-complement. **The code never sign-extends from a narrower field** — there is no masking to 11 or 12 bits anywhere. Whatever the hardware's real coordinate width is, this implementation treats all 16 bits as significant. `VIDSoftVdp1PolylineDraw` and `VIDSoftVdp1LineDraw` bypass `Vdp1ReadCommand` for coordinates entirely and re-read them with explicit `(s16)` casts (`vidsoft.c:3363-3370`, `vidsoft.c:3398-3401`), confirming the intent is signed 16-bit.

### 3.1 CMDCTRL — command control

| Bits | Field | Meaning | Source |
| --- | --- | --- | --- |
| 15 | **END** | 1 = Draw End. Terminates the whole command list | `vdp1.cpp:561`, `570`, `681`, `1018` |
| 14 | **JP skip** | 1 = skip this command's drawing (but still follow the jump) | `vdp1.cpp:584` `if (!(command & 0x4000))`, `vdp1.cpp:1076` |
| 13–12 | **JP** (jump select) | 0 = NEXT, 1 = ASSIGN, 2 = CALL, 3 = RETURN | `vdp1.cpp:637` `switch ((command & 0x3000) >> 12)` |
| 11–8 | **ZP** (zoom point) | Scaled-sprite anchor. See §5.2 | `vdp1.cpp:1095` `(cmd.CMDCTRL >> 8) & 0xF`, `vidsoft.c:3230` |
| 7–6 | — | Not read by any code path | — |
| 5–4 | **Dir** (character read direction) | 0 = normal, 1 = H flip, 2 = V flip, 3 = HV flip | `vdp1.cpp:1193` `(cmd.CMDCTRL >> 4) & 0x3`, `vidsoft.c:2536` `flip = (cmd->CMDCTRL & 0x30) >> 4` |
| 3–0 | **COMM** (command select) | Command opcode, see §4 | `vdp1.cpp:585` `switch (command & 0x000F)` |

Two derived tests appear in the debugger and are useful structural facts:

* `if (!(cmd.CMDCTRL & 0x000C))` — "Only Sprite commands use CMDSRCA, CMDSIZE" (`vdp1.cpp:1187`). True for COMM 0–3.
* `if (!(cmd.CMDCTRL & 0x0008))` — "Only draw commands use CMDPMOD" (`vdp1.cpp:1212`). True for COMM 0–7.
* `if ((command & 0x000C) == 0x000C)` — invalid command, abort (`vdp1.cpp:966`). True for COMM 12–15.

Note `getpixel` uses a **3-bit** mask when deciding textured vs untextured: `int currentShape = cmd->CMDCTRL & 0x7` (`vidsoft.c:2528`), then `if (currentShape == 4 || currentShape == 5 || currentShape == 6) isTextured = 0` (`vidsoft.c:2539-2542`). Consequence: **COMM 7 (the undocumented polyline mirror) is treated as textured**, because `7 & 0x7 == 7`, which is not in the untextured set. Whether that matches hardware is not determinable here. **[Ambiguous]**

### 3.2 CMDLINK — next command pointer

A word-pair index, not a byte address. Every use is `T1ReadWord(ram, regs->addr + 2) * 8` (`vdp1.cpp:642`, `655`, `738`, `744`, `977`, `983`). So the target byte address is `CMDLINK * 8`, giving 8-byte granularity over the 512 KiB VRAM (`0xFFFF * 8 = 0x7FFF8`, exactly covering VRAM). CMDLINK is only consulted for JP = ASSIGN (1) and JP = CALL (2).

### 3.3 CMDPMOD — draw mode

Bit assignments, cross-referenced between the debugger decoder (`vdp1.cpp:1212-1315`) and the renderer:

| Bits | Field | Meaning | Source |
| --- | --- | --- | --- |
| 15 | **MON** (MSB on) | Force the framebuffer MSB. `putpixel`: `if (cmd->CMDPMOD & (1 << 15)) { if (currentPixel) { *iPix \|= 0x8000; return; } }` — sets bit 15 of the *existing* framebuffer word and writes nothing else | `vdp1.cpp:1214`, `vidsoft.c:2764-2770` |
| 14–13 | — | Never read | — |
| 12 | **HSS** (high-speed shrink) | Recognised by the debugger only (`vdp1.cpp:1219-1222`, "High Speed Shrink Enabled"). **The software renderer ignores it entirely.** [Ambiguous] | `vdp1.cpp:1219` |
| 11 | **PCLP** (pre-clipping *disable*) | Debugger: `if (!(cmd.CMDPMOD & 0x0800)) "Pre-clipping Enabled"` — so 0 enables. **The software renderer never tests this bit**; `is_pre_clipped()` runs unconditionally at `vidsoft.c:3087` | `vdp1.cpp:1224-1227` |
| 10 | **CLIP** (user clipping enable) | 1 = apply the user clip rectangle | `vdp1.cpp:1229`, `vidsoft.c:2692` |
| 9 | **CMOD** (clipping mode) | With bit 10 set: 0 = draw *inside* the user rect, 1 = draw *outside*. Tested as `((cmd->CMDPMOD >> 9) & 0x3) == 0x3` — i.e. **both** bits 10 and 9 must be set for outside mode | `vdp1.cpp:1232`, `vidsoft.c:2696-2697` |
| 8 | **MESH** | 1 = mesh (checkerboard stipple). `if (mesh && (x^y)&1) return;` | `vdp1.cpp:1235`, `vidsoft.c:2745`, `2758-2759`, `2711`, `2722` |
| 7 | **ECD** (end code *disable*) | 0 = end codes enabled. `endcodesEnabled = ((cmd->CMDPMOD & 0x80) == 0) ? 1 : 0` | `vdp1.cpp:1240`, `vidsoft.c:2535` |
| 6 | **SPD** (transparent pixel *disable*) | 0 = transparent-pixel processing enabled; 1 = draw index/colour 0 opaquely. `SPD = ((cmd->CMDPMOD & 0x40) != 0)` | `vdp1.cpp:1245`, `vidsoft.c:2534`, `2712`, `2746` |
| 5–3 | **Colour mode** | 0–5, see §6 | `vdp1.cpp:1252` `(cmd.CMDPMOD >> 3) & 0x7`, `vidsoft.c:2561` |
| 2–0 | **Colour calculation** | 0–7, see §7 | `vdp1.cpp:1288` `cmd.CMDPMOD & 0x7`, `vidsoft.c:2731`, `2774` |

Colour-mode field values 6 and 7 have no `case` in either the debugger switch (`vdp1.cpp:1252-1284`, `default: break`) or `getpixel` (`vidsoft.c:2561-2631`, no default). In `getpixel` this means `currentPixel` and `currentPixelIsVisible` retain values from the *previous* pixel/command — undefined behaviour, not a documented mode.

### 3.4 CMDCOLR — colour control

Interpretation depends entirely on the colour mode (§6). Summary of every use:

| Colour mode | Use of CMDCOLR | Source |
| --- | --- | --- |
| 0 (4bpp bank) | Colour bank: `currentPixel = (colorbank & 0xfff0) \| currentPixel` | `vidsoft.c:2569` |
| 1 (4bpp LUT) | LUT base address: `colorlut = (u32)CMDCOLR << 3`, then `T1ReadWord(ram, (index*2 + colorlut) & 0x7FFFF)` | `vidsoft.c:2533`, `2579` |
| 2 (8bpp 64-colour) | `(colorbank & 0xffc0) \| currentPixel` | `vidsoft.c:2596` |
| 3 (8bpp 128-colour) | `(colorbank & 0xff80) \| currentPixel` — with the comment "dead or alive needs colorbank to be masked" | `vidsoft.c:2605` |
| 4 (8bpp 256-colour) | `(colorbank & 0xff00) \| currentPixel` | `vidsoft.c:2615` |
| 5 (16bpp RGB) | Not used for textured commands (pixel comes from VRAM) | — |
| any, untextured (COMM 4/5/6) | The literal pixel value: `untexturedColor = cmd->CMDCOLR`, later `currentPixel = untexturedColor` | `vidsoft.c:2541`, `2633-2634` |

The debugger decoder additionally prints `cmd.CMDCOLR << 3` as "Color bank"/"Color lookup table" for modes 0–4 (`vdp1.cpp:1256,1260,1264,1268,1272`), which conflicts with the renderer's non-shifted bank use in modes 0, 2, 3, 4. The renderer is the behavioural authority; the debugger's `<< 3` appears correct only for mode 1 (the LUT). **[Ambiguous]** — the two disagree.

### 3.5 CMDSRCA — character (texture) address

`characterAddress = cmd->CMDSRCA << 3` (`vidsoft.c:2531`); the debugger prints the same (`vdp1.cpp:1189`, `((unsigned int)cmd.CMDSRCA) << 3`). So CMDSRCA is a byte address divided by 8 — 8-byte granularity, covering all 512 KiB of VRAM. Only meaningful for COMM 0–3.

Row addressing within the character adds a per-mode row stride (all `vidsoft.c:2565-2619`):

| Colour mode | Row base expression | Bytes per row |
| --- | --- | --- |
| 0, 1 (4bpp) | `characterAddress + (linenumber * (characterWidth >> 1))` | `width / 2` |
| 2, 3, 4 (8bpp) | `characterAddress + (linenumber * characterWidth)` | `width` |
| 5 (16bpp) | `characterAddress + (linenumber * characterWidth * 2)` | `width * 2` |

Within a row, the pattern readers (`vidsoft.c:2460-2486`) take a pixel offset:

```c
Vdp1ReadPattern16 (base, off) = T1ReadByte(ram, (base + (off>>1)) & 0x7FFFF), high nibble if off even, low nibble if odd
Vdp1ReadPattern64 (base, off) = T1ReadByte(ram, (base + off) & 0x7FFFF) & 0x3F
Vdp1ReadPattern128(base, off) = T1ReadByte(ram, (base + off) & 0x7FFFF) & 0x7F
Vdp1ReadPattern256(base, off) = T1ReadByte(ram, (base + off) & 0x7FFFF) & 0xFF
Vdp1ReadPattern64k(base, off) = T1ReadWord(ram, (base + 2*off) & 0x7FFFF)
```

Note 4bpp is **high-nibble-first** (`if ((offset & 0x1) == 0) dot >>= 4; else dot &= 0xF;`, `vidsoft.c:2463-2464`).

### 3.6 CMDSIZE — character size

| Bits | Field | Extraction | Value |
| --- | --- | --- | --- |
| 15–14 | — | masked off | unused |
| 13–8 | Character width, in 8-pixel units | `((CMDSIZE >> 8) & 0x3F) * 8` | 0–504 pixels |
| 7–0 | Character height, in lines | `CMDSIZE & 0xFF` | 0–255 lines |

Sources: `vidsoft.c:3090-3091` (`characterWidth`/`characterHeight` used by `getpixel`), `vidsoft.c:3207-3208` (`spriteWidth`/`spriteHeight` used by Normal Sprite), `vdp1.cpp:1190` and `vdp1.cpp:1436-1437` (debugger, expressed equivalently as `(CMDSIZE & 0x3F00) >> 5`).

### 3.7 CMDXA/YA … CMDXD/YD — vertices

Which vertices are used depends on the command; see §5. All are offset by `regs->localX` / `regs->localY` before rasterisation.

### 3.8 CMDGRDA — gouraud shading table address

`gouraudTableAddress = ((unsigned int)cmd->CMDGRDA) << 3` (`vidsoft.c:3004`; debugger agrees at `vdp1.cpp:1304`). Four consecutive 16-bit words are read (`vidsoft.c:3006-3009`):

| Offset from table base | Variable | Corner |
| --- | --- | --- |
| `+0` | `gouraudA` | A |
| `+2` | `gouraudB` | B |
| `+4` | `gouraudC` | C |
| `+6` | `gouraudD` | D |

Each word is an XBGR-1555 value unpacked by the `COLOR` union (`vidsoft.c:2976-2991`): bits 4–0 = r, 9–5 = g, 14–10 = b, 15 = x. The table is only fetched when colour-calculation bit 2 is set (`vidsoft.c:3105` `if (cmd->CMDPMOD & (1 << 2))`), i.e. colour-calc modes 4–7.

Note the gouraud table read at `vidsoft.c:3006-3009` uses **no `& 0x7FFFF` mask** — unlike every other VRAM access in the file. With a maximal `CMDGRDA` of `0xFFFF` the base is `0x7FFF8` and the last word read is at `0x7FFFE`, so it happens to stay in range; the missing mask is nonetheless inconsistent with the rest of the code.

---

## 4. Command list walking and dispatch

The walker is `Vdp1DrawCommands` (`vdp1.cpp:550-690`).

### 4.1 Entry conditions

```c
regs->COPR = regs->addr >> 3;                                  // vdp1.cpp:554
if (regs->addr > 0x7FFFF) { status = IDLE; return; }            // vdp1.cpp:555-559  address error
u16 command = T1ReadWord(ram, regs->addr);                      // vdp1.cpp:560
if (command & 0x8000) { status = IDLE; return; }                // vdp1.cpp:561-565  immediate finish
```

An address above `0x7FFFF` aborts silently with no error flag set. A Draw End as the very first command also returns immediately with no flags set.

### 4.2 The loop

```c
while (!(command & 0x8000) && command_count < 4096)   // vdp1.cpp:570
```

The `4096` iteration cap is marked `// fix me` in the source (`vdp1.cpp:570`) — it is an emulator infinite-loop guard, not hardware. Falling out of the loop by exhausting the cap leaves `status == VDP1_STATUS_RUNNING` and only logs (`vdp1.cpp:687-689`), so the draw is silently truncated and, crucially, **no Draw End is signalled** (§9). The parallel `Vdp1FakeDrawCommands` uses a cap of 2000, also `// fix me` (`vdp1.cpp:699`).

Per iteration:

1. `regs->COPR = regs->addr >> 3` (`vdp1.cpp:571`).
2. If bit 14 (skip) is clear, dispatch on `command & 0x000F` (`vdp1.cpp:584-624`).
3. Check the "internal command error" escape: if `regs->EDSR & 0x02` is now set, write LOPR and COPR, set status IDLE, and return (`vdp1.cpp:628-634`). Comment: "Force to quit internal command error( This technic(?) is used by BATSUGUN )".
4. Advance `regs->addr` per the JP field (§4.4).
5. `command = T1ReadWord(ram, regs->addr & 0x7FFFF)` (`vdp1.cpp:679`). Note the mask here, absent from the read at step 1's equivalent inside the loop.
6. If the new command has bit 15 set, set `status = IDLE` (`vdp1.cpp:681-684`) — the loop condition then exits.

There is a disabled clock-budget block at `vdp1.cpp:573-582` (`#if 0`) that would have consumed 10 units of `vdp1_clock` per command and stalled/returned when exhausted. **In the shipped build, the entire command list executes atomically in zero emulated time.** Any cycle-accurate VDP1 timing model must be built from scratch; this source has none.

### 4.3 Dispatch table

`vdp1.cpp:585-624`:

| `CMDCTRL & 0x0F` | Name (`vdp1.cpp:1022-1050`) | Dispatched to | Line |
| --- | --- | --- | --- |
| 0 | Normal Sprite | `VIDCore->Vdp1NormalSpriteDraw` | `vdp1.cpp:587` |
| 1 | Scaled Sprite | `VIDCore->Vdp1ScaledSpriteDraw` | `vdp1.cpp:590` |
| 2 | Distorted Sprite | `VIDCore->Vdp1DistortedSpriteDraw` | `vdp1.cpp:595` |
| 3 | Distorted Sprite * | `VIDCore->Vdp1DistortedSpriteDraw` | `vdp1.cpp:595` — falls through from 2. Comment: "this one should be invalid, but some games (Hardcore 4x4 for instance) use it instead of 2" (`vdp1.cpp:593-594`) |
| 4 | Polygon | `VIDCore->Vdp1PolygonDraw` | `vdp1.cpp:598` |
| 5 | Polyline | `VIDCore->Vdp1PolylineDraw` | `vdp1.cpp:602` |
| 6 | Line | `VIDCore->Vdp1LineDraw` | `vdp1.cpp:605` |
| 7 | Polyline * | `VIDCore->Vdp1PolylineDraw` | `vdp1.cpp:602` — "undocumented mirror" (`vdp1.cpp:601`) |
| 8 | User Clipping Coordinates | `VIDCore->Vdp1UserClipping` | `vdp1.cpp:609` |
| 9 | System Clipping Coordinates | `VIDCore->Vdp1SystemClipping` | `vdp1.cpp:612` |
| 10 | Local Coordinates | `VIDCore->Vdp1LocalCoordinate` | `vdp1.cpp:615` |
| 11 | User Clipping Coordinates * | `VIDCore->Vdp1UserClipping` | `vdp1.cpp:609` — "undocumented mirror" (`vdp1.cpp:608`) |
| 12–15 | Bad command | abort: `EDSR \|= 2; LOPR = addr>>3; COPR = addr>>3; status = IDLE; return;` | `vdp1.cpp:617-623` |

In the software renderer, **Polygon (4) is bound to the same function as Distorted Sprite (2)**. `VIDSoftVdp1PolygonDraw` is declared (`vidsoft.c:86`) but never defined; the `VIDSoft` vtable slot contains `VIDSoftVdp1DistortedSpriteDraw` (`vidsoft.c:126`), preceded by this comment (`vidsoft.c:122-125`):

> "for the actual hardware, polygons are essentially identical to distorted sprites / the actual hardware draws using diagonal lines, which is why using half-transparent processing on distorted sprites and polygons is not recommended since the hardware overdraws to prevent gaps / thus, with half-transparent processing some pixels will be processed more than once, producing moire patterns in the drawn shapes"

The behavioural difference between the two commands therefore comes entirely from `getpixel`'s `currentShape` test (§3.1): COMM 4 is untextured (uses CMDCOLR as a flat colour), COMM 2 is textured.

### 4.4 Jump / link processing

`vdp1.cpp:637-677`. A single-level return address (`returnAddr`, initialised `0xFFFFFFFF` at `vdp1.cpp:568`) is kept — **there is no call stack**.

| JP (`(CMDCTRL >> 12) & 3`) | Name | Action |
| --- | --- | --- |
| 0 | NEXT | `regs->addr += 0x20` (`vdp1.cpp:639`) |
| 1 | ASSIGN | `regs->addr = T1ReadWord(ram, regs->addr + 2) * 8` (`vdp1.cpp:642`) |
| 2 | CALL | `if (returnAddr == 0xFFFFFFFF) returnAddr = regs->addr + 0x20;` then jump to CMDLINK (`vdp1.cpp:653-655`). A nested CALL while a return address is already pending **does not** overwrite it — the inner return address is lost |
| 3 | RETURN | If `returnAddr != 0xFFFFFFFF`, `regs->addr = returnAddr; returnAddr = 0xFFFFFFFF;` else `regs->addr += 0x20` (`vdp1.cpp:664-669`). A RETURN with no pending call behaves as NEXT |

For JP 1, 2 and 3, a resulting `regs->addr == 0` triggers an abort: `status = IDLE; return;` with the log "VDP1: BAD jump to 0, forced to finish" (`vdp1.cpp:644-649`, `656-661`, `670-675`). This is an emulator infinite-loop guard — address 0 is a perfectly legal command table address in general (drawing always *starts* at 0, `vdp1.cpp:815`), so this is **not** hardware behaviour. NEXT has no such check.

### 4.5 `Vdp1FakeDrawCommands`

`vdp1.cpp:693-759`, described as "ensure that registers are set correctly". It walks the same list with identical jump logic but only executes the three *state-setting* commands (User Clipping, System Clipping, Local Coordinates) and skips all drawing (`vdp1.cpp:703-712` — all draw opcodes fall through to an empty `break`). Used in two situations:

* `Vdp1NoDraw()` when display is toggled off (`vdp1.cpp:850`).
* `VIDSoftVdp1DrawStart` when the renderer's VDP1 worker thread is enabled: the thread gets a snapshot of VRAM and registers, and the main thread runs `Vdp1FakeDrawCommands` against the *real* registers so that `Vdp1Regs`' clip/local-coordinate state stays current for the CPU to read back (`vidsoft.c:2427-2444`).

`Vdp1FakeDrawCommands` has **no** address-0 guards and **no** `& 0x7FFFF` mask on its command reads (`vdp1.cpp:756`), unlike the real walker.

### 4.6 Draw start

`Vdp1Draw()` (`vdp1.cpp:797-838`):

```c
if (!Vdp1External.disptoggle) { Vdp1NoDraw(); return; }     // vdp1.cpp:808-812
if (Vdp1External.status == VDP1_STATUS_IDLE) {
    Vdp1Regs->addr = 0;                                      // vdp1.cpp:815
    Vdp1Regs->COPR = 0;                                      // vdp1.cpp:821
}
Vdp1External.status = VDP1_STATUS_RUNNING;                   // vdp1.cpp:825
VIDCore->Vdp1DrawStart();                                    // vdp1.cpp:826
```

Drawing always begins at VRAM address 0 — **the command list root is fixed at VDP1 VRAM offset 0**; there is no register that points at it. If the engine is already `RUNNING`, `addr` is left where it was, so the draw *resumes*.

`VIDSoftVdp1DrawStart` (`vidsoft.c:2425-2450`) then does the geometry/erase setup (`VIDSoftVdp1DrawStartBody`, §8) and calls `Vdp1DrawCommands` synchronously.

`Vdp1NoDraw()` (`vdp1.cpp:842-855`) is the display-off path: reset COPR to 0, run `Vdp1FakeDrawCommands`, then `Vdp1Regs->EDSR |= 2; ScuSendDrawEnd();` — i.e. it signals completion immediately and unconditionally.

---

## 5. Draw commands, algorithm by algorithm

All drawing routines cited in this section are in `vidsoft.c`.

### 5.0 The shared rasteriser

Every filled shape goes through `drawQuad` (`vidsoft.c:3070-3195`). The approach is explicitly *not* scanline-based; the header comment (`vidsoft.c:3066-3069`) states:

> "a real vdp1 draws with arbitrary lines / this is why endcodes are possible / this is also the reason why half-transparent shading causes moire patterns / and the reason why gouraud shading can be applied to a single line draw command"

`drawQuad(tl, bl, tr, br)` — note the argument order is top-left, **bottom-left**, top-right, bottom-right:

1. **Pre-clip.** `is_pre_clipped()` (`vidsoft.c:3028-3064`) rejects the quad if all four X are `< 0`, all `> systemclipX2`, all Y `< 0`, or all Y `> systemclipY2` (doubled when interlaced). Runs unconditionally — CMDPMOD bit 11 is not consulted (`vidsoft.c:3087`).
2. **Character size.** `characterWidth = ((CMDSIZE >> 8) & 0x3F) * 8; characterHeight = CMDSIZE & 0xFF` into file-scope globals read by `getpixel` (`vidsoft.c:3090-3091`).
3. **Edge walk.** The left edge (tl→bl) and right edge (tr→br) are each walked by `iterateOverLine` with `greedy = 0` and the `storeLineCoords` callback, filling the fixed 1000-entry arrays `xleft/yleft` and `xright/yright` (`vidsoft.c:3012-3015`, `3093-3096`). The returned lengths are `totalleft` and `totalright`; `total = max(totalleft, totalright)`.
4. **Step normalisation.** The shorter edge is sub-sampled: if left is longer, `leftLineStep = 1, rightLineStep = totalright/totalleft`, else the reverse (`vidsoft.c:3122-3131`). Comment: "we have to step the equivalent of less than one pixel on the shorter side to make sure textures stretch properly and the shape is correct".
5. **Span loop.** For `i` in `[0, total)`, connect `(xleft[i*leftLineStep], yleft[...])` to `(xright[i*rightLineStep], yright[...])`:
   * Measure the span length first with a dry `iterateOverLine(..., greedy=1, NULL, NULL)` (`vidsoft.c:3145-3150`).
   * `xtexturestep = interpolate(0, characterWidth, xlinelength)` (`vidsoft.c:3153`).
   * `ytexturestep = interpolate(0, characterHeight, total)` (`vidsoft.c:3156`) — computed once per quad, then the row used is `ytexturestep * i`.
   * Draw the span with `DrawLine(..., greedy=1, linenumber = ytexturestep*i, texturestep = xtexturestep, ...)` (`vidsoft.c:3179-3193`).

   `interpolate(start, end, n)` = `(end - start) / n`, returning **1** when `n == 0` (`vidsoft.c:2964-2974`).

So texture mapping is **affine per-span, with the source row taken as a linear function of the span index and the source column as a linear function of the position along the span**. There is no perspective correction and no bilinear filtering. The texture V coordinate is a function of `i` (the edge-walk step index), not of screen Y.

**`iterateOverLine`** (`vidsoft.c:2831-2911`) is a Bresenham variant:

* `dx = x2-x1`, `dy = y2-y1`, `ax = sign(dx)`, `ay = sign(dy)`.
* Bails out returning `INT_MAX` if `abs(dx) > 999 || abs(dy) > 999` (`vidsoft.c:2843-2844`), commented "burning rangers tries to draw huge shapes / this will at least let it run". This is a hard cap imposed by the 1000-entry coordinate arrays, **not** hardware. `drawQuad` discards the whole quad if either edge returns `INT_MAX` (`vidsoft.c:3099-3100`).
* Major axis chosen by `abs(dx) > abs(dy)`. Error accumulator `a` stepped by the minor delta; on overflow the minor coordinate advances.
* **Greedy mode** (`greedy != 0`) emits an *extra* pixel each time the minor axis steps, at `(x1+ax, y1-ay)` when `ax == ay` and at `(x1, y1)` otherwise (`vidsoft.c:2857-2869`, mirrored for the y-major case at `vidsoft.c:2889-2900`). Comment: "Make sure we 'fill holes' the same as the Saturn" and "If the line isn't greedy here, we end up with gaps that don't occur on the Saturn". Span fills use greedy=1; edge walks use greedy=0.
* The endpoint `(x2,y2)` is always emitted at the end (`vidsoft.c:2874-2877`) — note the guard condition is hard-coded to `1` with the original condition commented out.
* The callback returning non-zero aborts the line early, returning `i+1`.

**`DrawLine`** (`vidsoft.c:2949-2962`) wraps `iterateOverLine` with `DrawLineCallback` (`vidsoft.c:2923-2947`), which per pixel:

1. Advances the gouraud accumulator: `leftColumnColor.{r,g,b} += linedata->{xredstep,xgreenstep,xbluestep}`.
2. `currentStep = (int)i * texturestep` — the texture column.
3. Calls `getpixel(linenumber, currentStep, cmd, ram)`. If it returns 1 (end code hit), increments `endcodesdetected` — but only when `currentStep` differs from the previous step, so a magnified texture does not count the same source pixel twice.
4. Otherwise writes via `putpixel` (16-bit framebuffer) or `putpixel8` (8-bit).
5. Returns `-1` once `endcodesdetected == 2`, aborting the span.

Note the gouraud step is applied **before** the first pixel is drawn, so the first pixel of a span already carries one step of offset.

### 5.1 COMM 0 — Normal Sprite

`VIDSoftVdp1NormalSpriteDraw` (`vidsoft.c:3197-3218`).

```
topLeft     = (CMDXA + localX,                    CMDYA + localY)
spriteWidth  = ((CMDSIZE >> 8) & 0x3F) * 8
spriteHeight = CMDSIZE & 0xFF
topRight    = (topLeftx + spriteWidth - 1,  topLefty)
bottomRight = (topLeftx + spriteWidth - 1,  topLefty + spriteHeight - 1)
bottomLeft  = (topLeftx,                    topLefty + spriteHeight - 1)
drawQuad(topLeft, bottomLeft, topRight, bottomRight)
```

Only CMDXA/CMDYA are used; B, C, D are ignored. The sprite is drawn 1:1 with no scaling, as an axis-aligned rectangle, but still through the general quad rasteriser. Coordinates are `s16`.

### 5.2 COMM 1 — Scaled Sprite

`VIDSoftVdp1ScaledSpriteDraw` (`vidsoft.c:3220-3316`). `x0,y0 = CMDXA + localX, CMDYA + localY`, then the ZP field `(CMDCTRL >> 8) & 0xF` selects how the extent is derived:

| ZP | Anchor | Computation (`vidsoft.c:3230-3301`) |
| --- | --- | --- |
| `0x0` (and `default`) | Two-point mode | `x1 = CMDXC - x0 + localX + 1; y1 = CMDYC - y0 + localY + 1` — the opposite corner is given by C |
| `0x5` | Upper-left | `x1 = CMDXB + 1; y1 = CMDYB + 1` |
| `0x6` | Upper-center | `x1 = CMDXB; y1 = CMDYB; x0 -= x1/2; x1++; y1++` |
| `0x7` | Upper-right | `x0 -= x1;` then `x1++, y1++` |
| `0x9` | Center-left | `y0 -= y1/2;` then `x1++, y1++` |
| `0xA` | Center-center | `x0 -= x1/2; y0 -= y1/2;` then `x1++, y1++` |
| `0xB` | Center-right | `x0 -= x1; y0 -= y1/2;` then `x1++, y1++` |
| `0xD` | Lower-left | `y0 -= y1;` then `x1++, y1++` |
| `0xE` | Lower-center | `x0 -= x1/2; y0 -= y1;` then `x1++, y1++` |
| `0xF` | Lower-right | `x0 -= x1; y0 -= y1;` then `x1++, y1++` |

For every non-zero ZP, `x1/y1` start as `CMDXB/CMDYB` (the *size*, not a corner).

The implemented value set decomposes cleanly as two 2-bit sub-fields — bits 11–10 = vertical anchor (`01` upper, `10` center, `11` lower) and bits 9–8 = horizontal anchor (`01` left, `10` center, `11` right) — which is consistent with all nine non-zero cases. Values `0x1`–`0x4`, `0x8` and `0xC` are unimplemented and fall to `default:` (two-point mode).

**Discrepancy:** the debugger's ZP decoder (`vdp1.cpp:1095-1128`) lists `0xC` as "Lower-left" and has no `0xD` case, while the renderer implements `0xD` as lower-left and has no `0xC` case. Only one can be right; the sub-field decomposition above favours the renderer. **[Ambiguous]** — the source contradicts itself.

Final quad (`vidsoft.c:3303-3315`): `topLeft = (x0, y0)`, `topRight = (x0+x1-1, y0)`, `bottomRight = (x0+x1-1, y0+y1-1)`, `bottomLeft = (x0, y0+y1-1)`. Note this is computed in `s32`, unlike Normal Sprite's `s16`.

### 5.3 COMM 2 / COMM 3 — Distorted Sprite

`VIDSoftVdp1DistortedSpriteDraw` (`vidsoft.c:3318-3338`). All four vertices are used, each offset by the local coordinate and widened to `s32`:

```
xa,ya = CMDXA+localX, CMDYA+localY
xb,yb = CMDXB+localX, CMDYB+localY
xc,yc = CMDXC+localX, CMDYC+localY
xd,yd = CMDXD+localX, CMDYD+localY
drawQuad(A, D, B, C)                              // vidsoft.c:3337
```

Mapping `drawQuad(tl, bl, tr, br)` onto that call: **A is top-left, D is bottom-left, B is top-right, C is bottom-right.** So the left edge is A→D and the right edge is B→C, and the vertex order around the quad is A, B, C, D. The texture is mapped affinely across the resulting spans (§5.0).

### 5.4 COMM 4 — Polygon

Dispatched to `VIDSoftVdp1DistortedSpriteDraw` (`vidsoft.c:126`), so the geometry is byte-for-byte identical to §5.3. The only difference is inside `getpixel`: `currentShape == 4` sets `isTextured = 0` and `untexturedColor = CMDCOLR` (`vidsoft.c:2539-2542`), so after the colour-mode switch runs, `currentPixel` is overwritten with `CMDCOLR` (`vidsoft.c:2633-2634`).

Consequences worth noting: the colour-mode switch **still executes** for polygons — it reads texture data from `CMDSRCA` and can still hit an end code and abort the span (the `isTextured &&` guard prevents the end-code *return* for modes 0,1,3,4,5, but mode 2 unconditionally forces `currentPixel = 0` on endcode match, and all modes still perform the VRAM read). `currentPixelIsVisible` is also set by the colour mode and is used by the visibility test in `putpixel` (§7). For an untextured polygon this means both the mask and any VRAM traffic depend on CMDPMOD's colour mode field. This looks like an artefact of code sharing rather than intended hardware behaviour. **[Ambiguous]**

### 5.5 COMM 5 / COMM 7 — Polyline

`VIDSoftVdp1PolylineDraw` (`vidsoft.c:3353-3387`). Reads the four vertices directly from VRAM with explicit signed casts rather than from the `vdp1cmd_struct`:

```
X[0],Y[0] = localX + (s16)[addr+0x0C], localY + (s16)[addr+0x0E]   // A
X[1],Y[1] = localX + (s16)[addr+0x10], localY + (s16)[addr+0x12]   // B
X[2],Y[2] = localX + (s16)[addr+0x14], localY + (s16)[addr+0x16]   // C
X[3],Y[3] = localX + (s16)[addr+0x18], localY + (s16)[addr+0x1A]   // D
```

Four edges are drawn, each preceded by a length measurement (dry `iterateOverLine` with `greedy=1`) and a gouraud setup, then drawn with `greedy = 0`, `linenumber = 0`, `texturestep = 0`:

| Edge | Measured | Gouraud endpoints | `DrawLine` call | Line |
| --- | --- | --- | --- | --- |
| A→B | `A→B` | `gouraudA, gouraudB` | `DrawLine(X0,Y0, X1,Y1, ...)` | `vidsoft.c:3372-3374` |
| B→C | `B→C` | `gouraudB, gouraudC` | `DrawLine(X1,Y1, X2,Y2, ...)` | `vidsoft.c:3376-3378` |
| C→D | `C→D` | `gouraudD, gouraudC` | `DrawLine(X3,Y3, X2,Y2, ...)` — **drawn D→C** | `vidsoft.c:3380-3382` |
| D→A | `D→A` | `gouraudA, gouraudD` | `DrawLine(X0,Y0, X3,Y3, ...)` — **drawn A→D** | `vidsoft.c:3384-3386` |

The last two edges are measured in one direction and drawn in the opposite one, so that the gouraud interpolation runs from the correct corner. Because `texturestep = 0`, `currentStep` is always 0 and `getpixel` always samples texture column 0.

`gouraudLineSetup` (`vidsoft.c:3340-3351`) fetches the table, computes the per-pixel r/g/b steps over the measured length, and seeds `leftColumnColor` with the start corner's colour. Note that it calls `gouraudTable()` **unconditionally** — polylines and lines fetch the table regardless of whether the colour-calculation mode has bit 2 set (unlike `drawQuad`, which gates it).

### 5.6 COMM 6 — Line

`VIDSoftVdp1LineDraw` (`vidsoft.c:3389-3406`). Two vertices, read the same way:

```
x1,y1 = localX + (s16)[addr+0x0C], localY + (s16)[addr+0x0E]   // A
x2,y2 = localX + (s16)[addr+0x10], localY + (s16)[addr+0x12]   // B
length = iterateOverLine(x1,y1,x2,y2, greedy=1, NULL, NULL)
gouraudLineSetup(&redstep, &bluestep, &greenstep, length, gouraudA, gouraudB, ...)
DrawLine(x1,y1, x2,y2, greedy=0, 0, 0, redstep, greenstep, bluestep, ...)
```

**Note the argument order at `vidsoft.c:3404`:** `gouraudLineSetup(&redstep, &bluestep, &greenstep, ...)` — the second and third out-parameters are swapped relative to the function's signature (`double *redstep, double *greenstep, double *bluestep`, `vidsoft.c:3340`) and relative to how the polyline path calls it (`vidsoft.c:3373`). The green step is therefore written into `bluestep` and vice versa, and the subsequent `DrawLine` passes them in declaration order. This is an apparent bug producing swapped green/blue gouraud gradients on single-line commands. It is not hardware behaviour.

Colour comes from CMDCOLR (untextured, since `currentShape == 6`).

### 5.7 COMM 8 / COMM 11 — User Clipping Coordinates

`VIDSoftVdp1UserClipping` (`vidsoft.c:3410-3416`):

```c
regs->userclipX1 = T1ReadWord(ram, regs->addr + 0xC);   // CMDXA
regs->userclipY1 = T1ReadWord(ram, regs->addr + 0xE);   // CMDYA
regs->userclipX2 = T1ReadWord(ram, regs->addr + 0x14);  // CMDXC
regs->userclipY2 = T1ReadWord(ram, regs->addr + 0x16);  // CMDYC
```

CMDXB/CMDYB and CMDXD/CMDYD are unused. Note the targets are `u16` (`vdp1.h:78-81`) — the values are stored **unsigned**, and `localX/localY` are **not** applied. This command draws nothing.

### 5.8 COMM 9 — System Clipping Coordinates

`VIDSoftVdp1SystemClipping` (`vidsoft.c:3420-3426`):

```c
regs->systemclipX1 = 0;
regs->systemclipY1 = 0;
regs->systemclipX2 = T1ReadWord(ram, regs->addr + 0x14);  // CMDXC
regs->systemclipY2 = T1ReadWord(ram, regs->addr + 0x16);  // CMDYC
```

The upper-left corner is **hard-wired to (0,0)** — the system clip rectangle is always anchored at the origin, and CMDXA/CMDYA are ignored. The debugger prints this explicitly: `"x1 = 0, y1 = 0, x2 = %d, y2 = %d"` (`vdp1.cpp:1175`). Draws nothing.

### 5.9 COMM 10 — Local Coordinates

`VIDSoftVdp1LocalCoordinate` (`vidsoft.c:3430-3434`):

```c
regs->localX = T1ReadWord(ram, regs->addr + 0xC);   // CMDXA
regs->localY = T1ReadWord(ram, regs->addr + 0xE);   // CMDYA
```

Targets are `s16` (`vdp1.h:70-71`), so the value is reinterpreted as signed. This offset is added to every vertex of every subsequent draw command until changed. Draws nothing.

---

## 6. Colour modes and pixel decoding

`CMDPMOD` bits 5–3 select the colour mode. The authoritative implementation is `getpixel` (`vidsoft.c:2561-2631`); a second, independent decode exists in the debugger's `Vdp1DebugTexture` (`vdp1.cpp:1480-1682`), which resolves indices through VDP2 colour RAM. The two answer different questions:

* **`getpixel`** produces the value that is *written into the VDP1 framebuffer*. For indexed modes that value is a **colour-RAM index**, not an RGB colour — VDP1's framebuffer stores indices, and VDP2 resolves them at readout time (§10).
* **`Vdp1DebugTexture`** resolves indices to ARGB immediately, for the debugger's texture viewer. Use it to understand the palette path, not the framebuffer contents.

| Mode | Name (`vdp1.cpp:1254-1282`) | Fetch | Transparent test | Value written to framebuffer | `currentPixelIsVisible` |
| --- | --- | --- | --- | --- | --- |
| 0 | 4 BPP, 16-colour bank | `Vdp1ReadPattern16` | index `== 0` and `!SPD` | `(CMDCOLR & 0xFFF0) \| index` | `0xF` |
| 1 | 4 BPP, 16-colour LUT | `Vdp1ReadPattern16`, then `T1ReadWord(ram, (index*2 + (CMDCOLR<<3)) & 0x7FFFF)` | index `== 0` and `!SPD` | the 16-bit LUT entry, verbatim | `0xFFFF` |
| 2 | 8 BPP, 64-colour bank | `Vdp1ReadPattern64` (`& 0x3F`) | index `== 0` and `!SPD` | `(CMDCOLR & 0xFFC0) \| index` | `0x3F` |
| 3 | 8 BPP, 128-colour bank | `Vdp1ReadPattern128` (`& 0x7F`) | index `== 0` and `!SPD` | `(CMDCOLR & 0xFF80) \| index` | `0x7F` |
| 4 | 8 BPP, 256-colour bank | `Vdp1ReadPattern256` (`& 0xFF`) | index `== 0` and `!SPD` | `(CMDCOLR & 0xFF00) \| index` | `0xFF` |
| 5 | 16 BPP RGB | `Vdp1ReadPattern64k` (word) | `!(pixel & 0x8000)` and `!SPD` → forced to 0 | the 16-bit word verbatim | `0xFFFF` |
| 6, 7 | — | **no case** | — | `currentPixel` retains its previous value | retains previous value |

Precise transparent-pixel semantics in `getpixel`: for modes 0–4 the guard is `if (!((currentPixel == 0) && !SPD)) currentPixel = (colorbank & mask) | currentPixel;` — that is, **when the pixel is transparent the colour bank is not OR'd in**, leaving `currentPixel == 0`. The actual suppression of the write then happens in `putpixel`.

Mode 5's transparency rule is different and carries an explanatory comment (`vidsoft.c:2623-2627`):

> "the transparent pixel in 16bpp is supposed to be 0x0000 but some games use pixels with invalid values and expect them to be transparent (see vdp1 doc p. 92)"

so the implemented test is `if (!(currentPixel & 0x8000) && !SPD) currentPixel = 0;` — *any* word without bit 15 set is treated as transparent, not just `0x0000`.

### 6.1 End codes

Enabled when `CMDPMOD` bit 7 is **clear** (`endcodesEnabled = ((CMDPMOD & 0x80) == 0)`, `vidsoft.c:2535`). Only applied when `isTextured`.

| Mode | End code value | Action on match |
| --- | --- | --- |
| 0 | `0xF` | `return 1` — signals end code to the caller (`vidsoft.c:2566-2567`) |
| 1 | `0xF` | `return 1` (`vidsoft.c:2576-2577`) |
| 2 | `63` | **`currentPixel = 0`** — treated as transparent instead of an end code. The `return 1` is commented out (`vidsoft.c:2592-2594`) |
| 3 | `0xFF` | `return 1` (`vidsoft.c:2602-2603`) |
| 4 | `0xFF` | `return 1` (`vidsoft.c:2611-2612`) |
| 5 | `0x7FFF` | `return 1` (`vidsoft.c:2620-2621`) |

Mode 3's end code is `0xFF` even though `Vdp1ReadPattern128` masks the fetch to `0x7F`, so the comparison can never succeed — mode 3 end codes are effectively dead. Mode 2 carries a long comment admitting uncertainty (`vidsoft.c:2583-2588`):

> "is there a hardware bug with endcodes in this color mode? there are white lines around some characters in scud / using an endcode of 63 eliminates the white lines / but also causes some dropout due to endcodes being triggered that aren't triggered on hardware / the closest thing i can do to match the hardware is make all pixels with color index 63 transparent / this needs more hardware testing"

**[Ambiguous]** — end-code behaviour in the 8bpp modes is explicitly not settled in this source.

Two end codes in a span terminate that span (`DrawLineCallback` returns `-1` at `vidsoft.c:2944`). The debugger has an independent implementation with different semantics: `CheckEndcode`/`DoEndcode` (`vdp1.cpp:1362-1396`), where the *first* end code writes one transparent pixel and the *second* fills the remainder of the row transparent and advances the character address past it.

### 6.2 Character read direction (flip)

Applied in `getpixel` before any fetch, using the globals `characterWidth`/`characterHeight` set by `drawQuad` (`vidsoft.c:2544-2559`):

| `(CMDCTRL >> 4) & 3` | Transform |
| --- | --- |
| 0 | none |
| 1 | `currentlineindex = characterWidth - currentlineindex - 1` (horizontal) |
| 2 | `linenumber = characterHeight - linenumber - 1` (vertical) |
| 3 | both |

### 6.3 Colour RAM resolution (debugger path)

`ColorRamGetColor` (`vdp1.cpp:1333-1358`) shows how an index becomes a colour, switching on `Vdp2Internal.ColorMode`:

| ColorMode | Behaviour |
| --- | --- |
| 0, 1 | `index <<= 1; tmp = T2ReadWord(Vdp2ColorRam, index & 0xFFF); return SAT2YAB1(0xFF, tmp)` — 16-bit palette entries, 4 KiB window |
| 2 | `index <<= 2; index &= 0xFFF; tmp1 = word(index); tmp2 = word(index+2); return SAT2YAB2(0xFF, tmp1, tmp2)` — 32-bit palette entries |

`SAT2YAB1` (`vdp1.cpp:1321-1325`) expands XBGR-1555 to ARGB-8888 by shifting each 5-bit channel up by 3: little-endian form `alpha<<24 | (t&0x1F)<<3 | (t&0x3E0)<<6 | (t&0x7C00)<<9`. `SAT2YAB2` (`vdp1.cpp:1327-1331`) packs two words of a 32-bit colour RAM entry.

The debugger's bank-mode index calculation adds a VDP2 colour offset: `colorOffset = (Vdp2Regs->CRAOFB & 0x70) << 4`, and the index becomes `(dot | colorBank) + colorOffset` (`vdp1.cpp:1486`, `1506`). The same `CRAOFB` offset is applied by the real VDP2 readout path (`vidsoft.c:3592`, `vidsoft.c:3720`).

---

## 7. Colour calculation and pixel write

`CMDPMOD` bits 2–0. Implemented in `putpixel` for 16-bit framebuffers (`vidsoft.c:2742-2829`) and `putpixel8` for 8-bit (`vidsoft.c:2707-2740`).

### 7.1 `putpixel` (16-bit framebuffer)

Order of operations:

1. `if (CheckDil(y, regs)) return;` — interlace line rejection (`vidsoft.c:2749-2750`).
2. `y /= vdp1interlace;` then `iPix = &((u16*)back_framebuffer)[(y * vdp1width) + x]` (`vidsoft.c:2752-2753`).
3. Bounds check: `if (iPix >= (u16*)(back_framebuffer + 0x40000)) return;` — **upper bound only, no lower bound and no X-range check**. A negative or oversized `x` can therefore index into the previous row or before the buffer. Clipping (step 5) is what normally prevents this.
4. Mesh: `if (mesh && (x^y)&1) return;` (`vidsoft.c:2758-2759`).
5. `if (IsClipped(x, original_y, regs, cmd)) return;` — note `original_y`, i.e. pre-interlace-division (`vidsoft.c:2761-2762`).
6. MSB-on: `if (CMDPMOD & (1<<15)) { if (currentPixel) { *iPix |= 0x8000; return; } }` (`vidsoft.c:2764-2770`).
7. Visibility gate: `if (SPD || (currentPixel & currentPixelIsVisible))` (`vidsoft.c:2772`).
8. The colour-calculation switch.

| Mode | Name (`vdp1.cpp:1290-1313`) | Implementation (`vidsoft.c:2776-2826`) |
| --- | --- | --- |
| 0 | Replace | `if (!((currentPixel == 0) && !SPD)) *iPix = currentPixel;` |
| 1 | Cannot overwrite / Shadow | `if (*iPix & (1<<15)) *iPix = alphablend16(*iPix, 0, 128) \| 0x8000;` — halves the existing framebuffer colour, only where the framebuffer MSB is set |
| 2 | Half luminance | `*iPix = ((currentPixel & ~0x8421) >> 1) \| (1<<15);` — shift each 5-bit channel right by one, masking the LSBs of all channels and the MSB |
| 3 | Replace / Half transparent | `if (*iPix & (1<<15)) *iPix = alphablend16(*iPix, currentPixel, 128) \| 0x8000; else *iPix = currentPixel;` |
| 4 | Gouraud Shading | See below |
| 5 | *(unnamed in the debugger)* | Falls to `default:` |
| 6 | Gouraud + Half luminance | Falls to `default:` |
| 7 | Gouraud / Gouraud + Half transparent | Falls to `default:` |

`default:` (modes 5, 6, 7) is `*iPix = alphablend16(COLOR(leftColumnColor.r, .g, .b), currentPixel, 128) | 0x8000` (`vidsoft.c:2824-2826`) — a fixed 50/50 blend of the interpolated gouraud colour with the source pixel. Modes 6 and 7 are therefore **not** distinguished from each other, and the "half luminance" part of mode 6 is not applied. **[Ambiguous]** — this is a simplification, not a hardware description.

`alphablend16(d, s, level)` (`vidsoft.c:2490-2502`) blends per 5-bit channel in place within the packed XBGR-1555 word: `r = (sr*level + dr*(256-level)) >> 8`, masked per channel; the result has bit 15 clear (callers OR it back in).

**Mode 4 (gouraud)** has a special case (`vidsoft.c:2796-2809`), commented "handle the special case demonstrated in the sgl chrome demo":

```c
if (colour_mode != 5 && colour_mode != 1 &&
    (int)leftColumnColor.g == 16 && (int)leftColumnColor.b == 16) {
    int c = (int)(leftColumnColor.r - 0x10);
    if (c < 0) c = 0;
    currentPixel = currentPixel + c;
    *iPix = currentPixel;
    break;
}
```

i.e. in an indexed bank mode, when the green and blue gouraud values are both exactly neutral (`0x10`), the red gouraud value is added to the **palette index** rather than being applied as RGB. Otherwise the normal path runs:

```c
*iPix = COLOR(gouraudAdjust(currentPixel & 0x001F,        (int)leftColumnColor.r),
              gouraudAdjust((currentPixel & 0x03E0) >> 5, (int)leftColumnColor.g),
              gouraudAdjust((currentPixel & 0x7C00) >> 10,(int)leftColumnColor.b));
```

with `gouraudAdjust(color, tableValue) = clamp(color + (tableValue - 0x10), 0, 0x1F)` (`vidsoft.c:2642-2650`). So a gouraud table channel value of `0x10` is neutral, below darkens, above brightens, each channel independently, saturating at 0 and 31. `COLOR(r,g,b)` packs to XBGR-1555 with bit 15 forced set (`vidsoft.c:2794`).

### 7.2 `putpixel8` (8-bit framebuffer)

`vidsoft.c:2707-2740`. Same order (bounds, DIL, mesh, clip) but:

* `currentPixel &= 0xFF` before use (`vidsoft.c:2720`).
* Mesh uses `y2 = y / vdp1interlace` rather than the raw `y` (`vidsoft.c:2709`, `2722`).
* **Only colour-calculation mode 0 (replace) is implemented.** The `switch` has `default:` and `case 0:` collapsed together and no other cases (`vidsoft.c:2731-2738`). Shadow, half-luminance, half-transparent and gouraud are all silently treated as replace in 8-bit mode.
* MSB-on (CMDPMOD bit 15) is not handled at all.

### 7.3 Interlace line rejection

`CheckDil` (`vidsoft.c:2653-2672`): active only when `vdp1interlace == 2` (FBCR bit 3 set). With DIL (FBCR bit 2) set, even `y` are rejected; with DIL clear, odd `y` are rejected. Returns 0 (draw) otherwise.

---

## 8. Clipping

Two independent rectangles, both stored in the `Vdp1` register struct.

### 8.1 System clipping

`IsSystemClipped` (`vidsoft.c:2682-2688`):

```c
return !(x >= 0 && x <= regs->systemclipX2 &&
         y >= 0 && y <= regs->systemclipY2);
```

* `systemclipX1` and `systemclipY1` are **never read** — the origin is hard-coded to 0. This is consistent with the System Clipping command, which forces them to 0 anyway (§5.8).
* The comparison is **inclusive** on the upper bound (`<=`), so the rectangle covers `[0, X2]` × `[0, Y2]`.
* System clipping is **always applied**, in both branches of `IsClipped`.

### 8.2 User clipping

`IsUserClipped` (`vidsoft.c:2674-2680`):

```c
return !(x >= regs->userclipX1 && x <= regs->userclipX2 &&
         y >= regs->userclipY1 && y <= regs->userclipY2);
```

Inclusive on both bounds. The clip fields are `u16`, and `x`/`y` are `int`, so the comparison promotes the unsigned clip bounds to `int` — meaning a clip value written as, say, `0xFFF0` compares as `65520`, not `-16`. User clip coordinates are effectively unsigned.

### 8.3 Combination

`IsClipped` (`vidsoft.c:2690-2705`):

```c
if (cmd->CMDPMOD & 0x0400) {                       // user clipping enabled
    int is_user_clipped = IsUserClipped(x, y, regs);
    if (((cmd->CMDPMOD >> 9) & 0x3) == 0x3)        // outside clipping mode
        is_user_clipped = !is_user_clipped;
    return is_user_clipped || IsSystemClipped(x, y, regs);
} else {
    return IsSystemClipped(x, y, regs);
}
```

| CMDPMOD bit 10 (CLIP) | CMDPMOD bit 9 (CMOD) | Effect |
| --- | --- | --- |
| 0 | x | System clipping only |
| 1 | 0 | Draw only *inside* the user rect, and inside the system rect |
| 1 | 1 | Draw only *outside* the user rect, and inside the system rect |

Note the test is on both bits at once (`(CMDPMOD >> 9) & 0x3) == 0x3`), which is equivalent to `bit10 && bit9` — and bit 10 is already known set inside that branch, so it reduces to testing bit 9. Written this way it is defensive but not different in effect.

### 8.4 Pre-clipping

`is_pre_clipped` (`vidsoft.c:3028-3064`) is a whole-quad trivial-reject run at the top of `drawQuad`. It rejects only when **all four** corners are on the same outside side:

* all X `< 0`, or all X `> systemclipX2`, or
* all Y `< 0`, or all Y `> y_val`, where `y_val = systemclipY2` doubled if `vdp1interlace` is non-zero (`vidsoft.c:3030-3033`).

It uses the system clip rectangle only, never the user one, and — as noted in §3.3 — it runs unconditionally, ignoring CMDPMOD bit 11. Lines and polylines never call it. This is a performance optimisation whose visible effect should be nil; the observable per-pixel clipping is §8.3.

---

## 9. Framebuffer erase and swap

Two 256 KiB buffers exist (`vdp1.cpp:290-294`). In the software renderer they are pointed at by `vdp1frontframebuffer` (displayed by VDP2) and `vdp1backframebuffer` (drawn into by VDP1, and what the CPU port sees). `Vdp1External.current_frame` is the index used by the raw fallback paths in `vdp1.cpp`.

### 9.1 The two flags and where they are set

| Flag | Set at | Meaning |
| --- | --- | --- |
| `Vdp1External.manualchange` | `vdp1.cpp:479` when `(FBCR & 3) == 3` | A manual frame change was requested |
| `Vdp1External.manualerase` | `vdp1.cpp:483` when `(FBCR & 3) == 2` | A manual erase was requested |
| `Vdp1External.vbalnk_erase` | `vdp2.cpp:1405-1409`, from TVMR bit 3 (VBE), evaluated at VBlank-IN | VBlank erase enabled |
| `Vdp1External.swap_frame_buffer` | `vdp2.cpp:936` (from `manualchange`) and `vdp2.cpp:943` (one-cycle mode) | A swap is pending |
| `Vdp1External.frame_change_plot` | `vdp2.cpp:948/952`, from `PTMR == 2` | Draw should start on frame change |

Decision point, once per frame (`vdp2.cpp:931-954`), gated on `Vdp2External.frame_render_flg == 0 && vdp1_clock > 0`:

```c
if (Vdp1External.manualchange == 1) { Vdp1External.swap_frame_buffer = 1; Vdp1External.manualchange = 0; }
if ((FBCR & 0x03) == 0x00 || (FBCR & 0x03) == 0x01) { Vdp1External.swap_frame_buffer = 1; }
Vdp1External.frame_change_plot = (Vdp1Regs->PTMR == 2) ? 1 : 0;
```

### 9.2 Erase and swap execution

`vdp2.cpp:1220-1255`, run after `VIDCore->Vdp2DrawStart()`:

```c
// VBlank Erase
if (Vdp1External.vbalnk_erase || ((Vdp1Regs->FBCR & 2) == 0))     // VBE1, or one cycle mode
    VIDCore->Vdp1EraseWrite();

// Frame Change
if (Vdp1External.swap_frame_buffer == 1) {
    if (Vdp1External.manualerase) {                                // FCM1 FCT0, just before frame change
        VIDCore->Vdp1EraseWrite();
        Vdp1External.manualerase = 0;
    }
    VIDCore->Vdp1FrameChange();
    Vdp1External.current_frame = !Vdp1External.current_frame;
    Vdp1External.swap_frame_buffer = 0;
    Vdp1Regs->EDSR >>= 1;                                          // BEF <- CEF, CEF <- 0
    if (Vdp1External.frame_change_plot == 1 || Vdp1External.status == VDP1_STATUS_RUNNING) {
        Vdp1Regs->addr = 0;  Vdp1Regs->COPR = 0;  Vdp1Draw();
    }
} else {
    if (Vdp1External.status == VDP1_STATUS_RUNNING) Vdp1Draw();    // resume an unfinished draw
}
```

So the ordering per frame is: **VBlank erase → (manual erase) → swap → EDSR shift → conditional draw start**.

In the software renderer, `VIDSoftVdp1EraseWrite` and `VIDSoftVdp1FrameChange` are **empty stubs** (`vidsoft.c:94-95`). The software renderer instead does its erase at draw-start time (`VIDSoftVdp1DrawStartBody` → `VIDSoftVdp1EraseFrameBuffer`, `vidsoft.c:2417`) and its swap at VDP2 draw-end time (`VIDSoftVdp2DrawEnd` → `VIDSoftVdp1SwapFrameBuffer`, `vidsoft.c:3874`). The two renderers use genuinely different timing models for the same registers. **[Ambiguous]** — the correct hardware timing is not deducible from this source.

### 9.3 The swap

`VIDSoftVdp1SwapFrameBuffer` (`vidsoft.c:4111-4127`):

```c
if (((Vdp1Regs->FBCR & 2) == 0) || Vdp1External.manualchange) {
    temp = vdp1frontframebuffer;
    vdp1frontframebuffer = vdp1backframebuffer;
    vdp1backframebuffer = temp;
    Vdp1External.manualchange = 0;
}
```

A pointer swap gated on either one-cycle mode (FCM=0) or a pending manual change. Note this consumes `manualchange` a second time — `vdp2.cpp:937` also clears it — so which one wins depends on relative timing.

### 9.4 The erase

`VIDSoftVdp1EraseFrameBuffer` (`vidsoft.c:4130-4168`):

```c
if (((regs->FBCR & 2) == 0) || Vdp1External.manualerase) {
    h = (regs->EWRR & 0x1FF) + 1;         if (h > vdp1height) h = vdp1height;
    w = ((regs->EWRR >> 6) & 0x3F8) + 8;  if (w > vdp1width)  w = vdp1width;

    if (vdp1pixelsize == 2) {
        for (i2 = (regs->EWLR & 0x1FF); i2 < h; i2++)
            for (i = ((regs->EWLR >> 6) & 0x1F8); i < w; i++)
                ((u16*)back_framebuffer)[(i2 * vdp1width) + i] = regs->EWDR;
    } else {
        w = regs->EWRR >> 9;  w *= 16;
        for (i2 = (regs->EWLR & 0x1FF); i2 < h; i2++)
            for (i = ((regs->EWLR >> 6) & 0x1F8); i < w; i++) {
                int pos = (i2 * vdp1width) + i;
                if (pos < 0x3FFFF) back_framebuffer[pos] = regs->EWDR & 0xFF;
            }
    }
    Vdp1External.manualerase = 0;
}
```

Key points:

* Same gating as the swap: one-cycle mode, or a pending manual erase.
* The erase always targets the **back** buffer.
* X granularity is 8 pixels on both the start and end coordinate; Y granularity is 1 line.
* The end coordinates are **exclusive after the `+1`/`+8`**, i.e. the rectangle is `[X1, X3+8)` × `[Y1, Y3+1)` in 16-bit mode.
* The 8-bit path recomputes `w` in 16-pixel units (§2.2, EWRR) *after* the clamp, relying on the `pos < 0x3FFFF` guard instead.
* `manualerase` is consumed here as well as at `vdp2.cpp:1232`.

### 9.5 Draw-start geometry setup

`VIDSoftVdp1DrawStartBody` (`vidsoft.c:2386-2422`) runs before every command list: sets `vdp1interlace` from FBCR bit 3, sets `vdp1width`/`vdp1height`/`vdp1pixelsize` from TVMR bits 1–0 (§2.2 table), then calls the erase. A comment at `vidsoft.c:2419-2421` notes that clipping values are deliberately *not* reset here:

> "night warriors doesn't set clipping most frames and uses the last part of the vdp1 framebuffer as scratch ram / the previously set clipping values need to be reused"

so **clip rectangles and local coordinates persist across command lists / frames.**

---

## 10. Draw End status and interrupt

### 10.1 EDSR transitions, complete list

| Event | Action | Line |
| --- | --- | --- |
| Bad command opcode (12–15) encountered | `EDSR \|= 2` (CEF set), LOPR/COPR ← `addr>>3`, status ← IDLE, return | `vdp1.cpp:617-623` |
| `EDSR & 0x02` already set at loop top | LOPR/COPR ← `addr>>3`, status ← IDLE, return (no further EDSR change) | `vdp1.cpp:628-634` |
| PTMR written with 1 | `EDSR >>= 1` **before** drawing (CEF→BEF, CEF cleared) | `vdp1.cpp:511` |
| Frame change executed | `EDSR >>= 1` | `vdp2.cpp:1240` |
| Draw completed, `wait_line_count` reached, status IDLE | `EDSR \|= 2` **and** `ScuSendDrawEnd()` | `vdp2.cpp:1000-1003` |
| `Vdp1NoDraw()` (display toggled off) | `EDSR \|= 2` **and** `ScuSendDrawEnd()` | `vdp1.cpp:853-854` |

Bit semantics as the comments state them (`vdp1.cpp:816-818`, `vdp1.cpp:843-845`, the latter citing "ST-013-R3-061694 page 53"):

* **Bit 1 = CEF (Current End Flag)** — set when the current draw finished.
* **Bit 0 = BEF (Before End Flag)** — the previous frame's CEF, shifted down.

### 10.2 The Draw End interrupt

`ScuSendDrawEnd()` (`scu.c:3382-3385`):

```c
void ScuSendDrawEnd(void) {
   SendInterrupt(0x4D, 0x2, 0x2000, 0x00002000);
   ScuChekIntrruptDMA(6);
}
```

* SH-2 exception vector **`0x4D`**
* Interrupt level **`2`**
* SCU interrupt mask bit **`0x2000`** (bit 13)
* SCU interrupt status bit **`0x00002000`**
* Also triggers SCU DMA start factor **6**

### 10.3 When it fires

`vdp2.cpp:995-1010` (synchronous build), evaluated once per scanline:

```c
if (yabsys.wait_line_count != -1 && yabsys.LineCount == yabsys.wait_line_count) {
    if (Vdp1External.status == VDP1_STATUS_IDLE) {
        ScuSendDrawEnd();
        yabsys.wait_line_count = -1;
        Vdp1Regs->EDSR |= 2;
    } else {
        yabsys.wait_line_count = (yabsys.wait_line_count + 10) % yabsys.VBlankLineCount;
    }
}
```

`wait_line_count` is armed to `LineCount + 50` when PTMR=1 is written (`vdp1.cpp:514-515`). If VDP1 is still `RUNNING` when the deadline arrives, the deadline is pushed out 10 lines and retried.

**Note that CEF is set *after* `ScuSendDrawEnd()`** — an interrupt handler that reads EDSR immediately would observe CEF still clear. Whether that ordering matters is not determinable here.

Paths that leave **no** Draw End signalled:

* ENDR written (`vdp1.cpp:533` sets `wait_line_count = -1` without ever setting CEF).
* Command list exceeded the 4096-iteration cap (status stays RUNNING; the retry loop above keeps deferring).
* Address error `addr > 0x7FFFF` at entry (`vdp1.cpp:555-559`).
* Bad jump to address 0 (`vdp1.cpp:644-649` etc.) — status goes IDLE, so if a deadline was armed the interrupt *will* fire on the next check; if none was armed, nothing happens.
* First command is Draw End (`vdp1.cpp:561-565`).

### 10.4 Engine status

`Vdp1External.status` (`vdp1.h:151-154, 165`) is the "am I drawing" flag. `VDP1_STATUS_RUNNING` is set at `vdp1.cpp:825`; `VDP1_STATUS_IDLE` at `vdp1.cpp:556`, `562`, `622`, `631`, `647`, `659`, `673`, `683`, and on any ENDR write (`vdp1.cpp:532`). It is **not** exposed through any register — `EDSR`, `LOPR` and `COPR` are the only externally visible progress indicators.

---

## 11. How VDP2 reads the framebuffer (boundary summary)

This is VDP2's responsibility and belongs in the VDP2 document; included only where it constrains what VDP1 must write. `VidsoftDrawSprite` (`vidsoft.c:3541` onwards) is the consumer.

* Gated on `Vdp1External.disptoggle && (Vdp2Regs->TVMD & 0x8000)` (`vidsoft.c:3566`).
* Reads `vdp1frontframebuffer` (`vidsoft.c:3685`), i.e. the buffer VDP1 is *not* drawing into.
* In 16-bit framebuffer mode: pixel `0x0000` is transparent (`vidsoft.c:3687-3688`). A pixel with bit 15 set, when `Vdp2Regs->SPCTL & 0x20` (`colormode`) is set, is treated as direct **RGB** and expanded via `COLSAT2YAB16` (`vidsoft.c:3689-3703`). Otherwise the pixel is a **colour-bank index**, offset by `(Vdp2Regs->CRAOFB & 0x70) << 4` and resolved through VDP2 colour RAM (`vidsoft.c:3592`, `3720`).
* Before index resolution, the pixel is split into priority / colour-calculation / colour fields according to the VDP2 sprite type `Vdp2Regs->SPCTL & 0xF` — sixteen distinct layouts, `Vdp1GetSpritePixelInfo` (`vidshared.h:829-985`). Types 0–7 use 16-bit pixels; types 8–F use 8-bit pixels. Each type also defines a "normal shadow" value (all-ones minus one in the colour field: `0x7FE`, `0x3FE`, `0x1FE`, `0x7E`, `0xFE` depending on field width).
* Special case at `vidsoft.c:3698-3703`: a pixel of exactly `0x8000` is only drawn if `vdp1spritetype < 2`, or `< 8` with sprite window disabled. Comment: "sprite types 0 and 1 are -always- drawn and sprite types 8-F are always transparent".
* Resolution matching (`vidsoft.c:3657-3679`): VDP1 1024-wide + VDP2 hi-res → 1:1; VDP1 512 + VDP2 hi-res → pixel doubling; VDP1 1024 + VDP2 lo-res → read out at half rate.

The practical constraint on VDP1: **the framebuffer holds either raw RGB-1555 words or colour-bank indices with priority/colour-calc bits packed alongside, and which one a given pixel is depends on its bit 15 plus VDP2's `SPCTL`.** VDP1's own colour-calculation modes (§7) operate on those words as if they were RGB-1555 regardless — which is exactly why the gouraud special case at `vidsoft.c:2796-2809` exists.

---

## 12. Summary of gaps and contradictions in this source

Collected so they are not mistaken for settled facts:

1. **TVM2 (TVMR bit 2)** and **EOS (FBCR bit 4)** are stored and readable via MODR but have no behavioural decode anywhere.
2. **HSS (CMDPMOD bit 12, high-speed shrink)** is recognised only by the debugger; the renderer ignores it.
3. **PCLP (CMDPMOD bit 11, pre-clipping)** is never tested; pre-clipping runs unconditionally.
4. **Scaled sprite ZP `0xC` vs `0xD`** — the debugger and the renderer disagree about which selects lower-left.
5. **CMDCOLR `<< 3`** — the debugger applies it to all bank modes; the renderer applies it only to the LUT mode.
6. **Colour-calculation modes 5, 6, 7** are collapsed into one `default:` blend; mode 6's half-luminance component is not implemented.
7. **8-bit framebuffer mode implements only colour-calculation mode 0**, and ignores MSB-on.
8. **End codes in 8bpp modes** carry an explicit "this needs more hardware testing" comment; mode 2 substitutes transparency for termination and mode 3's end code value can never match its masked fetch.
9. **`Vdp1Reset`'s `memset` uses `sizeof(pointer)`**, leaving EDSR / LOPR / COPR / addr uncleared.
10. **`VIDSoftVdp1LineDraw` swaps the green and blue gouraud out-parameters** (`vidsoft.c:3404`).
11. **`FBCR & 3 == 1` treated as one-cycle mode** is an explicit *Sonic R* workaround.
12. **The 999-pixel line cap, the 4096/2000 command caps, the "jump to 0" abort, and the `0x40000` reset terminator** are all emulator guards or game hacks, not hardware.
13. **There is no VDP1 timing model.** The clock-budget code is `#if 0`'d out; command lists execute atomically, and the Draw End delay is a hand-tuned 50 scanlines.
14. **LOPR is only maintained on error paths**, never on normal completion.
15. **The software and OpenGL renderers place the erase and swap at different points in the frame** (`vidsoft.c:2417` / `vidsoft.c:3874` vs. `vdp2.cpp:1221`/`1236`), so the source does not settle when either really happens.
