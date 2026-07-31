# CS2 — CD Block (disc controller, A-bus CS2 area)

**Source of truth.** Everything in this document is derived *exclusively* from the Yabause
(YabaSanshiro fork) C source:

- `yabause/src/cs2.c` (4420 lines) — CD block command processor and register interface
- `yabause/src/cs2.h` (424 lines) — register/command constants and struct layouts
- `yabause/src/cdbase.c` (2027 lines) — disc-image abstraction (TOC / sector / track / session)
- `yabause/src/cdbase.h` (87 lines) — `CDInterface` vtable and core/status IDs

Four facts that those files cannot supply on their own are marked inline and cited to their own
file: the A-bus CS2 mapping (`yabause/src/memory.c:657`), the argument passed to `Cs2Exec`
(`yabause/src/yabause.c:789, 835`), the cartridge indirection used for byte accesses
(`yabause/src/cs0.c:135, 1069`), and the absence of callers for several exported functions
(whole-tree grep).

**No outside Saturn documentation was used.** Where the code is ambiguous, contradicts itself,
or is plainly broken, that is stated rather than smoothed over. Such notes are tagged
**[QUIRK]** (deliberate emulator shortcut / unimplemented hardware behaviour), **[BUG]** (a
defect in the C source: wrong index, unreachable test, out-of-bounds access), **[HACK]**
(game-specific special case), or **[DEAD]** (code with no callers). Anything Yabause does not
implement is called out as *not implemented in this source* rather than guessed at.

Line citations are of the form `yabause/src/cs2.c:1481`. They point at the code that
establishes the claim.

---

## 0. Structural overview

The CD block is modelled as a single heap allocation, `Cs2 *Cs2Area` (`cs2.c:102`, type at
`cs2.h:162-269`), plus a separate `ip_struct *cdip` (`cs2.c:103`, type at `cs2.h:271-287`) that
caches the parsed IP.BIN header of the mounted disc.

| Field group | Where | Contents |
|---|---|---|
| `reg` (`blockregs_struct`) | `cs2.h:149-160` | `DTR`, `UNKNOWN`, `HIRQ`, `HIRQMASK`, `CR1`-`CR4`, `MPEGRGB` |
| Drive report state | `cs2.h:164-172` | `FAD`, `status`, `options`, `repcnt`, `ctrladdr`, `track`, `index` |
| Authentication | `cs2.h:175-176` | `satauth`, `mpgauth` |
| Info-transfer state | `cs2.h:179-198` | `transfercount`, `cdwnum`, `TOC[102]`, `infotranstype`, `transfileinfo[12]`, `transscodeq[10]`, `transscoderw[24]` |
| Filters | `cs2.h:200-210` | `filter[24]`, five "output connector" pointers + their numbers |
| Buffers | `cs2.h:212-232` | `partition[24]`, `block[200]`, `blockfreespace`, `workblock` (2448-byte data) |
| Data-transfer state | `cs2.h:214-219` | `datatranspartition`, `datatransoffset`, `datanumsecttrans`, `datatranssectpos`, `datasectstotrans` |
| Filesystem | `cs2.h:234-238` | `curdirsect`, `curdirsize`, `curdirfidoffset`, `fileinfo[256]`, `numfiles` |
| MPEG | `cs2.h:240-246, 259-264` | `mpegintmask`, `mpegmode`, `mpegcon[2]`, `mpegstm[2]`, `actionstatus`, `pictureinfo`, `mpegaudiostatus`, `mpegvideostatus`, `vcounter` |
| Scheduling | `cs2.h:248-253, 266-267` | `_command`, `_statuscycles/_statustiming`, `_periodiccycles/_periodictiming`, `_commandtiming`, `_command_execlock`, `_delay_irq` |
| Disc backend | `cs2.h:254` | `CDInterface *cdi` |

Compile-time sizes (`cs2.h:51-53`):

```
MAX_BLOCKS    200    // sector buffers
MAX_SELECTORS  24    // filters AND partitions (one array of each)
MAX_FILES     256    // directory records cached
```

### 0.1 Entry points

| Function | Line | Role |
|---|---|---|
| `Cs2Init` / `Cs2DeInit` / `Cs2Reset` | `cs2.c:662` / `746` / `768` | Lifecycle |
| `Cs2ChangeCDCore` | `cs2.c:698` | Swap the `CDInterface` backend |
| `Cs2ReadByte` / `Cs2WriteByte` | `cs2.c:143` / `150` | **Not CD block registers** — forwarded to the cartridge (§1.1) |
| `Cs2ReadWord` / `Cs2WriteWord` | `cs2.c:157` / `295` | 16-bit register access + info-transfer port |
| `Cs2ReadLong` / `Cs2WriteLong` | `cs2.c:340` / `447` | 32-bit register access + data-transfer port |
| `Cs2Exec(u32 timing)` | `cs2.c:937` | Drive state machine, periodic report, command dispatch trigger |
| `Cs2Execute` | `cs2.c:1182` | The command opcode switch |
| `Cs2SetCommandTiming` | `cs2.c:1169` | Arms command execution (called from the CR4 write) |
| `Cs2GetIP` / `Cs2GetRegionID` | `cs2.c:4018` / `4147` | IP.BIN parse, region autodetect |
| `Cs2ForceOpenTray` / `Cs2ForceCloseTray` | `cs2.c:908` / `915` | Front-end tray control (bypasses the command set) |
| `Cs2SaveState` / `Cs2LoadState` | `cs2.c:4154` / `4270` | Savestates, format version 3 |

`Cs2Exec` is called once per emulated scanline-decile-10 from the main loop as
`Cs2Exec(cdb_accum_t)` where `cdb_accum_t` accumulates `yabsys.UsecFrac >> YABSYS_TIMING_BITS`
(`yabause/src/yabause.c:789, 835`), i.e. **the `timing` argument is elapsed microseconds**.

---

## 1. Register map

### 1.1 Address decoding

The CS2 area occupies A-bus pages `0x580`-`0x58F`, i.e. `0x05800000-0x058FFFFF`
(`FillMemoryArea(0x580, 0x58F, &Cs2ReadByte, &Cs2ReadWord, &Cs2ReadLong, &Cs2WriteByte,
&Cs2WriteWord, &Cs2WriteLong)`, `yabause/src/memory.c:657-662`). Through the SH-2 cache-through
window this is `0x25800000-0x258FFFFF`.

Every word/long accessor masks the address with `0xFFFFF` (`cs2.c:159, 296, 343, 448`, each
carrying the comment `// fix me(I should really have proper mapping)`), so the offsets below are
relative to `0x05800000` and there is exactly one instance of the register file (the mask width
equals the mapped width).

**Byte accesses do not reach the CD block at all.** `Cs2ReadByte`/`Cs2WriteByte` forward to
`CartridgeArea->Cs2ReadByte`/`Cs2WriteByte` (`cs2.c:145, 152`), which for a normal (non-modem)
cartridge is `DummyCs2ReadByte` returning a constant `0xFF` (`cs0.c:135-138`, installed at
`cs0.c:1069-1074`). With `CART_NETLINK` or `CART_JAPMODEM` the byte port becomes the modem UART
(`cs0.c:1222-1223, 1261-1262`). See §10 [QUIRK 1].

### 1.2 Complete offset table

Legend: **W16** = decoded by `Cs2ReadWord`/`Cs2WriteWord`; **W32** = decoded by
`Cs2ReadLong`/`Cs2WriteLong`; **—** = falls into the `default:` branch, which only logs
`"Undocumented register read/write"` and returns 0 (`cs2.c:284-287, 331-334, 436-439, 497-500`).

| Offset | Absolute | Name | R16 | W16 | R32 | W32 | Notes |
|---|---|---|---|---|---|---|---|
| `0x18000` | `0x05818000` | Data transfer FIFO | — | — | `:375` | `:452` | Sector data in/out. **32-bit only.** |
| `0x90008` | `0x05890008` | `HIRQ` | `:162` | `:299` | `:346` | — | Read returns flags; write is AND-mask (clear) |
| `0x9000A` | `0x0589000A` | `HIRQ` (alias) | `:163` | `:300` | — | — | Same register |
| `0x9000C` | `0x0589000C` | `HIRQMASK` | `:185` | `:309` | `:368` | — | Gates the A-bus interrupt only |
| `0x9000E` | `0x0589000E` | `HIRQMASK` (alias) | `:186` | `:310` | — | — | |
| `0x90018` | `0x05890018` | `CR1` | `:187` | `:312` | `:369` | — | Write has side effects (§2) |
| `0x9001A` | `0x0589001A` | `CR1` (alias) | `:188` | `:313` | — | — | |
| `0x9001C` | `0x0589001C` | `CR2` | `:189` | `:318` | `:370` | — | |
| `0x9001E` | `0x0589001E` | `CR2` (alias) | `:190` | `:319` | — | — | |
| `0x90020` | `0x05890020` | `CR3` | `:191` | `:321` | `:371` | — | |
| `0x90022` | `0x05890022` | `CR3` (alias) | `:192` | `:322` | — | — | |
| `0x90024` | `0x05890024` | `CR4` | `:193` | `:324` | `:372` | — | **Read clears `_command`; write launches the command** |
| `0x90026` | `0x05890026` | `CR4` (alias) | `:194` | `:325` | — | — | Same side effects |
| `0x90028` | `0x05890028` | `MPEGRGB` | `:196` | `:328` | `:374` | — | Plain 16-bit scratch; nothing reads it |
| `0x9002A` | `0x0589002A` | `MPEGRGB` (alias) | `:197` | `:329` | — | — | |
| `0x98000` | `0x05898000` | Info transfer port | `:198` | — | — | — | TOC / file info / subcode. **16-bit read only.** |

`blockregs_struct.DTR` (u32) and `blockregs_struct.UNKNOWN` (u16) (`cs2.h:151-152`) are never
read or written by any code in `cs2.c`; they exist only to pad the savestate blob written at
`cs2.c:4166`.

The long reads of `HIRQ`, `HIRQMASK`, `CR1`-`CR4` and `MPEGRGB` all return the 16-bit value
duplicated into both halves: `((reg << 16) | reg)` (`cs2.c:366-374`).

### 1.3 `HIRQ` — Host Interrupt Request (`0x05890008`)

16-bit. Defined bits (`cs2.c:56-69`); bits 14-15 are undefined and never set by this source.

| Bit | Mask | Name | Meaning as used in this source |
|---|---|---|---|
| 0 | `0x0001` | `CMOK` | Command complete / command register free |
| 1 | `0x0002` | `DRDY` | Data ready in the transfer port |
| 2 | `0x0004` | `CSCT` | One sector has been stored in a buffer |
| 3 | `0x0008` | `BFUL` | Sector buffer full |
| 4 | `0x0010` | `PEND` | Play/read operation ended |
| 5 | `0x0020` | `DCHG` | Disc changed / tray operated |
| 6 | `0x0040` | `ESEL` | "Selector" (filter/partition) operation ended |
| 7 | `0x0080` | `EHST` | Host data transfer ended |
| 8 | `0x0100` | `ECPY` | Sector copy/move ended |
| 9 | `0x0200` | `EFLS` | Filesystem operation ended |
| 10 | `0x0400` | `SCDQ` | Subcode Q updated (periodic response) |
| 11 | `0x0800` | `MPED` | MPEG operation ended |
| 12 | `0x1000` | `MPCM` | MPEG command complete |
| 13 | `0x2000` | `MPST` | MPEG status/interrupt |

**Read** (`cs2.c:162-184` word, `346-367` long): returns `reg.HIRQ` verbatim. The handler
performs a dead read-modify-write (`val = HIRQ; … HIRQ = val;`) around a block of commented-out
code that used to derive `BFUL`, `DCHG` and `CSCT` from `isbufferfull`, `isdiskchanged` and
`isonesectorstored` at read time (`cs2.c:166-181`). Those three internal flags are therefore
maintained but **never surfaced through HIRQ on read**; the flags only appear when a command
explicitly sets them.

**Write** (`cs2.c:299-308`): `HIRQ = HIRQ & val`. Writing a 0 bit clears it; writing a 1 bit
leaves it. Writing `0xFFFF` is a no-op; writing `0x0000` clears everything. After the AND, if
`HIRQ & HIRQMASK` is still non-zero the A-bus interrupt is re-asserted
(`ScuSendExternalInterrupt00()`, `cs2.c:305-307`) — i.e. the interrupt is **level-like**: it
re-fires on any HIRQ write that leaves an unmasked bit set.

**Setting** goes through `Cs2SetIRQ` (`cs2.c:129-134`):

```c
static INLINE void Cs2SetIRQ(u32 irq){
  Cs2Area->reg.HIRQ |= irq;
  if (Cs2Area->reg.HIRQ & Cs2Area->reg.HIRQMASK)
    ScuSendExternalInterrupt00();
}
```

There is no auto-clear anywhere. In particular the line that would clear `CMOK` at the start of
command execution is commented out (`cs2.c:1185`, `//Cs2Area->reg.HIRQ &= ~CDB_HIRQ_CMOK;`), so
`CMOK` stays set from the previous command unless software clears it. See §10 [QUIRK 4].

Reset value: `HIRQ = 0xFFFF` — **every flag set** (`cs2.c:820`).

### 1.4 `HIRQMASK` (`0x0589000C`)

16-bit, plain read/write (`cs2.c:185-186, 309-311, 368`). Comment at `cs2.h:154`:
*"Masks bits from HIRQ -only- when generating A-bus interrupts"*. It never affects the value
read back from `HIRQ`. Reset value `0x0000` (`cs2.c:821`) — all interrupts masked off.

### 1.5 `CR1`-`CR4` (`0x05890018`, `0x1C`, `0x20`, `0x24`)

Plain 16-bit read/write with three side effects:

| Access | Side effect | Line |
|---|---|---|
| Write `CR1` | `status &= ~CDB_STAT_PERI` (clears the 0x20 periodic-response flag) | `cs2.c:313` |
| Write `CR1` | `_command = 1` — suppresses the periodic status report | `cs2.c:314` |
| Write `CR4` | `Cs2SetCommandTiming(CR1 >> 8)` — arms command execution | `cs2.c:326` |
| Read `CR4` | `_command = 0` — re-enables the periodic status report | `cs2.c:194, 372` |

Reset values spell "CDBLOCK" (`cs2.c:816-819`):

```
CR1 = 0x0043   // 0x00, 'C'
CR2 = 0x4442   // 'D', 'B'
CR3 = 0x4C4F   // 'L', 'O'
CR4 = 0x434B   // 'C', 'K'
```

### 1.6 Data transfer port `0x05818000`

**Read (32-bit)** — `cs2.c:375-435`. Active only when `datatranstype != CDB_DATATRANSTYPE_INVALID`
(`cs2.c:377`). Each read:

1. Source pointer is
   `datatranspartition->block[datatranssectpos + datanumsecttrans]->data[datatransoffset]`
   (`cs2.c:384`).
2. Value is byte-swapped to big-endian on little-endian hosts (`BSWAP32`, `cs2.c:393`).
3. `cdwnum += 4`, `datatransoffset += 4` (`cs2.c:398-399`).
4. When `datatransoffset >= block->size`, offset resets to 0 and `datanumsecttrans++`
   (`cs2.c:402-406`).

Once `datanumsecttrans >= datasectstotrans` (all requested sectors read out), further reads
return 0 — and, **if** `datatranstype == CDB_DATATRANSTYPE_GETDELSECTOR` (2), the deferred
deletion fires (`cs2.c:410-432`): the blocks `[datatranssectpos, datatranssectpos +
datasectstotrans)` are freed, the partition's `block[]` array is compacted by `Cs2SortBlocks`,
`partition->size -= cdwnum`, `partition->numblocks -= datasectstotrans`, and `datatranstype`
becomes `INVALID`. No HIRQ bit is set by this path.

**Write (32-bit)** — `cs2.c:452-496`. Active only when `datatranstype == CDB_DATATRANSTYPE_PUTSECTOR`
(3). Destination is `datatranspartition->block[datanumsecttrans]->data[offset]` where

```c
int size   = (Cs2Area->putsectsize - Cs2Area->getsectsize) / 24;   // cs2.c:462
int offset = Cs2Area->datatransoffset - size;                      // cs2.c:464
if (offset >= 0) { …store BSWAP32(val)… }                          // cs2.c:466-479
```

so the first `size` bytes written are silently discarded and the remainder lands at offset 0.
`cdwnum += 4`, `datatransoffset += 4`; on crossing `block->size` the offset resets and
`datanumsecttrans++`, and when `datanumsecttrans >= datasectstotrans` **`EHST` is raised**
(`cs2.c:491-492`). This is the only place `EHST` is raised by the transfer port itself.

`datatranstype` values (`cs2.c:90-96`):

```
CDB_DATATRANSTYPE_INVALID      = -1
CDB_DATATRANSTYPE_GETSECTOR    =  0
CDB_DATATRANSTYPE_GETDELSECTOR =  2
CDB_DATATRANSTYPE_PUTSECTOR    =  3
```

Value 1 is never used.

`Cs2RapidCopyT1` (`cs2.c:509-568`) and `Cs2RapidCopyT2` (`cs2.c:575-658`) are bulk versions of
the read path "as though `0x25818000` had been read that many times", intended for a DMA
fast path. **Neither has any caller in the tree** — see §10 [DEAD 44].

### 1.7 Info transfer port `0x05898000`

**16-bit read only** (`cs2.c:198-283`). What it returns is selected by `infotranstype`
(`-1` = idle). Every mode increments `transfercount` and `cdwnum` by 2 per read and resets
`transfercount = 0, infotranstype = -1` when the terminator is reached.

| `infotranstype` | Set by | Source | Declared length (in `CR2`) | Terminator |
|---|---|---|---|---|
| 0 | `Cs2GetToc` (`:1513`) | `TOC[transfercount>>2]`, high word when `transfercount % 4 == 0` (`:203-206`) | `0xCC` words = 204 | `transfercount > 0xCC*2` (`:211`) |
| 1 | `Cs2GetFileInfo` single (`:1833`) | `transfileinfo[]` (`:219`) | `0x06` words = 12 bytes | `transfercount > 0x6*2` (`:224`) |
| 2 | `Cs2GetFileInfo` all (`:1821`) | `transfileinfo[]`, refilled from `fileinfo[2 + transfercount/12]` every 12 bytes (`:235-241`) | `0x05F4` words = 1524 | `transfercount > 254*12` (`:246`) |
| 3 | `Cs2GetSubcodeQRW` Q (`:1954`) | `transscodeq[]` (`:255`) | `5` words = 10 bytes | `transfercount > 5*2` (`:261`) |
| 4 | `Cs2GetSubcodeQRW` RW (`:1979`) | `transscoderw[]` (`:269`) | `12` words = 24 bytes | `transfercount > 12*2` (`:275`) |

Every terminator uses `>` where `>=` is required, so exactly one word past the declared length
is readable — and in all five cases that word is out of bounds. See §10 [BUG 17].

---

## 2. The command handshake protocol

There is **no** dedicated "go" bit. The protocol is entirely implicit in the register write
order:

```
  SH-2                                        CD block
  ────────────────────────────────────────────────────────────────────────
  1. write CR1 (opcode<<8 | arg)   ─────────►  status &= ~PERI          cs2.c:313
                                               _command = 1             cs2.c:314
                                               (periodic report now suppressed)
  2. write CR2, CR3               ─────────►  stored verbatim          cs2.c:318-322
  3. write CR4                    ─────────►  Cs2SetCommandTiming(CR1>>8)
                                               _commandtiming = 250 for opcode 0x02
                                               _commandtiming = 50  otherwise
                                                                        cs2.c:324-326, 1169-1178
  4. …Cs2Exec ticks…              ─────────►  when _commandtiming <= timing:
                                               Cs2Execute()             cs2.c:949-960
                                               → switch on CR1>>8       cs2.c:1183-1476
                                               → handler overwrites CR1-CR4
                                               → handler calls Cs2SetIRQ(CMOK | …)
                                               → ScuSendExternalInterrupt00 if unmasked
  5. poll HIRQ for CMOK           ◄─────────
  6. read CR1..CR3                ◄─────────  response
  7. read CR4                     ◄─────────  response, and _command = 0  cs2.c:194
                                               (periodic report resumes)
  8. write HIRQ = ~CMOK (etc.)    ─────────►  HIRQ &= val               cs2.c:301
```

Key properties, all traceable to the code:

- **Only the CR4 write launches the command.** Writing CR1 alone does nothing but set
  `_command` and clear the `PERI` bit. `Cs2Execute` re-reads `CR1 >> 8` at execution time
  (`cs2.c:1183`), so a CR1 written after CR4 but before the timer expires would change the
  opcode actually executed.
- **The response is protected by `_command`, not by a latch.** While `_command != 0`,
  `Cs2Exec` returns early before the periodic report (`cs2.c:1116-1117`), so `doCDReport` does
  not clobber CR1-CR4. Reading CR4 releases the lock. This is the emulator's stand-in for a
  hardware handshake; nothing prevents the periodic report from overwriting a response if
  software reads CR4 before reading CR1-CR3.
- **`CMOK` is not cleared on command start** (`cs2.c:1185` commented out), so polling for a
  `0 → 1` edge on `CMOK` requires software to have cleared it first.
- **Unimplemented opcodes never complete.** The `default:` case only logs
  (`cs2.c:1473-1475`); no CR update, no `CMOK`. See §10 [QUIRK 6].
- Two commands additionally arm a **deferred second interrupt** via `_command_execlock` /
  `_delay_irq`, fired by `Cs2Exec` when the lock counts down (`cs2.c:942-947`):
  `Cs2ResetSelector` (450 µs, `ESEL`, `cs2.c:2274-2275`),
  `Cs2CalculateActualSize` (30 µs × sector count, `ESEL`, `cs2.c:2346-2347`), and
  `Cs2DeleteSectorData` (30 µs × sector count, `EHST`, `cs2.c:2554-2555`).
  While `_command_execlock > 0`, `_commandtiming` is **not** decremented, so no new command can
  execute (`cs2.c:942-961`).

### 2.1 The standard CD report (`doCDReport`)

Most commands answer with the generic drive report (`cs2.c:109-115`):

| Register | Contents |
|---|---|
| `CR1` | `(status << 8) \| ((options & 0xF) << 4) \| (repcnt & 0xF)` |
| `CR2` | `(ctrladdr << 8) \| track` |
| `CR3` | `(index << 8) \| ((FAD >> 16) & 0xFF)` |
| `CR4` | `FAD & 0xFFFF` |

`options` is a 4-bit field the code writes as: `0` at reset with a disc present (`cs2.c:777`),
`0xFF` with no disc / tray open (`cs2.c:787, 797`), `8` whenever `Cs2SetupDefaultPlayStats`
runs (`cs2.c:3316`), `0x8` on entering `PLAY` from `SEEK` and at end-of-play
(`cs2.c:1040, 1105`), and `0x00` when the buffer fills mid-play (`cs2.c:1032`) or a play is
started (`cs2.c:1835`). No other meaning is assigned to it in this source.

`repcnt` is the repeat counter, incremented on each loop of a repeating play up to `0xE`
(`cs2.c:1055-1056, 1082-1083`).

### 2.2 The MPEG report (`doMPEGReport`)

Used by the `0x9x`/`0xAx` commands (`cs2.c:119-125`):

| Register | Contents |
|---|---|
| `CR1` | `(status << 8) \| actionstatus` |
| `CR2` | `vcounter` |
| `CR3` | `(pictureinfo << 8) \| mpegaudiostatus` |
| `CR4` | `mpegvideostatus` |

All four MPEG state variables are zero-initialised and **never written** by anything in this
file, so the MPEG report is always `status<<8 / 0 / 0 / 0`.

---

## 3. Disc / drive state machine

### 3.1 The `status` byte

Encoding (`cs2.c:71-85`). The low nibble is the drive state; `0x20` is an OR-in flag.

| Value | Name | Set where |
|---|---|---|
| `0x00` | `CDB_STAT_BUSY` | `Cs2AuthenticateDevice` only, and reverted before the function returns (`cs2.c:3163, 3185`) |
| `0x01` | `CDB_STAT_PAUSE` | Reset with disc (`:775`), `Cs2ChangeCDCore` (`:738`), status poll on disc insert (`:974`), `GetToc` (`:1520`), `GetSessionInfo` (`:1542`), `InitializeCDSystem` (`:1557`), `SeekDisc` pause/FAD/index (`:1863, 1872, 1889`), end of play (`:1039, 1071`), `AbortFile` (`:2880`), end of `AuthenticateDevice` (`:3185`) |
| `0x02` | `CDB_STAT_STANDBY` | `SeekDisc` stop and `SeekDisc` error (`:1851, 1896`) |
| `0x03` | `CDB_STAT_PLAY` | `Cs2Exec` `SEEK → PLAY` (`:1104`), `Cs2ReadFile` (`:2867`) |
| `0x04` | `CDB_STAT_SEEK` | `Cs2PlayDisc` (`:1834`), buffer-full during play (`:1031`) |
| `0x05` | `CDB_STAT_SCAN` | `Cs2ScanDisc` (`:1915`) — and never left |
| `0x06` | `CDB_STAT_OPEN` | Reset with tray open (`:794`), status poll (`:986`), `Cs2OpenTray` (`:1663`) |
| `0x07` | `CDB_STAT_NODISC` | Reset with no disc (`:784`), status poll (`:981`) |
| `0x08` | `CDB_STAT_RETRY` | **Never set.** Only a no-op case in `Cs2Exec` (`:1111`) |
| `0x09` | `CDB_STAT_ERROR` | `Cs2CopySectorData` / `Cs2MoveSectorData` on a bad partition index (`:2662, 2703`) |
| `0x0A` | `CDB_STAT_FATAL` | **Never set** |
| `0x20` | `CDB_STAT_PERI` | OR-ed in by the periodic report (`:1119`); cleared by a CR1 write (`:313`) |
| `0x40` | `CDB_STAT_TRNS` | **Never set** |
| `0x80` | `CDB_STAT_WAIT` | **Never set** |
| `0xFF` | `CDB_STAT_REJECT` | Passed to `doCDReport` on argument errors; also stuffed into CR1 by `Cs2GetSectorInfo` (`:2384`) |

Note that `CDB_STAT_REJECT` is generally passed *to `doCDReport`* rather than assigned to
`Cs2Area->status`, so the internal state is unchanged and only the reported CR1 shows the
rejection.

### 3.2 Backend status polling (3 Hz)

`Cs2Exec` runs `cdi->GetStatus()` whenever `_statuscycles >= _statustiming` (`cs2.c:964-990`).
`_statustiming` is set once, to `1000000` at reset (`cs2.c:884`), and `_statuscycles` advances
by `timing * 3` (`cs2.c:938`) — i.e. the units are 1/3 µs, so the poll period is
**333 333 µs ≈ 3 Hz**.

| `cdi->GetStatus()` | Meaning (`cdbase.c:180-197`) | Transition |
|---|---|---|
| 0 | disc present, spinning | if state is `NODISC` or `OPEN` → `PAUSE`, `isdiskchanged = 1` |
| 1 | disc present, not spinning | same as 0 |
| 2 | no disc | if state ≠ `NODISC` → `NODISC` |
| 3 | tray open | if state ≠ `OPEN` → `OPEN` |

`Cs2Reset` (`cs2.c:771-804`) initialises the report fields from the same three cases:

| Backend status | `status` | `FAD` | `options` | `repcnt` | `ctrladdr` | `track` | `index` |
|---|---|---|---|---|---|---|---|
| 0 or 1 | `PAUSE` | `150` | `0` | `0` | `0x41` | `1` | `1` |
| 2 | `NODISC` | `0xFFFFFFFF` | `0xFF` | `0xFF` | `0xFF` | `0xFF` | `0xFF` |
| 3 | `OPEN` | `0xFFFFFFFF` | `0xFF` | `0xFF` | `0xFF` | `0xFF` | `0xFF` |

`isdiskchanged` is set by `Cs2ChangeCDCore` (`:737`), `Cs2Reset` (`:811`) and the status poll
(`:975`); it is cleared **only** by `Cs2GetHardwareInfo` when the drive is neither `OPEN` nor
`NODISC` (`cs2.c:1489-1490`). It is consumed only by `Cs2InitializeCDSystem` to decide whether
to raise `DCHG` (`cs2.c:1648-1651`).

### 3.3 Periodic sector engine

The second timer in `Cs2Exec` fires when `_periodiccycles >= _periodictiming`
(`cs2.c:992-1125`); `_periodiccycles` also advances at `timing * 3`, so `_periodictiming` is in
1/3 µs. `Cs2SetTiming(int playing)` (`cs2.c:1155-1165`):

| Condition | `_periodictiming` | Real period | Sector rate |
|---|---|---|---|
| `playing` and (`isaudio` or `speed1x`) | `40000` | 13 333 µs | 75/s (1×) |
| `playing` and not 1× | `20000` | 6 667 µs | 150/s (2×) |
| not playing | `50000` | 16 667 µs | 60 Hz report tick |

Per tick, the state machine (`cs2.c:1000-1114`):

- `PAUSE`, `SCAN`, `RETRY`: nothing.
- `SEEK`: if `!isbufferfull` → `PLAY`, `options = 0x8` (`cs2.c:1101-1108`).
- `PLAY`: call `Cs2ReadFilteredSector(FAD, &playpartition)` (§5.4):
  - return `0` (sector read): `FAD++`, `track = Cs2FADToTrack(FAD)`, `cdi->ReadAheadFAD(FAD)`.
    - If a partition was produced: raise `CSCT`, set `isonesectorstored = 1`. If `isbufferfull`
      → `status = SEEK`, `options = 0` (back-pressure). If `FAD >= playendFAD`: either finish
      (`status = PAUSE`, `options = 0x8`, `Cs2SetTiming(0)`, raise `PEND`, and if
      `playtype == CDB_PLAYTYPE_FILE` also raise `EFLS` **and `EHST`**) or repeat
      (`FAD = playFAD`, `repcnt++` up to `0xE`).
    - If the sector was filtered out (`playpartition == NULL`): the same end-of-play/repeat
      logic runs but *without* the `options = 0x8` assignment and without the `EHST`
      (`cs2.c:1064-1089`).
  - return `-1` ("things weren't set up correctly") or `-2` ("do a read retry"): both are
    empty cases (`cs2.c:1091-1096`) — a read error is silently ignored and `FAD` does not
    advance, so playback stalls in place.
- Then, unless `_command` is set, `status |= CDB_STAT_PERI`, `doCDReport(status)` and
  **`SCDQ`** are issued (`cs2.c:1116-1124`). This is the "periodic response".

`Cs2GetTimeToNextSector` (`cs2.c:1137-1145`) computes `(_periodictiming - _periodiccycles + 2)/3`
µs, returning 0 unless the state is `PLAY`. **No caller anywhere in the tree** — see
§10 [DEAD 45].

### 3.4 Tray control outside the command set

`Cs2ForceOpenTray` (`cs2.c:908-913`) calls `cdi->SetStatus(CDCORE_OPEN)` then `Cs2Reset()`.
`Cs2ForceCloseTray(coreid, cdpath)` (`cs2.c:915-933`) re-inits the CD core, resets, optionally
quick-loads the game when `yabsys.emulatebios`, and re-reads the TOC. These are front-end hooks,
not reachable from the guest. The `0x05` OpenTray *command* does not touch the backend at all —
see §10 [QUIRK 21].

---

## 4. Sector buffers, partitions and filters

### 4.1 Blocks

`block_struct` (`cs2.h:55-64`):

```c
typedef struct {
  s32 size;        // -1 == free
  u32 FAD;
  u8  cn, fn, sm, ci;   // channel, file, submode, coding info (mode 2 subheader)
  u8  data[2352];
} block_struct;
```

There are exactly `MAX_BLOCKS` = 200 of them in a flat array `Cs2Area->block[200]`
(`cs2.h:222`), plus one oversized scratch `workblock` whose `data` is **2448** bytes
(`cs2.h:223-232`) so that a raw sector plus 96 bytes of subcode fits.

| Function | Line | Behaviour |
|---|---|---|
| `Cs2AllocateBlock(u8 *blocknum, s32 sectsize)` | `cs2.c:3328-3352` | Linear scan for `size == -1`; on success `blockfreespace--`, `block.size = sectsize`, `*blocknum = index`. If `blockfreespace <= 0` after the decrement → `isbufferfull = 1` and raise `BFUL`. If no free block is found → `isbufferfull = 1`, raise `BFUL`, return `NULL`. |
| `Cs2FreeBlock(block_struct *blk)` | `cs2.c:3356-3361` | `size = -1`, `blockfreespace++`, `isbufferfull = 0` unconditionally. |
| `Cs2SortBlocks(partition_struct *part)` | `cs2.c:3365-3383` | Compacts `part->block[]` (moves all non-NULL pointers to the front, NULL-fills the tail). Does **not** touch `part->blocknum[]`. |

`blockfreespace` is reset to `MAX_BLOCKS` by `Cs2Reset` (`:868`), `Cs2InitializeCDSystem` init
flag 0 (`:1609`), and nothing else. It is reported verbatim by `Cs2GetBufferSize`.

### 4.2 Partitions

`partition_struct` (`cs2.h:81-87`):

```c
typedef struct {
  s32 size;                        // -1 == "never used"; otherwise sum of block sizes
  block_struct *block[MAX_BLOCKS]; // 200 pointers PER PARTITION
  u8 blocknum[MAX_BLOCKS];
  u8 numblocks;
} partition_struct;
```

24 partitions (`MAX_SELECTORS`). Note the array is 200 entries *per partition* — one partition
can hold every block in the machine; there is no per-partition quota modelled.
`numblocks` is a `u8`, so a partition holding all 200 blocks is representable, but nothing
range-checks writes to `block[numblocks]`.

Reset state (`cs2.c:849-859`, and identically in `Cs2InitializeCDSystem` `:1590-1600` and
`Cs2ResetSelector` `:2249-2259`): `size = -1`, `numblocks = 0`, all `block[] = NULL`, all
`blocknum[] = 0xFF`.

### 4.3 Filters

`filter_struct` (`cs2.h:66-79`):

| Field | Set by | Meaning in `Cs2FilterData` |
|---|---|---|
| `FAD` (u32) | `SetFilterRange` (0x40) | Low bound of the FAD-range test |
| `range` (u32) | `SetFilterRange` (0x40) | Length of the FAD range |
| `mode` (u8) | `SetFilterMode` (0x44) | Condition enable bits, see below |
| `chan` (u8) | `SetFilterSubheaderConditions` (0x42) | Channel number to match |
| `smmask`, `smval` | 0x42 | Submode mask/value |
| `cimask`, `cival` | 0x42 | Coding-information mask/value |
| `fid` (u8) | 0x42 | File number to match |
| `condtrue` (u8) | `SetFilterConnection` (0x46) | Partition index used when the condition passes |
| `condfalse` (u8) | 0x46 | Filter index to fall through to; `0xFF` = drop the sector |

`mode` bits, as decoded in `Cs2FilterData` (`cs2.c:3412-3455`):

| Bit | Mask | Test |
|---|---|---|
| 0 | `0x01` | File number: `workblock.fn != fid` → fail |
| 1 | `0x02` | Channel number: `workblock.cn != chan` → fail |
| 2 | `0x04` | Submode: `(workblock.sm & smmask) != smval` → fail |
| 3 | `0x08` | Coding info: `(workblock.ci & cimask) != cival` → fail |
| 4 | `0x10` | Reverse the subheader result (`condresults ^= 1`) |
| 5 | `0x20` | **Not decoded** |
| 6 | `0x40` | FAD range: fail unless `FAD <= workblock.FAD < FAD + range` |
| 7 | `0x80` | Not a runtime condition — in `SetFilterMode` it means "initialize this filter" (`cs2.c:2097-2108`) |

Bits 0-4 are only evaluated for **mode 2 data sectors** (`workblock.data[0xF] == 0x02` and
`!isaudio`, `cs2.c:3407`). For mode 1 or audio, only the FAD-range test applies.

Filter reset state (`cs2.c:833-846`, repeated at `:1574-1587` and partially at `:2224-2235`):
`FAD = 0`, `range = 0xFFFFFFFF`, `mode/chan/smmask/cimask/fid/smval/cival = 0`,
`condtrue = i` (identity mapping filter *i* → partition *i*), `condfalse = 0xFF`.

### 4.4 Output connectors

Five pointers select which filter a data source feeds (`cs2.h:201-210`):

| Pointer | Number field | Written by |
|---|---|---|
| `outconcddev` | `outconcddevnum` | `SetCDDeviceConnection` (0x30, `cs2.c:1996-2000`), `Cs2ReadFile` (`:2865`), `Cs2ReadFileSystem` (`:3643`), `Cs2GetIP` (`:4022`) |
| `outconmpegrom` | `outconmpegromnum` | `Cs2GetMPEGRom` (`:3225-3226`) — hardwired to filter 0 |
| `outconmpegfb`, `outconmpegbuf`, `outconhost` | ditto | **Never written** except by savestate load (`cs2.c:4333-4355`) |

Only `outconcddev` participates in the live data path.

### 4.5 Sector flow, disc → CPU

```
 cdi->ReadSectorFAD(FAD, workblock.data)          cdbase.c:1630 / 1962
   → 2352 or 2448 raw bytes in Cs2Area->workblock.data
 Cs2ReadFilteredSector                            cs2.c:3934
   ├─ sync-header compare → isaudio                cs2.c:3951
   │    if audio: ScspReceiveCDDA(workblock.data); return, no buffering   cs2.c:3960
   ├─ workblock.size = getsectsize; if mode 2 form 2 → 2324               cs2.c:3948, 3967
   ├─ mode 2 → workblock.fn/cn/sm/ci = data[0x10..0x13]                   cs2.c:3969-3972
   └─ Cs2FilterData(outconcddev, isaudio)                                 cs2.c:3979
        ├─ walk the filter chain via condfalse until a condition passes   cs2.c:3401-3472
        │    lastbuffer := condtrue on pass, condfalse on fail            cs2.c:3459, 3465
        │    condfalse == 0xFF → return NULL (sector dropped)             cs2.c:3467-3468
        ├─ partition = &partition[condtrue]
        ├─ Cs2AllocateBlock(…, getsectsize)                               cs2.c:3475
        ├─ copy fn/cn/sm/ci/FAD/size from workblock                       cs2.c:3481-3486
        ├─ strip the sector down per workblock.size                       cs2.c:3489-3518
        └─ partition->size += block->size; partition->numblocks++         cs2.c:3521-3523
 Cs2Exec raises CSCT, sets isonesectorstored                              cs2.c:1026-1027
 ── guest issues 0x61/0x63 GetSectorData ──
 datatranstype/datatranspartition/datatranssectpos/datasectstotrans set   cs2.c:2491-2498
 HIRQ DRDY raised                                                          cs2.c:2501
 ── guest reads 0x05818000 (32-bit) repeatedly, or SCU-DMAs from it ──     cs2.c:375
 ── guest issues 0x06 EndDataTransfer ──                                   cs2.c:1670
```

The stripping table in `Cs2FilterData` (`cs2.c:3489-3518`), keyed on `workblock.size`:

| `workblock.size` | Source offset in the raw sector | Comment in source |
|---|---|---|
| 2048 | `+24` if `data[0xF] == 0x02`, else `+16` | m2f1 / m1 |
| 2324 | `+24` | m2f2 user data only |
| 2336 | `+16` | m2f2 skip sync+header |
| 2340 | `+12` | m2f2 skip sync |
| 2352 | `+0` | copy as-is |

`Cs2ReadUnFilteredSector` (`cs2.c:3847-3930`) is the bypass path used by the filesystem
commands and `Cs2GetIP`. It allocates into `Cs2GetPartition(outconcddev)` (which, see below,
just means `partition[outconcddev->condtrue]`), then applies a *different* stripping table
keyed on `getsectsize` (`cs2.c:3865-3907`) that additionally distinguishes mode 2 form 1 from
form 2 via `data[0x12] & 0x20` and sets `workblock.size = 2324` for form 2. It also copies the
subheader into the destination block only when the 12-byte sync pattern matches **and**
`data[0xF] == 0x02` (`cs2.c:3910-3917`).

`Cs2GetPartition` (`cs2.c:3387-3392`) is a stub: *"go through various filter conditions
here(fix me)"* — it returns `&partition[curfilter->condtrue]` unconditionally.

---

## 5. Command reference

`Cs2Execute` (`cs2.c:1182-1477`) switches on `CR1 >> 8`. The table below lists every opcode the
switch handles. "In" describes the fields the handler reads; "Out" the CR1-CR4 it writes;
"HIRQ" the bits it raises. `report` = the standard `doCDReport` layout of §2.1; `mpeg` = the
`doMPEGReport` layout of §2.2.

### 5.1 Status and drive control

| Code | Name | In | Out | HIRQ | Line |
|---|---|---|---|---|---|
| `0x00` | Get Status | — | `report(status)` | `CMOK` **via raw `\|=`, no SCU IRQ** | `1481` |
| `0x01` | Get Hardware Info | — | `CR1 = status<<8`; `CR2 = 0x0201`; `CR3 = mpgauth ? 1 : 0`; `CR4 = 0x0400` | `CMOK` | `1488` |
| `0x02` | Get TOC | — | `CR1 = status<<8` (pre-update); `CR2 = 0xCC`; `CR3 = CR4 = 0`. Reads the TOC from the backend into `TOC[102]`, arms `infotranstype = 0`, then sets `status = PAUSE` | `CMOK\|DRDY` | `1509` |
| `0x03` | Get Session Info | `CR1 & 0xFF` = session no. | sess 0: `CR3 = 0x0100 \| (TOC[101]>>16 & 0xFF)`, `CR4 = TOC[101] & 0xFFFF`; sess 1: `CR3 = 0x0100`, `CR4 = 0`; else `CR3 = CR4 = 0xFFFF`. Then `status = PAUSE`, `CR1 = status<<8`, `CR2 = 0` | `CMOK` | `1525` |
| `0x04` | Initialize CD System | `CR1 & 0xFF` = init flags (below) | `report(status)`; `status = PAUSE` and `FAD = 150` unless `OPEN`/`NODISC`; `isbufferfull = 0` | `CMOK\|ESEL` (+`DCHG` if `isdiskchanged`) | `1551` |
| `0x05` | Open Tray | — | `status = OPEN`; `report(status)` | `CMOK\|DCHG` | `1659` |
| `0x06` | End Data Transfer | — | if `cdwnum != 0`: `CR1 = (status<<8) \| ((cdwnum>>17)&0xFF)`, `CR2 = cdwnum>>1`, `CR3 = CR4 = 0`; else `CR1 = (status<<8)\|0xFF`, `CR2 = 0xFFFF`. Then `cdwnum = 0` | `CMOK`, plus `EHST` if `datatranstype` was 0 or 2 | `1670` |
| `0x10` | Play Disc | see §5.2 | `report(status)`, `status = SEEK` | `CMOK` | `1734` |
| `0x11` | Seek Disc | see §5.2 | `report(status)` | `CMOK` | `1846` |
| `0x12` | Scan Disc | — | none; only `status = SCAN` | `CMOK` | `1914` |
| `0x20` | Get Subcode Q/RW | `CR1 & 0xFF`: 0 = Q, 1 = RW | Q: `CR1 = status<<8`, `CR2 = 5`, `CR3 = CR4 = 0`, fills `transscodeq[10]`, `infotranstype = 3`. RW: `CR2 = 12`, `CR4 = group`, fills `transscoderw[24]`, `infotranstype = 4` | `CMOK\|DRDY` | `1923` |

`Cs2InitializeCDSystem` init-flag bits (`CR1 & 0xFF`, `cs2.c:1561-1643`):

| Bit | Mask | Effect in this source |
|---|---|---|
| 0 | `0x01` | Full software reset: clears `playFAD`/`playendFAD`/`playtype`/`maxrepeat`, `satauth`/`mpgauth`, all 24 filters, all 24 partitions, all 200 blocks, `blockfreespace = 200`, `curdir*`, `fileinfo[]`, `numfiles`, `lastbuffer = 0xFF`. **Does not** clear the TOC (the `memset` is commented out, `cs2.c:1612`) |
| 1 | `0x02` | Empty branch, comment *"Decode RW subcode"* |
| 2 | `0x04` | Empty branch, comment *"Don't confirm Mode 2 subheader"* |
| 3 | `0x08` | Empty branch, comment *"Retry reading Form 2 sectors"* |
| 4 | `0x10` | `speed1x = 1`, else `speed1x = 0` |

`Cs2GetSubcodeQRW` Q-channel payload (`cs2.c:1942-1951`), all BCD except byte 0:

```
[0] ctrladdr    [1] BCD(track)  [2] BCD(index)
[3..5] BCD(relative M,S,F)      [6] 0
[7..9] BCD(absolute M,S,F)
```

Relative position is `FAD - (TOC[track-1] & 0xFFFFFF)`; `Cs2FADToMSF` is
`m = v/4500, s = (v%4500)/75, f = v%75` (`cs2.c:3302-3309`). The RW channel copies 24 bytes
from `workblock.data[2352 + i + 24*group] & 0x3F`, where `group` is a function-`static`
counter that resets whenever `FAD` changes and otherwise increments per call
(`cs2.c:1959-1976`).

### 5.2 Playback and seek in detail

**`0x10` Play Disc** (`cs2.c:1734-1842`).

Arguments:

```
pdspos  = ((CR1 & 0xFF) << 16) | CR2      // start position, 24-bit
pdepos  = ((CR3 & 0xFF) << 16) | CR4      // end position, 24-bit
pdpmode =   CR3 >> 8                      // play mode
```

Start position decode:

| Condition | Action |
|---|---|
| `pdspos == 0xFFFFFF` or `pdpmode == 0xFF` | no change (source comment: *"This still isn't right"*) |
| `pdspos & 0x800000` | FAD mode: `playFAD = pdspos & 0xFFFFF`; `Cs2SetupDefaultPlayStats(Cs2FADToTrack(playFAD), 0)`; if `!(pdpmode & 0x80)` also `FAD = playFAD` |
| otherwise | Track mode: `pdspos == 0` is promoted to `0x0100` (track 1, index 0). If `!(pdpmode & 0x80)`: `Cs2SetupDefaultPlayStats(pdspos>>8, 1)` (which sets `FAD` to the track start), `playFAD = FAD`, `track = pdspos>>8`, `index = pdspos & 0xFF`. If `pdpmode & 0x80` ("preserve pickup position"): only `Cs2SetupDefaultPlayStats(pdspos>>8, 0)` |

Play mode: `pdpmode &= 0x7F`; if the result is not `0x7F`, `maxrepeat = pdpmode`. Any non-zero
mode logs *"Unsupported play mode"* (`cs2.c:1811-1814`) — nothing else in the mode byte is
implemented.

End position decode:

| Condition | `playendFAD` |
|---|---|
| `pdepos == 0xFFFFFF` | unchanged |
| `pdepos & 0x800000` | `playFAD + (pdepos & 0xFFFFF)` (a *length*, not an absolute FAD) |
| `pdepos != 0` | `Cs2TrackToFAD((u16)(pdepos \| 0x0063))` — end of the given track |
| `pdepos == 0` | `Cs2TrackToFAD(0xFFFF)` — the lead-out FAD |

Then: `Cs2SetTiming(1)`, `_periodiccycles = 0`, and a pseudo seek delay
`_periodictiming = clamp(abs(current_fad - FAD), 40000, SEEK_TIME)` where
`SEEK_TIME = 60000*5 = 300000` (`cs2.c:100, 1820-1829`). Finally `status = SEEK`, `options = 0`,
`playtype = CDB_PLAYTYPE_SECTOR` (1), `cdi->ReadAheadFAD(FAD)`, `doCDReport`, `CMOK`.

`Cs2Exec` releases `SEEK → PLAY` on the next periodic tick (`cs2.c:1101-1108`), and
`_periodictiming` snaps back to normal because `Cs2ReadFilteredSector` calls `Cs2SetTiming(1)`
on every sector (`cs2.c:3955, 3997`). A separate line resets timing when
`_periodictiming == SEEK_TIME` exactly (`cs2.c:995-997`).

**`0x11` Seek Disc** (`cs2.c:1846-1910`), four mutually exclusive forms:

| Condition | Action |
|---|---|
| `(CR1 & 0xFF) == 0x00 && CR2 == 0x0000` | **Stop**: `status = STANDBY`; `options/repcnt/ctrladdr/track/index = 0xFF`; `FAD = 0xFFFFFFFF` |
| `(CR1 & 0xFF) == 0xFF && CR2 == 0xFFFF` | **Pause**: `status = PAUSE` (nothing else) |
| `CR1 & 0x80` | **Seek by FAD**: `sdFAD = ((CR1 & 0x0F) << 16) \| CR2`; `status = PAUSE`; scan `TOC[0..15]` for the first entry whose FAD ≥ `sdFAD`, then `Cs2SetupDefaultPlayStats(i, 1)` and `FAD = sdFAD` |
| `CR2 >> 8 != 0` | **Seek by track/index**: `status = PAUSE`; `Cs2SetupDefaultPlayStats(CR2>>8, 1)`; `index = CR2 & 0xFF` |
| else | **Error**: same field wipe as Stop, `status = STANDBY` |

All forms end with `Cs2SetTiming(0)` (idle 60 Hz tick), `doCDReport(status)`, `CMOK`.

**Helper: FAD ↔ track** —
`Cs2FADToTrack(val)` (`cs2.c:3265-3276`) scans `TOC[0..98]`, returns `0xFF` on the first
`0xFFFFFFFF` entry, returns `i+1` when `TOC[i] <= val < TOC[i+1]` (masking each to 24 bits),
and `0` if it falls off the end.
`Cs2TrackToFAD(trackandindex)` (`cs2.c:3280-3298`) returns `TOC[101] & 0xFFFFFF` for `0xFFFF`
(lead-out); for index byte `0x01` returns `TOC[track-1] & 0xFFFFFF` (start of track); for index
byte `0x63` returns `(TOC[track] & 0xFFFFFF) - 1` (end of track); otherwise `0` ("assume it's
lead-in"). Source comment: *"really, we should be fetching subcode q's here"*.
`Cs2SetupDefaultPlayStats(track_number, writeFAD)` (`cs2.c:3313-3324`) is a no-op for
`track_number == 0xFF`; otherwise `options = 8`, `repcnt = 0`, `ctrladdr = TOC[track-1] >> 24`,
`index = 1`, `track = track_number`, and if `writeFAD` also `FAD = TOC[track-1] & 0xFFFFFF`.

### 5.3 Device-connection and filter commands

| Code | Name | In | Out | HIRQ | Line |
|---|---|---|---|---|---|
| `0x30` | Set CD Device Connection | `CR3>>8` = filter no. (`0xFF` = disconnect) | `report(status)`; sets `outconcddev`, `outconcddevnum` | `CMOK\|ESEL` | `1990` |
| `0x31` | Get CD Device Connection | — | **Dispatches `Cs2SetCDDeviceConnection` instead** (`cs2.c:1242`) | `CMOK\|ESEL` | `2008` (dead) |
| `0x32` | Get Last Buffer Destination | — | `CR1 = status<<8`; `CR2 = 0`; `CR3 = lastbuffer<<8`; `CR4 = 0` | `CMOK` | `2019` |
| `0x40` | Set Filter Range | `CR3>>8` = filter; `((CR1&0xFF)<<16)\|CR2` = FAD; `((CR3&0xFF)<<16)\|CR4` = range | `report(status)` | `CMOK\|ESEL` | `2029` |
| `0x41` | Get Filter Range | `CR3>>8` = filter | `CR1 = (status<<8)\|(FAD>>16)`; `CR2 = FAD & 0xFFFF`; `CR3 = range>>16`; `CR4 = range & 0xFFFF` | `CMOK` | `2044` |
| `0x42` | Set Filter Subheader Conditions | `CR1&0xFF`=chan; `CR2>>8`=smmask; `CR2&0xFF`=cimask; `CR3>>8`=filter; `CR3&0xFF`=fid; `CR4>>8`=smval; `CR4&0xFF`=cival | `report(status)` | `CMOK\|ESEL` | `2058` |
| `0x43` | Get Filter Subheader Conditions | `CR3>>8` = filter | Mirror of 0x42 | `CMOK\|ESEL` | `2076` |
| `0x44` | Set Filter Mode | `CR1&0xFF` = mode; `CR3>>8` = filter | `report(status)`. Mode bit 7 → zero `mode/FAD/range/chan/smmask/cimask/smval/cival` (note: `range` is zeroed, **not** restored to `0xFFFFFFFF`) | `CMOK\|ESEL` | `2090` |
| `0x45` | Get Filter Mode | `CR3>>8` = filter | `CR1 = (status<<8)\|mode`; `CR2 = CR3 = CR4 = 0` | `CMOK\|ESEL` | `2116` |
| `0x46` | Set Filter Connection | `CR1 & 1` → `condtrue = CR2>>8`; `CR1 & 2` → `condfalse = CR2&0xFF`; `CR3>>8` = filter | `report(status)` | `CMOK\|ESEL` | `2130` |
| `0x47` | Get Filter Connection | `CR3>>8` = filter | `CR1 = status<<8`; `CR2 = (condtrue<<8)\|condfalse`; `CR3 = CR4 = 0` | `CMOK` | `2153` |
| `0x48` | Reset Selector | `CR1&0xFF` = flags; `CR3>>8` = partition (when flags == 0) | `report(status)` | `CMOK`, then deferred `ESEL` after 450 µs | `2168` |

`Cs2ResetSelector` flag decode (`cs2.c:2168-2276`):

| `CR1 & 0xFF` | Effect |
|---|---|
| `== 0` | Reset **only** partition `CR3>>8`: free its blocks, `size = -1`, `numblocks = 0`. Then: `blockfreespace > 0` → `isbufferfull = 0`; `blockfreespace == MAX_BLOCKS` → `isonesectorstored = 0` and `datatranstype = INVALID`; else if the reset partition was the transfer partition → `datatranstype = INVALID`. Raises `CMOK\|ESEL` **immediately** and returns (no deferred IRQ) |
| bit 7 `0x80` | All filters `condfalse = 0xFF` |
| bit 6 `0x40` | All filters `condtrue = i` |
| bit 4 `0x10` | All filter conditions reset (`FAD = 0`, `range = 0xFFFFFFFF`, rest 0) |
| bit 3 `0x08` | Empty branch, comment *"reset partition output connectors"* |
| bit 2 `0x04` | `isbufferfull = 0`; all 24 partitions cleared; all 200 blocks freed (`size = -1`, data zeroed); `isonesectorstored = 0`; `datatranstype = INVALID`. **Note:** `blockfreespace` is *not* restored to 200 here |
| bits 5, 1, 0 | Not decoded |

### 5.4 Buffer / sector commands

| Code | Name | In | Out | HIRQ | Line |
|---|---|---|---|---|---|
| `0x50` | Get Buffer Size | — | `CR1 = status<<8`; `CR2 = blockfreespace`; `CR3 = MAX_SELECTORS<<8` (`0x1800`); `CR4 = MAX_BLOCKS` (`0x00C8`) | `CMOK` | `2280` |
| `0x51` | Get Sector Number | `CR3>>8` = partition | `CR4 = numblocks` (0 if `size == -1`); `CR1 = status<<8`; `CR2 = CR3 = 0` | `CMOK` | `2290` |
| `0x52` | Calculate Actual Size | `CR2` = sector offset; `CR3>>8` = partition; `CR4` = sector count | `report(status)`; `calcsize` computed | `CMOK`, then deferred `ESEL` after `30 × count` µs | `2309` |
| `0x53` | Get Actual Size | — | `CR1 = (status<<8) \| ((calcsize>>16)&0xFF)`; `CR2 = calcsize & 0xFFFF`; `CR3 = CR4 = 0` | `CMOK\|ESEL` | `2353` |
| `0x54` | Get Sector Info | `CR2&0xFF` = sector no.; `CR3>>8` = partition | `CR1 = (status<<8)\|(FAD>>16)`; `CR2 = FAD & 0xFFFF`; `CR3 = (fn<<8)\|cn`; `CR4 = (sm<<8)\|ci`. On a bad index: `CR1 = (0xFF<<8) \| (CR1 & 0xFF)` | `CMOK\|ESEL` | `2363` |
| `0x55` | Exec FAD Search | (ignored) | `report(status)` — *"finish me"* | `CMOK\|ESEL` | `2390` |
| `0x56` | Get FAD Search Results | — | nothing — *"finish me"* | `CMOK` | `2398` |
| `0x60` | Set Sector Length | `CR1&0xFF` = get-size code; `CR2>>8` = put-size code | `report(status)` | `CMOK\|ESEL` | `2405` |
| `0x61` | Get Sector Data | `CR2` = sector offset; `CR3>>8` = partition; `CR4` = sector count | `report(status)`; arms `datatranstype = GETSECTOR` | `CMOK\|DRDY`; on error `CMOK\|EHST` with `report(REJECT)` | `2460` |
| `0x62` | Delete Sector Data | same three arguments | `report(status)`; frees the sectors immediately | `CMOK`, then deferred `EHST` after `30 × count` µs; on error `CMOK\|EHST` + `report(REJECT)` | `2506` |
| `0x63` | Get Then Delete Sector Data | same three arguments | `report(status)`; arms `datatranstype = GETDELSECTOR` | `CMOK\|DRDY\|EHST`; on error `CMOK\|EHST` + `report(REJECT)` | `2560` |
| `0x64` | Put Sector Data | `CR3>>8` = partition; `CR4` = sector count | no CR update on success; arms `datatranstype = PUTSECTOR` and allocates `count` blocks of `putsectsize` | `CMOK\|DRDY`; not enough free blocks → `CMOK\|EHST`; bad partition → `report(REJECT)` + `CMOK\|EHST` | `2605` |
| `0x65` | Copy Sector Data | `CR1&0xFF` = dest partition; `CR2` = source offset; `CR3>>8` = source partition; `CR4&0xFF` = count | `report(status)` | `CMOK\|ECPY`; bad index → `status = ERROR`, `CMOK` only | `2655` |
| `0x66` | Move Sector Data | same as 0x65 | `report(status)` | `CMOK\|ECPY`; bad index → `status = ERROR`, `CMOK` only | `2695` |
| `0x67` | Get Copy Error | — | `CR1 = status<<8`; `CR2 = CR3 = CR4 = 0` — always "no error" | `CMOK` | `2735` |

Sector-length codes for `0x60` (`cs2.c:2406-2436`), applied independently to `getsectsize`
(from `CR1 & 0xFF`) and `putsectsize` (from `CR2 >> 8`):

| Code | Bytes |
|---|---|
| 0 | 2048 |
| 1 | 2336 |
| 2 | 2340 |
| 3 | 2352 |
| other | unchanged |

Both default to 2048 at reset (`cs2.c:810`).

Sector-range shorthand, applied by `CalcSectorOffsetNumber` (`cs2.c:2444-2456`) to commands
0x61/0x62/0x63:

- `sectoffset == 0xFFFF` → `sectoffset = numblocks - 1` (last sector). **The count is left
  as given**; the two cases are `else if`-chained, so `0xFFFF/0xFFFF` resolves only the offset.
- else if `sectnum == 0xFFFF` → `sectnum = numblocks - sectoffset` (to end of partition).

`Cs2GetSectorData` / `Cs2GetThenDeleteSectorData` reject with `report(CDB_STAT_REJECT)` and
`CMOK|EHST` when `bufno >= MAX_SELECTORS` or the partition is empty
(`cs2.c:2472-2486, 2570-2584`). `Cs2GetThenDeleteSectorData` notably does **not** set
`datatranspartitionnum` (compare `cs2.c:2494` with `2591`).

`Cs2CalculateActualSize` result units are **16-bit words**: it accumulates `block->size / 2`
(`cs2.c:2335`).

### 5.5 Filesystem commands

These operate on the ISO-9660 directory records the CD block parses itself. `dirrec_struct`
(`cs2.h:99-119`) is filled by `Cs2CopyDirRecord` (`cs2.c:3530-3629`), which walks the
little-endian half of each both-endian ISO field, handles the name-length padding byte, and
detects an XA record purely by *"the best way I can think of"*: a trailing 14 bytes
(`cs2.c:3599-3601`). `Cs2ReadFileSystem` (`cs2.c:3633-3824`) is the workhorse:

- `changeDirectory` with `fid == 0xFFFFFF`: reads **FAD 166** (= LBA 16, the PVD) via
  `Cs2ReadUnFilteredSector`, parses the root directory record at offset `0x9C`, sets
  `curdirsect = dirrec.lba`, `curdirsize = (dirrec.size / blocksectsize) - 1`,
  `curdirfidoffset = 0` (`cs2.c:3661-3688`).
- `changeDirectory` with a real fid: `curdirsect = fileinfo[fid - curdirfidoffset].lba - 150`
  (`cs2.c:3697`).
- `readDirectory`: `curdirfidoffset = fid - 2`, then skip forward through directory records
  until entry `fid` (`cs2.c:3653-3655, 3730-3766`).
- In all cases it then fills `fileinfo[0..1]` from the first two records and `fileinfo[2..255]`
  from the rest, adding 150 to every `lba` to convert LBA → FAD (`cs2.c:3715-3804`), spilling
  into the next directory sector when a record length of 0 is hit and `numsectorsleft > 0`.
- Every intermediate sector is freed from the partition and `Cs2SortBlocks` is run, so the
  filesystem walk leaves the partition as it found it (`cs2.c:3739-3758, 3806-3814`).

| Code | Name | In | Out | HIRQ | Line |
|---|---|---|---|---|---|
| `0x70` | Change Directory | `CR3>>8` = filter no.; `((CR3&0xFF)<<16)\|CR4` = fid (`0xFFFFFF` = root) | `report(status)`; on `0xFF` filter or a `Cs2ReadFileSystem` failure → `report(REJECT)` | `CMOK\|EFLS` | `2745` |
| `0x71` | Read Directory | `CR3>>8` = filter no.; `((CR3&0xFF)<<8)\|CR4` = fid offset (note: `<<8`, not `<<16`) | as above | `CMOK\|EFLS` | `2773` |
| `0x72` | Get File System Scope | — | `CR1 = status<<8`; `CR2 = numfiles - 2`; `CR3 = 0x0100`; `CR4 = 0x0002`. Comment: *"may need to fix this"* | `CMOK\|EFLS` | `2801` |
| `0x73` | Get File Info | `((CR3&0xFF)<<16)\|CR4` = fid | `0xFFFFFF`: `infotranstype = 2`, `CR2 = 0x05F4`. Else `Cs2SetupFileInfoTransfer(fid)`, `infotranstype = 1`, `CR2 = 0x06`. `CR1 = status<<8`, `CR3 = CR4 = 0` | `CMOK\|DRDY` | `2813` |
| `0x74` | Read File | `((CR1&0xFF)<<8)\|CR2` = sector offset; `CR3>>8` = filter no.; `((CR3&0xFF)<<8)\|CR4` = fid | `report(status)`. Sets `playFAD = FAD = fileinfo[fid].lba + offset`, `playendFAD = playFAD + ceil(size/getsectsize) - offset`, `maxrepeat = 0`, `options = 0x8`, `outconcddev = filter + filternum`, `status = PLAY`, `playtype = CDB_PLAYTYPE_FILE`, `Cs2SetTiming(1)` | `CMOK` | `2846` |
| `0x75` | Abort File | — | `report(status)`; `status = PAUSE` unless `OPEN`/`NODISC`; `isonesectorstored = 0`; `datatranstype = INVALID`; `cdwnum = 0` | `CMOK\|EFLS` | `2877` |

`Cs2SetupFileInfoTransfer(fid)` (`cs2.c:3828-3843`) builds the 12-byte record read out through
the info port:

```
[0..3]  lba (big-endian)
[4..7]  size (big-endian)
[8]     interleavegapsize
[9]     fileunitsize
[10]    fid (truncated to 8 bits)
[11]    flags
```

### 5.6 MPEG commands

Every one of these is a stub that updates internal fields at most and answers with
`doMPEGReport`. None of them decodes MPEG data; the MPEG "status" fields never change.

| Code | Name | In | Out | HIRQ | Line |
|---|---|---|---|---|---|
| `0x90` | MPEG Get Status | — | `mpeg` | `CMOK\|MPCM` | `2890` |
| `0x91` | MPEG Get Interrupt | — | `CR1 = (status<<8)\|((int>>16)&0xFF)`, `CR2 = int & 0xFFFF`, `CR3 = CR4 = 0`; `int` is hardcoded to 0 then ANDed with `mpegintmask` | `CMOK\|MPCM` | `2897` |
| `0x92` | MPEG Set Interrupt Mask | `((CR1&0xFF)<<16)\|CR2` | `mpeg` | `CMOK\|MPCM` | `2916` |
| `0x93` | MPEG Init | `CR2` | `CR1 = mpgauth ? status<<8 : 0xFF00`; `CR2 = CR3 = CR4 = 0` | `CR2 == 1`: `CMOK\|MPCM\|MPED\|MPST`; else `CMOK\|MPED\|MPST` | `2926` |
| `0x94` | MPEG Set Mode | `CR1&0xFF`=vidplaymode, `CR2>>8`=dectimingmode, `CR2&0xFF`=outmode, `CR3>>8`=slmode; `0xFF` means "leave alone" | `mpeg` | `CMOK\|MPCM` | `2948` |
| `0x95` | MPEG Play | — | `mpeg` — *"fix me"* | `CMOK\|MPCM` | `2972` |
| `0x96` | MPEG Set Decoding Method | — | `mpeg` — *"fix me"* | `CMOK\|MPCM` | `2981` |
| `0x9A` | MPEG Set Connection | `CR3>>8` = 0 (current) / non-zero (next); `CR1&0xFF`=audcon, `CR2>>8`=audlay, `CR2&0xFF`=audbufnum, `CR3&0xFF`=vidcon, `CR4>>8`=vidlay, `CR4&0xFF`=vidbufnum | `mpeg` | `CMOK\|MPCM` | `2990` |
| `0x9B` | MPEG Get Connection | `CR3>>8` selects current/next | mirror of 0x9A | `CMOK\|MPCM` | `3020` |
| `0x9D` | MPEG Set Stream | `CR3>>8` selects current/next; `audstm/audstmid/audchannum/vidstm/vidstmid/vidchannum` laid out like 0x9A | `mpeg` | `CMOK\|MPCM` | `3045` |
| `0x9E` | MPEG Get Stream | `CR3>>8` selects | mirror of 0x9D | `CMOK\|MPCM` | `3075` |
| `0xA0` | MPEG Display | — | `mpeg` — *"fix me"* | `CMOK\|MPCM` | `3100` |
| `0xA1` | MPEG Set Window | — | `mpeg` — *"fix me"* | `CMOK\|MPCM` | `3109` |
| `0xA2` | MPEG Set Border Color | — | `mpeg` — *"fix me"* | `CMOK\|MPCM` | `3119` |
| `0xA3` | MPEG Set Fade | — | `mpeg` — *"fix me"* | `CMOK\|MPCM` | `3128` |
| `0xA4` | MPEG Set Video Effects | — | `mpeg` — *"fix me"* | `CMOK\|MPCM` | `3137` |
| `0xAF` | MPEG Set LSI | — | nothing — *"fix me"* | `CMOK\|MPCM` | `3146` |

`cs2.h:366-386` names further MPEG opcodes as comments — `0x97` MPEG Out Decoding Sync,
`0x98` Get Timecode, `0x99` Get Pts, `0x9C` Change Connection, `0x9F` Get Picture Size,
`0xA5`-`0xAA` Get/Set/Read/Write Image and Read/Write Sector, `0xAE` Get LSI — **none of which
appear in the dispatch switch**. They fall through to `default:` and hang (§10 [QUIRK 6]).

### 5.7 Authentication and MPEG ROM

| Code | Name | In | Out | HIRQ | Line |
|---|---|---|---|---|---|
| `0xE0` | Authenticate Device | `CR2 & 0xFF`: 1 = MPEG card, else Saturn disc | With a disc: sets `status = BUSY`, `CR1 = (BUSY<<8)\|0xFF`, `CR2 = CR3 = CR4 = 0xFFFF`; then MPEG path → `MPED`, `mpgauth = 2`; disc path → `isonesectorstored = 1`, `EFLS\|CSCT`, `satauth = 4`. Then `status = PAUSE` and `report(status)` overwrites CR1-CR4 anyway. With no disc / tray open the same HIRQ bits are raised but the auth variables are not set | `CMOK` plus the above | `3153` |
| `0xE1` | Is Device Authenticated | `CR2 != 0` → report `mpgauth`, else `satauth` | `CR1 = status<<8`; `CR2 = auth value`; `CR3 = CR4 = 0` | `CMOK` | `3204` |
| `0xE2` | Get MPEG ROM | `((CR1&0xFF)<<8)\|CR2` = read offset in sectors; `CR4` = sector count | `report(status)`. Sets `mpgauth \|= 0x300`, forces `outconmpegrom = filter[0]`, opens the host file at `Cs2Area->mpegpath`, `fseek(offset * getsectsize)` and streams `count` sectors into `partition[filter[0].condtrue]` | `CMOK\|MPED`, plus `CSCT` if any sector was stored | `3217` |

Authentication is unconditional — the source comment reads *"if authentication passes(obviously
it always does)"* (`cs2.c:3177`). `satauth` and `mpgauth` reset to 0 (`cs2.c:829-830`) and are
also cleared by `InitializeCDSystem` init flag bit 0 (`cs2.c:1570-1571`).

### 5.8 Full opcode index

| Code | Handler | Implemented? |
|---|---|---|
| `0x00`-`0x06` | GetStatus, GetHardwareInfo, GetToc, GetSessionInfo, InitializeCDSystem, OpenTray, EndDataTransfer | yes |
| `0x07`-`0x0F` | — | no (hangs) |
| `0x10`-`0x12` | PlayDisc, SeekDisc, ScanDisc | yes / yes / stub |
| `0x13`-`0x1F` | — | no (hangs) |
| `0x20` | GetSubcodeQRW | yes |
| `0x21`-`0x2F` | — | no (hangs) |
| `0x30`-`0x32` | SetCDDeviceConnection, GetCDDeviceConnection, GetLastBufferDestination | yes / **misrouted** / yes |
| `0x33`-`0x3F` | — | no (hangs) |
| `0x40`-`0x48` | Filter set/get × 4 + ResetSelector | yes |
| `0x49`-`0x4F` | — | no (hangs) |
| `0x50`-`0x56` | GetBufferSize, GetSectorNumber, CalculateActualSize, GetActualSize, GetSectorInfo, ExecFadSearch, GetFadSearchResults | yes (last two are stubs) |
| `0x57`-`0x5F` | — | no (hangs) |
| `0x60`-`0x67` | SetSectorLength, Get/Delete/GetThenDelete/Put/Copy/Move SectorData, GetCopyError | yes |
| `0x68`-`0x6F` | — | no (hangs) |
| `0x70`-`0x75` | ChangeDirectory, ReadDirectory, GetFileSystemScope, GetFileInfo, ReadFile, AbortFile | yes |
| `0x76`-`0x8F` | — | no (hangs) |
| `0x90`-`0x96` | MPEG status/interrupt/mask/init/mode/play/decode | stubs |
| `0x97`-`0x99` | named in `cs2.h` only | no (hangs) |
| `0x9A`, `0x9B`, `0x9D`, `0x9E` | MPEG connection/stream | stubs |
| `0x9C`, `0x9F` | named in `cs2.h` only | no (hangs) |
| `0xA0`-`0xA4` | MPEG display/window/border/fade/effects | stubs |
| `0xA5`-`0xAE` | named in `cs2.h` only | no (hangs) |
| `0xAF` | MPEG Set LSI | stub |
| `0xB0`-`0xDF` | — | no (hangs) |
| `0xE0`-`0xE2` | AuthenticateDevice, IsDeviceAuthenticated, GetMPEGRom | yes |
| `0xE3`-`0xFF` | — | no (hangs) |

---

## 6. Disc image abstraction (`cdbase.c`)

### 6.1 The `CDInterface` vtable

`cdbase.h:61-72`:

```c
typedef struct {
   int id;
   const char *Name;
   int  (*Init)(const char *);
   void (*DeInit)(void);
   int  (*GetStatus)(void);
   s32  (*ReadTOC)(u32 *TOC);
   int  (*ReadSectorFAD)(u32 FAD, void *buffer);
   void (*ReadAheadFAD)(u32 FAD);
   void (*SetStatus)(int status);
} CDInterface;
```

Core IDs (`cdbase.h:51-55`): `CDCORE_DEFAULT -1`, `CDCORE_DUMMY 0`, `CDCORE_ISO 1`,
`CDCORE_ARCH 2`, `CDCORE_CHD 3`. `cdbase.c` defines `DummyCD` (`:126-136`) and `ISOCD`
(`:146-156`); `ArchCD` is only declared (`cdbase.h:85`).

Status codes returned by `GetStatus` (`cdbase.h:57-59` plus the authoritative comment at
`cdbase.c:180-197`): `0` = disc present and spinning, `1` = disc present, not spinning,
`2` = `CDCORE_NODISC`, `3` = `CDCORE_OPEN` (tray open).

`Cs2ChangeCDCore` (`cs2.c:698-742`) scans the port-supplied `CDCoreList[]`, falls back to
`DummyCD` if the requested core is missing or its `Init` fails, then sets `isdiskchanged = 1`,
`status = PAUSE`, and calls `SmpcRecheckRegion()`.

### 6.2 The Dummy core

- `GetStatus` returns the file-static `dmy_status`, initialised to `2` (no disc)
  (`cdbase.c:158, 196`).
- `SetStatus` accepts only `3`; anything else is forced back to `2` (`cdbase.c:199-205`), so
  the dummy drive can never contain a disc.
- `ReadTOC` returns 0 and **does not write the caller's TOC** (`cdbase.c:209-246`).
- `ReadSectorFAD` `memset`s **2352** bytes (not 2448) and returns success
  (`cdbase.c:250-267`).
- `ReadAheadFAD` is a no-op with a long explanatory comment (`cdbase.c:271-286`).

### 6.3 TOC format

The canonical description lives in the `DummyCDReadTOC` comment (`cdbase.c:209-246`). The TOC
is 102 × `u32` = 408 bytes:

| Index | Contents |
|---|---|
| `TOC[0]`-`TOC[98]` | Tracks 1-99. Bits 0-23 = track start FAD; bits 24-27 = ADR; bits 28-31 = CTL. Unused tracks = `0xFFFFFFFF` |
| `TOC[99]` | Point A0. Bits 0-7 = PFRAME (always 0); bits 8-15 = PSEC = program-area format (`0x00` CDDA/CD-ROM, `0x10` CD-I, `0x20` CD-ROM XA); bits 16-23 = PMIN = first track number; bits 24-27 ADR; bits 28-31 CTL |
| `TOC[100]` | Point A1. PFRAME 0, PSEC 0, bits 16-23 = last track number, bits 24-31 ADR/CTL |
| `TOC[101]` | Point A2. Bits 0-23 = lead-out FAD, bits 24-31 ADR/CTL |

(The comment writes "bits 7 - 15" for PSEC; the surrounding fields make bits 8-15 the only
consistent reading.)

`LBA + 150 = FAD` throughout (`cdbase.c:243, 256`).

The ISO core builds the TOC in `BuildTOC` (`cdbase.c:1457-1471`) from session 0 only:

```c
for (i = 0; i < session->track_num; i++)
   isoTOC[i] = (track->ctl_addr << 24) | track->fad_start;
isoTOC[99]  = (isoTOC[0] & 0xFF000000) | 0x010000;
isoTOC[100] = (isoTOC[track_num-1] & 0xFF000000) | (track_num << 16);
isoTOC[101] = (isoTOC[track_num-1] & 0xFF000000) | session->fad_end;
```

`isoTOC` is pre-filled with `0xFF` for all 408 bytes in `ISOCDInit` (`cdbase.c:1484`), so unused
track slots read back as `0xFFFFFFFF` as required. `ctl_addr` packs CTL in the high nibble and
ADR in the low nibble: `0x41` = data track (CTL 4, ADR 1), `0x01` = audio track (CTL 0, ADR 1).
`ISOCDReadTOC` `memcpy`s exactly `0xCC * 2 = 408` bytes and returns 408 (`cdbase.c:1620-1624`).

### 6.4 Track / session / disc model

```c
typedef struct {              // cdbase.c:292-311
   u8  ctl_addr;              // CTL<<4 | ADR
   u32 fad_start, fad_end;    // inclusive FAD range covered by this track
   u32 file_offset;           // byte offset of fad_start within the backing file
   u32 sector_size;           // 2048 / 2324 / 2336 / 2352 / 2448
   FILE *fp;  int file_size;  int file_id;
   int interleaved_sub;       // 2448-byte tracks with P-W interleaved subcode
   char *filename;
   u32 frames, extraframes, pregap, postgap;      // CHD only
   u32 physframeofs, chdframeofs, logframeofs;    // CHD only
} track_info_struct;

typedef struct {              // cdbase.c:313-319
   u32 fad_start, fad_end;
   track_info_struct *track;
   int track_num;
} session_info_struct;

typedef struct {              // cdbase.c:321-325
   int session_num;
   session_info_struct *session;
} disc_info_struct;
```

There is one file-static `disc` (`cdbase.c:412`), one `isoTOC[102]` (`:411`), one
`iso_cd_status` (`:413`) and one `imgtype` (`:410`, enum at `:409`:
`IMG_NONE, IMG_ISO, IMG_BINCUE, IMG_MDS, IMG_CCD, IMG_CHD, IMG_NRG`).

`ISOCDInit` (`cdbase.c:1478-1566`) selects the loader by file extension, falling back to a CHD
magic test and then to raw ISO:

| Extension / test | Loader | Line |
|---|---|---|
| `.CUE` | `LoadBinCue` → `LoadBinCueMultiFile` if a second `FILE` line appears | `1519` / `527` / `736` |
| `.MDS` **and** header starts `"MEDIA "` | `LoadMDS` → `LoadMDSTracks` | `1525` / `1064` / `901` |
| `.CCD` | `LoadCCD` (+ `LoadParseCCD` INI parser) | `1531` / `1308` / `1205` |
| `.CHD`, or `checkCHD()` succeeds | `LoadCHD` | `1537` / `1754` |
| anything else | `LoadISO` | `1549` / `1125` |

`LoadISO` (`cdbase.c:1125-1172`) fabricates a single data track: `ctl_addr = 0x41`,
`fad_start = 150`, `file_offset = 0`; `sector_size` is 2048 if the file length is a multiple of
2048, else 2352 if a multiple of 2352, else the load fails;
`fad_end = 150 + file_size / sector_size`.

`LoadBinCue` (`cdbase.c:527-733`) parses `TRACK`/`INDEX`/`PREGAP`/`POSTGAP`:
`MODE1`/`MODE2` → `ctl_addr = 0x41` and `sector_size = atoi(type+6)` (so `MODE1/2352` yields
2352); `AUDIO` → `ctl_addr = 0x01`, `sector_size = 2352`. `INDEX 1` sets
`fad_start = MSF_TO_FAD(m,s,f) + pregap + 150` and
`file_offset = MSF_TO_FAD(m,s,f) * sector_size`, where
`MSF_TO_FAD(m,s,f) = m*4500 + s*75 + f` (`cdbase.c:417`). Each track's `fad_end` becomes
`next.fad_start - 1`, except the last, which is recomputed from the file size
(`cdbase.c:704-712`).

`LoadBinCueMultiFile` (`cdbase.c:736-897`) additionally tracks one `FILE` per track and
accumulates a running `fad`, with an explicit distinction: data tracks other than the first
take their start from `INDEX 0`, audio tracks from `INDEX 1` (`cdbase.c:840-853`).

`LoadMDS`/`LoadMDSTracks` (`cdbase.c:1064`, `901`) is the only loader that can produce
`session_num > 1`. It reads the packed on-disk structs at `cdbase.c:327-389`, rejects DVDs
(`medium_type & 0x10`) and MDS version > 1, nibble-swaps `addr_ctl`
(`((ctl<<4)|(ctl>>4)) & 0xFF`, `cdbase.c:1041`), sets `fad_start = start_sector + 150`, uses the
`0xA2` pseudo-track to derive the lead-out, and sets `interleaved_sub` from `subchannel_mode`.

`LoadCCD` (`cdbase.c:1308-1453`) parses the CloneCD INI, refuses `Sessions != 1` and
`DataTracksScrambled`, and builds tracks from `Point 1..99` entries with
`ctl_addr = (Control<<4) | ADR`, `fad_start = MSF_TO_FAD(PMin,PSec,PFrame)`,
`file_offset = PLBA*2352`, `sector_size = 2352`. `Point 0xA2` supplies the session lead-out.

`LoadCHD` (`cdbase.c:1754-1959`) reads `CDROM_TRACK_METADATA[2]` entries, maps the MAME track
type strings to `ctl_addr`/`sector_size`:

| CHD track type | `ctl_addr` | `sector_size` |
|---|---|---|
| `MODE1`, `MODE1/2048`, `MODE2_FORM1`, `MODE2/2048` | `0x41` | 2048 |
| `MODE1_RAW`, `MODE1/2352`, `MODE2_RAW`, `MODE2/2352` | `0x41` | 2352 |
| `MODE2`, `MODE2/2336`, `MODE2_FORM_MIX` | `0x41` | 2336 |
| `MODE2_FORM2`, `MODE2/2324` | `0x41` | 2324 |
| `AUDIO` | `0x01` | 2352 |

and then builds three parallel frame numberings — `logframeofs` (the FAD space, starting at
150), `physframeofs`, and `chdframeofs` (which includes `extraframes` padding to the 4-frame
`CD_TRACK_PADDING` boundary) — so that a FAD can be translated to a CHD hunk offset. The CHD
frame size is `2352 + 96 = 2448` (`cdbase.c:1726-1728`).

### 6.5 Sector reads

`ISOCDReadSectorFAD(FAD, buffer)` (`cdbase.c:1630-1713`):

1. Delegates to `ISOCDReadSectorFADFromCHD` when `imgtype == IMG_CHD`.
2. `memset(buffer, 0, 2448)` — the caller's buffer must therefore be at least 2448 bytes.
   `Cs2Area->workblock.data` is exactly 2448 (`cs2.h:231`) and is the only buffer ever passed.
3. Linear search over sessions × tracks for `fad_start <= FAD <= fad_end`, caching the result
   in the file-global `currentTrack`.
4. `offset = file_offset + (FAD - fad_start) * sector_size`, `fseek`, then:

| `sector_size` | Read behaviour |
|---|---|
| 2448, `!interleaved_sub` | one `fread` of 2448 bytes (2352 data + 96 subcode) |
| 2448, `interleaved_sub` | 2352 data, then three 96-byte subcode groups (skipping 2352 bytes between them) de-interleaved through a 96-entry offset table (`cdbase.c:1679-1699`) into `buffer[2352..2447]` |
| 2352 | one `fread` of 2352 bytes. Comment: *"Generate subcodes here"* — **not implemented** |
| 2048 | `memcpy(buffer, syncHdr, 12)` then `fread(buffer+0x10, 2048)`. Bytes `0x0C`-`0x0F` (MIN/SEC/FRAME/MODE) are left **zero** |
| anything else | nothing is read |

`syncHdr` is `00 FF FF FF FF FF FF FF FF FF FF 00` (`cdbase.c:408`) — the same 12 bytes
`cs2.c` compares against to distinguish data from audio (`cs2.c:3849-3850, 3935-3936`).

Consequence of the 2048-byte case: `workblock.data[0xF]` (the mode byte) is `0`, so `cs2.c`
treats every such sector as **mode 1** (`cs2.c:3492, 3868, 3964`) and strips at `+16`, which is
the correct offset for the synthesized layout — but a genuine mode 2 form 1 track stored as
2048-byte sectors is indistinguishable from mode 1, and no subheader is ever available for it.

`ISOCDReadSectorFADFromCHD` (`cdbase.c:1962-2026`) maps `FAD → logframeofs → physframeofs →
chdframeofs`, computes `hunkid = (chdlba * 2448) / hunkbytes` and
`hunk_offset = (chdlba * 2448) % hunkbytes`, re-reads the hunk if it changed, then copies
`sector_size` bytes — byte-swapping pairwise for audio tracks (`ctl_addr == 0x01`) and
prepending `syncHdr` for 2048-byte tracks.

### 6.6 CDDA

Audio sectors never enter a partition. `Cs2ReadFilteredSector` detects them by sync-header
mismatch, forwards the raw 2352 bytes to `ScspReceiveCDDA(workblock.data)`, sets
`isaudio = 1`, forces `Cs2SetTiming(1)` (which selects the 1× rate because `isaudio` is set),
and returns `0` with `*partition = NULL` (`cs2.c:3951-3963`, duplicated at `3993-4005`).

### 6.7 IP.BIN parsing (`Cs2GetIP`)

`Cs2GetIP(autoregion)` (`cs2.c:4018-4143`) forces `outconcddev = filter[0]`, reads FAD 150
(LBA 0) unfiltered, and if the sector begins with `"SEGA SEGASATURN"` fills `cdip`
(`cs2.h:271-287`):

| `cdip` field | Sector offset | Length |
|---|---|---|
| `system` | `0x00` | 16 |
| `company` | `0x10` | 16 |
| `itemnum` | `0x20` | 10 (also packed into `gameid` as a little-endian u64 of the first 8 chars) |
| `version` | `0x2A` | 6 |
| `date` | `0x30`-`0x37` | reformatted as `DD/MM/YYYY` from `buf[0x34..0x37]` + `buf[0x30..0x33]` |
| `cdinfo` | `0x38` | 8 |
| `region` | `0x40` | 10 |
| `peripheral` | `0x50` | 16 |
| `gamename` | `0x60` | 112 |
| `ipsize` | `0xE0` | u32 BE |
| `msh2stack` | `0xE8` | u32 BE |
| `ssh2stack` | `0xEC` | u32 BE |
| `firstprogaddr` | `0xF0` | u32 BE |
| `firstprogsize` | `0xF4` | u32 BE |

Region autodetect from `region[0]` (`cs2.c:4100-4127`): `J`→1, `T`→2, `U`→4, `B`→5, `K`→6,
`A`→0xA, `E`→0xC, `L`→0xD, anything else → 0. The sector's block is freed and the partition
re-sorted before returning.

---

## 7. Timing model summary

| Counter | Units | Set by | Consumed by |
|---|---|---|---|
| `timing` argument | µs | `yabause.c:789, 835` | `Cs2Exec` |
| `_statuscycles` / `_statustiming` | 1/3 µs | `+= timing*3` / fixed `1000000` (`cs2.c:938, 884`) | Backend status poll, ≈3 Hz |
| `_periodiccycles` / `_periodictiming` | 1/3 µs | `+= timing*3` / `Cs2SetTiming` (`cs2.c:939, 1155`) | Sector engine + periodic report |
| `_commandtiming` | µs | `Cs2SetCommandTiming` on the CR4 write: 250 for `0x02`, 50 otherwise (`cs2.c:1169-1178`) | Delay before `Cs2Execute` |
| `_command_execlock` | µs | 450 (ResetSelector), 30×N (CalculateActualSize, DeleteSectorData) | Blocks command execution; fires `_delay_irq` on expiry |
| `SEEK_TIME` | 1/3 µs | `#define SEEK_TIME (60000*5)` = 300000 (`cs2.c:100`) | Upper clamp on the fake seek delay |

---

## 8. Savestate format

`Cs2SaveState` writes chunk `"CS2 "` version 3 (`cs2.c:4154-4266`). Notable properties:

- The whole `blockregs_struct` is written raw, including the unused `DTR`/`UNKNOWN` fields.
- All 200 `block_struct`s are written in full (`sizeof(block_struct) * 200` ≈ 470 KB).
- Partition `block[]` **pointers** are converted to indices into `Cs2Area->block[]`, with
  `0xFFFFFFFF` for NULL (`cs2.c:4230-4238`), and restored the same way (`4375-4385`).
- The five output-connector pointers are stored as filter numbers and rebuilt on load, with
  `0xFF` meaning NULL (`cs2.c:4207-4211, 4328-4355`).
- `_periodictiming` is stored divided by 3 and reconstructed as `((temp * 3) / 10) * 10`
  (*"Derive the actual, accurate value (always a multiple of 10)"*, `cs2.c:4203-4205, 4320-4325`).
- Version gating: `isaudio` only if version > 1; `_command_execlock` only if version > 2.
- **Not saved**: `_statuscycles`, `_statustiming`, `_periodiccycles`, `_delay_irq`,
  `curdirsize`, `curdirfidoffset`, `numfiles`, `isaudio` on v1, the `static` `lastfad`/`group`
  in `Cs2GetSubcodeQRW`, and the entire `cdip` / `disc` / `isoTOC` state in `cdbase.c`.

---

## 9. Constants quick reference

```
MAX_BLOCKS      200          sector buffers                            cs2.h:51
MAX_SELECTORS    24          filters and partitions                    cs2.h:52
MAX_FILES       256          cached directory records                  cs2.h:53
SEEK_TIME    300000          1/3 µs, seek-delay clamp                  cs2.c:100
ToBCD(v)  ((v%10)+((v/10)<<4))                                          cs2.c:98
MSF_TO_FAD(m,s,f)  (m*4500 + s*75 + f)                                  cdbase.c:417
LBA + 150 == FAD                                                        cdbase.c:243
sync header  00 FF FF FF FF FF FF FF FF FF FF 00                        cdbase.c:408
sector layout used by cs2.c:
  [0x00..0x0B] sync    [0x0C..0x0E] MIN/SEC/FRAME    [0x0F] mode
  mode 2 subheader: [0x10] fn  [0x11] cn  [0x12] sm  [0x13] ci
  form 2 flag: data[0x12] & 0x20                                        cs2.c:3871, 3967
```

---

## 10. Known deviations, bugs and gaps in this implementation

### Register interface

1. **[QUIRK]** Byte accesses to `0x05800000-0x058FFFFF` never reach the CD block. They are
   forwarded to `CartridgeArea->Cs2ReadByte`/`WriteByte` (`cs2.c:143-153`), which is
   `DummyCs2ReadByte` returning a constant `0xFF` (`cs0.c:135-138`) unless a Netlink/JapModem
   cartridge is installed (`cs0.c:1222, 1261`). A guest doing byte-wide register access sees
   `0xFF` for every CD block register.
2. **[QUIRK]** Access-width decoding is asymmetric and absolute: the data port `0x05818000` is
   decoded **only** for 32-bit accesses (`cs2.c:375, 452`), the info port `0x05898000` **only**
   for 16-bit reads (`cs2.c:198`). Any other width falls through to the "Undocumented register"
   log and returns 0. There is no 16-bit path to the data FIFO and no write path to the info
   port.
3. **[DEAD]** The `HIRQ` read handler contains a dead read-modify-write and a commented-out
   block that once derived `BFUL`/`DCHG`/`CSCT` from `isbufferfull`/`isdiskchanged`/
   `isonesectorstored` (`cs2.c:164-181`, `346-364`). Those three internal booleans are
   maintained but never reflected in `HIRQ` on read.
4. **[QUIRK]** `CMOK` is never cleared by the CD block. The clear-on-command-start line is
   commented out (`cs2.c:1185`). Software polling for a `CMOK` edge must clear it itself.
5. **[BUG]** `Cs2GetStatus` sets `CMOK` with a bare `reg.HIRQ |= CDB_HIRQ_CMOK` instead of
   `Cs2SetIRQ` (`cs2.c:1483`), so opcode `0x00` — uniquely among all commands — never raises the
   SCU external interrupt.
6. **[QUIRK]** Unimplemented opcodes fall into `default:` which only logs
   (`cs2.c:1473-1475`). No CR update, no `CMOK`, no interrupt — the guest's command never
   completes. This covers every code not listed in §5.8, including nine MPEG opcodes that
   `cs2.h:366-386` explicitly names.
7. **[DEAD]** `blockregs_struct.DTR` and `.UNKNOWN` (`cs2.h:151-152`) are never read or written;
   they exist only to occupy savestate bytes.
8. **[DEAD]** `cs0.c`'s `DummyCs2ReadWord`/`ReadLong`/`WriteWord`/`WriteLong` (`cs0.c:142-170`)
   are unreachable: `memory.c:657-662` wires word and long accesses straight to the `cs2.c`
   handlers, bypassing `CartridgeArea` entirely.

### Command dispatch

9. **[BUG]** Opcode `0x31` (Get CD Device Connection) calls `Cs2SetCDDeviceConnection`
   (`cs2.c:1242`). The correct handler `Cs2GetCDDeviceConnection` (`cs2.c:2008-2015`) is never
   invoked. A guest issuing `0x31` will silently *rewrite* the CD device connection from
   whatever happens to be in `CR3`.
10. **[BUG]** Filter-index bound checks use `< 0x24` (36 decimal) although `MAX_SELECTORS` is
    24 (`cs2.c:1997`, `2756`, `2784`). Filter numbers 24-35 index past `filter[24]`. This looks
    like a decimal/hex confusion between `24` and `0x24`.
11. **[BUG]** Most filter commands do not bound-check at all: `Cs2SetFilterRange` (`:2032`),
    `GetFilterRange` (`:2047`), `SetFilterSubheaderConditions` (`:2061`),
    `GetFilterSubheaderConditions` (`:2079`), `SetFilterMode` (`:2093`), `GetFilterMode`
    (`:2119`), `SetFilterConnection` (`:2133`), `GetFilterConnection` (`:2156`) all take
    `CR3 >> 8` (0-255) as a direct index into a 24-element array.
12. **[BUG]** `Cs2GetSectorNumber` (`:2293`) and `Cs2CalculateActualSize` (`:2325`) index
    `partition[CR3>>8]` with no range check either.

### Command semantics

13. **[BUG]** `Cs2CalculateActualSize` never advances the sector index inside its loop
    (`cs2.c:2332-2336`): it adds `block[cassectoffset]->size / 2` once per iteration, so the
    result is *count × the size of a single sector* rather than the sum over the range. It also
    tests `partition[].size != 0` while an unused partition is marked with `-1`, and the
    "reject while seeking" path is compiled out with `#if 0` (`cs2.c:2315-2322`).
14. **[BUG]** `Cs2CopySectorData` and `Cs2MoveSectorData` mask the count with `0xFF`
    (`cs2.c:2659, 2700`) and then test `count == 0xFFFF` (`:2674, 2715`) — an unreachable
    branch, so "copy to end of partition" is impossible. Both hardcode 2352-byte blocks
    regardless of `getsectsize`/`putsectsize`, neither checks `Cs2AllocateBlock` for `NULL`, and
    `MoveSectorData` decrements `srcpartition->numblocks` and subtracts 2352 from
    `srcpartition->size` inside the copy loop while still indexing `block[offset + i]`.
15. **[BUG]** `Cs2PutSectorData` sets `putpartition->size = 0` (`cs2.c:2624`) before appending,
    discarding the accumulated size of any sectors already in the partition — even though it
    then deliberately appends starting at `startpos = numblocks`. It also declares an unused
    `IOCheck_struct check` (`:2620`).
16. **[QUIRK]** The put-sector write path applies an unexplained offset shift:
    `size = (putsectsize - getsectsize) / 24; offset = datatransoffset - size;` and drops
    everything written while `offset < 0` (`cs2.c:462-466`). Nothing in the source motivates
    the `/24`.
17. **[BUG]** Get and put use inconsistent block indexing: the read path uses
    `block[datatranssectpos + datanumsecttrans]` (`cs2.c:384`), the write path uses
    `block[datanumsecttrans]` (`cs2.c:468`), and `Cs2GetThenDeleteSectorData` never sets
    `datatranspartitionnum` at all (compare `cs2.c:2494` with `2591`).
18. **[BUG]** `Cs2ReadLong`'s data path forms `ptr = &block[…]->data[…]` and *then* checks
    whether that same block pointer is `NULL` (`cs2.c:384-389`) — the dereference happens before
    the guard.
19. **[BUG]** All five info-transfer terminators use `>` where `>=` is required
    (`cs2.c:211, 224, 246, 261, 275`), so exactly one word past the declared length is
    readable, and every such read is out of bounds: `TOC[102]` (array is 102 entries),
    `transfileinfo[12..13]` (12 bytes), `fileinfo[256]` (256 entries), `transscodeq[10..11]`
    (10 bytes), `transscoderw[24..25]` (24 bytes).
20. **[BUG]** `Cs2GetSubcodeQRW` Q-channel computes `TOC[Cs2Area->track - 1]` with no check that
    `track` is valid; after a Stop or with no disc, `track == 0xFF` gives `TOC[254]`
    (`cs2.c:1938`). The RW channel reads `workblock.data[2352 + i + 24*group]` where `group` is
    an unbounded function-`static` counter (`cs2.c:1960-1976`); from `group == 4` onward it
    reads past the 2448-byte `workblock.data`. `lastfad`/`group` are also invisible to the
    savestate.
21. **[QUIRK]** `Cs2ScanDisc` (`cs2.c:1914-1919`) sets `CDB_STAT_SCAN` and nothing else; the
    corresponding case in `Cs2Exec` is empty (`cs2.c:1109-1110`). Scan mode is a one-way door:
    the drive stays in `SCAN` forever. Marked *"finish me"*.
22. **[QUIRK]** `Cs2ExecFadSearch` and `Cs2GetFadSearchResults` are pure stubs marked
    *"finish me"* (`cs2.c:2390, 2398`). `Cs2GetCopyError` always reports zero (`cs2.c:2735`).
23. **[QUIRK]** `Cs2OpenTray` (opcode `0x05`) only assigns `status = CDB_STAT_OPEN`; it never
    calls `cdi->SetStatus` (`cs2.c:1659-1666`). Within one backend poll period (≈333 ms) the
    poll sees the core still reporting "disc present" and forces the state back to `PAUSE`
    (`cs2.c:969-977`). Real tray-open is reachable only via the front-end hook
    `Cs2ForceOpenTray` (`cs2.c:908`).
24. **[BUG]** `Cs2SeekDisc`'s seek-by-FAD path scans only `TOC[0..15]` regardless of track
    count, and passes the loop index `i` — not `i + 1` — to `Cs2SetupDefaultPlayStats`
    (`cs2.c:1873-1880`), which indexes `TOC[track_number - 1]`. For `i == 0` that is `TOC[-1]`.
    It also selects the first track whose start is *at or after* the target FAD, i.e. the track
    after the one containing it.
25. **[QUIRK]** `Cs2PlayDisc` fabricates its seek delay by assigning a FAD *difference* to
    `_periodictiming`, which is measured in 1/3 µs (`cs2.c:1820-1829`). The units are
    unrelated; the value is simply clamped into `[40000, 300000]`.
26. **[BUG]** `Cs2PlayDisc`'s track-mode end position does
    `Cs2TrackToFAD((u16)(pdepos | 0x0063))` (`cs2.c:1802`) — it truncates the 24-bit argument to
    16 bits and ORs `0x63` into the index byte instead of replacing it, so a non-zero index in
    the request produces a value `Cs2TrackToFAD` treats as "lead-in" and returns 0.
27. **[QUIRK]** Play modes are parsed but unimplemented: only the repeat count survives; any
    other bit logs *"Unsupported play mode"* (`cs2.c:1811-1814`).
28. **[BUG]** `Cs2InitializeCDSystem`'s `val = HIRQ & 0xFFE5` (`cs2.c:1645`) is dead. Since
    `Cs2SetIRQ` only ORs, `DRDY`, `BFUL` and `PEND` can never be cleared by it; likewise the
    `val &= ~CDB_HIRQ_DCHG` at `:1651` has no effect.
29. **[QUIRK]** `Cs2InitializeCDSystem` init flags 1, 2 and 3 are empty branches with comments
    only — "Decode RW subcode", "Don't confirm Mode 2 subheader", "Retry reading Form 2 sectors"
    (`cs2.c:1625-1638`). Flag 0 deliberately does **not** clear the TOC (the `memset` is
    commented out, `cs2.c:1612`).
30. **[QUIRK]** `Cs2GetHardwareInfo` returns hardcoded constants: `CR2 = 0x0201` with the
    comment *"mpeg card exists"* and `CR4 = 0x0400` (`cs2.c:1494, 1503`). The emulated machine
    therefore always claims an MPEG card is fitted, regardless of configuration.
31. **[QUIRK]** `Cs2GetSessionInfo` hardcodes a single-session disc (`cs2.c:1529-1535`);
    `CR3 = 0x0100` is the fixed session count.
32. **[QUIRK]** `Cs2AuthenticateDevice` always succeeds — the source says so:
    *"if authentication passes(obviously it always does)"* (`cs2.c:3177`). It sets
    `status = BUSY` and stuffs `0xFF`/`0xFFFF` into the CRs, then restores `PAUSE` and calls
    `doCDReport` before returning (`cs2.c:3163-3198`), so the `BUSY` state and the invalid
    register values are never externally observable.
33. **[QUIRK]** `Cs2GetMPEGRom` reads an arbitrary **host file** (`Cs2Area->mpegpath`) directly
    into partition buffers and forces `outconmpegrom` to filter 0 (`cs2.c:3217-3261`). It also
    sets `mpgauth |= 0x300` with a *"fix me"*.
34. **[QUIRK]** Every MPEG command is a report-and-return stub (`cs2.c:2890-3149`). The MPEG
    state fields `actionstatus`, `pictureinfo`, `mpegaudiostatus`, `mpegvideostatus`, `vcounter`
    are declared, saved and loaded — and never written by anything. `mpegmode`, `mpegcon[2]` and
    `mpegstm[2]` are written by the setters but read only by the matching getters.
35. **[HACK]** End-of-file-play raises `EHST` in addition to `PEND`/`EFLS`, with the inline
    comment *"Need for Assault Leynos 2"* (`cs2.c:1046`). The same block on the
    "sector filtered out" path (`cs2.c:1075-1076`) does **not** raise `EHST`, and does not set
    `options = 0x8`, so the reported state differs depending on whether the last sector passed
    the filter.
36. **[HACK]** `Cs2GetIP` rewrites `msh2stack`/`ssh2stack` when bit 31 is set, with the comment
    *"for Panzer Dragoon Zwei. This operation is not written in the document."*
    (`cs2.c:4080-4084`), and substitutes `0x6002000`/`0x6001000` when the IP.BIN values are 0
    (`cs2.c:4075-4089`). These fixups are inside the `#else` (little-endian) branch only — a
    big-endian build gets none of them.
37. **[BUG]** `Cs2ReadFile` and `Cs2ReadDirectory` build their file ID as
    `((CR3 & 0xFF) << 8) | CR4` (`cs2.c:2786, 2851`) while `Cs2ChangeDirectory` and
    `Cs2GetFileInfo` use `((CR3 & 0xFF) << 16) | CR4` (`cs2.c:2758, 2816`). At most one of these
    can match the hardware. `Cs2ReadFile` additionally indexes `fileinfo[rffid]` with a value up
    to `0xFFFF` into a 256-entry array, and does not subtract `curdirfidoffset` the way
    `Cs2ReadFileSystem` does.

### Buffer, filter and partition model

38. **[QUIRK]** `Cs2GetPartition` evaluates no filter conditions at all — *"go through various
    filter conditions here(fix me)"* — and returns `partition[condtrue]` unconditionally
    (`cs2.c:3387-3392`). Every unfiltered read path (filesystem, IP.BIN, MPEG ROM) therefore
    ignores filtering entirely.
39. **[BUG]** `Cs2SortBlocks` compacts `part->block[]` but leaves `part->blocknum[]` untouched
    (`cs2.c:3365-3383`), so after any deletion the two parallel arrays disagree. Nothing in the
    live data path reads `blocknum[]`, but it is written to savestates.
40. **[BUG]** `Cs2ReadUnFilteredSector` allocates the destination block *before* attempting the
    read and returns `NULL` without freeing it if `ReadSectorFAD` fails (`cs2.c:3855-3862`) —
    a block leak on every failed read.
41. **[QUIRK]** Filter `mode` bit 5 (`0x20`) is not decoded (`cs2.c:3412-3455`). The FAD-range
    condition (bit 6) is evaluated for audio sectors too, even though the subheader conditions
    are skipped for them.
42. **[QUIRK]** `Cs2SetFilterMode`'s "initialize" bit sets `range = 0` (`cs2.c:2102`), whereas
    every other reset path sets `range = 0xFFFFFFFF` (`cs2.c:836, 1577, 2227`). A filter
    initialized this way will reject everything if bit 6 is later enabled.
43. **[QUIRK]** `Cs2ResetSelector` CR1 bit 3 ("reset partition output connectors") is an empty
    branch (`cs2.c:2238-2241`); bits 5, 1 and 0 are not decoded. Bit 2 clears every block but
    does **not** restore `blockfreespace` to `MAX_BLOCKS` (`cs2.c:2243-2270`), so the free
    counter permanently under-reports after a full buffer reset.
44. **[QUIRK]** Back-pressure on a full buffer is an emulator invention: mid-play, if
    `isbufferfull`, the drive is pushed into `CDB_STAT_SEEK` with `options = 0`
    (`cs2.c:1029-1033`) and returns to `PLAY` when space frees (`cs2.c:1101-1108`).
45. **[QUIRK]** Each of the 24 partitions carries a full 200-entry `block[]` array
    (`cs2.h:84`), so a single partition can hold every buffer in the machine. No per-partition
    quota is modelled, and nothing range-checks `block[numblocks]` before writing to it.
46. **[QUIRK]** `Cs2FreeBlock` clears `isbufferfull` unconditionally (`cs2.c:3360`) without
    consulting `blockfreespace`, and `Cs2AllocateBlock` tests `blockfreespace <= 0` on an
    unsigned counter (`cs2.c:3337`), so the `BFUL` flag transitions are driven by exact-zero
    equality rather than a threshold.
47. **[QUIRK]** Read errors are swallowed: `Cs2ReadFilteredSector` returns `-1`/`-2` and
    `Cs2Exec`'s handling of both is an empty case (`cs2.c:1091-1096`). No `CDB_STAT_RETRY`,
    no `CDB_STAT_ERROR`, no HIRQ — playback simply stalls at the same FAD forever.

### Dead code

48. **[DEAD]** `Cs2RapidCopyT1` (`cs2.c:509`) and `Cs2RapidCopyT2` (`cs2.c:575`) have no callers
    anywhere in the tree. Beyond being unused, `T2` byte-swaps 32-bit values with `BSWAP16`
    (`cs2.c:601-604, 617`) — the wrong macro — and both index
    `block[datanumsecttrans]` rather than `block[datatranssectpos + datanumsecttrans]`, so they
    would ignore the sector offset the get-sector command established.
49. **[DEAD]** `Cs2Command` (`cs2.c:1149-1151`) and `Cs2GetTimeToNextSector` (`cs2.c:1137-1145`)
    are exported in `cs2.h` and called by nothing.
50. **[DEAD]** The commented-out `Cs2SetDelayIRQ` scaffolding at `cs2.c:136-139` refers to an
    `irq_index`/`delay_irq[]` queue that does not exist in the struct; the shipped mechanism is
    the single `_delay_irq`/`_command_execlock` pair.
51. **[BUG]** `Cs2Exec`'s early `return` when a command is pending (`cs2.c:1116-1117`) exits the
    whole function, skipping the `NetlinkExec`/`JapModemExec` calls at the bottom
    (`cs2.c:1127-1130`). Modem emulation stalls whenever a CD command is in flight.

### `cdbase.c`

52. **[BUG]** `ISOCDReadSectorFAD`'s track lookup is broken (`cdbase.c:1644-1660`): `found` is
    only set when the matched track *differs* from the cached `currentTrack`, the
    `if (found == 1) break;` sits after the inner `break` so it can never run for the matching
    iteration, and `currentTrack` is a file-global that is never cleared. A FAD outside every
    track therefore reads from whatever track was used last, at a bogus offset, instead of
    failing.
53. **[BUG]** `ISOCDReadSectorFAD` returns `1` (success) unconditionally (`cdbase.c:1712`), even
    when `fread` fails or when `sector_size` is none of 2048/2352/2448 — for example the
    2336- and 2324-byte sector sizes `LoadCHD` can assign (`cdbase.c:1840, 1860`). In those
    cases the caller receives a zero-filled buffer and treats it as a valid sector.
54. **[BUG]** `ISOCDReadSectorFADFromCHD` iterates `j < track_num - 1` (`cdbase.c:1973`), so the
    **last track of a CHD is never matched**; for a single-track CHD the loop body never runs at
    all and every read fails with `track == NULL`. It also omits the `memset(buffer, 0, 2448)`
    that the non-CHD path performs.
55. **[QUIRK]** For 2048-byte tracks only the 12-byte sync pattern is synthesized; bytes
    `0x0C`-`0x0F` (MIN/SEC/FRAME and, critically, the **mode byte**) stay zero
    (`cdbase.c:1707-1711`, `2015-2019`). `cs2.c` consequently classifies every such sector as
    mode 1 and no mode 2 subheader is ever available for these images.
56. **[QUIRK]** Subcode is only ever present for 2448-byte tracks. The 2352-byte path carries
    the comment *"Generate subcodes here"* and generates nothing (`cdbase.c:1704`), so
    `Cs2GetSubcodeQRW`'s RW channel returns whatever stale bytes happen to be in
    `workblock.data[2352..]`.
57. **[QUIRK]** Multi-session support is nominal. `LoadMDS` can populate more than one session,
    but `BuildTOC` reads only `disc.session[0]` (`cdbase.c:1457-1471`), `LoadCCD` rejects
    `Sessions != 1` outright (`cdbase.c:1352-1357`), and `Cs2GetSessionInfo` hardcodes one
    session.
58. **[QUIRK]** The Dummy core can never contain a disc: `dmy_status` starts at 2 and
    `DummyCDSetStatus` forces anything other than 3 back to 2 (`cdbase.c:158, 199-205`).
    `DummyCDReadTOC` returns 0 without writing the caller's TOC (`cdbase.c:209-246`), and
    `DummyCDReadSectorFAD` zeroes only 2352 of the 2448 bytes the caller expects
    (`cdbase.c:264`).
59. **[QUIRK]** `ISOCDReadAheadFAD` is a no-op (`cdbase.c:1717-1720`), so `cdi->ReadAheadFAD`
    calls from `Cs2Exec`/`Cs2PlayDisc`/`Cs2ReadFile` do nothing and every sector read is a
    synchronous `fread` on the emulation thread.
60. **[BUG]** `LoadBinCue` and `LoadBinCueMultiFile` compute the last track's `fad_end` as
    `fad_start + (file_size - file_offset)/sector_size` with no `-1`
    (`cdbase.c:712, 873`), while all other tracks use `next.fad_start - 1`. The last track is
    one sector longer than it should be, overlapping the lead-out.
61. **[BUG]** `LoadCCD` computes `track->file_size = (track->fad_end + 1 - track->fad_start) * 2352`
    (`cdbase.c:1439`) using `fad_end`, which for this track has not been assigned yet (it is set
    when the *next* point is processed, `cdbase.c:1435`). `fad_end` is still 0 from the
    `memset`, so `file_size` is computed from `1 - fad_start`.
62. **[BUG]** `LoadMDSTracks` sets `session->track[track_num-1].fad_end = session->track[track_num].fad_start`
    with no `-1` (`cdbase.c:1043-1044`), so consecutive MDS tracks overlap by one FAD; the
    linear search in `ISOCDReadSectorFAD` resolves the ambiguity in favour of the earlier track.
63. **[QUIRK]** `LoadCHD` ignores pregap/postgap in the FAD map — the lines that would apply
    them are commented out (`cdbase.c:1894-1898, 1917-1918`) — and byte-swaps audio sectors
    pairwise in place, assuming 16-bit samples of the opposite endianness
    (`cdbase.c:2007-2012`).
64. **[QUIRK]** `ISOCD`'s TOC is built once, at `Init` (`cdbase.c:1564`). `Cs2GetToc` re-reads
    the same static `isoTOC` array (`cdbase.c:1621`), so a disc change that does not go through
    `Cs2ChangeCDCore` is invisible to the TOC.
65. **[QUIRK]** `LoadBinCueMultiFile` shadows the file-scope `current_file_id`
    (`cdbase.c:415`) with a function-local of the same name (`cdbase.c:749`); the global is
    never used for anything. `imgtype` (`cdbase.c:410`) is a non-`static` global.
66. **[QUIRK]** `LoadCHD` returns `-1` on an unrecognised metadata tag (`cdbase.c:1805-1806`)
    after having already allocated `pChdInfo` and the 512 KB metadata buffer — both leak.
