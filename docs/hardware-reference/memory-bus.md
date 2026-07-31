# System Memory Map, Bus Decode, and the A-Bus CS0/CS1 Cartridge Interface

**Source of truth.** Everything in this document is derived *exclusively* from the Yabause
(YabaSanshiro fork) C source:

- `yabause/src/memory.c` (2224 lines) — the central address decoder and dispatch table
- `yabause/src/memory.h` (456 lines) — T1/T2/T3 storage models, region prototypes
- `yabause/src/cs0.c` (1483 lines) — A-Bus CS0 emulation, all cartridge models
- `yabause/src/cs0.h` (99 lines) — cartridge type IDs and the `cartridge_struct` vtable
- `yabause/src/cs1.c` (101 lines) — A-Bus CS1 emulation
- `yabause/src/cs1.h` (34 lines)

**No outside Saturn documentation was used.** The address map below is *the map Yabause builds*,
not a map of the real machine; where the two are known to differ from the code alone, that is
stated.

A memory map is by nature a document about the boundaries between files, so a small number of
facts cannot be established from the six files above. Those are cited to their own file and are
listed here so they can be audited separately:

| Fact | Cited to |
|---|---|
| Per-region address masks applied *inside* peripheral handlers (SMPC, SCSP, VDP1, VDP2, SCU, CS2) | `smpc.c`, `scsp.c`, `vdp1.cpp`, `vdp2.cpp`, `scu.c`, `cs2.c` |
| Allocation sizes of the peripheral RAMs | same files |
| The SH-2 cache's own area decode (`CACHE_ENABLE` builds) | `sh2cache.c` |
| The separate *instruction fetch* address decode | `sh2int.c` |
| SH-2 cache address/data array handlers | `sh2core.c` |
| Init order and backup-file setup | `yabause.c`, `bios.c` |
| `CACHE_ENABLE` default | `src/CMakeLists.txt:134-137` |

Notes are tagged **[QUIRK]** (a deliberate emulator shortcut, or hardware behaviour that is
simply not modelled), **[BUG]** (a defect in the C source: out-of-bounds access, dead code,
constant-true condition, asymmetric read/write), or **[HACK]** (a game-specific special case).

Line citations are of the form `yabause/src/memory.c:723`.

---

## 0. How an address is decoded

There is no single decode step. A CPU-side access passes through **three** independent stages,
and each stage throws away different address bits. Getting this exactly right matters more than
any individual region, because almost all of the "mirroring" in this emulator is an artifact of
which bits each stage ignores.

### 0.1 Stage 1 — area select, `addr >> 29`

`MappedMemoryReadByte/Word/Long` and `MappedMemoryWriteByte/Word/Long`
(`yabause/src/memory.c:850`, `:914`, `:977`, `:1043`, `:1107`, `:1171`) all begin with the same
`switch (addr >> 29)`, i.e. the top **three** bits pick one of eight 512 MB areas:

| `addr>>29` | Range | Byte | Word | Long | Source |
|---|---|---|---|---|---|
| `0` | `0x00000000`–`0x1FFFFFFF` | page table | page table | page table | `memory.c:859` |
| `1` | `0x20000000`–`0x3FFFFFFF` | page table | page table | page table | `memory.c:860` |
| `2` | `0x40000000`–`0x5FFFFFFF` | read `0xFF`, write dropped | read `0xFFFF`, write dropped | read `0xFFFFFFFF`, write dropped | `memory.c:867-872` |
| `3` | `0x60000000`–`0x7FFFFFFF` | **falls to `default`** → `UnhandledMemoryReadByte` | **falls to `default`** | `AddressArrayReadLong` / `AddressArrayWriteLong` | `memory.c:1001-1005`, `:1196-1200` |
| `4` | `0x80000000`–`0x9FFFFFFF` | page table | page table | page table | `memory.c:861` |
| `5` | `0xA0000000`–`0xBFFFFFFF` | read `0xFF`, write dropped | read `0xFFFF`, write dropped | read `0xFFFFFFFF`, write dropped | `memory.c:868` |
| `6` | `0xC0000000`–`0xDFFFFFFF` | `DataArrayReadByte`/`WriteByte` | `DataArray…Word` | `DataArray…Long` | `memory.c:876-878` |
| `7` | `0xE0000000`–`0xFFFFFFFF` | see §0.4 | see §0.4 | see §0.4 | `memory.c:879-896` |

Areas 2 and 5 are commented `// Purge Area` (`memory.c:870`). Only area 2 corresponds to the
SH-2 associative purge space; area 5 is lumped in with it — see §12.

### 0.2 Stage 2 — the 4096-entry page table, `(addr >> 16) & 0xFFF`

For areas 0, 1 and 4 the access is forwarded to one of six 4096-entry function-pointer tables
(`memory.c:92-98`, declared in `memory.h:392-398`):

```c
writebytefunc WriteByteList[0x1000];   readbytefunc ReadByteList[0x1000];
writewordfunc WriteWordList[0x1000];   readwordfunc ReadWordList[0x1000];
writelongfunc WriteLongList[0x1000];   readlongfunc ReadLongList[0x1000];
```

The index is `(addr >> 16) & 0xFFF` (`memory.c:864`, `:928`, `:991`, `:1057`, `:1121`, `:1185`),
i.e. **address bits 27:16**. One table entry therefore covers exactly 64 KB, and the table as a
whole covers a 256 MB window.

The consequences, stated exactly:

- **Bits 31:29** were consumed by stage 1.
- **Bit 28 is consumed by nobody.** It is not part of the stage-1 area select and it is not part
  of the stage-2 index. Any pair of addresses differing only in bit 28 reach the same handler
  with different `addr` values, and every handler in the map applies a mask of 20 bits or fewer,
  so bit 28 is discarded there too. `0x00200000`, `0x10200000`, `0x20200000`, `0x30200000`,
  `0x80200000` and `0x90200000` are all the same byte of Low WRAM.
- **Bits 15:0** are passed through to the handler untouched; the handler decides what to do with
  them.

### 0.3 Stage 3 — the per-handler mask

Each handler masks the full 32-bit address itself. This is where all remaining mirroring is
decided. `HighWramMemoryReadByte` is representative (`memory.c:355-358`):

```c
static u8 FASTCALL HighWramMemoryReadByte(u32 addr)
{
   return T2ReadByte(HighWram, addr & 0xFFFFF);
}
```

No handler ever range-checks; the mask *is* the range check. §5 tabulates every mask.

### 0.4 Area 7 (`0xE0000000`–`0xFFFFFFFF`)

Handled inline in all six accessors (`memory.c:879-896`, `:943-960`, `:1010-1027`, `:1073-1091`,
`:1137-1155`, `:1207-1225`):

```c
if (addr >= 0xFFFFFE00)      { addr &= 0x1FF; return OnchipReadByte(addr); }
else if (addr >= 0xFFFF8000 && addr < 0xFFFFC000) { /* ??? */ }
else                                              { /* Garbage data */ }
break;   // -> function returns 0
```

So: the top 512 bytes are the SH-2 on-chip register file; `0xFFFF8000`–`0xFFFFBFFF` is an
explicitly-empty `???` branch; everything else in area 7 silently reads `0` and drops writes.
Neither of the two non-on-chip branches logs anything.

### 0.5 What this means for cache-through addressing

**In the default build there is no difference whatsoever between a cached and a cache-through
access.** `addr>>29 == 0` and `addr>>29 == 1` are adjacent `case` labels that fall into the same
body (`memory.c:859-865`), so `0x06000000` and `0x26000000` are byte-for-byte identical paths.
Bit 29 is discarded before the page table is indexed and never reaches a handler that could care
about it.

Two second-order effects survive:

1. **The cycle-cost model deliberately masks bit 29 out.** `GET_MEM_CYCLE_R` /
   `GET_MEM_CYCLE_W` switch on `addr & 0xDFF00000` (`memory.c:750`, `:772`). `0xDFF00000` has
   bit 29 clear, so the cache-through mirror is *intentionally* folded onto the cached address
   for cost lookup. Bit 28 is **not** masked out, so a bit-28 mirror (`0x16000000`) falls to
   `default:` and costs 0 cycles. See §7.
2. **When the SH-2 cache is compiled in, bit 29 becomes load-bearing.** `CACHE_ENABLE` is off by
   default (`src/CMakeLists.txt:134-137`, `option(YAB_WANT_SH2_CACHE … OFF)`), and
   `memory.c:82-87` then defines the `…Nocache` entry points as trivial forwarders. When it *is*
   on, `MappedMemoryReadByte` becomes `cache_memory_read_b(&CurrentSH2->onchip.cache, addr)`
   (`memory.c:845-847`) and the cache does its own area decode with
   `AREA_MASK == 0xE0000000`, `CACHE_USE == 0<<29`, `CACHE_THROUGH == 1<<29`
   (`sh2cache.c:39-46`, `:303-355`):
   - area 0 → 4-way / 64-entry / 16-byte-line cache lookup; on a miss the line is filled with
     four `ReadLongList[(addr >> 16) & 0xFFF]((addr & 0xFFFFFFF0) + i)` calls (`sh2cache.c:337`),
     **bypassing the stage-1 area dispatch entirely**;
   - area 1 → `MappedMemoryRead*Nocache`;
   - **every other area** → also `MappedMemoryRead*Nocache` (the `default:` arm,
     `sh2cache.c:351`), so areas 2/3/5/6/7 are routed back through the full stage-1 switch;
   - writes are **write-through with no-allocate**: on a hit the line is patched *and* the write
     goes to memory; on a miss only memory is written (`sh2cache.c:140-178`);
   - if `ca->enable == 0` (CCR cache-disable) area 0 degrades to `…Nocache` (`sh2cache.c:311`).

**[QUIRK]** With the default build, code that relies on the cached/uncached distinction — the
standard Saturn idiom of writing through `0x2…` to make a store visible to the SCU DMA or the
other SH-2 — is silently "correct" because there is no cache to be stale. Mimas must decide
whether it wants the same shortcut; if it implements a cache, the aliasing rules above (bit 28
ignored, bit 29 selecting cache vs. through, area 4 aliased onto area 0) are what Saturn software
was written against in this emulator.

**[BUG]** `memory.c:80-88` uses `#if CACHE_ENABLE` while `sh2core.c:1933`, `:1948`, `:1962` etc.
use `#ifdef CACHE_ENABLE`. If anyone ever defines `CACHE_ENABLE 0` (as the commented-out line
`memory.h:50` invites), `memory.c` takes the no-cache path while `sh2core.c` takes the cache path
and the address/data array handlers stop agreeing with the rest of the build.

---

## 1. The full physical address map

Built by `MappedMemoryInit` (`yabause/src/memory.c:598-729`) via
`FillMemoryArea(start_index, end_index, r8, r16, r32, w8, w16, w32)` (`memory.c:578-594`), where
the indices are table indices (units of 64 KB), *not* addresses. Fills are applied in source
order and later fills overwrite earlier ones.

The table is written once at boot, from `YabauseInit` at `yabause.c:271`, immediately after
`CartInit` at `yabause.c:265`.

Addresses below are given in area 0 form. Add `0x20000000` for the cache-through alias, and see
§0.2 for the bit-28 and area-4 aliases.

| Region | Base | End | Size | Handler set | Fill |
|---|---|---|---|---|---|
| *(default: everything)* | `0x00000000` | `0x0FFFFFFF` | 256 MB | `UnhandledMemory*` | `memory.c:601` |
| **BIOS ROM** | `0x00000000` | `0x000FFFFF` | 1 MB window / 512 KB device | `BiosRomMemory*` | `memory.c:609` |
| **SMPC registers** | `0x00100000` | `0x0017FFFF` | 512 KB window / 128 B device | `Smpc*` (`smpc.c`) | `memory.c:615` |
| **Internal backup RAM** | `0x00180000` | `0x001FFFFF` | 512 KB window / 64 KB device | `BupRamMemory*` | `memory.c:621` |
| **Low Work RAM** | `0x00200000` | `0x002FFFFF` | 1 MB | `LowWramMemory*` | `memory.c:627` |
| *(hole)* | `0x00300000` | `0x00FFFFFF` | 13 MB | `UnhandledMemory*` | — |
| **SSH2 FRT input capture** | `0x01000000` | `0x017FFFFF` | 8 MB | word-write only: `SSH2InputCaptureWriteWord` | `memory.c:633` |
| **MSH2 FRT input capture** | `0x01800000` | `0x01FFFFFF` | 8 MB | word-write only: `MSH2InputCaptureWriteWord` | `memory.c:639` |
| **A-Bus CS0 (cartridge)** | `0x02000000` | `0x03FFFFFF` | 32 MB | `CartridgeArea->Cs0*` (see §9) | `memory.c:645` |
| **A-Bus CS1** | `0x04000000` | `0x04FFFFFF` | 16 MB | `Cs1*` → `CartridgeArea->Cs1*` (see §10) | `memory.c:651` |
| *(hole)* | `0x05000000` | `0x057FFFFF` | 8 MB | `UnhandledMemory*` | — |
| **A-Bus CS2 (CD block)** | `0x05800000` | `0x058FFFFF` | 1 MB | `Cs2*` (`cs2.c`) | `memory.c:657` |
| *(hole)* | `0x05900000` | `0x059FFFFF` | 1 MB | `UnhandledMemory*` | — |
| **Sound RAM (68000)** | `0x05A00000` | `0x05AFFFFF` | 1 MB window / 512 KB device | `SoundRam*` (`scsp.c`) | `memory.c:663` |
| **SCSP registers** | `0x05B00000` | `0x05BFFFFF` | 1 MB window / 4 KB device | `scsp_r_b/w/d`, `scsp_w_b/w/d` | `memory.c:669` |
| **VDP1 VRAM** | `0x05C00000` | `0x05C7FFFF` | 512 KB | `Vdp1Ram*` (`vdp1.cpp`) | `memory.c:675` |
| **VDP1 framebuffer** | `0x05C80000` | `0x05CFFFFF` | 512 KB window / 256 KB device | `Vdp1FrameBuffer*` | `memory.c:681` |
| **VDP1 registers** | `0x05D00000` | `0x05D7FFFF` | 512 KB window / 256 B device | `Vdp1Read*`/`Vdp1Write*` | `memory.c:687` |
| *(hole)* | `0x05D80000` | `0x05DFFFFF` | 512 KB | `UnhandledMemory*` | — |
| **VDP2 VRAM** | `0x05E00000` | `0x05EFFFFF` | 1 MB window / 512 KB device | `Vdp2Ram*` (`vdp2.cpp`) | `memory.c:693` |
| **VDP2 colour RAM** | `0x05F00000` | `0x05F7FFFF` | 512 KB window / 4 KB device | `Vdp2ColorRam*` | `memory.c:699` |
| **VDP2 registers** | `0x05F80000` | `0x05FBFFFF` | 256 KB window / 512 B device | `Vdp2Read*`/`Vdp2Write*` | `memory.c:705` |
| *(hole)* | `0x05FC0000` | `0x05FDFFFF` | 128 KB | `UnhandledMemory*` | — |
| **SCU registers** | `0x05FE0000` | `0x05FEFFFF` | 64 KB window / 256 B device | `Scu*` (`scu.c`) | `memory.c:711` |
| *(hole)* | `0x05FF0000` | `0x05FFFFFF` | 64 KB | `UnhandledMemory*` | — |
| **High Work RAM** | `0x06000000` | `0x0610FFFF` | 1 MB + 64 KB window / 1 MB device | `HighWramMemory*` | `memory.c:717` |
| *(hole)* | `0x06110000` | `0x062FFFFF` | 1984 KB | `UnhandledMemory*` | — |
| **Extended backup RAM window** | `0x06300000` | `0x07FFFFFF` | 29 MB window / 8 MB file | `BupRamMemory*` | `memory.c:723` |
| *(hole)* | `0x08000000` | `0x0FFFFFFF` | 128 MB | `UnhandledMemory*` | — |

The last fill's start index is computed at runtime:
`FillMemoryArea(((tweak_backup_file_addr >> 16) & 0xFFF), 0x7ff, …)` (`memory.c:723`), with
`const u32 tweak_backup_file_addr = 0x06300000` (`bios.c:52`) giving index `0x630`. Because it is
the *last* fill it wins over anything overlapping it — but the High WRAM fill ends at index
`0x610`, so in practice there is no overlap at the default address.

---

## 2. Region-by-region detail

### 2.1 BIOS ROM — `0x00000000`–`0x000FFFFF`

- Storage: `u8 *BiosRom` (`memory.c:102`), `T2MemoryInit(0x80000)` = **512 KB**
  (`yabause.c:214`).
- Read handlers mask `addr & 0x7FFFF` (`memory.c:441`, `:448`, `:455`) → the 512 KB device
  **mirrors twice** across the 1 MB window.
- Writes are unconditional no-ops with the comment `// read-only` (`memory.c:460-477`). No
  logging, no bus error.
- Loaded by `LoadBios` → `T123Load(BiosRom, 0x80000, 2, filename)` (`memory.c:1353-1356`), i.e.
  as a **T2** image (see §6). `T123Load` truncates an oversized file rather than failing
  (`memory.h:237-242`).

### 2.2 SMPC registers — `0x00100000`–`0x0017FFFF`

- `SmpcReadByte`/`SmpcWriteByte` mask `addr &= 0x7F` (`smpc.c:636`, `:755`) → the 128-byte
  register file **mirrors 4096 times** across the 512 KB window.
- Registers are stored in `SmpcRegsT[addr >> 1]` — i.e. the SMPC file is byte-wide on **odd**
  addresses of a 16-bit bus, and the emulator collapses that by shifting the address right one
  bit. An access to `0x2010001F` and one to `0x2010001E` therefore reach the same register.
- **Word and long accesses are stubs**: `SmpcReadWord`/`SmpcReadLong` return `0` and log
  `"byte access only"` (`smpc.c:647-659`); `SmpcWriteWord`/`SmpcWriteLong` do nothing
  (`smpc.c:910-920`).
- `SmpcReadByte` special-cases `addr == 0x63` (SF) to merge `bustmp` with `SmpcRegs->SF`
  (`smpc.c:637-641`).

### 2.3 Internal backup RAM — `0x00180000`–`0x001FFFFF`

Storage depends on a runtime flag:

| `yabsys.extend_backup` | Storage | Size | Set at |
|---|---|---|---|
| `0` (default for libretro) | `T1MemoryInit(0x10000)` | 64 KB | `yabause.c:253-259`, `libretro.c:1077` |
| non-zero | `YabMemMap(bupfilename, tweak_backup_file_size)` — an `mmap`'d file | 8 MB (`bios.c:53`) | `yabause.c:224-251` |

`BupRamMemoryReadByte` (`memory.c:481-510`):

```c
if (yabsys.extend_backup) {
  addr = (addr & 0x0FFFFFFF) - tweak_backup_file_addr;
  if (addr >= tweak_backup_file_size) return 0;
} else {
  addr = addr & 0x0000FFFF;
}
return T1ReadByte(BupRam, addr);
```

`BupRamMemoryWriteByte` (`memory.c:529-560`) is the same, but the final store is
`T1WriteByte(BupRam, addr | 0x1, val)`.

Observations:

- **Non-extended mode**: mask `0xFFFF` → the 64 KB device **mirrors 8 times** across the 512 KB
  window at `0x00180000`.
- **Extended mode**: the subtraction is relative to `0x06300000`. For an access in the
  `0x00180000` window this **underflows** to a huge unsigned value, trips
  `addr >= tweak_backup_file_size`, and returns `0` / drops the write. See §13 item 3.
- **Word and long accesses are stubs**: return `0` and log (`memory.c:514-526`); writes log and
  do nothing (`memory.c:564-574`). The internal backup RAM is byte-accessible only.
- **The read/write address asymmetry is real**: writes force bit 0 set, reads do not. A byte
  written at an even address lands at `addr|1` and is *not* visible when reading that even
  address. This models the 8-bit backup SRAM being wired to the low half of a 16-bit bus, but
  only on the write side.
- `FormatBackupRam` (`memory.c:1428-1444`) writes a 32-byte header repeated 4 times
  (`memory.c:1365-1370`, the ASCII `BackUpRam Format` interleaved with `0xFF`) and then fills
  from `0x80` with the alternating pattern `0xFF, 0x00` — even bytes `0xFF`, odd bytes `0x00`.
  So the even (unconnected) half consistently reads back `0xFF`.
- `FormatBackupRamFile` / `ExtendBackupFile` / `CheckBackupFile` (`memory.c:1372-1426`) are the
  file-backed equivalents for extended mode; `CheckBackupFile` validates the 4×32-byte header and
  `ExtendBackupFile` pads an existing file up to `size` with the same `0xFF,0x00` pattern.
- The BIOS-emulation layer reports this device as base `0x00180000`, size `0x800`, block size
  `0x40` in non-extended mode, and base `0x06300000`, size 8 MB, block size `0x40` in extended
  mode (`bios.c:462-471`).

### 2.4 Low Work RAM — `0x00200000`–`0x002FFFFF`

- `u8 *LowWram`, `T2MemoryInit(0x100000)` = 1 MB (`yabause.c:220`).
- Mask `addr & 0xFFFFF` (`memory.c:397-435`) — exactly 1 MB, so the fill window and the device
  are the same size and **there is no mirroring inside the window**.
- T2 storage (see §6): byte accesses go through `mem[addr ^ 1]`, longs through `WSWAP32`.

### 2.5 SH-2 FRT input capture windows — `0x01000000`–`0x01FFFFFF`

Two 8 MB windows where **only 16-bit writes do anything** (`memory.c:633-644`):

| Window | Word write | Everything else |
|---|---|---|
| `0x01000000`–`0x017FFFFF` | `SSH2InputCaptureWriteWord` | `UnhandledMemory*` |
| `0x01800000`–`0x01FFFFFF` | `MSH2InputCaptureWriteWord` | `UnhandledMemory*` |

`MSH2InputCaptureWriteWord` (`sh2core.c:2569-2582`) ignores both the address and the data: it
sets `FTCSR |= 0x80`, copies `FRC` into `FICR`, and raises the FRT interrupt if `TIER & 0x80`.
`SSH2InputCaptureWriteWord` is the mirror image (`sh2core.c:2600-2613`).

Note the pairing: the **lower** window drives the **slave** SH-2 and the **upper** window drives
the **master**. That is what the code does; nothing in these files justifies it.

Byte and long writes, and all reads, fall to `UnhandledMemory*` — reads return `0`, writes log.

### 2.6 A-Bus CS0 — `0x02000000`–`0x03FFFFFF`

Filled directly with the cartridge's own function pointers,
`CartridgeArea->Cs0ReadByte` … `Cs0WriteLong` (`memory.c:645-650`). There is **no wrapper** — CS0
is the only chip-select whose handlers are installed into the page table by value. Full detail in
§9.

Because the pointers are copied into the table at boot, replacing `CartridgeArea` later does not
update the table. See §13 item 20.

### 2.7 A-Bus CS1 — `0x04000000`–`0x04FFFFFF`

Filled with the `Cs1*` wrappers in `cs1.c`, which forward to `CartridgeArea->Cs1*` after a
cart-ID special case. Full detail in §10.

### 2.8 A-Bus CS2 (CD block) — `0x05800000`–`0x058FFFFF`

- Word and long accesses go to the CD block: `Cs2ReadWord`/`Cs2ReadLong`/`Cs2WriteWord`/
  `Cs2WriteLong` mask `addr &= 0xFFFFF` (`cs2.c:159`, `:296`, `:343`, `:448`) — the mask exactly
  matches the 1 MB window, so no mirroring. Each carries the comment
  `// fix me(I should really have proper mapping)`. The register decode is a flat `switch` on
  offsets such as `0x90008` (HIRQ), `0x9000C` (HIRQMASK), `0x18000` (data transfer FIFO).
- **Byte accesses never reach the CD block.** `Cs2ReadByte` and `Cs2WriteByte` are
  one-line forwarders to `CartridgeArea->Cs2ReadByte` / `Cs2WriteByte` (`cs2.c:143-153`). With
  no cartridge that is `DummyCs2ReadByte` → `0xFF` (`cs0.c:135-138`). This is how the Netlink
  and Japanese Modem carts get their 8-bit UART window inside the CS2 area; the cost is that the
  CD block itself has no byte interface at all.

### 2.9 Sound RAM — `0x05A00000`–`0x05AFFFFF`

- `SoundRam = T2MemoryInit(0x80000)` = **512 KB** (`scsp.c:5062`).
- All six handlers first mask `addr &= 0xFFFFF` (1 MB), then apply the SCSP `MEM4MB` bit
  (`scsp.c:4869-5055`):

  ```c
  if (scsp.mem4b == 0) addr &= 0x3FFFF;      // 256 KB mode: mirror every 256 KB
  else if (addr > 0x7FFFF) return 0xFFFF;    // 512 KB mode: above the device -> all ones
  ```

  So the mirror period is **runtime-selectable**: 256 KB when `mem4b == 0`, and 512 KB-with-a-hole
  when `mem4b != 0` (the top half of the 1 MB window reads all-ones and drops writes).
- Writes call `M68K->WriteNotify(addr, size)` (`scsp.c:4899`, `:4972`, `:5049`) so the 68000 core
  can invalidate anything it caches.
- Reads call `SyncSh2And68k()` (`scsp.c:4906`, `:4952`, `:4995`), which every 512 accesses either
  signals the SCSP thread's condvar or yields, depending on `g_scsp_main_mode`. This is a
  scheduling hook, not a hardware behaviour.
- **[BUG]** `SoundRamReadByte` (`scsp.c:4869-4883`) sets `val = 0xFF` for `addr > 0x7FFFF` but
  then **unconditionally overwrites it** with `T2ReadByte(SoundRam, addr)`, where `addr` can be
  up to `0xFFFFF` against a `0x80000`-byte allocation. Byte reads of the upper half of the sound
  RAM window read 512 KB past the end of the buffer. The word and long variants return early and
  are correct.

### 2.10 SCSP registers — `0x05B00000`–`0x05BFFFFF`

- `scsp_r_b`/`scsp_w_b` mask `a &= 0xFFF`, `scsp_r_w`/`scsp_w_w` mask `a &= 0xFFE`,
  `scsp_r_d`/`scsp_w_d` mask `a &= 0xFFC` (`scsp.c:4380`, `:4423`, `:4515`, `:4135`, `:4249`,
  `:4333`) → the 4 KB register file **mirrors 256 times** across the 1 MB window, and unaligned
  word/long accesses are silently rounded down.
- Misalignment is *logged* (`"ERROR: scsp r_w misaligned"`, `scsp.c:4420`) but not faulted.
- Sub-decode: `a < 0x400` → slot registers (`a >> 5` selects the slot); `a < 0x600` with
  `a < 0x440` → common registers; the rest is the DSP/MIDI area.

### 2.11 VDP1 — `0x05C00000`–`0x05D7FFFF`

| Sub-region | Window | Device | Mask | Mirror |
|---|---|---|---|---|
| VRAM | `0x05C00000`–`0x05C7FFFF` (512 KB) | `T1MemoryInit(0x80000)` (`vdp1.cpp:286`) | `0x7FFFF` (`vdp1.cpp:146`,`153`,`160`,`167`,`175`,`183`) | none |
| Framebuffer | `0x05C80000`–`0x05CFFFFF` (512 KB) | 2 × `T1MemoryInit(0x40000)` (`vdp1.cpp:290-293`) | `0x3FFFF` (`vdp1.cpp:194`,`208`,`222`,`236`,`251`,`266`) | **×2** |
| Registers | `0x05D00000`–`0x05D7FFFF` (512 KB) | 256-byte register file | `0xFF` (`vdp1.cpp:418`,`426`,`451`,`459`,`468`,`544`) | **×2048** |

- The framebuffer window always addresses `Vdp1FrameBuffer[Vdp1External.current_frame]`
  (`vdp1.cpp:202`), i.e. the CPU sees the *back* buffer selection maintained by the VDP1 core;
  there is no separate "draw"/"display" address decode.
- Framebuffer accesses can be routed to the video backend instead of the shadow buffer when
  `VIDCore->Vdp1ReadFrameBuffer` / `Vdp1WriteFrameBuffer` are non-NULL (`vdp1.cpp:209-215`,
  `:238-243`), under `VdpLockVram()`. Byte *reads* have that path commented out
  (`vdp1.cpp:195-201`) and always read the shadow buffer.
- **VDP1 registers are 16-bit only**: `Vdp1ReadByte`/`Vdp1ReadLong` return `0` and log;
  `Vdp1WriteByte`/`Vdp1WriteLong` log and discard (`vdp1.cpp:417-421`, `:450-454`, `:458-461`,
  `:543-546`).
- VRAM writes reset `vdp1_clock = 0` and optionally mark tile-cache pages dirty
  (`vdp1.cpp:168-171`) — emulator bookkeeping, not hardware.

### 2.12 VDP2 — `0x05E00000`–`0x05FBFFFF`

| Sub-region | Window | Device | Mask | Mirror |
|---|---|---|---|---|
| VRAM | `0x05E00000`–`0x05EFFFFF` (1 MB) | `T1MemoryInit(0x80000)` (`vdp2.cpp:379`) | `0x7FFFF` (`vdp2.cpp:174`,`181`,`188`,`198`,`228`,`258`) | **×2** |
| Colour RAM | `0x05F00000`–`0x05F7FFFF` (512 KB) | `T2MemoryInit(0x1000)` (`vdp2.cpp:382`) | `0xFFF` (`vdp2.cpp:288`,`295`,`302`,`309`,`317`,`343`) | **×128** |
| Registers | `0x05F80000`–`0x05FBFFFF` (256 KB) | 512-byte register file | `0x1FF` (`vdp2.cpp:1450`,`1457`,`1920`,`1928`,`1934`) | **×512** |

- **VDP2 registers are 16-bit only for reads**: `Vdp2ReadByte`/`Vdp2ReadLong` mask and return `0`
  with a log (`vdp2.cpp:1448-1452`, `:1918-1922`); `Vdp2WriteByte` masks and discards
  (`vdp2.cpp:1926-1929`). `Vdp2WriteLong` is the one exception — it decomposes into two
  `Vdp2WriteWord` calls, high half first (`vdp2.cpp:2388-2393`).
- Colour RAM writes are conditional on `Vdp2Internal.ColorMode` and notify the GL backend
  (`vdp2.cpp:319-324`, `:346-350`), which changes *when* a write is committed but not *where*.

### 2.13 SCU registers — `0x05FE0000`–`0x05FEFFFF`

- All six handlers mask `addr &= 0xFF` (`scu.c:2674`, `:2690`, `:2699`, `:2751`, `:2770`,
  `:2777`) → the 256-byte register file **mirrors 256 times** across the 64 KB window.
- The window is exactly one table entry (`FillMemoryArea(0x5FE, 0x5FE, …)`, `memory.c:711`).
  `0x05FF0000`–`0x05FFFFFF` is *not* SCU; it is unmapped.
- **Byte access is limited to one register**: `ScuReadByte`/`ScuWriteByte` handle only offset
  `0xA7` (the low byte of IST) and log everything else (`scu.c:2673-2683`, `:2750-2767`).
  **Word access does nothing at all** — both `ScuReadWord` and `ScuWriteWord` only log
  (`scu.c:2689-2694`, `:2769-2772`). The SCU is effectively a long-word-only peripheral here.

### 2.14 High Work RAM — `0x06000000`–`0x0610FFFF`

- `u8 *HighWram`, `T2MemoryInit(0x100000)` = 1 MB (`yabause.c:217`).
- Mask `addr & 0xFFFFF` (`memory.c:355-393`).
- **The fill covers 17 table entries, not 16**: `FillMemoryArea(0x600, 0x610, …)`
  (`memory.c:717`). Index `0x610` is `0x06100000`–`0x0610FFFF`, which the mask folds onto
  `HighWram[0x00000..0x0FFFF]`. So there is exactly **one 64 KB mirror** of the first 64 KB of
  High WRAM, and then the map stops. `0x06110000`–`0x062FFFFF` is unmapped, and
  `0x06300000` onward is backup RAM (§2.15).

  This is almost certainly an off-by-one — the natural intent is `0x600`–`0x60F`. As written it
  produces a 64 KB window of extra RAM that no real machine has, and it leaves a ~2 MB hole
  where hardware would mirror High WRAM. See §13 item 2 for the disagreement with the
  instruction-fetch map.

### 2.15 Extended backup RAM window — `0x06300000`–`0x07FFFFFF`

- Filled unconditionally at `memory.c:723`, using the same `BupRamMemory*` handlers as §2.3.
- 464 table entries = **29 MB of address space** mapped onto an **8 MB** file
  (`tweak_backup_file_size`, `bios.c:53`). `BupRamMemoryReadByte` returns `0` for the
  `0x06B00000`–`0x07FFFFFF` remainder (`memory.c:501-503`); writes there are dropped
  (`memory.c:551-553`).
- **[QUIRK]** The fill is not gated on `yabsys.extend_backup`. In the default (non-extended)
  build the handlers take the `addr & 0x0000FFFF` branch, so `0x06300000`–`0x07FFFFFF` becomes
  a 464-fold mirror of the 64 KB internal backup RAM — 29 MB of address space that on hardware
  would be High WRAM mirrors or open bus.

### 2.16 Unmapped regions

Every table entry not covered above retains the initial fill of `UnhandledMemory*`
(`memory.c:601-606`). Behaviour (`memory.c:310-351`):

| Access | Result |
|---|---|
| read byte / word / long | returns `0`, logs `"Unhandled … read %08X"` |
| write byte / word / long | discarded, logs `"Unhandled … write %08X"` |

Reads return **zero**, not all-ones. The cartridge dummies (`cs0.c:57-170`) return
**all-ones**. Yabause therefore has two mutually inconsistent open-bus models depending on
whether an unmapped address happens to fall inside a chip-select window. Neither is derived from
anything in these files.

The `LOG` macro compiles out in release builds, so in practice unmapped accesses are silent.

---

## 3. Mirroring — complete list

Mirroring in this implementation comes from exactly two places: address bits dropped by the
decode (§0.2) and the per-handler mask (§0.3). There is no explicit mirror logic anywhere.

### 3.1 Global aliases (apply to every region)

| Alias | Cause | Cited |
|---|---|---|
| `+0x20000000` (cache-through) | `addr>>29` cases 0 and 1 share a body | `memory.c:859-860` |
| `+0x10000000` (bit 28) | bit 28 is in neither the area select nor the table index | `memory.c:864` |
| `+0x80000000` (area 4) | `addr>>29` case 4 shares the body with 0 and 1 | `memory.c:861` |

Combined, every mapped address appears at **six** distinct 32-bit addresses:
`X`, `X+0x10000000`, `X+0x20000000`, `X+0x30000000`, `X+0x80000000`, `X+0x90000000`.

### 3.2 Per-region mirror periods

| Region | Window | Handler mask | Device | Mirror period | Copies in window |
|---|---|---|---|---|---|
| BIOS ROM | 1 MB | `0x7FFFF` | 512 KB | 512 KB | 2 |
| SMPC | 512 KB | `0x7F` | 128 B | 128 B | 4096 |
| Internal backup (non-ext) | 512 KB | `0xFFFF` | 64 KB | 64 KB | 8 |
| Internal backup (ext) | 512 KB | subtract `0x06300000` | — | n/a — always reads `0` | — |
| Low WRAM | 1 MB | `0xFFFFF` | 1 MB | none | 1 |
| CS0 (DRAM 32 Mbit) | 32 MB | `0x1FFFFFF` then `>>20` decode | 4 MB | window-limited | see §9.4 |
| CS0 (ROM 16 Mbit) | 32 MB | `0x1FFFFF` | 2 MB | **2 MB** | **16** |
| CS1 | 16 MB | `0xFFFFFF` | per cart | per cart | see §10 |
| CS2 | 1 MB | `0xFFFFF` | 1 MB | none | 1 |
| Sound RAM (`mem4b==0`) | 1 MB | `0xFFFFF` → `0x3FFFF` | 256 KB | 256 KB | 4 |
| Sound RAM (`mem4b!=0`) | 1 MB | `0xFFFFF`, `>0x7FFFF` → all-ones | 512 KB | none | 1 + hole |
| SCSP registers | 1 MB | `0xFFF` | 4 KB | 4 KB | 256 |
| VDP1 VRAM | 512 KB | `0x7FFFF` | 512 KB | none | 1 |
| VDP1 framebuffer | 512 KB | `0x3FFFF` | 256 KB | 256 KB | 2 |
| VDP1 registers | 512 KB | `0xFF` | 256 B | 256 B | 2048 |
| VDP2 VRAM | 1 MB | `0x7FFFF` | 512 KB | 512 KB | 2 |
| VDP2 colour RAM | 512 KB | `0xFFF` | 4 KB | 4 KB | 128 |
| VDP2 registers | 256 KB | `0x1FF` | 512 B | 512 B | 512 |
| SCU registers | 64 KB | `0xFF` | 256 B | 256 B | 256 |
| High WRAM | 1 MB + 64 KB | `0xFFFFF` | 1 MB | 1 MB | 1 + 64 KB tail |
| Extended backup (ext) | 29 MB | subtract base, bound 8 MB | 8 MB | none | 1 + 21 MB of zeros |
| Extended backup (non-ext) | 29 MB | `0xFFFF` | 64 KB | 64 KB | 464 |

**Regions that do *not* mirror inside their window** — Low WRAM, CS2, VDP1 VRAM — are the ones
where the fill width and the device size coincide. Do not assume mirroring for these.

---

## 4. Byte / word / long dispatch

### 4.1 Three parallel tables, not one

There is no size parameter anywhere in the dispatch path. `ReadByteList`, `ReadWordList` and
`ReadLongList` are separate arrays, and `FillMemoryArea` takes six function pointers so a region
can install a different handler per width (`memory.c:578-594`). The FRT input-capture windows
(§2.5) exploit this to map *only* the word-write path.

A consequence: a region whose handler set is incomplete simply behaves differently per width, and
nothing detects that. SMPC is word/long-dead, VDP1 and VDP2 registers are byte/long-dead, SCU is
word-dead, internal backup RAM is word/long-dead.

### 4.2 There is no unaligned-access check

`memory.c` never inspects the low address bits. The T1/T2/T3 accessors in `memory.h` do raw
pointer casts (`*((u16 *)(mem + addr))`, `memory.h:66`, `:119`) with no alignment handling, so an
unaligned word or long access to RAM is a misaligned host load — undefined behaviour in C, and in
practice whatever the host CPU does. No SH-2 address-error exception is ever raised from this
path. The only alignment awareness in the whole map is the SCSP, which *logs* misalignment and
then rounds the address down (`scsp.c:4418-4423`).

### 4.3 Sub-word access into wider storage

Where a device is narrower than the access, the pattern is composition, not a bus model:

| Case | Behaviour | Cited |
|---|---|---|
| `Vdp2WriteLong` | two `Vdp2WriteWord` calls, **high half first** | `vdp2.cpp:2388-2392` |
| `FlashCs0ReadWord` | `(ReadByte(addr) << 8) \| ReadByte(addr+1)` — big-endian composition | `cs0.c:239` |
| `FlashCs0ReadLong` | `(ReadWord(addr) << 16) \| ReadWord(addr+2)` | `cs0.c:246` |
| `FlashCs0WriteWord` | `WriteByte(addr, val>>8); WriteByte(addr+1, val)` | `cs0.c:337-338` |
| `FlashCs0WriteLong` | two `FlashCs0WriteWord` | `cs0.c:345-346` |
| `scsp_r_d` (slot area) | `(read_word(a) << 16) \| read_word(a+2)` | `scsp.c:4520-4522` |
| `Cs1ReadWord` ID | `0xFF00 \| cartid` — the 8-bit ID in the low byte, upper byte pulled high | `cs1.c:48` |
| `Cs1ReadLong` ID | `0xFF00FF00 \| (cartid << 16) \| cartid` — ID replicated in both halves | `cs1.c:60` |

The `Cs1ReadLong` composition is the clearest statement in the codebase of the A-Bus open-bus
model: unconnected byte lanes read as `0xFF`.

---

## 5. Storage models: T1, T2, T3

`memory.h` defines three byte-ordering conventions. Which one a region uses determines what a
byte access to an odd address actually touches, so this is part of the memory map, not an
implementation detail.

### 5.1 T1 — "faster for byte accesses" (`memory.h:52-101`)

Backing store holds data in **big-endian byte order**.

```c
T1ReadByte(mem, addr)  ->  mem[addr]
T1ReadWord(mem, addr)  ->  BSWAP16L(*(u16*)(mem + addr))   // little-endian host
T1ReadLong(mem, addr)  ->  BSWAP32(*(u32*)(mem + addr))
```

Used by: internal backup RAM, all cartridge DRAM and backup-RAM images, the 16 Mbit ROM cart,
VDP1 VRAM, VDP1 framebuffer, VDP2 VRAM.

### 5.2 T2 — "faster for word accesses" (`memory.h:103-152`)

Backing store holds an array of **host-native 16-bit words** laid out in big-endian *word* order.
Byte access XORs the address.

```c
T2ReadByte(mem, addr)  ->  mem[addr ^ 1]                   // little-endian host
T2ReadWord(mem, addr)  ->  *(u16*)(mem + addr)             // no swap
T2ReadLong(mem, addr)  ->  WSWAP32(*(u32*)(mem + addr))    // swap halfwords only
```

Used by: BIOS ROM, Low WRAM, High WRAM, sound RAM, VDP2 colour RAM, the Action Replay / USB Dev
flash firmware image.

`T2MemoryInit` and `T2MemoryDeInit` are `#define`d straight to the T1 versions
(`memory.h:105-106`) — the difference is purely in the accessors.

### 5.3 T3 — "faster for long accesses" (`memory.h:154-217`, `memory.c:269-293`)

Reverse-addressed: `mem->mem` points at the *end* of the buffer and accessors index backwards
(`(mem->mem - addr - 1)[0]`).

**[QUIRK] `T3MemoryInit` is never called anywhere in the tree.** No region in the memory map uses
T3. It is dead code, carried along with `T123Load`/`T123Save` support for `type == 3`
(`memory.h:273-277`, `:320-323`).

### 5.4 `T123Load` / `T123Save`

`T123Load(mem, size, type, filename)` (`memory.h:219-289`) loads a file byte-by-byte through the
type-appropriate `WriteByte`, so the on-disk image is always plain big-endian regardless of host
endianness. **An oversized file is silently truncated to `size`** (`memory.h:237-242` — the
`return -1` is commented out); an undersized file leaves the tail as whatever `calloc` produced,
i.e. zeros. `T123Save` is the inverse and treats an empty filename as success
(`memory.h:298-302`).

---

## 6. Access cost model

`MappedMemory*` take a trailing `u32 *cycle` parameter (`memory.h:365-370`). If non-NULL, a
coarse per-1 MB lookup writes a cycle penalty before the access is performed.

`GET_MEM_CYCLE_R` (`memory.c:771-798`), switching on `addr & 0xDFF00000`:

| Masked address | Region | Read cycles |
|---|---|---|
| `0x00000000` | BIOS ROM | 16 |
| `0x00100000` | *(comment says "Backup"; actually SMPC + backup RAM)* | 16 |
| `0x00200000` | Low WRAM | 12 |
| `0x02000000` | CS0 | 24 |
| `0x05800000` | CS2 | 24 |
| `0x05A00000` | Sound RAM | 50 |
| `0x05B00000` | SCSP registers | 50 |
| `0x05C00000` | VDP1 VRAM | 50 |
| `0x05E00000` | VDP2 VRAM | `getVramCycle(addr)` |
| `0x06000000` | High WRAM | 0 |
| *(anything else)* | — | 0 |

`GET_MEM_CYCLE_W` (`memory.c:749-769`):

| Masked address | Region | Write cycles |
|---|---|---|
| `0x00200000` | Low WRAM | 7 |
| `0x05A00000` | Sound RAM | 7 |
| `0x05C00000` | VDP1 | 2 |
| `0x05E00000` | VDP2 | `getVramCycle(addr)` |
| `0x06000000` | High WRAM | 2 |
| *(anything else)* | — | 0 |

`getVramCycle` (`memory.c:735-746`):

```c
if (yabsys.LineCount >= yabsys.VBlankLineCount) return 2;       // during VBlank
if ((addr & 0x000F0000) < 0x00040000) return Vdp2External.cpu_cycle_a;   // banks A0/A1
else                                  return Vdp2External.cpu_cycle_b;   // banks B0/B1
```

i.e. the VDP2 VRAM penalty is bank-dependent (split at offset `0x40000`) and comes from the
VDP2's CPU access-cycle allocation, except during vertical blanking where it is a flat 2.

Who passes a non-NULL pointer:

- The SH-2 interpreter does, for every data access — e.g. `MappedMemoryReadByte(…, &rcycle)`
  (`sh2int.c:466`, `:1434`, `:1447`, `:1460`).
- **SCU DMA does not.** Every `MappedMemoryReadLong`/`WriteWord`/`WriteLong` in `scu.c` passes
  `NULL` (`scu.c:247-344`, `:402-404`). DMA transfers are free.
- Exception-stacking pushes in `sh2int.c:187-189`, `:341-345` also pass `NULL`.

**[QUIRK]** The numbers have no derivation in the source, the granularity is 1 MB, and large
parts of the map (CS1, SMPC, SCU registers, VDP1 registers, VDP2 colour RAM, VDP2 registers, the
extended backup window) cost zero. High WRAM reads cost 0 while High WRAM writes cost 2. Bit 28
mirrors cost 0 while the base address does not. Treat this as a heuristic, not a bus model.

---

## 7. The separate instruction-fetch map

`memory.c` is **not** the only address decoder. The SH-2 interpreter fetches opcodes through its
own 256-entry table indexed by `(addr >> 20) & 0xFF` — address bits **27:20**, so 1 MB
granularity, with bits 31:28 discarded entirely (`sh2int.c:174`, `:295`, `:3121`, `:3171`).

Built in `SH2InterpreterInit` (`sh2int.c:2934-2971`):

| Index | Address range | Fetch function | Behaviour |
|---|---|---|---|
| `0x000` | `0x00000000`–`0x000FFFFF` | `FetchBios` | `T2ReadWord(BiosRom, addr & 0x7FFFF)` |
| `0x002` | `0x00200000`–`0x002FFFFF` | `FetchLWram` | `T2ReadWord(LowWram, addr & 0xFFFFF)` |
| `0x020` | `0x02000000`–`0x020FFFFF` | `FetchCs0` | `CartridgeArea->Cs0ReadWord(addr)` |
| `0x05C` | `0x05C00000`–`0x05CFFFFF` | `FetchVram` | `T1ReadWord(Vdp1Ram, addr & 0x7FFFF)` — commented **`// Fighting Viper`** |
| `0x060`–`0x06F` | `0x06000000`–`0x06FFFFFF` | `FetchHWram` | `T2ReadWord(HighWram, addr & 0xFFFFF)` |
| *(all others)* | — | `FetchInvalid` | returns `0xFFFF` |

Discrepancies with the data map that Mimas needs to be aware of:

1. **High WRAM**: the fetch map covers the *full* 16 MB `0x06000000`–`0x06FFFFFF`, mirrored every
   1 MB. The data map stops at `0x0610FFFF` and then hands `0x06300000`+ to backup RAM (§2.14,
   §2.15). Executing from `0x06400000` reads High WRAM; reading data from `0x06400000` reads
   backup RAM. The same address is two different things depending on how you touch it.
2. **CS0**: only the first 1 MB is executable. Code in a DRAM cart at `0x02400000` fetches
   `0xFFFF` from `FetchInvalid`.
3. **[HACK]** Index `0x05C` maps VDP1 VRAM as executable with the comment `// Fighting Viper`
   (`sh2int.c:2947-2948`) — a game-specific entry. Note the mask is `0x7FFFF` while the index
   covers `0x05C00000`–`0x05CFFFFF`, so a fetch from the *framebuffer* half `0x05C80000`+ wraps
   back to VDP1 VRAM offset 0.
4. **[HACK]** `FetchBios` contains a backup-library trap: when `yabsys.extend_backup` is set, a
   fetch at BIOS offset `0x0007D600` returns `0`, and once `extend_backup == 2` any fetch in
   `0x0380`–`0x03A8` also returns `0` (`sh2int.c:209-221`). The matching `SH2undecoded` handler
   (`sh2int.c:313-329`) intercepts those PCs and calls `BiosBUPInit` / `BiosHandleFunc` instead —
   the emulator replaces the BIOS backup library with native code so it can address an 8 MB
   backup file. Any Mimas equivalent of the extended-backup feature needs the same hook or the
   `0x06300000` window is unreachable through the BIOS API.
5. With `EXEC_FROM_CACHE` defined, `SH2delay` additionally short-circuits
   `(addr & 0xC0000000) == 0xC0000000` to `DataArrayReadWord` before consulting the table
   (`sh2int.c:292-295`).

---

## 8. The SH-2 cache arrays as memory regions

Two areas of the stage-1 map are the SH-2's own cache structures rather than the system bus.

### 8.1 Address array — area 3, `0x60000000`–`0x7FFFFFFF`

Long access only (`memory.c:1001-1005`, `:1196-1200`). Byte and word accesses fall through to
`default:` and hit `UnhandledMemory*`.

`AddressArrayReadLong` (`sh2core.c:1932-1943`) / `AddressArrayWriteLong` (`:1947-1957`):

- Non-cache build: a flat `CurrentSH2->AddressArray[(addr & 0x3FC) >> 2]` — 256 longs, mirrored
  every 1 KB across the whole 512 MB area.
- Cache build: way = `(CCR >> 6) & 3`, entry = `(addr & 0x3FC) >> 4`; a read composes
  `tag | (lru << 4) | (v << 2)`; a write sets `tag = addr & 0x1FFFFC00`, `v = (addr >> 2) & 1`,
  `lru = (val >> 4) & 0x3F`. Note the write takes the tag from the **address**, not the data.

### 8.2 Data array — area 6, `0xC0000000`–`0xDFFFFFFF`

All three widths (`memory.c:876-878`, `:940-942`, `:1007-1009`, `:1069-1072`, `:1133-1136`,
`:1203-1206`).

`DataArrayReadByte/Word/Long` (`sh2core.c:1961-1997`):

- Non-cache build: `T2Read*(CurrentSH2->DataArray, addr & 0xFFF)` — a 4 KB scratchpad mirrored
  every 4 KB across the whole 512 MB area.
- Cache build: way = `(addr >> 10) & 3`, entry = `(addr >> 4) & 0x3F`, byte = `addr & 0xF`,
  reading the cache line data directly and composing multi-byte values big-endian.

Both arrays are per-CPU (`CurrentSH2`), so which SH-2 is executing changes what these addresses
contain.

---

## 9. A-Bus CS0 — the cartridge slot

CS0 occupies `0x02000000`–`0x03FFFFFF` (32 MB). `cs0.c` models eleven cartridge types plus "no
cart", selected by an integer passed to `CartInit(filename, type)` (`cs0.c:1046`), called once
from `yabause.c:265`.

### 9.1 The cartridge vtable

`cartridge_struct` (`cs0.h:58-88`) holds **eighteen** function pointers — a full read/write ×
byte/word/long set for each of CS0, CS1 and CS2 — plus `carttype`, `cartid`, `filename`, and
three storage pointers `rom`, `bupram`, `dram`.

A single cartridge object therefore owns all three A-Bus chip selects. `CartInit` first installs
the dummy set for all eighteen slots (`cs0.c:1055-1074`) and then the type-specific `switch`
overrides only the ones that cartridge implements (`cs0.c:1076-1302`).

How each chip select reaches the vtable differs:

| Chip select | Path |
|---|---|
| CS0 | the vtable pointers are **copied into the page table** at `memory.c:645-650` |
| CS1 | the page table holds `Cs1*` wrappers in `cs1.c`, which call the vtable at call time |
| CS2 | byte accesses only; `cs2.c:143-153` calls the vtable at call time |

### 9.2 Cartridge types and IDs

| Constant | Value | `cartid` | Allocations | CS0 | CS1 | CS2 | `CartInit` line |
|---|---|---|---|---|---|---|---|
| `CART_NONE` | 0 | `0xFF` | none | dummy | dummy | dummy | `cs0.c:1297-1301` |
| `CART_PAR` | 1 | `0x5C` | `rom` 256 KB (T2), `dram` 4 MB (T1) | `AR4M*` | dummy | dummy | `cs0.c:1078-1105` |
| `CART_BACKUPRAM4MBIT` | 2 | `0x21` | `bupram` 1 MB (T1) | dummy | `BUP4MBIT*` | dummy | `cs0.c:1107-1125` |
| `CART_BACKUPRAM8MBIT` | 3 | `0x22` | `bupram` 2 MB | dummy | `BUP8MBIT*` | dummy | `cs0.c:1127-1145` |
| `CART_BACKUPRAM16MBIT` | 4 | `0x23` | `bupram` 4 MB | dummy | `BUP16MBIT*` | dummy | `cs0.c:1147-1165` |
| `CART_BACKUPRAM32MBIT` | 5 | `0x24` | `bupram` 8 MB | dummy | `BUP32MBIT*` | dummy | `cs0.c:1167-1185` |
| `CART_DRAM8MBIT` | 6 | `0x5A` | `dram` 1 MB (T1) | `DRAM8MBIT*` | dummy | dummy | `cs0.c:1187-1201` |
| `CART_DRAM32MBIT` | 7 | `0x5C` | `dram` 4 MB (T1) | `DRAM32MBIT*` | dummy | dummy | `cs0.c:1203-1217` |
| `CART_NETLINK` | 8 | `0xFF` | none | dummy | dummy | `NetlinkRead/WriteByte` | `cs0.c:1219-1225` |
| `CART_ROM16MBIT` | 9 | `0xFF` | `rom` 2 MB (T1) | `ROM16MBIT*` | dummy | dummy | `cs0.c:1226-1245` |
| `CART_JAPMODEM` | 10 | `0xFF` | none | `JapModemCs0*` (read only) | `JapModemCs1*` | `JapModemCs2*Byte` | `cs0.c:1246-1264` |
| `CART_USBDEV` | 11 | `0x00` | `rom` 256 KB (T2), `dram` 4 MB (T1) | `AR4M*` | dummy | dummy | `cs0.c:1265-1295` |

Constants at `cs0.h:45-56`.

**The ID mechanism.** `cartid` is a single byte, readable only through CS1 (§10). Two facts
follow from how `bios.c` consumes it (`bios.c:474-482`):

```c
if ((CartridgeArea->cartid & 0xF0) == 0x20) {     // backup-RAM cart family
   *addr = 0x04000000;
   *size = 0x40000 << (CartridgeArea->cartid & 0x0F);
   *blocksize = (CartridgeArea->cartid == 0x24) ? 0x400 : 0x200;
}
```

- The high nibble `0x2` identifies the **backup RAM cart family**; the low nibble is a size code,
  and the usable data size is `0x40000 << code` — exactly **half** the allocation, because the
  cart backup RAM is likewise odd-byte-only.
- `0x5A` / `0x5C` identify **DRAM (extended RAM) carts**; the low nibble distinguishes 8 Mbit
  from 32 Mbit.
- `0xFF` is "nothing recognisable", shared by no-cart, Netlink, the ROM cart and the modem.

**[QUIRK]** `CART_PAR` reports `0x5C`, the 32 Mbit DRAM ID, with the explicit comment
`// Use 32 Mbit Dram id` (`cs0.c:1086`) — so software sees an Action Replay as a plain 4 MB RAM
cart. `CART_ROM16MBIT` carries the comment `// I have no idea what the real id is`
(`cs0.c:1231`). `CART_USBDEV` reports `0x00` with `// No extra dram, etc. built-in`
(`cs0.c:1273`) even though it installs the AR4M handlers, which *do* expose 4 MB of DRAM at
`0x02400000`.

### 9.3 No-cart / dummy behaviour

`DummyCs0ReadByte/Word/Long` return `0xFF` / `0xFFFF` / `0xFFFFFFFF`; the write functions are
empty (`cs0.c:57-92`). Identical dummy sets exist for CS1 (`cs0.c:96-131`) and CS2
(`cs0.c:135-170`). This is the A-Bus open-bus model: **all ones**.

### 9.4 DRAM (extended RAM) carts

**8 Mbit / 1 MB — `DRAM8MBITCs0*` (`cs0.c:591-701`).** After `addr &= 0x1FFFFFF`, dispatch on
`addr >> 20`:

| `addr>>20` | CS0 address | Maps to | Line |
|---|---|---|---|
| `0x04` | `0x02400000`–`0x024FFFFF` | `dram[addr & 0x7FFFF]` (mirrored ×2 in the 1 MB block) | `cs0.c:597-598` |
| `0x06` | `0x02600000`–`0x026FFFFF` | `dram[0x80000 \| (addr & 0x7FFFF)]` (mirrored ×2) | `cs0.c:599-600` |
| all others | — | read all-ones, write dropped | `cs0.c:601-605` |

So the 1 MB cart appears as **two 512 KB banks at `0x02400000` and `0x02600000`**, with
`0x02500000` and `0x02700000` reading `0xFF`/`0xFFFF`/`0xFFFFFFFF`. This split-bank layout is the
distinguishing behaviour of the 1 MB cart and software detects it.

**32 Mbit / 4 MB — `DRAM32MBITCs0*` (`cs0.c:707-820`).** Banks `0x04`–`0x07` all map to
`dram[addr & 0x3FFFFF]`, giving a **contiguous 4 MB at `0x02400000`–`0x027FFFFF`**. Everything
else reads all-ones.

Neither type maps anything at `0x02000000`–`0x023FFFFF` or above `0x02800000`.

### 9.5 16 Mbit ROM cart — `ROM16MBITCs0*` (`cs0.c:1002-1040`)

The simplest handler in the file: `T1Read*(CartridgeArea->rom, addr & 0x1FFFFF)` with no bank
decode at all. The 2 MB image therefore **mirrors 16 times across the entire 32 MB CS0 window**,
starting at `0x02000000`.

**[BUG]** `ROM16MBITCs0WriteByte/Word/Long` (`cs0.c:1023-1040`) call `T1Write*` into the ROM
image. A "ROM" cart is fully writable, and the modification is not persisted (`CartFlush` only
saves `rom` for `CART_PAR`, `cs0.c:1312-1319`), so it silently diverges from the file on disk.

### 9.6 Action Replay 4M Plus / USB Dev — `AR4MCs0*` (`cs0.c:351-585`)

After `addr &= 0x1FFFFFF`, dispatch on `addr >> 20`:

| `addr>>20` | CS0 address | Behaviour | Line |
|---|---|---|---|
| `0x00`, `addr & 0x80000 == 0` | `0x02000000`–`0x0207FFFF` | flash EEPROM (§9.7) | `cs0.c:359-360` |
| `0x00`, `addr & 0x80000 != 0` | `0x02080000`–`0x020FFFFF` | "Outport" — **entirely commented out**, falls through to all-ones | `cs0.c:362-364` |
| `0x01` | `0x02100000`–`0x021FFFFF` | "Commlink Status Flag" / "Inport" — **entirely commented out** | `cs0.c:366-373` |
| `0x04`–`0x07` | `0x02400000`–`0x027FFFFF` | `dram[addr & 0x3FFFFF]` — 4 MB contiguous | `cs0.c:374-378` |
| `0x12`, `0x1E` | `0x03200000…`, `0x03E00000…` | word/long reads return `0xFFFD` / `0xFFFDFFFD` | `cs0.c:415-419`, `:463-467` |
| `0x13`, `0x16`, `0x17`, `0x1A`, `0x1B`, `0x1F` | various | word/long reads return `0xFFFD` / `0xFFFDFFFD` | `cs0.c:420-426`, `:468-474` |
| default | — | all-ones | `cs0.c:379-380` |

The `0xFFFD` returns exist only in the **word and long** read paths; `AR4MCs0ReadByte` has no
such cases and returns `0xFF` (`cs0.c:383`). They are presumably an AR detection signature; the
source does not say.

**[BUG]** `case 0x12: case 0x1E: if (0x80000) return 0xFFFD;` (`cs0.c:415-419` and `:463-467`).
`if (0x80000)` is a **constant-true** condition. The intent was clearly `if (addr & 0x80000)`, so
the intended half-block distinction is lost and the whole 1 MB block returns `0xFFFD`.

### 9.7 The flash EEPROM state machine — `Flash*` (`cs0.c:176-347`)

The AR/USB-Dev firmware ROM is modelled as **two independent 8-bit flash chips** on the even and
odd byte lanes: every access selects `flstate0`/`flreg0`/`flbuf0` or `flstate1`/`flreg1`/`flbuf1`
by `addr & 1` (`cs0.c:208-217`, `:257-268`).

Chip identity is a pair of globals, set by `CartInit`:

| Cart | `vendorid` | `deviceid` | Chip named in comments | Line |
|---|---|---|---|---|
| `CART_PAR` | `0x1F` | `0xD5` | AT29C010 | `cs0.c:1093-1094`, default at `:192-193` |
| `CART_USBDEV` | `0xBF` | `0xB5` | SST39SF010A | `cs0.c:1281-1282` |

States (`cs0.c:176-186`) and transitions (`FlashCs0WriteByte`, `cs0.c:251-331`):

| From | Trigger | To |
|---|---|---|
| `FL_READ` | write `0xAA` to `addr & 0xFFFE == 0xAAAA` | `FL_SDP` |
| `FL_SDP` | write `0x55` to `addr & 0xFFFE == 0x5554` | `FL_CMD` |
| `FL_SDP` | anything else | `FL_READ` |
| `FL_CMD` | write `0xA0` to `0xAAAA` | `FL_WRITEBUF` |
| `FL_CMD` | write `0x90` to `0xAAAA` | `FL_ID` |
| `FL_CMD` | anything else | `FL_READ` |
| `FL_WRITEBUF` | any write | buffers `val` at `buf[(addr >> 1) & 0x7F]`; when index `0x7F` is written, flushes 128 bytes and returns to `FL_READ` |
| `FL_ID` | write `0xAA` to `0xAAAA` | `FL_IDSDP`, else stays `FL_ID` |
| `FL_IDSDP` | write `0x55` to `0x5554` | `FL_READ`, else `FL_ID` |
| `FL_IDCMD` | write `0xF0` to `0xAAAA` | `FL_READ`, else `FL_ID` |

Reads (`FlashCs0ReadByte`, `cs0.c:203-233`):

- `FL_ID` / `FL_IDSDP` / `FL_IDCMD` → `deviceid` if `addr & 2`, else `vendorid`
- `FL_WRITEARRAY` → toggles `*reg ^= 0x02` then falls through to…
- `FL_WRITEBUF` → returns `*reg` (a DQ1 toggle-bit / data-polling stand-in)
- `FL_SDP` / `FL_CMD` → resets the state to `FL_READ`, falls through
- `FL_READ` / default → `T2ReadByte(CartridgeArea->rom, addr)`

The page-program flush (`cs0.c:278-288`):

```c
int j = addr & 0x1;
addr &= 0xffffff00;
for (i = 0; i <= 127; i++)
   T2WriteByte(CartridgeArea->rom, (addr + i*2 + j), buf[i]);
```

— 128 bytes per chip, striped across a 256-byte address window.

**[BUG] Out-of-bounds flash access.** `CartridgeArea->rom` is `T2MemoryInit(0x40000)` = 256 KB
(`cs0.c:1080`, `:1267`), but `AR4MCs0*` routes bank `0x00` with bit `0x80000` clear, so `addr`
reaches `0x7FFFF`. Both `T2ReadByte(CartridgeArea->rom, addr)` (`cs0.c:231`) and the flush's
`T2WriteByte` (`cs0.c:285`) are **unmasked**. Addresses `0x40000`–`0x7FFFF` read and write up to
256 KB past the end of the allocation.

**[QUIRK] `FL_WRITEARRAY` and `FL_IDCMD` are unreachable.** No transition in
`FlashCs0WriteByte` ever assigns either state, so the toggle-bit read path (`cs0.c:226`) and the
`0xF0` software-ID-exit path (`cs0.c:305-309`) are dead code. Exiting ID mode works only via the
two-cycle `AA`/`55` sequence, not the three-cycle `AA`/`55`/`F0`.

**[QUIRK] A partial page is never committed.** The flush fires only when the byte whose index is
`0x7F` is written (`cs0.c:278`). Real flash commits on a write timeout. Software that programs
fewer than 128 bytes, or programs them out of order ending on a non-`0x7F` index, loses the
write entirely.

`flreg0` / `flreg1` are file-scope and only ever zeroed at load and XOR-toggled from the dead
`FL_WRITEARRAY` path — they are effectively always `0`.

### 9.8 Netlink and Japanese Modem

- **`CART_NETLINK`** installs only `Cs2ReadByte`/`Cs2WriteByte` (`cs0.c:1222-1223`). `cs2.c`
  forwards *all* CS2 byte accesses to it (§2.8), and `NetlinkReadByte` decodes on
  `addr & 0xFFFFF` with 16550-style offsets such as `0x95001` (RBR/DLL), i.e. physical
  `0x05895001` (`netlink.c:91-118`). CS0 and CS1 stay dummy.
- **`CART_JAPMODEM`** installs read-only CS0 handlers that return a fixed signature —
  `0xA5` on odd byte addresses, `0xFF` on even; `0xFFA5` for words; `0xFFA5FFA5` for longs
  (`japmodem.c:35-54`) — plus full CS1 handlers and CS2 byte handlers. There is no CS0 write
  handler, so writes fall to the dummy.

### 9.9 Cartridge persistence

- `CartFlush` (`cs0.c:1309-1356`) saves the flash image only for `CART_PAR`, and saves `bupram`
  for the four backup-RAM cart sizes. DRAM carts and the ROM cart are never written back.
- `CartDeInit` (`cs0.c:1360-1422`) repeats the same saves then frees. Note `rom` is freed with
  `T2MemoryDeInit` for `CART_PAR` and `T1MemoryDeInit` otherwise (`cs0.c:1364-1377`) — both
  resolve to the same `free`, so the distinction is cosmetic.
- `CartSaveState` (`cs0.c:1426-1447`) writes the cart type and then **only the DRAM** for
  `CART_DRAM8MBIT` / `CART_DRAM32MBIT`. Cartridge backup RAM, the flash image and all AR DRAM are
  **not** in savestates.
- `CartLoadState` (`cs0.c:1451-1480`) reads the type and does nothing at all unless it is one of
  the two DRAM types; if the type changed it calls `CartDeInit()` then `CartInit(NULL, newtype)`.
  See §13 item 20 for why that is unsafe.

---

## 10. A-Bus CS1 — `0x04000000`–`0x04FFFFFF`

CS1 is a 16 MB window whose page-table entries hold the six wrappers in `cs1.c`, not the cart's
own pointers. Each wrapper does exactly three things (`cs1.c:31-99`):

```c
u8 FASTCALL Cs1ReadByte(u32 addr)
{
   addr &= 0xFFFFFF;                               // 1. mask to 24 bits
   if (addr == 0xFFFFFF)                           // 2. cart ID at the very top
      return CartridgeArea->cartid;
   return CartridgeArea->Cs1ReadByte(addr);        // 3. forward
}
```

| Function | Masked to | ID address | ID value | Line |
|---|---|---|---|---|
| `Cs1ReadByte` | `0xFFFFFF` | `0x04FFFFFF` | `cartid` | `cs1.c:31-39` |
| `Cs1ReadWord` | `0xFFFFFF` | `0x04FFFFFE` | `0xFF00 \| cartid` | `cs1.c:43-51` |
| `Cs1ReadLong` | `0xFFFFFF` | `0x04FFFFFC` | `0xFF00FF00 \| (cartid << 16) \| cartid` | `cs1.c:55-63` |
| `Cs1WriteByte` | `0xFFFFFF` | `0x04FFFFFF` → dropped | — | `cs1.c:67-75` |
| `Cs1WriteWord` | `0xFFFFFF` | `0x04FFFFFE` → dropped | — | `cs1.c:79-87` |
| `Cs1WriteLong` | `0xFFFFFF` | `0x04FFFFFC` → dropped | — | `cs1.c:91-99` |

The 24-bit mask exactly matches the 16 MB window, so CS1 does not mirror at the wrapper level.

**What CS1 is for, per the code:** it is the **cartridge backup RAM** window and the **cartridge
ID** port. Only three cart types install non-dummy CS1 handlers:

| Cart | CS1 handler | Mask | Effective range | Line |
|---|---|---|---|---|
| 4 Mbit backup | `BUP4MBITCs1*` | `0xFFFFF` | 1 MB, mirrored ×16 across CS1 | `cs0.c:826-864` |
| 8 Mbit backup | `BUP8MBITCs1*` | `0x1FFFFF` | 2 MB, mirrored ×8 | `cs0.c:870-908` |
| 16 Mbit backup | `BUP16MBITCs1*` | `0x3FFFFF` | 4 MB, mirrored ×4 | `cs0.c:914-952` |
| 32 Mbit backup | `BUP32MBITCs1*` | `0x7FFFFF` | 8 MB, mirrored ×2 | `cs0.c:958-996` |
| Japanese modem | `JapModemCs1*` | — | returns `0xFF`/`0xFFFF`/`0xFFFFFFFF`, writes logged | `japmodem.c:56-90` |

`bios.c:474-482` confirms the intended layout: the BIOS backup library is told the cart device
lives at `0x04000000` with size `0x40000 << (cartid & 0xF)`.

Two notes on the cart backup RAM handlers:

- Unlike the internal backup RAM (§2.3), **word and long accesses work** and there is **no
  `| 0x1` on writes** (`cs0.c:847-864`). Cart backup RAM is modelled as a plain byte array; the
  odd-byte convention survives only because `FormatBackupRam` writes the same `0xFF`/`0x00`
  pattern into it (`cs0.c:1116`, `:1136`, `:1156`, `:1176`) and the BIOS layer only ever uses
  half the space.
- The ID special case is a **single exact address per width**. A byte read of `0x04FFFFFE`, or a
  word read of `0x04FFFFFC`, misses the check and goes to the cart handler
  (`bupram[0x…FFFE]` or `0xFF` from the dummy). Nothing generalises the ID over the top few
  bytes.

---

## 11. Debug / tooling paths that touch the map

Not hardware, but they share the decoder and are worth knowing about when porting.

- **`MappedMemoryLoad` / `MappedMemorySave`** (`memory.c:1236-1308`) copy a file to/from the bus
  one byte at a time through `MappedMemoryWriteByte`/`ReadByte` with `cycle == NULL`. Loading
  across a register region therefore performs live register writes with side effects.
- **`MappedMemoryLoadExec`** (`memory.c:1312-1349`) dispatches on file extension to
  `MappedMemoryLoadCoff` / `MappedMemoryLoadElf`, otherwise resets, calls `YabauseSpeedySetup`,
  loads the raw image at `pc` and sets `MSH2->regs.PC`.
- **`MappedMemorySearch`** (`memory.c:2105-2224`) and `SearchString` (`:1969-2101`) walk the map
  byte/word/long through the same accessors — again with register side effects on read.
- **Memory breakpoints** (`sh2core.c:787-848`) *overwrite table entries in place*, saving the old
  pointer in the breakpoint record and restoring it on clear (`sh2core.c:907`). Any Mimas design
  that makes the dispatch table immutable loses this.
- **Savestate "OTHR" chunk** (`memory.c:1572-1590`): writes `0x10000` bytes of `BupRam`,
  `0x100000` of `HighWram`, `0x100000` of `LowWram`, then timing state. The loader
  (`memory.c:1828-1831`) **skips** the backup RAM with `fseek(fp, 0x10000, SEEK_CUR)` and reads
  the two WRAMs. The write is annotated `// do we really want to save this?`. In extended-backup
  mode `BupRam` is an 8 MB mapping and only the first 64 KB is written.

---

## 12. Known deviations, bugs and gaps in this implementation

Numbered so they can be referenced from Mimas code comments. Section references are to this
document.

### Address decode

| # | Item | Kind | Where |
|---|---|---|---|
| 1 | **Bit 28 is ignored by the entire decoder.** Every region is spuriously mirrored at `+0x10000000`, so the map repeats six times across the 32-bit space instead of the two (cached/through) that hardware provides. | [BUG] | §0.2, §3.1 |
| 2 | **Area 4 (`0x80000000`–`0x9FFFFFFF`) is routed to the normal peripheral table.** Nothing in the source justifies treating it as a third alias of the system bus. | [QUIRK] | §0.1 |
| 3 | **Area 5 (`0xA0000000`–`0xBFFFFFFF`) is lumped in with the associative-purge area** and returns all-ones. | [QUIRK] | §0.1 |
| 4 | **Area 3 (address array) is long-only.** Byte and word accesses fall to `default:` and hit `UnhandledMemory*` rather than the array. | [BUG] | §0.1, §8.1 |
| 5 | `MappedMemoryReadWord`'s purge case returns the literal `0xFFFFFFFF` into a `u16` (`memory.c:935`). Harmless truncation, but it signals the value was copy-pasted from the long variant. | [BUG] | §0.1 |
| 6 | **Area 7 outside `0xFFFFFE00` silently reads `0` and drops writes**, including the explicitly-empty `0xFFFF8000`–`0xFFFFBFFF` `???` branch. No logging, no bus error. | [QUIRK] | §0.4 |
| 7 | **Two incompatible open-bus models.** `UnhandledMemory*` reads `0`; the cartridge dummies read all-ones. Whether an unmapped address reads `0x00` or `0xFF` depends on which chip-select window it happens to fall in. | [QUIRK] | §2.16, §9.3 |
| 8 | **No unaligned-access handling anywhere.** `memory.c` never looks at the low address bits; the T1/T2 accessors do raw unaligned pointer casts. No SH-2 address-error exception is ever raised from the memory path. | [QUIRK] | §4.2 |
| 9 | **`#if CACHE_ENABLE` (memory.c) vs `#ifdef CACHE_ENABLE` (sh2core.c).** Defining `CACHE_ENABLE 0` puts the two files on different code paths. | [BUG] | §0.5 |
| 10 | **Cache line fill bypasses the area dispatch**, indexing `ReadLongList` directly (`sh2cache.c:337`). Only reachable in `CACHE_ENABLE` builds. | [QUIRK] | §0.5 |

### Region layout

| # | Item | Kind | Where |
|---|---|---|---|
| 11 | **High WRAM fill is off by one**: `FillMemoryArea(0x600, 0x610, …)` maps 1 MB + 64 KB, creating a phantom 64 KB mirror at `0x06100000` and leaving `0x06110000`–`0x062FFFFF` unmapped. | [BUG] | §2.14 |
| 12 | **The extended-backup fill is unconditional.** `0x06300000`–`0x07FFFFFF` (29 MB) is backup RAM even when `extend_backup == 0`, where it becomes a 464-fold mirror of the 64 KB internal backup. | [QUIRK] | §2.15 |
| 13 | **Data map and instruction-fetch map disagree above `0x06110000`.** Fetching from `0x06400000` reads High WRAM; reading data from it reads backup RAM. | [BUG] | §7 |
| 14 | **Extended-backup mode breaks the internal `0x00180000` window**: the `addr - 0x06300000` subtraction underflows, so every read returns `0` and every write is dropped. Only the BIOS-function hook (item 25) keeps backup working. | [BUG] | §2.3 |
| 15 | **Internal backup RAM has no word/long access**, while cartridge backup RAM does. Real hardware behaviour for a 16-bit read of an 8-bit device is not modelled either way. | [QUIRK] | §2.3, §10 |
| 16 | **Internal backup RAM writes force `addr\|1` but reads do not.** A byte written to an even address is invisible at that address. | [BUG] | §2.3 |
| 17 | **`BupRamWritten` is never set.** The header documents it as the autosave trigger (`memory.c:108-111`), but the only assignment to `1` is inside a `#if 0` block (`memory.c:545`). Any port relying on it never autosaves. | [BUG] | §2.3 |
| 18 | Two large `#if 0` blocks of alternative file-backed backup code remain in `BupRamMemoryReadByte`/`WriteByte` (`memory.c:483-498`, `:531-547`). | [QUIRK] | §2.3 |
| 19 | **CS2 has no byte interface.** All byte accesses to `0x05800000`–`0x058FFFFF` are handed to the cartridge, so the CD block registers cannot be byte-accessed at all. | [QUIRK] | §2.8 |
| 20 | The three CD-block accessors each carry the comment `// fix me(I should really have proper mapping)` and decode a flat `switch` on 20-bit offsets. | [QUIRK] | §2.8 |
| 21 | **`SoundRamReadByte` reads up to 512 KB out of bounds** for addresses above `0x7FFFF` when `mem4b != 0`: it sets `val = 0xFF` and then unconditionally overwrites it with an unbounded `T2ReadByte`. | [BUG] | §2.9 |
| 22 | Sound RAM reads call `SyncSh2And68k()`, an emulator thread-scheduling hook, from inside the memory accessor. | [QUIRK] | §2.9 |
| 23 | **`T3Memory` is entirely dead code** — defined in `memory.h:156-217` and `memory.c:269-293`, never instantiated. | [QUIRK] | §5.3 |
| 24 | **`T123Load` silently truncates** an oversized image instead of failing (the `return -1` is commented out, `memory.h:237-241`). | [QUIRK] | §5.4 |

### Cost model and fetch path

| # | Item | Kind | Where |
|---|---|---|---|
| 25 | **[HACK] BIOS backup-library interception.** With `extend_backup` on, fetches at BIOS offset `0x0007D600` and (once armed) `0x0380`–`0x03A8` are forced to `0`, and `SH2undecoded` substitutes native `BiosBUPInit`/`BiosHandleFunc` implementations. | [HACK] | §7 |
| 26 | **[HACK] VDP1 VRAM is executable** solely because of the entry commented `// Fighting Viper` (`sh2int.c:2947-2948`). | [HACK] | §7 |
| 27 | **Only the first 1 MB of CS0 is executable.** Code in a DRAM cart above `0x02100000` fetches `0xFFFF`. | [QUIRK] | §7 |
| 28 | Cycle penalties are a 1 MB-granular table with no derivation, and **SCU DMA passes `NULL`** so DMA transfers cost nothing. CS1, SMPC, SCU regs, VDP1 regs, VDP2 CRAM/regs and the extended backup window all cost 0. | [QUIRK] | §6 |
| 29 | `GET_MEM_CYCLE_R` labels `0x00100000` "Backup", but that 1 MB block is SMPC (`0x00100000`) plus backup RAM (`0x00180000`). | [QUIRK] | §6 |
| 30 | High WRAM reads cost 0 cycles but writes cost 2; the bit-28 mirrors cost 0 while the base addresses do not (the mask `0xDFF00000` clears bit 29 but not bit 28). | [QUIRK] | §6 |
| 31 | A commented-out earlier cost model with different numbers survives at `memory.c:802-841`. | [QUIRK] | §6 |

### Cartridge / A-Bus

| # | Item | Kind | Where |
|---|---|---|---|
| 32 | **`MappedMemoryInit` runs exactly once** (`yabause.c:271`), but `CartLoadState` can `CartDeInit()` + `CartInit(NULL, newtype)` (`cs0.c:1461-1465`). The page table keeps the *old* cart's CS0 handlers while `CartridgeArea->dram` is a *new*, possibly smaller, allocation. Loading an 8 Mbit-DRAM savestate over a 32 Mbit cart leaves `DRAM32MBITCs0*` (mask `0x3FFFFF`) pointed at a 1 MB buffer — a 3 MB out-of-bounds window. | [BUG] | §2.6, §9.9 |
| 33 | **Flash reads and page-program writes are unmasked** against a 256 KB allocation reachable at addresses up to `0x7FFFF` — 256 KB of out-of-bounds access. | [BUG] | §9.7 |
| 34 | **Flash states `FL_WRITEARRAY` and `FL_IDCMD` are unreachable.** The toggle-bit / data-polling read path and the `0xF0` ID-exit command are dead code. | [QUIRK] | §9.7 |
| 35 | **Flash page program only commits on the 128th byte** of a page; partial or out-of-order pages are lost. No write timeout is modelled. | [QUIRK] | §9.7 |
| 36 | **`if (0x80000)` is constant-true** in `AR4MCs0ReadWord` and `AR4MCs0ReadLong` (`cs0.c:417`, `:465`); the intended `addr & 0x80000` test is lost. | [BUG] | §9.6 |
| 37 | **The AR Commlink ports are entirely commented out** — banks `0x00` upper half and `0x01` fall through to all-ones (`cs0.c:362-373` and the corresponding write cases). | [QUIRK] | §9.6 |
| 38 | **The 16 Mbit ROM cart is writable** (`ROM16MBITCs0Write*`, `cs0.c:1023-1040`), and the modification is neither persisted nor reverted. | [BUG] | §9.5 |
| 39 | **`CART_ROM16MBIT` reports `cartid = 0xFF`** with the comment `// I have no idea what the real id is`. | [QUIRK] | §9.2 |
| 40 | **`CART_PAR` deliberately reports the 32 Mbit DRAM ID `0x5C`** so software treats an Action Replay as a plain RAM cart. | [QUIRK] | §9.2 |
| 41 | **`CART_USBDEV` reports `cartid = 0x00`** with `// No extra dram, etc. built-in`, contradicting the AR4M handlers it installs, which expose 4 MB at `0x02400000`. | [BUG] | §9.2 |
| 42 | **Savestates cover only DRAM carts.** Cartridge backup RAM, the AR flash image and the ROM cart are outside the savestate; `CartLoadState` ignores every other type entirely and returns without consuming its chunk beyond the type word. | [QUIRK] | §9.9 |
| 43 | **The CS1 cart-ID check matches one exact address per width.** A byte read of `0x04FFFFFE` or a word read of `0x04FFFFFC` misses it and returns cart data or `0xFF`. There is no ID readback on CS0 at all. | [QUIRK] | §10 |
| 44 | **Cartridge backup RAM is a plain byte array** with word/long access and no odd-byte convention, unlike the internal backup RAM. The odd-byte layout survives only because `FormatBackupRam` writes the same pattern and the BIOS layer uses half the space. | [QUIRK] | §10 |
| 45 | **No A-Bus wait-state, refresh or arbitration registers are modelled.** The SCU's `ASR0`/`ASR1`/`AREF` have no effect on anything in this map, and the CS0/CS1/CS2 timings in `GET_MEM_CYCLE_R` are three constants (24, 24, and nothing for CS1). | [QUIRK] | §6 |
| 46 | **The Netlink and Japanese Modem "cartridges" reach into `netlink.c`/`japmodem.c`**, which perform real host socket I/O. They are not self-contained hardware models. | [QUIRK] | §9.8 |

### Debug plumbing entangled with the map

| # | Item | Kind | Where |
|---|---|---|---|
| 47 | **Memory breakpoints mutate the dispatch table in place** (`sh2core.c:787-848`). A design that makes the table immutable or generates code from it loses breakpoint support. | [QUIRK] | §11 |
| 48 | **The savestate writes 64 KB of backup RAM that the loader skips** (`memory.c:1575` vs `:1828-1829`), and in extended mode that 64 KB is only the first fragment of an 8 MB mapping. | [BUG] | §11 |
| 49 | `MappedMemoryLoad`/`Save`/`Search` walk the bus through the same side-effecting accessors, so debugger reads can change register state. | [QUIRK] | §11 |
