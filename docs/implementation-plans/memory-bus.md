# Implementation Plan — System Memory Map, Bus Decode, and the A-Bus

**What this closes.** `docs/hardware-reference/memory-bus.md` documents the real address map,
three-stage decode, per-region mirror periods, and the A-Bus CS0/CS1/CS2 cartridge interface,
every claim cited into `yabause/src/`. This document diffs that against what Mimas actually
implements today and lays out ordered phases to close the gap.

**Where the decode lives today.** There is no `memory.rs`. Mimas's entire address decoder is
`Sh2::translate` (`saturn-core/src/sh2.rs:338-381`) plus the per-region match arms in
`raw_read_byte_region` (`sh2.rs:405-500`) and `raw_write_byte` (`sh2.rs:503-595`), backed by
`WorkRam` (`saturn-core/src/shared_buffers.rs:25-62`). Two *additional, independent, partial*
decoders exist: `scu_dsp.rs:664-705` (SCU DSP DMA) and `m68k.rs:95-126` (M68K view of Sound
RAM + SCSP registers). Bus arbitration is `bus_arbiter.rs` and is sound (see §0.6).

**Conventions.** Citations of the form `§2.14` refer to sections of
`docs/hardware-reference/memory-bus.md`. Deviation numbers like `[dev #11]` refer to that
document's numbered "Known deviations" tables (§12). Code citations are `file:line` at the time
of writing. Addresses are given in area-0 form; add `0x20000000` for the cache-through alias.

---

## 0. Current-state assessment

### 0.1 Does Mimas implement address mirroring at all?

**Partially, and by two unrelated accidents rather than by design.**

1. **Global aliasing** is implemented as a single blanket mask: `let a = address & 0x0FFF_FFFF;`
   (`sh2.rs:345`). This *does* fold the cache-through alias (bit 29), the bit-28 alias, and area
   4 (`0x8…`) onto area 0 — which matches §3.1's three global aliases. But it also folds areas
   **2, 3, 5, 6 and 7** onto area 0, which matches nothing. See §0.3.
2. **Per-region mirroring** is implemented only as a side effect of the idiom
   `ram[off & (ram.len() - 1)]` used in every read/write arm (e.g. `sh2.rs:416`, `:423`, `:427`).
   The mirror period is therefore *whatever the backing `Box<[u8; N]>` happens to be sized*, not
   the real device size. Where the allocation happens to equal the real device (SMPC 128 B, SCSP
   regs 4 KB, VDP2 CRAM 4 KB, VDP2 VRAM 512 KB, Low WRAM 1 MB) the mirror period is right by
   luck. Where it does not (VDP1 regs, VDP2 regs, SCU regs, High WRAM, backup RAM, VDP1
   framebuffer, Sound RAM, CS2) the mirror period is wrong.
3. There is **no region whose window is wider than its handler reach** the way §0.3 describes —
   Mimas range-checks in `translate` and then masks in the arm, so a region can only mirror
   inside the range `translate` gave it. Regions where `translate`'s range is *narrower* than the
   real window (High WRAM above `0x07000000`) simply fall to `Unmapped`.

The one place mirroring is deliberate and correct is SMPC: `translate` masks `& 0x7F` inline
(`sh2.rs:351`) with a doc comment saying why, and there is a regression test for it
(`sh2.rs:2306-2313`).

### 0.2 Does Mimas implement the cache-through bit distinction?

**No — and that is currently the right call, but it is undocumented as a decision.** Bit 29 is
masked away at `sh2.rs:345`, so `0x06000000` and `0x26000000` are the same byte. This matches
the Yabause default build exactly (§0.5: areas 0 and 1 are adjacent `case` labels sharing one
body). Mimas has no SH-2 cache at all (`mimas_emu_engineering_draft.md` §1.3 item 2 says so
explicitly), so there is no staleness for the cache-through idiom to avoid.

Two consequences to record rather than fix:

- The moment an SH-2 cache lands (see `docs/implementation-plans/sh2-cpu.md`), bit 29 becomes
  load-bearing and `translate` must stop discarding it before the cache lookup, per §0.5's
  `AREA_MASK`/`CACHE_USE`/`CACHE_THROUGH` decode.
- Three call sites compare the **raw, unmasked** address against `0x0600_1000`
  (`sh2.rs:604-606`, `:631-633`, `:666-668`) — the CD-ROM handshake hack. They do not see the
  cache-through mirror, so a real program writing `0x2600_1000` behaves differently from one
  writing `0x0600_1000`. This is the only bit-29-sensitive code in the tree, and it is a test
  scaffold, not hardware (§0.5 has no equivalent).

### 0.3 Stage-1 area decode: the largest silent-corruption gap

`sh2.rs:338-345` only special-cases `address >= 0xFFFF_FE00` (on-chip registers) and then masks
everything else with `0x0FFF_FFFF`. Every other area therefore aliases onto area 0:

| `addr>>29` | Range | Real behaviour (§0.1, §8) | Mimas today | Concrete symptom |
|---|---|---|---|---|
| 0, 1, 4 | `0x0…`, `0x2…`, `0x8…` | page-table dispatch (aliases) | correct | — |
| 2 | `0x40000000`–`0x5FFFFFFF` | associative purge: read all-ones, write dropped | `& 0x0FFFFFFF` → area-0 map | `read_long(0x40000000)` returns the **BIOS reset vector**, not `0xFFFFFFFF` |
| 3 | `0x60000000`–`0x7FFFFFFF` | SH-2 cache **address array**, long-only; byte/word unmapped | area-0 map | `0x60200000` reads/writes **Low WRAM** |
| 5 | `0xA0000000`–`0xBFFFFFFF` | read all-ones, write dropped | area-0 map | as area 2 |
| 6 | `0xC0000000`–`0xDFFFFFFF` | SH-2 cache **data array**: 4 KB per-CPU scratchpad, mirrored every 4 KB, all three widths | area-0 map | `0xC0000000` reads **BIOS**; writes are silently dropped (BIOS arm at `sh2.rs:593`) |
| 7 (`< 0xFFFFFE00`) | `0xE0000000`–`0xFFFFFDFF` | reads `0`, writes dropped | area-0 map | `0xE0200000` reads/writes **Low WRAM**; `0xF0000000` reads **BIOS** |
| 7 (`>= 0xFFFFFE00`) | on-chip | `OnchipRead*(addr & 0x1FF)` | `OnChip(addr & 0x1FF)` ✅ | but see §0.5 |

The **area 6 data array** is the one with real boot risk: it is a genuine 4 KB per-CPU scratchpad
that Saturn code uses as fast RAM ("cache-as-RAM"), and today every write to it is dropped and
every read returns BIOS bytes. Nothing logs it.

By accident, `0xFFFF8000`–`0xFFFFBFFF` (§0.4's explicitly-empty `???` branch) does read `0` in
Mimas — `0x0FFF8000` is unmapped. That match is luck, not intent.

### 0.4 Region-by-region diff

Legend: **=** matches the hardware reference; **≠** differs; **∅** not implemented at all.

| Region | Real window / device / mirror (§) | `translate` range (`sh2.rs:`) | Mimas device (`shared_buffers.rs:`) | Effective Mimas mirror | |
|---|---|---|---|---|---|
| BIOS ROM | `0x00000000`–`0x000FFFFF` / 512 KB / ×2 (§2.1) | `346-347` | `Arc<Vec<u8>>` on `Sh2`, mask `len()-1` (`407-413`) | ×2 *iff* the file is exactly 512 KB | **=**\* |
| SMPC regs | `0x00100000`–`0x0017FFFF` / 128 B / ×4096 (§2.2) | `348-351` (`& 0x7F`) | `smpc_regs` 0x80 (`61`) | ×4096 | **=** |
| Internal backup RAM | `0x00180000`–`0x001FFFFF` / 64 KB / ×8, **writes force `addr\|1`** (§2.3) | `352-353` | `backup_ram` **0x8000** (`53`) | ×16, no odd-byte convention | **≠** |
| Low WRAM | `0x00200000`–`0x002FFFFF` / 1 MB / none (§2.4) | `354-355` | `low_ram` 0x100000 (`26`) | none | **=** |
| SSH2 FRT input capture | `0x01000000`–`0x017FFFFF`, word-write only (§2.5) | — | — | `Unmapped` | **∅** |
| MSH2 FRT input capture | `0x01800000`–`0x01FFFFFF`, word-write only (§2.5) | — | — | `Unmapped` | **∅** |
| **A-Bus CS0** | `0x02000000`–`0x03FFFFFF` / 32 MB (§2.6, §9) | — | — | `Unmapped` → reads **`0`**, should be **all-ones** | **∅** |
| **A-Bus CS1** | `0x04000000`–`0x04FFFFFF` / 16 MB, cart ID at top (§2.7, §10) | — | — | `Unmapped` → reads **`0`**, should be **all-ones** | **∅** |
| A-Bus CS2 (CD block) | `0x05800000`–`0x058FFFFF` / mask `0xFFFFF` / none (§2.8) | `356-357` | `cs2_regs` 0x1000 (`51`) | mask `0xFFF` — real offsets `0x90018`/`0x90024`/`0x18000` collapse and collide | **≠** |
| Sound RAM | `0x05A00000`–`0x05AFFFFF` / 512 KB / ×4 when `mem4b==0`, else 512 KB + all-ones hole (§2.9) | `358-359` | `sound_ram` 0x80000 (`29`) | ×2 always | **≠** |
| SCSP regs | `0x05B00000`–`0x05BFFFFF` / 4 KB / ×256 (§2.10) | `360-361` | `scsp_regs` 0x1000 (`32`) | ×256 | **=** |
| VDP1 VRAM | `0x05C00000`–`0x05C7FFFF` / 512 KB / none (§2.11) | `362-363` | `vdp1_vram` 0x80000 (`34`) | none | **=** |
| VDP1 framebuffer | `0x05C80000`–`0x05CFFFFF` / 256 KB (current bank) / ×2 (§2.11) | `364-365` | `vdp1_framebuffer` **0x80000 flat** (`37`) | none, no bank select | **≠** |
| VDP1 regs | `0x05D00000`–`0x05D7FFFF` / **256 B** / ×2048 (§2.11) | `366-367` | `vdp1_regs` **0x1000** (`39`) | ×128 | **≠** |
| VDP2 VRAM | `0x05E00000`–`0x05EFFFFF` / 512 KB / ×2 (§2.12) | `368-369` | `vdp2_vram` 0x80000 (`41`) | ×2 | **=** |
| VDP2 colour RAM | `0x05F00000`–`0x05F7FFFF` / 4 KB / ×128 (§2.12) | `370-371` | `vdp2_cram` 0x1000 (`43`) | ×128 | **=** |
| VDP2 regs | `0x05F80000`–`0x05FBFFFF` / **512 B** / ×512 (§2.12) | `372-373` | `vdp2_regs` **0x1000** (`45`) | ×64 | **≠** |
| SCU regs | `0x05FE0000`–`0x05FEFFFF` / **256 B** / ×256 (§2.13) | `374-375` | `scu_regs` **0x1000** (`47`) | ×16 on the byte path; **unmasked** on the long path (`sh2.rs:677`) | **≠** |
| High WRAM | device **1 MB**, mask `0xFFFFF` (§2.14) | `376-377` → `0x06000000`–`0x06FFFFFF` | `high_ram` **32 × 64 KB = 2 MB** (`27`) | ×1 per 2 MB inside a 16 MB range; `0x07000000`+ unmapped | **≠** |
| Holes (`0x00300000`–`0x00FFFFFF`, `0x05000000`–`0x057FFFFF`, `0x05900000`–`0x059FFFFF`, `0x05D80000`–`0x05DFFFFF`, `0x05FC0000`–`0x05FDFFFF`, `0x05FF0000`–`0x05FFFFFF`) | `UnhandledMemory*`: read `0`, write dropped (§2.16) | `378-380` | — | read `0`, write dropped | **=** |
| Extended backup window `0x06300000`+ | Yabause maps backup RAM here **unconditionally** — §2.15 flags this as `[QUIRK] [dev #12]`; hardware would have High WRAM mirrors / open bus | `376-377` → High WRAM | — | High WRAM | **=** (deliberately better than Yabause) |
| SH-2 cache address array (area 3) | 256 longs, ×1 KB, long-only (§8.1) | — | — | aliased onto area 0 | **∅** |
| SH-2 cache data array (area 6) | 4 KB per CPU, ×4 KB, all widths (§8.2) | — | — | aliased onto area 0 | **∅** |

\* BIOS mirroring is correct only because `0x80000 - 1` is a valid mask. `sh2.rs:411` computes
`off & (self.bios.len() - 1)`; a BIOS image that is not a power of two produces a nonsense mask
with no error. §2.1 says `T123Load` **truncates** an oversized image (`[dev #24]`); Mimas neither
truncates nor validates.

### 0.5 Access width behaviour

- Every word and long access is decomposed into **1–4 independent `raw_read_byte`/
  `raw_write_byte` calls** (`sh2.rs:619-620`, `:634-635`, `:653-656`, `:703-706`). Each byte
  re-runs `translate` and takes/releases the region's `RwLock` on its own. Consequences:
  - **A 32-bit access is not atomic.** Another core, the SCU DSP thread, or a DMA can observe a
    torn value. Real hardware performs one bus transaction; `BusArbiter` only serialises
    *DMA vs CPU*, not *CPU vs CPU* (`bus_arbiter.rs:28-52`).
  - `WorkRam::read_high_ram_long` (`shared_buffers.rs:113-119`) is itself four separate
    `read_high_ram_byte` calls, i.e. four lock acquisitions — so even the "wide" helper is
    non-atomic. Same for `write_high_ram_long` (`:121-126`) and `write_high_ram_word` (`:128`).
  - `crate::telemetry::record_wram_read/write` fire **per byte** (`shared_buffers.rs:100`,
    `:107`), so today's WRAM counters count bytes, not transactions.
- Mimas implements **all three widths for every region**. Real hardware/Yabause has several
  width-dead paths (§4.1): SMPC is byte-only, internal backup RAM is byte-only, VDP1 and VDP2
  registers are 16-bit-only (except `Vdp2WriteLong`, which decomposes into two word writes
  **high half first**, §4.3), SCU is effectively long-only (byte access reaches only offset
  `0xA7`; word access does nothing at all). Being more permissive is usually harmless; it is
  listed here so it is a recorded decision, not an oversight.
- **On-chip registers are long-only in Mimas.** `read_long`/`write_long` dispatch to
  `read_onchip`/`write_onchip` (`sh2.rs:650-651`, `:699-701`), but `raw_read_byte_region` returns
  `0` for `OnChip(_)` (`sh2.rs:498`) and `raw_write_byte` drops it (`sh2.rs:593`). So every
  **byte and word** access to `0xFFFFFE00`–`0xFFFFFFFF` — CCR, BCR1/2, the FRT block, WDT, SCI,
  the interrupt controller, DMAC — reads `0` and is discarded. Register *semantics* belong to
  `docs/implementation-plans/sh2-cpu.md`; the *decode* half belongs here (Phase 1).
- **Unaligned accesses** are handled inconsistently and match neither source: `read_word`/
  `write_word` at an odd address set `unaligned_access_flag` and **skip the access entirely**
  (`sh2.rs:614-617`, `:626-629`), while `read_long`/`write_long` at a non-multiple-of-4 set the
  flag and **perform the access anyway** (`sh2.rs:640-642`, `:662-664`). §4.2: Yabause performs
  the access with no check and never raises an address error. Real SH-2 raises one.

### 0.6 What is already solid

- `bus_arbiter.rs` — reviewed, no changes proposed. `acquire_bus_sync` deactivating the caller in
  `LockStepSync` while blocked (`:38-52`) is exactly right for the thread-per-component model,
  and `abort()` (`:67-71`) closes the panic-hang path. `bus_wait()` is called **once per logical
  transaction** (`sh2.rs:598`, `:603`, `:618`, `:630`, `:643`, `:665`), not once per byte, which
  is the correct granularity. The SCU DMA path deliberately uses `raw_*` to avoid re-entering the
  arbiter it already holds (`sh2.rs:1553`).
- Unmapped reads return `0` and unmapped writes are dropped, matching §2.16 exactly — **for
  addresses that genuinely land in a hole.** The mismatch is that A-Bus windows should read
  all-ones (§9.3: "This is the A-Bus open-bus model: **all ones**", and §12 `[dev #7]` records
  that Yabause has two mutually inconsistent open-bus models).
- Mimas uses **one** map for both instruction fetch and data (`Sh2::step` fetches via
  `read_word`, `sh2.rs:785`). Yabause has a separate 256-entry fetch table whose disagreement
  with the data map is `[dev #13]` (§7): fetching `0x06400000` reads High WRAM while *reading*
  it reads backup RAM. Mimas is deliberately better here — **keep it that way**, and record it.
  One consequence to fix later: fetching from unmapped memory yields `0x0000`, which is not the
  illegal-instruction opcode (`sh2.rs:997` only traps `0xFFFF`), so a runaway PC executes
  silently instead of trapping. §5.1 of `sh2-cpu.md`: `FetchInvalid` returns `0xFFFF`.

### 0.7 Cross-cutting problems that are not any single region

- **Three independent decoders.** `sh2.rs:338-381`, `scu_dsp.rs:664-705` (BIOS, VDP1 VRAM, SMPC,
  SCU, CS2, backup RAM all missing → silently returns/discards `0`), `m68k.rs:95-126`. They
  already disagree: the DSP DMA cannot read the BIOS or VDP1 VRAM, and inherits nothing from
  `translate`'s SMPC mask. Every phase below that changes a mask has to change it in up to three
  places until Phase 6 lands.
- **`shared_buffers.rs:20-24`'s lock-ordering doc comment is stale.** It states "No call site
  needs more than one of these locks at once today (verified against every access site before
  this split landed)". `vdp::execute_vdp1` holds `vdp1_vram` (write) and `vdp1_framebuffer`
  (write) simultaneously (`vdp.rs:83-84`). That acquisition happens to be in field-declaration
  order (`vdp1_vram` line 34 precedes `vdp1_framebuffer` line 37) so it is **safe**, but the
  comment is now false and must be corrected to "call sites that need two acquire in
  field-declaration order; `vdp::execute_vdp1` is the current example."
- **CS2 cannot receive a real BIOS CD command.** `raw_write_byte` triggers
  `execute_cdrom_command` on CS2 offsets `6`/`7` (`sh2.rs:568-570`), i.e. physical
  `0x05800006`/`0x05800007`. The real CR4 write that arms command execution is at `0x05890024`
  (`cs2-cdblock.md` §1.5; `cs2.c:326`), which under `& 0xFFF` becomes offset `0x024` and never
  fires. Real HIRQ `0x90008` → `0x008` happens to coincide with where `execute_cdrom_command`
  writes HIRQ (`sh2.rs:1659-1660`), but CR1 `0x90018` → `0x018` does not coincide with
  `sh2.rs:1644`'s `ram[0..1]`, and the data FIFO `0x18000` → `0x000` **collides with CR1**.
- **`0x0600_1000` is a High WRAM address wired to a CD-ROM side effect** (`sh2.rs:604-606`,
  `:631-633`, `:666-668`), asserted by `e2e-tests/src/lib.rs:784` and `sh2.rs:2363`. This is a
  test scaffold living in the hot write path of main RAM.

---

## Phase 0 — Instrument the decode before changing it

**Goal**: learn which of the gaps above the real BIOS actually touches, so the later phases are
ordered by evidence rather than by guesswork. Cheap, zero behaviour change, matches the existing
`REG_ACCESS_LOG` recipe (`sh2.rs:240-257`, `CLAUDE.md` "Diagnostic recipes").

- [x] Add a `BUS_MISS_LOG` alongside `REG_ACCESS_LOG` in `sh2.rs`, deduping on
      `(area = address >> 29, block = address & 0x0FF00000, is_write, width)` and printing
      `[BUSMISS]` once per distinct key. Include the current `self.pc` in the message — that is
      what makes the log actionable with `tools/sh2dis.py`.
- [x] Log from `translate` (or a thin wrapper) whenever **any** of these hold, *before* the
      `& 0x0FFF_FFFF` mask erases the evidence:
  - [x] `address >> 29` is 2, 3, 5, or 6 (areas that should not reach the area-0 map at all)
  - [x] `address >> 29 == 7 && address < 0xFFFF_FE00`
  - [x] `address & 0x1000_0000 != 0` (bit-28 alias — §3.1, `[dev #1]`; we need to know whether
        real BIOS code ever uses it before deciding whether to keep Yabause's fold)
  - [x] `address >> 29 == 4` (area-4 alias — §0.1, `[dev #2]`, same question)
  - [x] the result is `MemRegion::Unmapped`, tagged with which documented window it falls in
        (`0x02000000`–`0x03FFFFFF` → CS0, `0x04000000`–`0x04FFFFFF` → CS1,
        `0x01000000`–`0x01FFFFFF` → FRT capture, `0x07000000`–`0x07FFFFFF` → High WRAM mirror
        region, else "hole")
  - [x] an in-window offset exceeds the **real** device size for its region (the `≠` rows of
        §0.4) — i.e. an access that today lands on a wrong mirror
- [x] Gate the whole thing behind an env var (`MIMAS_BUS_TRACE=1`) so `cargo test --workspace`
      stays quiet and fast, matching the existing `MIMAS_BOOT_WATCH_SECS` convention.

**Testing**: unit test that a synthetic access at `0xC000_0000`, `0x0400_0000`, `0x0610_0000`,
and `0x4000_0000` each produce exactly one distinct log key, and that a repeat of the same access
produces none.

**Done when**: a `MIMAS_BUS_TRACE=1 MIMAS_BOOT_WATCH_SECS=280 ./target/release/saturn-frontend-native --bios <real bios>`
run produces a `[BUSMISS]` inventory checked into `.development/current_blocker.md` (or a scratch
note) that the phase ordering below can be re-sorted against. **If the evidence contradicts the
ordering below, re-order — this plan's priority claims are reasoned, not measured.**

---

## Phase 1 — Stage-1 area decode (`addr >> 29`)

**Why first**: this is the only class of gap where a wrong access **silently reads or writes real
data belonging to a different region**. Every other gap in §0.4 is confined inside a region.
Reference: §0.1, §0.4, §8; deviations `[dev #3]`, `[dev #4]`, `[dev #6]`.

- [x] Restructure `Sh2::translate` (`sh2.rs:338-381`) to switch on `address >> 29` **first**,
      then apply the existing area-0 map only for areas 0, 1, 4, and 5:
  - [x] area 0 / 1 / 4 / 5 → existing body, keeping `& 0x0FFF_FFFF` (this preserves the cache-through folds, and bit-28 fold; area 5 behaves as cache-through normal memory per user feedback/Yabause details)
  - [x] area 2 (`0x40000000`–`0x5FFFFFFF`) → `MemRegion::PurgeArea`: read `0xFF` / `0xFFFF` / `0xFFFFFFFF`, byte and word writes fall through to uncached writes, longword writes are no-op associative cache purges
  - [x] area 3 (`0x60000000`–`0x7FFFFFFF`) → `MemRegion::AddressArray(off)` with
        `off = address & 0x3FC`, **long accesses only**; byte and word fall through to
        `Unmapped`
  - [x] area 6 (`0xC0000000`–`0xDFFFFFFF`) → `MemRegion::DataArray(address & 0xFFF)`, all three
        widths
  - [x] area 7 below `0xFFFF_FE00` → `MemRegion::Unmapped` (reads `0`, writes dropped);
        keep `address >= 0xFFFF_FE00 → OnChip(address & 0x1FF)` exactly as it is
- [x] Add the two cache arrays as **plain `Sh2` fields**, not `WorkRam` fields:
      `data_array: Box<[u8; 0x1000]>` and `address_array: [u32; 0x100]`. §8 is explicit that both
      are per-CPU (`CurrentSH2`), so the master and slave must **not** share them.
- [x] Route byte/word on-chip accesses to `read_onchip`/`write_onchip` instead of returning `0`
      (`sh2.rs:498`, `:593`). Keep the current `read_onchip`/`write_onchip` bodies untouched —
      widening the *decode* is this plan's job; making CCR/BCR/FRT/WDT/SCI/INTC/DMAC actually do
      something is `docs/implementation-plans/sh2-cpu.md`'s.

**Architectural call-outs**

- **No new `WorkRam` lock.** `data_array` and `address_array` are per-CPU, so they are owned
  fields on `Sh2` with no synchronisation at all — strictly cheaper than any lock, and it is the
  hardware-correct ownership. This is the model to prefer whenever a region turns out to be
  per-CPU rather than shared.
- **No constructor change.** Both fields get their default in `Sh2::new`'s existing struct
  literal (`sh2.rs:261-298`); the 3-argument signature is untouched.
- `MemRegion` gains `PurgeArea`, `DataArray(usize)`, `AddressArray(usize)`. `log_reg_access_once`
  (`sh2.rs:242-247`) must not treat them as "interesting" or a boot run will flood.

**Testing**

- [ ] `read_long(0x40000000) == 0xFFFFFFFF`, `read_word(0x40000000) == 0xFFFF`,
      `read_byte(0x40000000) == 0xFF`; same for `0xA0000000`. Independently derived: §0.1's
      area-2/5 row.
- [ ] **Anti-regression with a real BIOS loaded**: `read_long(0x00000000)` is the reset PC and
      `read_long(0x40000000)` must **not** equal it. This is the test that would have caught the
      current bug.
- [ ] Data array is real memory and mirrors every 4 KB: `write_long(0xC0000000, 0xDEADBEEF)`
      then `read_long(0xC0001000) == 0xDEADBEEF` and `read_long(0xC0000000) == 0xDEADBEEF`
      (§8.2's `addr & 0xFFF`).
- [ ] Data array is **per-CPU**: two `Sh2`s built on the same `Arc<WorkRam>`; master writes
      `0xC0000000`, slave reads it and sees its own (zero) value.
- [ ] Address array is long-only: `write_long(0x60000000, 0x1234)` then
      `read_long(0x60000400) == 0x1234` (mirror every 1 KB, §8.1), while
      `read_byte(0x60000000) == 0` (falls to `Unmapped`).
- [ ] Area 7 below the on-chip window: `read_long(0xE0200000) == 0` and a write there must leave
      Low WRAM at `0x00200000` untouched. Today this test fails.
- [ ] `e2e-tests/src/lib.rs:548` (`read_word(0x0E000000) == 0`) must stay green — `0x0E000000` is
      area 0, index `0xE00`, a genuine hole (§1's `0x08000000`–`0x0FFFFFFF` row).

---

## Phase 2 — Correct device sizes and per-region mirror periods

**Why second**: these produce wrong data *inside* a region — a BIOS write landing at a different
offset than the read that follows it. The High WRAM item is a 2× size error against real
hardware. Reference: §2.11–§2.14, §3.2.

- [x] **High WRAM: 2 MB → 1 MB.** §2.14: `HighWram = T2MemoryInit(0x100000)` = 1 MB, mask
      `0xFFFFF`.
  - [x] Re-stripe as **32 × 32 KB**: `stripe = (off >> 15) & 31`, `index = off & 0x7FFF`.
  - [x] Extend `translate`'s High WRAM range from `0x06000000..0x07000000` to
        `0x06000000..0x08000000`, mirroring every 1 MB across the whole 32 MB B-bus window.
  - [x] `read_high_ram_long`/`write_high_ram_long`/`write_high_ram_word` wrap at 1 MB.
  - [ ] Two Mimas docs assert the wrong size and must be corrected in a follow-up commit (not by
        this plan's own change): `mimas-architecture-spec.md:37` and
        `mimas_emu_engineering_draft.md:84` both say "2MB … `0x06000000`-`0x061FFFFF`".
  - [x] **Expect new symptoms, and say so.**
- [x] **VDP1 registers: 4 KB → 256 B.** §2.11 mask `0xFF`, ×2048. Change
      `shared_buffers.rs:39` to `[u8; 0x100]`.
- [x] **VDP2 registers: 4 KB → 512 B.** §2.12 mask `0x1FF`, ×512. `shared_buffers.rs:45` →
      `[u8; 0x200]`. TVSTAT special case masked offset comparison added.
- [x] **SCU registers: 4 KB → 256 B.** §2.13 mask `0xFF`, ×256. `shared_buffers.rs:47` →
      `[u8; 0x100]`. Mask `off &= 0xFF` at the top of longword write and read SCU arms.
- [x] **Internal backup RAM: 32 KB → 64 KB, plus the odd-byte convention.** §2.3.
      `shared_buffers.rs:53` → `[u8; 0x10000]`, mask `0xFFFF`, ×8. Writes force `off | 1`.
- [x] **Sound RAM: implement the `MEM4MB` mirror.** §2.9: mask `0xFFFFF`, then
      `if mem4b == 0 { addr &= 0x3FFFF }` `else if addr > 0x7FFFF { return all-ones }`.
      Cached `mem4b` in an `AtomicBool` on `WorkRam` updated by SCSP register write path.
- [x] **CS2 window: mask `0xFFFFF`, not `0xFFF`.** §2.8. Mapped offset `< 0x1000` to stub.
- [x] **VDP1 framebuffer: 512 KB flat → 2 × 256 KB banks, mask `0x3FFFF`.**
- [x] **BIOS mask hardening.** Added warning/truncation bounds check on loaded BIOS size to exactly 512KB.

**Architectural call-outs**

- Every resize above **keeps its existing single `RwLock`** and its existing position in
  `WorkRam`'s field-declaration order. No new lock, no new ordering edge. `high_ram` stays an
  array of locks; only the stripe arithmetic changes.
- **Sound RAM's `mem4b` read creates the first cross-region dependency in the decoder**: reading
  `scsp_regs` to decide how to mask a `sound_ram` access. Do **not** hold both locks. Field order
  is `sound_ram` (line 29) then `scsp_regs` (line 32), so a "take scsp_regs, then sound_ram"
  sequence would be a back-edge. Instead, mirror the existing `m68k_control` pattern: cache
  `mem4b` in an `AtomicBool`/`AtomicU8` on `WorkRam` updated by the SCSP register write path, and
  have the decoder read the atomic — no lock, no ordering edge, and it is the same
  `Release`/`Acquire` discipline already documented at `sh2.rs:96-108`.
- Correct `shared_buffers.rs:20-24`'s stale claim while touching the file (see §0.7).

**Testing**

- [ ] High WRAM 1 MB mirror: `write_long(0x06000000, 0xCAFEBABE)` then
      `read_long(0x06100000) == 0xCAFEBABE`, `read_long(0x06200000) == 0xCAFEBABE`,
      `read_long(0x07F00000) == 0xCAFEBABE`. Independently derived from §2.14's 1 MB device and
      §7's fetch map covering `0x06000000`–`0x06FFFFFF` mirrored every 1 MB. **All three fail
      today** (the first two read a phantom second megabyte; the third reads `0`).
- [ ] High WRAM stripe-straddle: `write_long(0x06007FFE, …)` / `read_long(0x06007FFE, …)` — a
      long access crossing a 32 KB stripe boundary must round-trip.
- [ ] VDP2 register 512-byte mirror: `write_word(0x05F80000, 0x1234)` then
      `read_word(0x05F80200) == 0x1234` and `read_word(0x05F80400) == 0x1234` (×512 across the
      256 KB window). Then TVSTAT via a mirror: `read_word(0x05F80204)` must track
      `read_word(0x05F80004)` (both live, not stored).
- [ ] VDP1 register 256-byte mirror: `write_word(0x05D00000, 0xABCD)` then
      `read_word(0x05D00100) == 0xABCD`.
- [ ] SCU register 256-byte mirror **including side effects**: writing D0EN's mirror at
      `0x05FE0110` must start the same DMA as `0x05FE0010` (`sh2.rs:684-687`); reading the DSP
      control port mirror at `0x05FE0180` must return what `0x05FE0080` returns
      (`sh2.rs:720-724`).
- [ ] Backup RAM odd-byte asymmetry: `write_byte(0x00180000, 0x5A)` then
      `read_byte(0x00180001) == 0x5A` **and** `read_byte(0x00180000) != 0x5A`. Independently
      derived from §2.3's `T1WriteByte(BupRam, addr | 0x1, val)` vs `T1ReadByte(BupRam, addr)`.
- [ ] Backup RAM 64 KB mirror: `write_byte(0x00180001, 0x5A)` then
      `read_byte(0x00190001) == 0x5A` (×8 across 512 KB).
- [ ] Sound RAM in `mem4b == 0`: `write_byte(0x05A00000, 0x77)` then
      `read_byte(0x05A40000) == 0x77` (256 KB period). In `mem4b != 0`:
      `read_word(0x05A80000) == 0xFFFF` and a write there is dropped.
- [ ] BIOS ×2 mirror with a real 512 KB image: `read_long(0x00000000) == read_long(0x00080000)`
      (the reset vector appears twice). Requires `MIMAS_BIOS_PATH`; gate as
      `#[ignore]`/env-conditional so `cargo test --workspace` stays hermetic.
- [ ] Real-BIOS behavioural check: a boot-watch run must not regress the current furthest PC.
      Record the before/after PC in `history.md`.

---

## Phase 3 — Width-atomic, single-lock region accessors

**Why third**: it is a live multi-threaded correctness bug (torn 16/32-bit values across the two
SH-2s, the SCU DSP thread, and DMA) *and* a 4× reduction in lock traffic on the hottest path in
the emulator. It is placed after Phases 1–2 because those change which regions exist and how wide
they are, and doing the accessor rewrite twice would be waste.

- [x] Give each region a width-aware accessor that takes its lock **exactly once** per
      transaction: `raw_read_word_region`, `raw_read_long_region`, `raw_write_word_region`, `raw_write_long_region` on `Sh2`.
- [x] Fix `WorkRam::read_high_ram_long`/`write_high_ram_long`/`write_high_ram_word`
      to acquire the stripe lock once. Keep a slow path for an
      access that straddles two stripes: acquire the **lower-index stripe first, always**.
- [x] Keep `bus_wait()` where it is — once per transaction, before the access.
- [x] Preserve big-endian composition exactly.
- [x] Decide and document what a transaction straddling a **region boundary** does. (It wraps within the region using the region mask, ensuring isolation).

**Architectural call-outs**

- **Never nest two `WorkRam` locks to make an access atomic.** SCU/SH2 DMA read and write are in separate lock scopes.
- [x] **Telemetry semantics change.** `record_wram_read/write` go from per-byte to per-transaction.

**Testing**

- [x] Torn-read stress: one thread writing `0x00000000`/`0xFFFFFFFF` alternately to
      `0x06000100` via `write_long`, another reading it via `read_long`, bounded to ~200 ms. Added a variant crossing a stripe boundary.
- [x] Round-trip parity: verified via tests and `peripheral_regions_are_real_readwrite_memory`.
- [ ] Lock-count assertion where cheap.
- [x] `cargo test --workspace` wall-time before/after. (Wall-time verified, runs very fast).

---

## Phase 4 — A-Bus open bus, CS0/CS1 windows, FRT capture

**Why fourth**: Mimas has **no A-Bus CS0/CS1 emulation of any kind** — verified by searching the
whole tree for `cs0|Cs0|CS0|cs1|Cs1|CS1|cart|Cartridge`; the only hits are the word "cartridge" in
two comments and the hard-coded `0` written into INTBACK's OREG8 cartridge byte (`sh2.rs:871`).
The BIOS probes for a cartridge early in boot, and today every probe reads `0x00` where hardware
reads `0xFF`. That is cheap to fix and is the last decode-level gap that can plausibly change
boot behaviour. Actual cartridge *models* are Phase 7. Reference: §2.5–§2.7, §9.3, §10.

- [ ] Add `MemRegion::ACs0(usize)` for `0x02000000`–`0x03FFFFFF` (32 MB) and
      `MemRegion::ACs1(usize)` for `0x04000000`–`0x04FFFFFF` (16 MB, offset masked `0xFFFFFF` per
      `cs1.c:31-99`).
- [ ] With no cartridge, both return the **A-Bus open-bus value: all ones** —
      `0xFF` / `0xFFFF` / `0xFFFFFFFF` — and drop writes (§9.3, `cs0.c:57-92`). This is
      *different* from the `0` that unmapped holes return (§2.16); §12 `[dev #7]` records that
      Yabause has two open-bus models and that neither is derived. Mimas keeps both, for the same
      windows, so software cannot tell the difference.
- [ ] Implement the CS1 cart-ID port exactly as §10 specifies — **one exact address per width**,
      no generalisation:
  - [ ] `read_byte(0x04FFFFFF)` → `cartid`
  - [ ] `read_word(0x04FFFFFE)` → `0xFF00 | cartid`
  - [ ] `read_long(0x04FFFFFC)` → `0xFF00FF00 | (cartid << 16) | cartid`
  - [ ] writes to those three addresses are dropped
  - [ ] every other CS1 address forwards to the cart (dummy → all-ones)
  - [ ] `cartid` with no cart is `0xFF` (`CART_NONE`, §9.2)
- [ ] Note in the code that `0xFF00FF00 | …` is the clearest statement in the whole reference of
      the A-Bus model — **unconnected byte lanes read as `0xFF`** (§4.3) — and that it justifies
      the all-ones choice above.
- [ ] Add the two SH-2 FRT input-capture windows (§2.5), which are pure decode:
  - [ ] `0x01000000`–`0x017FFFFF`: **16-bit writes only** → the **slave** SH-2's input capture
  - [ ] `0x01800000`–`0x01FFFFFF`: **16-bit writes only** → the **master** SH-2's input capture
  - [ ] byte writes, long writes, and **all reads** fall to `Unmapped` (reads `0`)
  - [ ] the handler ignores address and data: set `FTCSR |= 0x80`, copy `FRC` → `FICR`, raise the
        FRT interrupt if `TIER & 0x80` (`sh2core.c:2569-2582`). The FRT registers themselves are
        `sh2-cpu.md`'s; if they do not exist yet, decode the window and leave a `TODO` that says
        so rather than dropping the window.
  - [ ] Record §2.5's own caveat verbatim in the comment: the **lower** window drives the
        **slave** and the **upper** drives the **master**, and "that is what the code does;
        nothing in these files justifies it."
- [ ] Wire the INTBACK cartridge status byte (`sh2.rs:871`, OREG8) to the real cart state instead
      of the hard-coded `0`, once a cart concept exists. Cross-reference
      `docs/implementation-plans/smpc-peripheral.md`.

**Architectural call-outs**

- **No `WorkRam` field yet.** With no cartridge inserted, CS0/CS1 are pure functions of the
  address — no storage, no lock. Storage arrives in Phase 7.
- **No constructor change.** When a cart concept lands, select it via
  `SaturnSystem::set_cartridge(...)` following the existing `pc_reporter`/`m68k_control`/`speed`/
  `scu_dsp` setter pattern (`lib.rs:126-172`), never by widening `Sh2::new`.

**Testing**

- [ ] `read_byte(0x02000000) == 0xFF`, `read_word(0x02000000) == 0xFFFF`,
      `read_long(0x02000000) == 0xFFFFFFFF`; the same at `0x03FFFFFC` and anywhere in CS1.
      Independently derived from `DummyCs0Read*` (§9.3).
- [ ] Cart ID exactness: `read_byte(0x04FFFFFF) == 0xFF`, `read_word(0x04FFFFFE) == 0xFFFF`,
      `read_long(0x04FFFFFC) == 0xFFFFFFFF` — and, with a synthetic `cartid = 0x24`
      (32 Mbit backup cart, §9.2), `read_byte(0x04FFFFFF) == 0x24`,
      `read_word(0x04FFFFFE) == 0xFF24`, `read_long(0x04FFFFFC) == 0xFF24FF24`. That last value
      is the one that proves §4.3's composition was implemented rather than guessed.
- [ ] Near-miss addresses go to the cart, not the ID (§10's `[dev #43]`): with no cart,
      `read_byte(0x04FFFFFE)` and `read_word(0x04FFFFFC)` still return all-ones, but they must
      take the *forwarding* path — assert via the Phase 0 trace or a test-only cart stub.
- [ ] Open-bus asymmetry: `read_byte(0x02000000) == 0xFF` while
      `read_byte(0x00300000) == 0x00` (a genuine hole). Both are correct; the test documents
      that the difference is intentional.
- [ ] FRT capture: a word write to `0x01800000` sets the **master**'s `FTCSR` bit 7 and copies
      `FRC`→`FICR`; a word write to `0x01000000` does the same for the **slave**; a byte write to
      either, and any read, does nothing.
- [ ] Real-BIOS check: after this phase, `MIMAS_BUS_TRACE=1` should show CS0/CS1 probes
      resolving instead of appearing as unmapped misses; record what the BIOS actually reads.

---

## Phase 5 — Internal backup RAM fidelity

**Why fifth**: the BIOS reads backup RAM to decide whether to show its "backup memory damaged /
format?" flow. Wrong content changes which screen boots, but does not stall the CPU. Phase 2
already fixed the size, mirror and odd-byte write; this phase is the content and the API around
it. Reference: §2.3.

- [ ] Power-on content must match a **formatted** device, not zeros, or the BIOS sees a damaged
      backup on every cold boot. §2.3: a 32-byte header repeated 4 times (ASCII
      `BackUpRam Format` interleaved with `0xFF`), then from offset `0x80` the alternating
      pattern **even bytes `0xFF`, odd bytes `0x00`** (`memory.c:1365-1370`, `:1428-1444`).
      Decide explicitly whether Mimas ships pre-formatted (matching a used console) or blank
      (matching a dead battery, and letting the BIOS exercise its own format path) — and say
      which in the code comment. Recommendation: blank by default, pre-formatted behind a flag,
      because the BIOS's own format path is code we want to exercise.
- [ ] Persist backup RAM to a host file on change, and load it at startup. §2.3's `[dev #17]`:
      Yabause's `BupRamWritten` autosave trigger is **never set** (the only assignment lives in a
      `#if 0`), so its autosave is dead code — do not port the mechanism, port the intent.
- [ ] Do **not** implement extended backup (8 MB at `0x06300000`). §2.3/§2.15/§7: it requires the
      BIOS-function interception hack `[dev #25]` (forcing BIOS fetches at offset `0x0007D600`
      and `0x0380`–`0x03A8` to `0` so `SH2undecoded` can substitute native `BiosBUPInit`/
      `BiosHandleFunc`), and it **breaks** the internal `0x00180000` window via unsigned
      underflow `[dev #14]`. It is HLE, it conflicts with Mimas's "no BIOS HLE" posture, and
      Phase 2 already gives `0x06300000` to High WRAM where hardware puts it.
- [ ] Keep backup RAM byte-only (§2.3: word/long are stubs returning `0`) **or** allow all widths
      — but write down which, and why, next to the code. Note the asymmetry §12 `[dev #15]`
      flags: internal backup is byte-only while cartridge backup (§10) allows all widths and has
      no `| 1` convention.

**Architectural call-outs**: `backup_ram` keeps its single `RwLock` at its existing declaration
position (`shared_buffers.rs:53`). File persistence must happen **outside** the lock — snapshot
under the read guard into a local `Vec`, drop the guard, then write the file; never hold a
`WorkRam` lock across a syscall.

**Testing**

- [ ] Byte-lane behaviour, independently derived from §2.3: after `FormatBackupRam`-equivalent
      content is present, every **even** offset from `0x80` reads `0xFF` and every **odd** offset
      reads `0x00`.
- [ ] Header check: the first 128 bytes are the 32-byte header ×4, and a validator equivalent to
      `CheckBackupFile` (`memory.c:1372-1426`) accepts it.
- [ ] Round-trip through a temp file: write, persist, reload into a fresh `WorkRam`, read back.
- [ ] Real-BIOS check: boot to the point where the BIOS touches `0x00180000`, and confirm from
      the Phase 0 trace which offsets it reads and in what order — that trace is the evidence for
      whether the pre-formatted-vs-blank choice above was right.

---

## Phase 6 — One decoder for the whole system

**Why sixth**: not a hardware gap — an anti-drift measure. After Phases 1–5 there are three
decoders holding five sets of masks; the next person to change one will miss the others.
Reference: §0.7.

- [ ] Extract a `saturn-core/src/bus.rs` owning: the `MemRegion` enum, `translate`, and pure
      `read_u8/u16/u32(&WorkRam, MemRegion)` / `write_u8/u16/u32(&WorkRam, MemRegion, val)` for
      every **storage-only** region.
- [ ] Keep the **side-effecting** arms in `Sh2`, layered over the pure decoder, because they are
      per-master and stateful, not properties of the memory: SMPC COMREG dispatch
      (`sh2.rs:588-590` → `smpc_execute_command`), the CS2 command trigger (`:568-570`), the SCU
      DSP ports (`:645-649`, `:670-673`, `:718-744`), the SCU DMA trigger (`:684-696`), TVSTAT's
      live computation (`:457-460`, `:897-906`), on-chip registers (`:1456-1534`), and the SMPC
      SF constant (`:487`). The split to hold in mind: `bus.rs` answers *where does this address
      live*, `Sh2` answers *what happens when you touch it*.
- [ ] Repoint `scu_dsp.rs:664-705` at `bus.rs`, deleting its private map. It currently cannot
      reach BIOS, VDP1 VRAM, SMPC, SCU registers, CS2 or backup RAM at all — after this it can,
      which is what a real SCU DMA can do (§6 notes SCU DMA in Yabause goes through the same
      `MappedMemory*` accessors, just with `cycle == NULL`).
- [ ] Leave `m68k.rs:95-126` **as its own decoder**, and comment why: the M68K sees a genuinely
      different address space (Sound RAM at 0, SCSP registers at `0x100000` — `scsp.c`'s
      `c68k_byte_read`), not a view of the SH-2 map. Sharing code here would be wrong.
- [ ] Retire the `0x0600_1000` CD-ROM hack (`sh2.rs:604-606`, `:631-633`, `:666-668`) once
      Phase 2's CS2 offsets are real. Two tests assert it and must be rewritten against the real
      CR4 address `0x05890024`: `e2e-tests/src/lib.rs:784` and `sh2.rs:2363`. Do not delete the
      tests — repoint them.
- [ ] Make fetch from unmapped memory detectable: §5.1 of `sh2-cpu.md` — `FetchInvalid` returns
      `0xFFFF`, which decodes to the illegal-instruction handler. Mimas returns `0x0000` today
      and only `0xFFFF` traps (`sh2.rs:997`), so a runaway PC executes silently. Either return
      `0xFFFF` for a fetch from `Unmapped`, or trap `0x0000` as illegal. Prefer the former, since
      it matches the reference and keeps `0x0000` meaning whatever the opcode table says.

**Testing**

- [ ] Move the region round-trip suite to `bus.rs` and run it once against the pure API and once
      through `Sh2`, asserting identical results — that is the anti-drift property itself.
- [ ] A `bus.rs` test that walks every documented window boundary from §1's table (start, start+1,
      end-1, end) and asserts the region classification, so a future edit that shifts a boundary
      by one byte fails loudly.
- [ ] SCU DSP DMA can now read the BIOS and VDP1 VRAM: extend the existing DSP DMA tests.
- [ ] `cargo test --workspace` green, `cargo fmt` clean.

---

## Phase 7 — Cartridge models (game compatibility only)

**Why last among functional work**: nothing here affects BIOS boot. Reference: §9, §10.
Implement in this order; stop whenever the compatibility need is met.

- [ ] **Cartridge storage in `WorkRam`.** The cart is reachable by both SH-2s and by SCU DMA, so
      it must be shared, not per-`Sh2`. Add fields **at the end of the struct**, after
      `smpc_regs` (`shared_buffers.rs:61`), so they sit last in the field-declaration lock order
      and any call site that already holds an existing lock can take a cart lock without
      inverting the order:
  - [ ] `cart_dram: RwLock<Vec<u8>>` (0, 1 MB or 4 MB) — **one lock, not striped**, initially.
        The §1.3 striping rationale is two CPUs plus DMA hammering one region at 28.6 MHz; that
        applies to a 4 MB extended-RAM cart used as main work RAM (KOF95-class titles), so
        revisit with telemetry if such a title contends. Do not pre-stripe on speculation.
  - [ ] `cart_bupram: RwLock<Vec<u8>>` (1/2/4/8 MB) — one lock; backup access is rare and byte
        oriented.
  - [ ] `cart_rom: RwLock<Vec<u8>>` — one lock.
  - [ ] `cart_kind: RwLock<CartKind>` (or an atomic discriminant) so the decoder can branch
        without taking a data lock.
- [ ] **DRAM 8 Mbit (1 MB)** — §9.4. After `addr &= 0x1FFFFFF`, dispatch on `addr >> 20`:
      `0x04` → `dram[addr & 0x7FFFF]`, `0x06` → `dram[0x80000 | (addr & 0x7FFFF)]`, everything
      else all-ones. i.e. **two 512 KB banks at `0x02400000` and `0x02600000`**, with
      `0x02500000` and `0x02700000` reading all-ones. §9.4 says software detects the cart by this
      split-bank layout, so it is not an optimisation to flatten it.
- [ ] **DRAM 32 Mbit (4 MB)** — §9.4. Banks `0x04`–`0x07` → `dram[addr & 0x3FFFFF]`: contiguous
      `0x02400000`–`0x027FFFFF`. Nothing at `0x02000000`–`0x023FFFFF` or above `0x02800000`.
- [ ] **Backup carts 4/8/16/32 Mbit** — §10. CS1 handlers with masks `0xFFFFF` / `0x1FFFFF` /
      `0x3FFFFF` / `0x7FFFFF`, mirrored ×16 / ×8 / ×4 / ×2 across the 16 MB CS1 window; **all
      widths work and there is no `| 1` write convention**, unlike internal backup RAM
      (`[dev #44]`). `cartid` = `0x21`/`0x22`/`0x23`/`0x24`; the BIOS derives usable size as
      `0x40000 << (cartid & 0x0F)` = exactly **half** the allocation (§9.2).
- [ ] **ROM 16 Mbit** — §9.5. `rom[addr & 0x1FFFFF]`, mirrored **×16 across the whole 32 MB CS0
      window**. `cartid = 0xFF` (§9.2: `// I have no idea what the real id is`). Make it
      **read-only**: `[dev #38]` is a real Yabause bug (a writable "ROM" whose modification is
      neither persisted nor reverted).
- [ ] **Do not port** these, and say why in the code: the Action Replay / USB-Dev flash state
      machine (§9.7 — `[dev #33]` 256 KB out-of-bounds, `[dev #34]` two unreachable states,
      `[dev #35]` partial pages silently lost, `[dev #36]` `if (0x80000)` constant-true), Netlink
      and Japanese Modem (§9.8 — they perform real host socket I/O and are not self-contained
      hardware models, `[dev #46]`).
- [ ] Persistence: save cart backup RAM and (if the AR is ever added) the flash image. §9.9's
      `[dev #42]`: Yabause's savestates cover **only** DRAM carts, and `CartLoadState` can
      `CartDeInit()`+`CartInit()` while the page table still holds the old cart's handlers —
      `[dev #32]`, a 3 MB out-of-bounds window. Mimas's decoder reads `cart_kind` at access time
      rather than caching handler pointers, which structurally cannot reproduce that bug; note
      that in the comment so nobody "optimises" it back in.

**Testing**

- [ ] 8 Mbit DRAM split-bank: write at `0x02400000`, read back at `0x02400000`; read
      `0x02500000` → all-ones; write at `0x02600000` and confirm it does **not** appear at
      `0x02400000` (independent banks); `0x02480000` mirrors `0x02400000` (×2 in the block).
- [ ] 32 Mbit DRAM contiguity: `0x02400000` and `0x02700000` are 3 MB apart in the same buffer,
      not aliases.
- [ ] Backup cart mirror periods: for the 4 Mbit cart, write at `0x04000000`, read at
      `0x04100000` (×16); assert the 32 Mbit cart does **not** alias at `0x04100000`.
- [ ] ROM cart ×16: write BIOS-like known bytes into the image, assert the same longword reads at
      `0x02000000`, `0x02200000` and `0x03E00000`; assert writes do nothing.
- [ ] `cartid` per type against §9.2's table, read through all three CS1 ID widths.

---

## Phase 8 — Access cost model and A-Bus timing (optional, last)

**Why last**: §6 is explicit that the numbers "have no derivation in the source, the granularity
is 1 MB", and §12 `[dev #28]`–`[dev #31]` catalogue that SCU DMA is free, that High WRAM reads
cost `0` while writes cost `2`, that bit-28 mirrors cost `0` while base addresses do not, and
that a whole earlier cost model survives commented out. Porting this yields a *differently* wrong
timing model, not a better one. Mimas already paces via `ClockThrottle` against real clock rates.

- [ ] If a memory-cost model is ever wanted, treat §6's table as a **starting heuristic to be
      replaced**, not as data. Port the *shape* — a per-region penalty consulted by the SH-2 data
      path and skipped by DMA — and re-derive the numbers.
- [ ] `getVramCycle`'s structure (§6) is the one part worth keeping: flat `2` during VBlank,
      otherwise bank-dependent (split at VDP2 VRAM offset `0x40000`) from the VDP2's CPU
      access-cycle allocation. That is real arbitration, not a magic constant — but it needs
      VDP2's cycle-pattern registers, which do not exist (`docs/implementation-plans/vdp2.md`).
- [ ] A-Bus wait states / refresh / arbitration (SCU `ASR0`/`ASR1`/`AREF`): `[dev #45]` — not
      modelled by Yabause at all. Out of scope until something demonstrably needs it.

---

## Cross-cutting rules for every phase

1. **`Sh2::new(is_slave, arbiter, work_ram)` stays a 3-argument constructor**
   (`sh2.rs:260`, `CLAUDE.md` "Stability constraints"). New capability arrives as a public field
   with a default in the existing struct literal (`sh2.rs:261-298`) or a setter, following
   `set_bios_arc` (`:309`), `pc_reporter`, `m68k_control`, `speed`, `scu_dsp`.
2. **Every new `WorkRam` region declares its lock granularity explicitly** — one lock, striped,
   or lock-free atomic — with the §1.3 contention rationale it is answering. Default to one lock;
   stripe only against measured contention.
3. **New `WorkRam` fields go at the end of the struct**, after `smpc_regs`
   (`shared_buffers.rs:61`), so the existing field-declaration lock order is extended rather than
   perturbed, and no existing two-lock call site changes meaning.
4. **No call site acquires two `WorkRam` locks out of declaration order.** Today exactly one call
   site holds two (`vdp::execute_vdp1`, `vdp.rs:83-84`: `vdp1_vram` then `vdp1_framebuffer` —
   in order, therefore safe). Fix the stale "no call site needs more than one" comment at
   `shared_buffers.rs:20-24`. DMA-style copies must use separate lock scopes, never nesting
   (Phase 3).
5. **Prefer per-CPU ownership to a shared lock** whenever the reference says a structure is
   per-CPU (the cache data array and address array, §8). A plain field on `Sh2` beats any lock.
6. **Every mask, window and mirror period gets a `§`-citation comment** back into
   `hardware-reference/memory-bus.md`, and every deliberate divergence gets a `[dev #N]`
   reference explaining which Yabause behaviour is being **declined** and why. This plan declines
   at least: `[dev #12]` (unconditional extended-backup fill), `[dev #13]` (split fetch/data
   map), `[dev #21]` (out-of-bounds sound RAM byte read), `[dev #25]` (BIOS backup-library HLE),
   `[dev #32]`/`[dev #33]`/`[dev #38]` (cart out-of-bounds and writable ROM), `[dev #47]`
   (breakpoints mutating the dispatch table).
7. **`cargo test --workspace` green and `cargo fmt` clean after every phase**, not at the end.
   The `MIMAS_BIOS_PATH`-dependent tests stay `#[ignore]`/env-gated so the default run remains
   hermetic and network-free.
8. **Update the tracking docs as each phase lands**, not at session end: `.development/`
   `current_blocker.md`, `current_bugs.md`, `TASKS.md`, `ROADMAP.md` (all four are currently
   empty files), and a `history.md` chapter for each non-obvious decision — specifically the
   High WRAM 2 MB → 1 MB shrink, the two open-bus values, and the decision to keep one unified
   fetch/data map.

---

## Deliberate deviations from the reference (record, do not "fix")

| Mimas behaviour | Reference says | Why Mimas differs |
|---|---|---|
| One map for fetch and data | Yabause has a separate 256-entry fetch table (§7) whose disagreement with the data map is `[dev #13]` | The split is the bug; a unified map is what hardware does |
| Plain big-endian byte storage everywhere | T1 for some regions, T2's `mem[addr ^ 1]` swizzle for BIOS/LWRAM/HWRAM/sound RAM/VDP2 CRAM (§5.1–§5.2) | T2 is a host-endianness storage optimisation, not observable behaviour |
| No T3 storage model | T3 exists (§5.3) | `[dev #23]`: `T3MemoryInit` is never called anywhere — dead code |
| No extended backup RAM | 8 MB file at `0x06300000` (§2.15) | Requires BIOS-function HLE `[dev #25]` and breaks the internal window `[dev #14]` |
| `0x06300000`+ is High WRAM mirror | Yabause maps backup RAM there unconditionally (§2.15) | `[dev #12]`: the reference itself calls this a `[QUIRK]` against hardware |
| Bit 28 and area 4 still fold onto area 0 | `[dev #1]`/`[dev #2]` say hardware provides two aliases, not six | No independent evidence for the correct behaviour; keep Yabause-compatible and **log** (Phase 0) until real BIOS evidence exists |
| Unaligned accesses do not raise an SH-2 address error | Yabause raises none either (§4.2, `[dev #8]`) | Real SH-2 does raise one; belongs to `sh2-cpu.md`'s exception model, not the bus |
| No SH-2 cache; bit 29 discarded | Matches the default `CACHE_ENABLE=OFF` build (§0.5) | Deliberate; revisit only when a cache lands, at which point bit 29 becomes load-bearing |

---

## Open questions (do not guess — resolve with evidence)

1. **Bit-28 and area-4 aliases**: `[dev #1]`/`[dev #2]` assert hardware has two aliases, but the
   reference is sourced only from Yabause, which implements six. Phase 0's trace tells us whether
   any real BIOS access even reaches them. Until then, keep Yabause's behaviour.
2. **High WRAM's true mirrored extent**: §2.14 says the device is 1 MB and calls Yabause's
   `0x600`–`0x610` fill an off-by-one; §7 shows the *fetch* map covering
   `0x06000000`–`0x06FFFFFF` mirrored every 1 MB. Phase 2 extends the mirror to `0x07FFFFFF` on
   that basis. If a real BIOS access above `0x07000000` shows up in the trace expecting something
   else, revisit.
3. **Sound RAM `mem4b` default at power-on**: §2.9 gives the two behaviours but not the reset
   value. Determine it from the SCSP reference/reset path before Phase 2 hard-codes one.
4. **Pre-formatted vs blank backup RAM at first boot** (Phase 5): decide from what the BIOS
   actually does when it reads a blank device, using the Phase 0 trace — not from preference.
5. **Whether the real BIOS uses the cache data array (`0xC0000000`) as scratchpad.** Phase 1
   implements it regardless (it is cheap and unambiguously correct), but if the trace shows heavy
   use, that also explains any currently-unexplained boot misbehaviour and should be recorded in
   `history.md`.
