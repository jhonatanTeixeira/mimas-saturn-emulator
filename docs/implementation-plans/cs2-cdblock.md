# CS2 / CD Block — Implementation Plan

Diffs Mimas's current Rust against `docs/hardware-reference/cs2-cdblock.md` (the authoritative
register/command reference, every claim cited to `yabause/src/cs2.c|cs2.h|cdbase.c` by line) and
lays out the concrete work to close the gap.

**Every "§n" reference below points into `docs/hardware-reference/cs2-cdblock.md`.** Code
locations are `file:line` against the tree as of commit `148cac7`.

---

## 0. Current-state assessment

### 0.1 What exists and is genuinely solid

`saturn-core/src/cdrom.rs` (131 lines) — a working CHD reader:

- `Cdrom::open_chd` (`cdrom.rs:14`) opens a real CHD via the `chd = "0.2"` crate
  (`saturn-core/Cargo.toml`), not a stub.
- `Cdrom::read_sector` (`cdrom.rs:54`) does real hunk decompression with a one-hunk cache
  (`cdrom.rs:97-102`) and sector-size detection (`cdrom.rs:83-91`).

That is the *only* part of the CD block that exists. It is a disc-image reader, i.e. Mimas's
equivalent of `cdbase.c`'s `ISOCD` backend (§6.5) — roughly the bottom 5% of the subsystem.

### 0.2 What does not exist — stated plainly

**`Cdrom` is not integrated into the emulated system at all.** There is no `Arc<Mutex<Cdrom>>`
field on `SaturnSystem` (`lib.rs:32-75`), no `Sh2` field, and no thread owns it. Its only caller
in the entire tree is `saturn-frontend-native/src/main.rs:150-161`, a one-shot demo that opens
the CHD, reads one sector, prints a line, and drops it — *outside* `SaturnSystem`, after
`system.start()` has already spawned all 8 threads.

Concretely missing:

| Layer | Reference | Status |
|---|---|---|
| A-bus CS2 address decode (`0x18000`/`0x9xxxx`/`0x98000`, `& 0xFFFFF`) | §1.1, §1.2 | **absent** — see §0.3 below |
| `HIRQ` / `HIRQMASK` (14 defined bits, AND-mask write, level-like IRQ) | §1.3, §1.4 | **absent** |
| `CR1`-`CR4` + their three write/read side effects | §1.5 | **absent** |
| Data transfer FIFO `0x05818000` (32-bit only) | §1.6 | **absent** |
| Info transfer port `0x05898000` (16-bit read only, 5 modes) | §1.7 | **absent** |
| The command handshake (write CR1-3 → write CR4 → timed exec → HIRQ → read CR4) | §2 | **absent** |
| `doCDReport` / `doMPEGReport` response layouts | §2.1, §2.2 | **absent** |
| Drive `status` byte and its state machine | §3.1 | **absent** |
| 3 Hz backend status poll, periodic sector engine, `Cs2SetTiming` | §3.2, §3.3, §7 | **absent** |
| 200 sector blocks, 24 partitions, 24 filters, output connectors | §4.1-§4.4 | **absent** |
| Sector flow disc → filter → partition → FIFO → CPU | §4.5 | **absent** |
| All 64 implemented opcodes | §5.1-§5.8 | **absent** (see §0.3) |
| TOC construction (102 × u32, points A0/A1/A2) | §6.3 | **absent** |
| CHD track metadata → `ctl_addr`/`sector_size`/FAD map | §6.4 | **absent** |
| CDDA path to SCSP | §6.6 | **absent** |
| IP.BIN parse / region autodetect | §6.7 | **absent** |
| Core 7 (`smpc-cd-block`) logic | — | **absent** — `lib.rs:347-360` is a bare `yield_now()` loop |

`shared_buffers.rs:48-51` says so itself: *"the real CD command protocol lives in `Cdrom`
(CR1-4/HIRQ/DTR); this is a plain memory stub until that's wired into the CPU's address space."*
The first half of that sentence is not true either — the protocol does not live in `Cdrom` and
never did.

### 0.3 Scope honesty: this is "build the subsystem", not "extend a partial one"

Unlike `scu_dsp.rs` (a real 897-line interpreter that needs 6 more DMA variants), or `scsp.rs`
(real PCM playback that needs envelope/LFO), or `vdp.rs` (real backdrop rendering that needs
NBG/RBG layers), **the CD block has no partial implementation to extend.** The three things that
look like one are all fictitious:

1. **`Cdrom::send_command` (`cdrom.rs:112-130`)** is a 2-arm toy: `0x01` returns `vec![0x01]`,
   `0x02` returns `vec![0x02, 0x05]`, anything else returns `vec![]`. It takes a `&[u8]` and
   returns a `Vec<u8>`. The real protocol (§2) has no byte-string command channel at all — it is
   four memory-mapped 16-bit registers plus a 16-bit interrupt-flag register. There is nothing
   here to build on; `send_command` must be **deleted**, not extended.

2. **`Sh2::execute_cdrom_command` (`sh2.rs:1641-1671`)** reads "CR1-CR4" from `cs2_regs[0..8]`
   and answers opcodes `0x00`/`0x02` with a hardcoded `CR1 = 0x0400`, `HIRQ = 0x0001`. Every
   detail is wrong against §1.2/§5.1: CR1 is at offset `0x90018` not `0x0000`, HIRQ is at
   `0x90008` not `0x0008`, opcode `0x02` is Get TOC (not "Get Play Status"), and the trigger is
   the **CR4 write**, not `off == 6 || off == 7` (`sh2.rs:568`). Must be deleted.

3. **`Sh2::cdrom_command_executed` (`sh2.rs:86`)** is set when the CPU writes address
   `0x0600_1000` (`sh2.rs:604-606`, `:631-633`, `:666-668`) — an address in **High Work RAM**,
   not the CD block. It exists solely so `e2e-tests/src/lib.rs:777-787`
   (`test_tier3_combination_f3_f5_sh2_cdrom_command_execution`) can assert something. Must be
   deleted along with that test.

Additionally, `.development/phased_development_plan.md:115-128` marks "Phase 6 … CD Block / CS2
Subsystem: Implement command registers `CR1`-`CR4` and status flags in `HIRQ`. Stream sectors
from CHD images into the CS2 address range via DMA" as **✅ Completed**. It is not. That status
line should be corrected to ⬜ when Phase 1 below starts.

### 0.4 Two real bugs in the existing address decode

Both must be fixed in Phase 1; neither is a "nice to have".

- **The register file is 4 KB but the window is 1 MB, and the mask aliases live ports onto each
  other.** `shared_buffers.rs:51` declares `cs2_regs: RwLock<Box<[u8; 0x1000]>>`;
  `sh2.rs:469-472` / `:562-571` index it with `off & (ram.len() - 1)` = `off & 0xFFF`. Real
  hardware masks with `0xFFFFF` (§1.1). Under `& 0xFFF`, the data FIFO `0x18000` and the info
  port `0x98000` **both collapse to offset `0x000`**, and every register is silently mirrored
  every 4 KB. §5.1 of `hardware-reference/memory-bus.md` explicitly lists CS2 as one of the
  three regions that do *not* mirror inside their window.

- **Byte accesses must not reach the CD block.** §1.1 and §10 [QUIRK 1]: `Cs2ReadByte`/
  `Cs2WriteByte` forward to the cartridge, which with no cart returns a constant `0xFF` and
  discards writes. Mimas currently services byte reads out of the plain array, and — worse —
  `read_word`/`read_long` (`sh2.rs:613-636`, `:639-707`) are implemented as 2 and 4 successive
  `raw_read_byte` calls, so any register with a read side effect would fire it 2-4× per access.
  See architectural call-out **E**.

### 0.5 Ground truth to collect *before* writing Phase 1 code

Do not guess which commands the BIOS issues. `.development/current_blocker.md` and
`current_bugs.md` are currently **empty files (0 bytes)**, so the only recorded wall is
`history.md` Chapter 12's: Core 0 settles at PC `0x060131A8` inside a counted loop, after the
SCU DSP interpreter landed. Nothing in that trace has been tied to the CD block yet.

- [x] Run `MIMAS_BOOT_WATCH_SECS=280 ./target/release/saturn-frontend-native --bios <real.bin>`
      and `grep '\[REGACCESS\] Cs2Regs'` on stderr. `log_reg_access_once` (`sh2.rs:242-257`)
      already treats `MemRegion::Cs2Regs` as interesting (`sh2.rs:246`), so this needs no code
      change. Record the exact offsets and directions in `.development/current_blocker.md`.
- [x] Repeat with `--chd <disc.chd>` to see whether the probe sequence differs with a disc
      "present" (it should not yet — nothing connects the CHD to the register block).
- [x] Expected, but **verify rather than assume**: a real BIOS probes the drive long before it
      wants game data. §3.1's `CDB_STAT_NODISC`/`CDB_STAT_OPEN` paths and §5.1's `0x00`
      Get Status / `0x01` Get Hardware Info / `0xE0` Authenticate Device exist precisely for
      that. The BIOS's own CD-player screen is already a tracked milestone
      (`milestone-tests/fixtures/cd_player_screen.jpg`), and that screen cannot render without a
      drive that answers status queries.
- [x] **Gotcha**: once Phase 1 moves CS2 decoding above `raw_read_byte`/`raw_write_byte`, the
      `log_reg_access_once` calls at `sh2.rs:400` and `sh2.rs:506` stop seeing CS2 traffic. Add
      an equivalent call at the new word/long interception point in the same commit, or the
      `[REGACCESS]` recipe silently goes blind on exactly the subsystem being built.

---

## Phase 1 — Wire the CD block into the system and decode CS2 for real

**Goal**: a `Cs2` object that exists, is owned by `SaturnSystem`, is reachable from the SH-2's
address space at the correct offsets and access widths, and holds a `Cdrom` as its disc backend.
No commands yet — this phase is pure plumbing plus the two decode-bug fixes.

### 1.1 New `saturn-core/src/cs2.rs`

- [x] `pub struct Cs2` with the register file from §0 / §1.2:
      `hirq: u16`, `hirqmask: u16`, `cr1..cr4: u16`, `mpegrgb: u16`.
- [x] Drive report state (§2.1, §3.1): `status: u8`, `fad: u32`, `options: u8`, `repcnt: u8`,
      `ctrladdr: u8`, `track: u8`, `index: u8`.
- [x] Command scheduling state (§2, §7): `command_pending: bool` (Yabause's `_command`),
      `command_timing_us: i32`, `command_execlock_us: i32`, `delay_irq: u16`.
- [x] Info-transfer state (§1.7): `infotranstype: i8` (−1 = idle), `transfercount: u32`,
      `cdwnum: u32`, `toc: [u32; 102]`.
- [x] Disc backend: `disc: Option<Cdrom>`, plus a `backend_status()` returning the four §6.1
      codes (0 spinning / 1 not spinning / 2 no disc / 3 tray open).
- [x] Reset values, exactly per §1.5 / §1.3 / §5.4 / §3.2 — these are the first
      independently-verifiable facts in the subsystem:
      `CR1=0x0043 CR2=0x4442 CR3=0x4C4F CR4=0x434B` (ASCII `\0C`,`DB`,`LO`,`CK`),
      `HIRQ=0xFFFF` (every flag set), `HIRQMASK=0x0000`,
      `getsectsize = putsectsize = 2048`,
      and with a disc present `status=PAUSE(0x01)`, `FAD=150`, `options=0`, `repcnt=0`,
      `ctrladdr=0x41`, `track=1`, `index=1`; with no disc `status=NODISC(0x07)` and
      `FAD=0xFFFFFFFF`, `options/repcnt/ctrladdr/track/index = 0xFF`.
- [x] `const` block for the 14 HIRQ bits (§1.3), named as in the reference:
      `CMOK 0x0001`, `DRDY 0x0002`, `CSCT 0x0004`, `BFUL 0x0008`, `PEND 0x0010`, `DCHG 0x0020`,
      `ESEL 0x0040`, `EHST 0x0080`, `ECPY 0x0100`, `EFLS 0x0200`, `SCDQ 0x0400`, `MPED 0x0800`,
      `MPCM 0x1000`, `MPST 0x2000`. Bits 14-15 undefined.
- [x] `const` block for the 12 status values (§3.1): `BUSY 0x00`, `PAUSE 0x01`, `STANDBY 0x02`,
      `PLAY 0x03`, `SEEK 0x04`, `SCAN 0x05`, `OPEN 0x06`, `NODISC 0x07`, `RETRY 0x08`,
      `ERROR 0x09`, `FATAL 0x0A`, plus flags `PERI 0x20`, `TRNS 0x40`, `WAIT 0x80` and the
      pseudo-value `REJECT 0xFF`.

### 1.2 Ownership and wiring — follow the `scu_dsp` precedent exactly

`Sh2::new()`'s 3-argument signature must not change (CLAUDE.md "Stability constraints"). The
established pattern is `scu_dsp`: an `Arc<Mutex<T>>` field on `SaturnSystem` (`lib.rs:73`),
constructed in `new()` (`lib.rs:102`), cloned into the Core 0 closure (`lib.rs:139`) and assigned
through a public `Option` field after construction (`lib.rs:151`).

- [x] `SaturnSystem.cs2: Arc<Mutex<Cs2>>`, constructed in `SaturnSystem::new` (`lib.rs:89-104`).
- [x] `Sh2.cs2: Option<Arc<Mutex<Cs2>>>`, defaulted to `None` in `Sh2::new` (`sh2.rs:260-...`)
      so every existing unit test still builds unchanged.
- [x] Assign `cpu.cs2 = Some(cs2_c0)` in the Core 0 closure (`lib.rs:140-153`). Core 1 (slave)
      also has a CS2 window on real hardware — wire it too, but note the A-bus interrupt goes to
      the master only (§4.2 of `hardware-reference/scu.md`: `SendInterrupt` targets `MSH2`).
- [x] `SaturnSystem::load_disc(&mut self, path: &str) -> Result<(), String>` that constructs the
      `Cdrom` and installs it into `Cs2`. Mirrors `load_bios` (`lib.rs:109-111`). Safe to call
      before *or* after `start()` — after `start()` it must also set `isdiskchanged` so the 3 Hz
      poll (§3.2) picks it up in Phase 3.
- [x] Rewrite `saturn-frontend-native/src/main.rs:148-161` to call `system.load_disc(chd)` before
      `system.start()` instead of doing its own one-shot `Cdrom::open_chd` + `read_sector(150,…)`
      demo. **Note that demo is also wrong on its own terms**: it passes `150` as an LBA, but
      `LBA + 150 = FAD` (§9) — FAD 150 *is* LBA 0. The IP.BIN sector it means to read is LBA 0.

### 1.3 Real CS2 address decode in `sh2.rs`

- [x] Change `MemRegion::Cs2Regs(usize)`'s payload to the §1.1 mask: `sh2.rs:356-357` currently
      computes `a - 0x0580_0000`, which is correct as an offset; the bug is downstream
      (`& 0xFFF`). Decode the offset against the §1.2 table instead of indexing an array.
- [x] Add a `Cs2Port` enum resolving an offset to one of: `DataFifo` (`0x18000`),
      `Hirq` (`0x90008`/`0x9000A`), `HirqMask` (`0x9000C`/`0x9000E`), `Cr1` (`0x90018`/`0x9001A`),
      `Cr2` (`0x9001C`/`0x9001E`), `Cr3` (`0x90020`/`0x90022`), `Cr4` (`0x90024`/`0x90026`),
      `MpegRgb` (`0x90028`/`0x9002A`), `InfoPort` (`0x98000`), `Undocumented`.
      Both aliases of each 16-bit register are the *same* register with the *same* side effects
      (§1.2, and §1.5's note that reading either CR4 alias clears `_command`).
- [x] Enforce the §10 [QUIRK 2] width asymmetry:
      - `DataFifo`: 32-bit read and 32-bit write only.
      - `InfoPort`: 16-bit read only (no write path, no 32-bit path).
      - `Hirq`/`HirqMask`/`Cr1`-`Cr4`/`MpegRgb`: 16-bit R/W; 32-bit **read** returns the 16-bit
        value duplicated into both halves, `(v << 16) | v` (§1.2 last paragraph); 32-bit write is
        *not* decoded (falls to `Undocumented`).
      - `Undocumented`: reads return 0, writes are dropped. (Log once, as Yabause does.)
- [x] Byte access: `raw_read_byte`'s `MemRegion::Cs2Regs` arm (`sh2.rs:469-472`) returns a
      constant `0xFF`; `raw_write_byte`'s arm (`sh2.rs:562-571`) drops the write. §1.1, §10
      [QUIRK 1]. Add a comment saying this models the empty-cartridge A-bus open-bus value and
      that a Netlink/JapModem cart would instead route these to a UART.
- [x] Delete `Sh2::execute_cdrom_command` (`sh2.rs:1641-1671`) and its `off == 6 || off == 7`
      trigger (`sh2.rs:568-570`).
- [x] Delete `Sh2::cdrom_command_executed` (`sh2.rs:86`, `:279`) and the three
      `address == 0x0600_1000` hacks (`sh2.rs:604-606`, `:631-633`, `:666-668`).
- [x] Delete `Cdrom::send_command` (`cdrom.rs:112-130`) and the unread `Cdrom::dma_triggered`
      field (`cdrom.rs:9`, set at `:58`, read by nothing).
- [x] Remove `WorkRam::cs2_regs` (`shared_buffers.rs:51`, `:81`) once nothing reads it — `Cs2`
      owns its own registers behind its own `Mutex`, and a byte array can't express any of the
      side effects. Its doc comment (`shared_buffers.rs:48-50`) goes with it.
- [x] Re-add `log_reg_access_once`-equivalent logging at the new interception point (see §0.5).

### 1.4 Interception hooks (see architectural call-out E)

- [x] `sh2.rs` has a long interception hook (`read_long`'s `MemRegion::ScuRegs` arm at
      `sh2.rs:645-649`, `write_long`'s at `:670-698`, feeding `read_scu_dsp_port`/
      `write_scu_dsp_port` at `:718-744`) but **no word-level hook at all** — `read_word`
      (`sh2.rs:613-622`) and `write_word` (`:625-636`) go straight to per-byte decomposition.
      Add `read_cs2_word`/`write_cs2_word` hooks in the same shape, called before decomposition.
- [x] Add `read_cs2_long`/`write_cs2_long` alongside the existing SCU-DSP long hooks.
- [x] All four take `&mut self` (or `&self` plus the `Mutex`) because reads mutate state:
      reading CR4 clears `command_pending` (§1.5), reading the info port advances
      `transfercount` (§1.7), reading the data FIFO advances `datatransoffset` (§1.6).

### 1.5 Fix `Cdrom` itself — remove the test-shaped fakery

`cdrom.rs`'s "mock mode" contradicts CLAUDE.md's honesty rule ("keep behavior honest … rather
than faking output") and will actively lie to the state machine built in Phase 3.

- [x] Delete the `path.contains("bad_chd")` early-error (`cdrom.rs:15-17`), the
      `path.contains("dummy")` flag (`cdrom.rs:19`), the `path == "dummy.chd" || !exists ||
      len < 124` mock branch (`cdrom.rs:23-39`), and the fabricated `SEGADISCSYSTEMKR` /
      `SEGADISCSYSTEM` sector contents (`cdrom.rs:64-78`). A missing/short/corrupt file must be a
      plain `Err`.
- [x] Model "no disc" as `Cs2.disc == None` reporting backend status `2` (§6.2) — the honest
      equivalent of Yabause's `DummyCD`, which likewise can never contain a disc.
- [x] Replace the sector-size sniff (`cdrom.rs:83-91`) and the flat
      `hunk = lba / sectors_per_hunk` map (`cdrom.rs:93-95`) with a real track map read from the
      CHD's `CDROM_TRACK_METADATA2` entries, per §6.4:
      - map the MAME track-type strings to `ctl_addr`/`sector_size` using §6.4's table
        (`MODE1`/`MODE1/2048`/`MODE2_FORM1`/`MODE2/2048` → `0x41`/2048;
        `MODE1_RAW`/`MODE1/2352`/`MODE2_RAW`/`MODE2/2352` → `0x41`/2352;
        `MODE2`/`MODE2/2336`/`MODE2_FORM_MIX` → `0x41`/2336;
        `MODE2_FORM2`/`MODE2/2324` → `0x41`/2324; `AUDIO` → `0x01`/2352);
      - build the three parallel frame numberings `logframeofs` (FAD space, starting at 150),
        `physframeofs`, `chdframeofs` (including `extraframes` padding to the 4-frame
        `CD_TRACK_PADDING` boundary);
      - **CHD frames are always 2448 bytes** (2352 + 96 subcode, §6.4 last line) regardless of
        the track's logical `sector_size` — so `hunkid = (chdlba * 2448) / hunkbytes`,
        `hunk_offset = (chdlba * 2448) % hunkbytes` (§6.5).
- [x] Change the signature to `read_sector_fad(&mut self, fad: u32, buffer: &mut [u8; 2448])`.
      §6.5: the destination must be 2448 bytes because §4.1's `workblock.data` is 2448 and the
      subcode tail is read from `data[2352..]` by `Cs2GetSubcodeQRW` (§5.1). Today the function
      copies `buffer.len()` bytes from the frame start, so a caller asking for 2048 gets the sync
      header and header bytes, not user data — stripping is the CD block's job (§4.5), not the
      backend's.
- [x] Byte-swap audio-track sectors pairwise (§6.5 last paragraph, `ctl_addr == 0x01`), and
      prepend the 12-byte sync header `00 FF FF FF FF FF FF FF FF FF FF 00` (§9) for 2048-byte
      tracks. **Deliberately deviate from §10 [QUIRK 55]**: also synthesize bytes
      `0x0C..0x0F` (MIN/SEC/FRAME and the mode byte) instead of leaving them zero, so a genuine
      MODE2/2048 track isn't misclassified as mode 1. Record this in §"Deliberate deviations".
- [x] Do **not** reproduce §10 [BUG 54] (`j < track_num - 1`, which makes the last track of a CHD
      unreadable and a single-track CHD entirely unreadable) or [BUG 52]/[BUG 53] (stale
      `currentTrack`, unconditional success return). A FAD outside every track must return `Err`.

### 1.6 Tests for Phase 1

Conventions: `saturn-core` unit tests live in `sh2.rs`'s / the new `cs2.rs`'s `#[cfg(test)] mod
tests`; cross-crate tests live in `e2e-tests/src/lib.rs`. `cargo test --workspace` must stay
fast, deterministic and offline.

- [x] `cs2_reset_registers_spell_cdblock`: fresh `Cs2`, assert `CR1==0x0043 CR2==0x4442
      CR3==0x4C4F CR4==0x434B`. Independently derivable from ASCII: `0x43='C'`, `0x44='D'`,
      `0x42='B'`, `0x4C='L'`, `0x4F='O'`, `0x4B='K'` — §1.5.
- [x] `cs2_reset_hirq_is_all_flags_set`: `HIRQ == 0xFFFF`, `HIRQMASK == 0x0000` (§1.3, §1.4).
- [x] `cs2_byte_access_returns_ff_and_drops_writes`: `cpu.read_byte(0x2589_0018) == 0xFF`;
      `cpu.write_byte(0x2589_0018, 0x00)` then `cpu.read_word(0x2589_0018) == 0x0043` (unchanged)
      — §1.1, §10 [QUIRK 1].
- [x] `cs2_register_aliases_are_the_same_register`: write `0x1234` to `0x2589_0018`, read
      `0x2589_001A`, expect `0x1234`; and vice-versa for all four CR pairs plus HIRQ/HIRQMASK/
      MPEGRGB (§1.2).
- [x] `cs2_long_read_duplicates_the_word`: `read_long(0x2589_0018) == 0x0043_0043` (§1.2).
- [x] `cs2_window_does_not_mirror_every_4kb`: `read_word(0x2589_1018)` must **not** return CR1;
      it is an undocumented offset returning 0. This is the direct regression test for the
      `& 0xFFF` bug (§0.4).
- [x] `cs2_data_fifo_is_32bit_only` / `cs2_info_port_is_16bit_read_only`: word access to
      `0x2581_8000` and long access to `0x2589_8000` return 0 without side effects
      (§10 [QUIRK 2]).
- [x] **Delete and replace**: `sh2.rs:2362-2376` `test_cdrom_handshake` asserts CR1 at
      `0x05800000` and HIRQ at `0x05800008` — offsets that are `Undocumented` on real hardware
      (§1.2). It is a self-consistent-but-wrong test, exactly what CLAUDE.md warns about.
- [x] **Update**: `sh2.rs:2281-2303` `peripheral_regions_are_real_readwrite_memory` probes
      `(0x0580_0000, "CS2/CD-ROM regs")` at `sh2.rs:2288` and asserts long round-trip. CS2 is
      *not* read/write memory; drop that row from the probe list and cover CS2 with the
      port-specific tests above.
- [x] **Delete**: `e2e-tests/src/lib.rs:292-299` (`…_cdrom_send_command`), `:310-317`
      (`…_cdrom_chd_header`), `:626-632` (`…_cdrom_invalid_command`) — all three assert the
      `send_command` toy; and `:777-787`
      (`test_tier3_combination_f3_f5_sh2_cdrom_command_execution`), which writes to High Work RAM
      `0x06001000` and calls it a CD-ROM command.
- [x] **Rewrite**: `e2e-tests/src/lib.rs:275-281`, `:283-290`, `:301-308`, `:618-624`, `:634-641`
      and the zero-buffer test — all depend on `open_chd`'s deleted mock mode (an empty temp file
      currently "opens successfully"). Re-point them at the real fixture from §"Test fixtures".

**Exit criterion**: a real-BIOS boot run's `[REGACCESS]` output shows CS2 word accesses at the
correct §1.2 offsets, and `cargo test --workspace` is green.

---

## Phase 2 — The CR1-4/HIRQ handshake, and the commands BIOS boot probes with

**Goal**: the 8-step handshake of §2 works end-to-end, and the drive answers the status/info
commands a booting BIOS asks before it ever wants sector data.

### 2.1 The handshake (§2)

- [x] Write `CR1` → `status &= ~PERI` (clear `0x20`) **and** `command_pending = true`
      (§1.5, `cs2.c:313-314`). This suppresses the periodic report so it can't clobber a
      response mid-read.
- [x] Write `CR2`, `CR3` → store verbatim, no side effects.
- [x] Write `CR4` → `set_command_timing(cr1 >> 8)`: `250 µs` for opcode `0x02`, `50 µs` for
      everything else (§2 step 3, §7).
- [x] Read `CR4` (either alias, 16-bit or 32-bit) → `command_pending = false` (§1.5,
      `cs2.c:194, 372`). Note §2's warning: nothing prevents the periodic report from clobbering
      a response if software reads CR4 *before* CR1-CR3. Reproduce that ordering faithfully.
- [x] `Cs2::exec(elapsed_us)` — the `Cs2Exec` equivalent (§7): while `command_execlock_us > 0`,
      **do not** decrement `command_timing_us` (§2's last bullet). When `command_timing_us`
      expires, dispatch on `cr1 >> 8` — re-reading CR1 *at execution time*, not latching it at
      the CR4 write (§2's first bullet; a CR1 written between CR4 and expiry changes the opcode).
- [x] `set_irq(bits)`: `hirq |= bits`; if `hirq & hirqmask != 0`, raise the A-bus interrupt
      (call-out D). §1.3.
- [x] HIRQ **write** is an AND-mask: `hirq &= val`. Writing `0xFFFF` is a no-op, `0x0000` clears
      all. After the AND, if `hirq & hirqmask` is still non-zero, **re-assert** the interrupt —
      the line is level-like (§1.3).
- [x] HIRQ **read** returns `hirq` verbatim. Do **not** derive `BFUL`/`DCHG`/`CSCT` from the
      internal `isbufferfull`/`isdiskchanged`/`isonesectorstored` booleans at read time —
      §10 [DEAD 3] documents that as commented-out dead code; those flags only surface when a
      command sets them explicitly.
- [x] `CMOK` is never auto-cleared (§10 [QUIRK 4], `cs2.c:1185` commented out). Software must
      clear it itself before polling for an edge. **Do not "improve" this** without evidence —
      real BIOS code was written against this behavior; if a boot trace shows the BIOS waiting
      for a `0→1` edge it never gets, revisit and record the finding.
- [x] Deferred second interrupts (§2, last bullet): `command_execlock_us` + `delay_irq`, fired by
      `exec` when the lock counts down. Used by `0x48` ResetSelector (450 µs → `ESEL`),
      `0x52` CalculateActualSize (30 µs × count → `ESEL`), `0x62` DeleteSectorData
      (30 µs × count → `EHST`). Implement the mechanism now; the three users land in Phase 4.
- [x] Unimplemented opcodes hit `default:` and **never complete** — no CR update, no `CMOK`, no
      interrupt (§10 [QUIRK 6]). Log the opcode once and hang, matching the reference. A silent
      fake `CMOK` here would mask exactly the diagnostic signal needed later.

### 2.2 `doCDReport` (§2.1)

- [x] `CR1 = (status << 8) | ((options & 0xF) << 4) | (repcnt & 0xF)`
- [x] `CR2 = (ctrladdr << 8) | track`
- [x] `CR3 = (index << 8) | ((fad >> 16) & 0xFF)`
- [x] `CR4 = fad & 0xFFFF`
- [x] Note §2.1: `REJECT (0xFF)` is passed *to* `do_cd_report`, not assigned to `self.status` —
      internal state is unchanged and only the reported CR1 shows the rejection.

### 2.3 TOC construction (§6.3) — needed before `0x02` Get TOC can answer

- [x] 102 × `u32`, pre-filled `0xFFFFFFFF`.
- [x] `TOC[0..=98]` = tracks 1-99: bits 0-23 = track start FAD, bits 24-27 = ADR, bits 28-31 =
      CTL. Built from the CHD track map as `(ctl_addr << 24) | fad_start` (§6.3's `BuildTOC`).
- [x] `TOC[99]` = point A0 = `(TOC[0] & 0xFF000000) | 0x010000` (first track = 1).
- [x] `TOC[100]` = point A1 = `(TOC[last] & 0xFF000000) | (track_num << 16)`.
- [x] `TOC[101]` = point A2 = `(TOC[last] & 0xFF000000) | session_fad_end` (lead-out FAD).
- [x] Session 0 only (§6.3; §10 [QUIRK 57] — multi-session support is nominal even in the
      reference).

### 2.4 Info transfer port, mode 0 (§1.7)

- [x] `infotranstype == 0`: return `TOC[transfercount >> 2]`, high word when
      `transfercount % 4 == 0`, low word otherwise. `transfercount += 2` and `cdwnum += 2` per
      read.
- [x] Terminate (reset `transfercount = 0`, `infotranstype = -1`) at the declared length
      `0xCC` words = 204 words = 408 bytes.
- [x] **Deliberately deviate from §10 [BUG 19]**: the reference's terminator uses `>` where `>=`
      is required, making exactly one word past the declared length readable — and in this case
      that read is `TOC[102]`, out of bounds on a 102-entry array. Use `>=`. Record in
      §"Deliberate deviations". If a real BIOS trace later shows it depends on the extra word,
      revisit with evidence.

### 2.5 Commands in this phase

| Opcode | Name | Ref | Response | HIRQ |
|---|---|---|---|---|
| `0x00` | Get Status | §5.1 `:1481` | `report(status)` | `CMOK` — **and no SCU interrupt**, see below |
| `0x01` | Get Hardware Info | §5.1 `:1488` | `CR1 = status<<8`, `CR2 = 0x0201`, `CR3 = mpgauth?1:0`, `CR4 = 0x0400`. Also clears `isdiskchanged` when the drive is neither `OPEN` nor `NODISC` | `CMOK` |
| `0x02` | Get TOC | §5.1 `:1509` | `CR1 = status<<8` (**pre-update**), `CR2 = 0xCC`, `CR3 = CR4 = 0`; loads `TOC[102]`, arms `infotranstype = 0`, *then* `status = PAUSE` | `CMOK\|DRDY` |
| `0x03` | Get Session Info | §5.1 `:1525` | sess 0: `CR3 = 0x0100 \| ((TOC[101]>>16)&0xFF)`, `CR4 = TOC[101] & 0xFFFF`; sess 1: `CR3 = 0x0100`, `CR4 = 0`; else `CR3 = CR4 = 0xFFFF`. Then `status = PAUSE`, `CR1 = status<<8`, `CR2 = 0` | `CMOK` |
| `0x04` | Initialize CD System | §5.1 `:1551` | `report(status)`; `status = PAUSE` and `FAD = 150` unless `OPEN`/`NODISC`; `isbufferfull = 0` | `CMOK\|ESEL`, plus `DCHG` if `isdiskchanged` |
| `0x05` | Open Tray | §5.1 `:1659` | `status = OPEN`; `report(status)` | `CMOK\|DCHG` |
| `0x06` | End Data Transfer | §5.1 `:1670` | `cdwnum != 0`: `CR1 = (status<<8) \| ((cdwnum>>17)&0xFF)`, `CR2 = cdwnum>>1`, `CR3 = CR4 = 0`; else `CR1 = (status<<8)\|0xFF`, `CR2 = 0xFFFF`. Then `cdwnum = 0` | `CMOK`, plus `EHST` if `datatranstype` was 0 or 2 |
| `0xE0` | Authenticate Device | §5.7 `:3153` | `CR2 & 0xFF == 1` → MPEG path (`MPED`, `mpgauth = 2`); else disc path (`isonesectorstored = 1`, `satauth = 4`). Then `status = PAUSE` and `report(status)` | `CMOK\|EFLS\|CSCT` (disc) / `CMOK\|MPED` (MPEG) |
| `0xE1` | Is Device Authenticated | §5.7 `:3204` | `CR1 = status<<8`; `CR2 = CR2!=0 ? mpgauth : satauth`; `CR3 = CR4 = 0` | `CMOK` |

- [x] `0x04` Initialize CD System init-flag decode from `CR1 & 0xFF` (§5.1's sub-table):
      bit 0 `0x01` = full software reset (clear `playFAD`/`playendFAD`/`playtype`/`maxrepeat`,
      `satauth`/`mpgauth`, all 24 filters, all 24 partitions, all 200 blocks,
      `blockfreespace = 200`, `curdir*`, `fileinfo[]`, `numfiles`, `lastbuffer = 0xFF`; **does
      not** clear the TOC);
      bit 1 `0x02` = "Decode RW subcode" (no-op in the reference);
      bit 2 `0x04` = "Don't confirm Mode 2 subheader" (no-op);
      bit 3 `0x08` = "Retry reading Form 2 sectors" (no-op);
      bit 4 `0x10` = `speed1x = 1`, else `speed1x = 0`.
- [x] **Reproduce §10 [BUG 5] deliberately?** `0x00` Get Status sets `CMOK` with a bare
      `HIRQ |= CMOK` rather than through `Cs2SetIRQ`, so it uniquely raises **no** SCU
      interrupt. Implement it the correct way (via `set_irq`) and note the divergence — a
      status poll that never interrupts is far more likely to be a Yabause bug than hardware
      behavior, and Get Status is the command the BIOS is most likely to poll on. Flag in
      `.development/current_bugs.md` so it can be flipped back if boot regresses.
- [x] **Reproduce §10 [QUIRK 30] as-is**: `0x01` returns hardcoded `CR2 = 0x0201` ("mpeg card
      exists") and `CR4 = 0x0400`. Keep the constants; add the comment that the emulated machine
      therefore always claims an MPEG card is fitted.
- [x] **Reproduce §10 [QUIRK 32] as-is**: `0xE0` always succeeds. Note that `status = BUSY` and
      the `0xFF`/`0xFFFF` register stuffing are never externally observable because
      `report(status)` overwrites them before returning.

### 2.6 Core 7 gets real work (see architectural call-out B)

- [x] Replace `lib.rs:347-360`'s bare `yield_now()` loop with the Core 6 pattern
      (`lib.rs:320-341`): `sync.set_thread_active(7, false)` at entry, then
      `park_while_inactive(7)` (`sync.rs:147-156`).
- [x] The CR4 write on Core 0 calls `sync.set_thread_active(7, true)` — exactly the SCU DSP `EX`
      precedent at `sh2.rs:732-736`. Core 7 wakes, calls `Cs2::exec`, and re-parks once
      `command_timing_us` has expired, the command has run, and nothing else is pending.
- [x] Phase 2 can park unconditionally between commands. Phase 3 cannot — see call-out B for the
      `park_until(deadline)` addition that the 3 Hz backend poll and 60 Hz periodic report force.

### 2.7 Tests for Phase 2

- [x] `cs2_cr4_write_arms_command_cr1_write_sets_pending`: assert `command_pending` after the
      CR1 write and `command_timing_us == 50` after the CR4 write (250 for opcode `0x02`) — §7.
- [x] `cs2_cr4_read_clears_command_pending` (both aliases, both widths) — §1.5.
- [x] `cs2_hirq_write_is_and_mask`: set `HIRQ = 0xFFFF`, write `0xFFFE`, read `0xFFFE`; write
      `0xFFFF`, still `0xFFFE` — §1.3.
- [x] `cs2_hirq_reasserts_irq_on_write_leaving_unmasked_bit`: with `HIRQMASK = CMOK`, a HIRQ
      write that leaves `CMOK` set must re-raise the A-bus IRQ flag — the "level-like" property
      of §1.3.
- [x] `cs2_get_status_reports_no_disc_state`: no disc installed → `CR1 == 0x07FF`
      (`status=NODISC(0x07)<<8 | (options=0xF)<<4 | (repcnt=0xF)`, from §3.2's reset table where
      `options`/`repcnt` are `0xFF` and §2.1's 4-bit masks), `CR2 == 0xFFFF`
      (`ctrladdr=0xFF, track=0xFF`), `CR3 == 0xFFFF` (`index=0xFF`, `FAD>>16 = 0xFF`),
      `CR4 == 0xFFFF` (`FAD & 0xFFFF` of `0xFFFFFFFF`). Derive each field by hand from §2.1 +
      §3.2 before writing the assertion.
- [x] `cs2_get_status_reports_pause_with_disc`: disc fixture installed → `CR1 == 0x0100`
      (`PAUSE(0x01)<<8 | options 0 | repcnt 0`), `CR2 == 0x4101` (`ctrladdr 0x41`, `track 1`),
      `CR3 == 0x0100` (`index 1`, `FAD>>16 == 0`), `CR4 == 0x0096` (FAD 150 = `0x96`).
- [x] `cs2_get_hardware_info_constants`: `CR2 == 0x0201`, `CR4 == 0x0400` — §5.1.
- [x] **`cs2_get_toc_against_a_hand_built_fixture`** — the headline test of this phase.
      Against the single-data-track fixture (see §"Test fixtures"): issue `0x02`, assert
      `CR2 == 0xCC` and `HIRQ & (CMOK|DRDY) != 0`, then read `0x2589_8000` repeatedly:
      | Read # | `transfercount` before | Expected | Derivation |
      |---|---|---|---|
      | 0 | 0 | `0x4100` | `TOC[0] >> 16`, `TOC[0] = (0x41 << 24) \| 150 = 0x41000096` |
      | 1 | 2 | `0x0096` | `TOC[0] & 0xFFFF`; 150 = `0x96` |
      | 2 | 4 | `0xFFFF` | `TOC[1] >> 16`, unused track slot |
      | 3 | 6 | `0xFFFF` | `TOC[1] & 0xFFFF` |
      | 198 | 396 | `0x4101` | `TOC[99] >> 16`; A0 = `(0x41000096 & 0xFF000000) \| 0x010000` |
      | 199 | 398 | `0x0000` | `TOC[99] & 0xFFFF` |
      | 200 | 400 | `0x4101` | `TOC[100] >> 16`; A1 = `0x41000000 \| (1 << 16)` |
      | 202 | 404 | `0x41xx` | `TOC[101] >> 16`; A2 = `0x41000000 \| lead_out_fad` |
      | 204 | 408 | — | terminator: `infotranstype` back to −1, `transfercount` back to 0 |
      Compute the whole 102-entry array in a throwaway Python script from §6.3's formulas and
      paste the derived values — do not read them out of the emulator.
- [x] `cs2_unimplemented_opcode_never_completes`: issue `0x07`, run `exec` well past the timing
      window, assert CR1-CR4 unchanged and `CMOK` not newly set — §10 [QUIRK 6].
- [x] `cs2_core7_parks_between_commands`: with the system started and no disc activity, Core 7's
      `LockStepSync` active flag is false. Mirrors the existing Core 6 parking coverage.

**Exit criterion**: a real-BIOS boot run shows the BIOS issuing at least one CD command,
receiving `CMOK`, and reading back a response — and Core 0's PC advances past whatever the
`[REGACCESS]` sweep from §0.5 identified. Record the result in `.development/current_blocker.md`.

---

## Phase 3 — Drive/disc state machine and the two free-running engines

**Goal**: `status` is a real state variable driven by the backend and by time, not a constant.

### 3.1 The status byte (§3.1)

- [x] Implement every transition in §3.1's table. Reachable states and their writers:
      `PAUSE(0x01)` — reset-with-disc, backend poll on insert, `0x02` GetToc, `0x03`
      GetSessionInfo, `0x04` InitializeCDSystem, `0x11` SeekDisc (pause/FAD/index forms), end of
      play, `0x75` AbortFile, end of `0xE0` AuthenticateDevice;
      `STANDBY(0x02)` — `0x11` SeekDisc stop and error forms;
      `PLAY(0x03)` — periodic engine's `SEEK → PLAY` release, `0x74` ReadFile;
      `SEEK(0x04)` — `0x10` PlayDisc, buffer-full back-pressure during play;
      `SCAN(0x05)` — `0x12` ScanDisc (one-way door, §10 [QUIRK 21]);
      `OPEN(0x06)` — reset with tray open, backend poll, `0x05` OpenTray;
      `NODISC(0x07)` — reset with no disc, backend poll;
      `ERROR(0x09)` — `0x65`/`0x66` on a bad partition index.
- [x] Never set: `BUSY(0x00)` outside `0xE0`'s internals, `RETRY(0x08)`, `FATAL(0x0A)`,
      `TRNS(0x40)`, `WAIT(0x80)`. Define the constants; leave them unreachable and say so.
- [x] `PERI(0x20)` is OR-ed in by the periodic report and cleared by a CR1 write (§1.5, §3.3).

### 3.2 Backend status poll — 3 Hz (§3.2)

- [x] `status_cycles += elapsed_us * 3`; fire when `>= 1_000_000`. Units are 1/3 µs, so the real
      period is 333 333 µs ≈ 3 Hz (§7).
- [x] Backend code 0 or 1 (disc present) → if state is `NODISC` or `OPEN`, go `PAUSE` and set
      `isdiskchanged = 1`. Code 2 → `NODISC`. Code 3 → `OPEN`.
- [x] `isdiskchanged` is set by disc-change/reset/poll and cleared **only** by `0x01`
      GetHardwareInfo when the drive is neither `OPEN` nor `NODISC`; it is consumed **only** by
      `0x04` InitializeCDSystem to decide whether to raise `DCHG` (§3.2).
- [x] Reset field table (§3.2) — the exact `status`/`FAD`/`options`/`repcnt`/`ctrladdr`/`track`/
      `index` values per backend code, already listed in §1.1's checklist.
- [x] `SaturnSystem::load_disc` / an eventual `open_tray`/`close_tray` frontend hook is the
      `Cs2ForceOpenTray`/`Cs2ForceCloseTray` equivalent (§3.4) — front-end only, not reachable
      from the guest. Note §10 [QUIRK 23]: the `0x05` OpenTray *command* only assigns
      `status = OPEN` and never touches the backend, so within one 333 ms poll period the poll
      forces the state back to `PAUSE`. Reproduce faithfully and comment it.

### 3.3 Periodic engine (§3.3)

- [x] `periodic_cycles += elapsed_us * 3`; fire when `>= periodic_timing`. `set_timing(playing)`
      (§3.3's table): playing and (`isaudio` or `speed1x`) → `40000` (13 333 µs, 75 sectors/s);
      playing and 2× → `20000` (6 667 µs, 150/s); not playing → `50000` (16 667 µs, 60 Hz).
- [x] Per tick: `PAUSE`/`SCAN`/`RETRY` do nothing; `SEEK` → if `!isbufferfull` go `PLAY` with
      `options = 0x8`; `PLAY` runs the sector read (Phase 4).
- [x] Then, **unless `command_pending`**, `status |= PERI`, `do_cd_report(status)` and raise
      `SCDQ` (§3.3 last bullet). This is the "periodic response".
- [x] `Cs2Exec` returns early while `command_pending` is set, *before* the periodic report
      (§2's second bullet) — that early return is the entire response-protection mechanism.
- [x] Skip `Cs2GetTimeToNextSector` (§10 [DEAD 45], no callers anywhere).

### 3.4 Tests for Phase 3

- [x] `cs2_poll_transitions_nodisc_to_pause_on_insert`: start with no disc, drive `exec` with
      333 334 µs of elapsed time after installing the fixture, assert `status == PAUSE`,
      `isdiskchanged == true`.
- [x] `cs2_poll_period_is_exactly_333333us`: assert no transition at 333 332 µs and a transition
      at 333 334 µs — derived from §7's `_statustiming = 1000000` at 1/3 µs, not from observing
      the implementation.
- [x] `cs2_get_hardware_info_clears_isdiskchanged_only_when_ready`: with `status == OPEN`, `0x01`
      leaves `isdiskchanged` set; with `status == PAUSE`, it clears it (§3.2).
- [x] `cs2_initialize_raises_dchg_only_when_disk_changed` (§5.1 `0x04`).
- [x] `cs2_periodic_report_sets_peri_and_scdq`: with `command_pending == false` and 16 667 µs
      elapsed at idle timing, assert `CR1 >> 8 == status | 0x20` and `HIRQ & SCDQ != 0`.
- [x] `cs2_periodic_report_suppressed_while_command_pending`: write CR1, advance 100 ms, assert
      CR1-CR4 untouched by the periodic report (§2 second bullet).
- [x] `cs2_open_tray_command_is_undone_by_backend_poll`: issue `0x05` with a disc present, assert
      `status == OPEN`; advance 333 334 µs, assert `status == PAUSE` — the faithful reproduction
      of §10 [QUIRK 23].

---

## Phase 4 — Sector buffers, partitions, filters, and getting data to the CPU

**Goal**: `Cdrom::read_sector_fad` output actually reaches CPU-visible Work RAM.

### 4.1 Blocks (§4.1)

- [x] `Block { size: i32 /* -1 == free */, fad: u32, cn: u8, fn_: u8, sm: u8, ci: u8,
      data: [u8; 2352] }`, a flat `blocks: Box<[Block; 200]>` (`MAX_BLOCKS`, §9), plus one
      oversized `workblock` whose `data` is **2448** bytes (raw sector + 96 subcode).
- [x] `allocate_block(sectsize) -> Option<u8>`: linear scan for `size == -1`; on success
      `blockfreespace -= 1` and `block.size = sectsize`. If `blockfreespace <= 0` after the
      decrement → `isbufferfull = true` and raise `BFUL`. If no free block → `isbufferfull`,
      `BFUL`, return `None`.
- [x] `free_block`: `size = -1`, `blockfreespace += 1`, `isbufferfull = false` unconditionally
      (§10 [QUIRK 46] — the transitions are exact-zero-equality driven, not threshold driven;
      reproduce, and comment why).
- [x] `sort_blocks(partition)`: compact the partition's block list. §10 [BUG 39] notes the
      reference leaves the parallel `blocknum[]` untouched, which is only invisible because
      nothing in the live path reads it. In Rust, model a partition's contents as a single
      `Vec<u8>` of block indices, which makes that class of desync unrepresentable.

### 4.2 Partitions (§4.2)

- [x] 24 partitions (`MAX_SELECTORS`, §9), each `{ size: i32 /* -1 == never used */,
      blocks: Vec<u8> }`. §10 [QUIRK 45]: the reference gives every partition a 200-entry array,
      so one partition can hold every buffer in the machine; a `Vec` capped at 200 models the
      same thing without the 24 × 200 pointer table.
- [x] Reset state: `size = -1`, empty block list.
- [x] **Bound-check the partition index everywhere.** §10 [BUG 12] documents `Cs2GetSectorNumber`
      and `Cs2CalculateActualSize` indexing `partition[CR3>>8]` (0-255) into a 24-element array
      with no check. Do not port that; reject with `report(REJECT)`.

### 4.3 Filters (§4.3)

- [x] 24 filters: `{ fad: u32, range: u32, mode: u8, chan: u8, smmask: u8, smval: u8,
      cimask: u8, cival: u8, fid: u8, condtrue: u8, condfalse: u8 }`.
- [x] Reset: `fad = 0`, `range = 0xFFFFFFFF`, everything else 0, `condtrue = i` (identity
      filter *i* → partition *i*), `condfalse = 0xFF`.
- [x] `mode` bit decode (§4.3's table): bit 0 `0x01` file number vs `fid`; bit 1 `0x02` channel
      vs `chan`; bit 2 `0x04` `(sm & smmask) != smval`; bit 3 `0x08` `(ci & cimask) != cival`;
      bit 4 `0x10` reverse the subheader result; bit 5 `0x20` **not decoded** (§10 [QUIRK 41]);
      bit 6 `0x40` FAD range `fad <= block.fad < fad + range`; bit 7 `0x80` is not a runtime
      condition — in `SetFilterMode` it means "initialize this filter".
- [x] Bits 0-4 apply **only** to mode 2 data sectors (`data[0xF] == 0x02 && !isaudio`); for mode
      1 or audio only the FAD-range test runs (§4.3).
- [x] Five output connectors (§4.4): `outconcddev`, `outconmpegrom`, `outconmpegfb`,
      `outconmpegbuf`, `outconhost`. Only `outconcddev` participates in the live data path;
      `outconmpegrom` is written only by `0xE2`; the other three are never written.
- [x] **Bound-check filter indices.** §10 [BUG 10] (`< 0x24` where `MAX_SELECTORS` is 24 — a
      decimal/hex confusion allowing indices 24-35 to run off the array) and §10 [BUG 11] (eight
      filter commands with no check at all). Use `< 24` and reject otherwise.

### 4.4 Sector flow (§4.5)

- [x] `read_filtered_sector(fad) -> (Result, Option<partition_idx>)`:
      1. `disc.read_sector_fad(fad, &mut workblock.data)` (2448 bytes).
      2. Compare the 12-byte sync header (§9) → `isaudio`. If audio: forward the raw 2352 bytes
         to the SCSP's CDDA input, set `isaudio`, force `set_timing(1)`, and return with no
         partition (§6.6). See Phase 5 for the SCSP hand-off.
      3. `workblock.size = getsectsize`; if mode 2 form 2 → 2324.
      4. Mode 2 → `fn/cn/sm/ci = data[0x10..=0x13]`.
      5. `filter_data(outconcddev, isaudio)`.
- [x] `filter_data`: walk the filter chain via `condfalse` until a condition passes;
      `lastbuffer := condtrue` on pass, `condfalse` on fail; `condfalse == 0xFF` → drop the
      sector (return `None`). Then allocate a block of `getsectsize`, copy
      `fn/cn/sm/ci/fad/size` from the workblock, strip, and
      `partition.size += block.size; partition.blocks.push(idx)`.
- [x] Stripping table keyed on `workblock.size` (§4.5): 2048 → `+24` if `data[0xF] == 0x02` else
      `+16`; 2324 → `+24`; 2336 → `+16`; 2340 → `+12`; 2352 → `+0`.
- [x] `read_unfiltered_sector` (the filesystem/IP.BIN bypass, §4.5): a *different* stripping
      table keyed on `getsectsize` that additionally distinguishes mode 2 form 1 from form 2 via
      `data[0x12] & 0x20` and sets `size = 2324` for form 2; copies the subheader into the
      destination only when the sync pattern matches **and** `data[0xF] == 0x02`.
      Do **not** port §10 [BUG 40] (allocating the destination block before the read and leaking
      it on failure).
- [x] `get_partition(filter)` — §10 [QUIRK 38] says the reference evaluates no filter conditions
      at all and just returns `partition[filter.condtrue]`. Match it for now (every unfiltered
      path depends on it), with the deviation recorded.

### 4.5 The data transfer FIFO (§1.6)

- [x] Read (32-bit) active only when `datatranstype != INVALID`. Source is
      `partition.blocks[datatranssectpos + datanumsecttrans].data[datatransoffset]`; value is
      **big-endian on the wire** (the reference byte-swaps on little-endian hosts — in Rust,
      `u32::from_be_bytes` over the four block bytes, no host-endianness branch);
      `cdwnum += 4`, `datatransoffset += 4`; on crossing `block.size`, offset resets to 0 and
      `datanumsecttrans += 1`.
- [x] Once `datanumsecttrans >= datasectstotrans`, further reads return 0 — and if
      `datatranstype == GETDELSECTOR (2)`, the deferred deletion fires: free blocks
      `[datatranssectpos, datatranssectpos + datasectstotrans)`, compact, `partition.size -=
      cdwnum`, `partition.numblocks -= datasectstotrans`, `datatranstype = INVALID`. No HIRQ bit
      on this path.
- [x] Write (32-bit) active only when `datatranstype == PUTSECTOR (3)`; raises `EHST` when
      `datanumsecttrans >= datasectstotrans` — the only place the transfer port itself raises
      `EHST`. **Do not** port §10 [QUIRK 16]'s unmotivated
      `size = (putsectsize - getsectsize) / 24; offset = datatransoffset - size` discard;
      implement a plain offset and record the deviation.
- [x] `datatranstype` values (§1.6): `INVALID = -1`, `GETSECTOR = 0`, `GETDELSECTOR = 2`,
      `PUTSECTOR = 3`. Value 1 is unused.
- [x] Skip `Cs2RapidCopyT1`/`T2` entirely — §10 [DEAD 48]: no callers, `T2` uses `BSWAP16` on
      32-bit values, and both index `block[datanumsecttrans]` instead of
      `block[datatranssectpos + datanumsecttrans]`. The SCU-DMA fast path they were meant for is
      handled by call-out A instead.

### 4.6 Commands in this phase

| Opcode | Name | Ref | Notes |
|---|---|---|---|
| `0x30` | Set CD Device Connection | §5.3 `:1990` | `CR3>>8` = filter (`0xFF` = disconnect); `CMOK\|ESEL` |
| `0x31` | Get CD Device Connection | §5.3 `:2008` | §10 [BUG 9]: the reference dispatches `Cs2SetCDDeviceConnection` here, silently *rewriting* the connection. **Implement the getter**; record the deviation |
| `0x32` | Get Last Buffer Destination | §5.3 `:2019` | `CR3 = lastbuffer<<8`; `CMOK` |
| `0x40` | Set Filter Range | §5.3 `:2029` | `CR3>>8` filter; `((CR1&0xFF)<<16)\|CR2` FAD; `((CR3&0xFF)<<16)\|CR4` range |
| `0x41` | Get Filter Range | §5.3 `:2044` | `CR1 = (status<<8)\|(FAD>>16)`, `CR2 = FAD&0xFFFF`, `CR3 = range>>16`, `CR4 = range&0xFFFF` |
| `0x42` | Set Filter Subheader Conditions | §5.3 `:2058` | `CR1&0xFF` chan, `CR2>>8` smmask, `CR2&0xFF` cimask, `CR3>>8` filter, `CR3&0xFF` fid, `CR4>>8` smval, `CR4&0xFF` cival |
| `0x43` | Get Filter Subheader Conditions | §5.3 `:2076` | mirror of `0x42` |
| `0x44` | Set Filter Mode | §5.3 `:2090` | mode bit 7 zeroes `mode/FAD/range/chan/smmask/cimask/smval/cival`. §10 [QUIRK 42]: this sets `range = 0`, not `0xFFFFFFFF` like every other reset path |
| `0x45` | Get Filter Mode | §5.3 `:2116` | `CR1 = (status<<8)\|mode` |
| `0x46` | Set Filter Connection | §5.3 `:2130` | `CR1&1` → `condtrue = CR2>>8`; `CR1&2` → `condfalse = CR2&0xFF` |
| `0x47` | Get Filter Connection | §5.3 `:2153` | `CR2 = (condtrue<<8)\|condfalse` |
| `0x48` | Reset Selector | §5.3 `:2168` | Flag decode below; `CMOK` then deferred `ESEL` at 450 µs |
| `0x50` | Get Buffer Size | §5.4 `:2280` | `CR2 = blockfreespace`, `CR3 = 0x1800` (24<<8), `CR4 = 0x00C8` (200) |
| `0x51` | Get Sector Number | §5.4 `:2290` | `CR4 = numblocks` (0 if `size == -1`) |
| `0x52` | Calculate Actual Size | §5.4 `:2309` | deferred `ESEL` at 30 µs × count. Result is in **16-bit words** |
| `0x53` | Get Actual Size | §5.4 `:2353` | `CR1 = (status<<8)\|((calcsize>>16)&0xFF)`, `CR2 = calcsize&0xFFFF` |
| `0x54` | Get Sector Info | §5.4 `:2363` | `CR3 = (fn<<8)\|cn`, `CR4 = (sm<<8)\|ci`; bad index → `CR1 = (0xFF<<8)\|(CR1&0xFF)` |
| `0x60` | Set Sector Length | §5.4 `:2405` | codes: 0→2048, 1→2336, 2→2340, 3→2352, other→unchanged; `CR1&0xFF` sets `getsectsize`, `CR2>>8` sets `putsectsize` |
| `0x61` | Get Sector Data | §5.4 `:2460` | arms `GETSECTOR`; `CMOK\|DRDY`; error → `report(REJECT)` + `CMOK\|EHST` |
| `0x62` | Delete Sector Data | §5.4 `:2506` | frees immediately; `CMOK` then deferred `EHST` at 30 µs × count |
| `0x63` | Get Then Delete Sector Data | §5.4 `:2560` | arms `GETDELSECTOR`; `CMOK\|DRDY\|EHST` |
| `0x64` | Put Sector Data | §5.4 `:2605` | arms `PUTSECTOR`, allocates `count` blocks of `putsectsize`; no CR update on success |
| `0x65` | Copy Sector Data | §5.4 `:2655` | `CMOK\|ECPY`; bad index → `status = ERROR`, `CMOK` only |
| `0x66` | Move Sector Data | §5.4 `:2695` | as `0x65` |
| `0x67` | Get Copy Error | §5.4 `:2735` | always "no error" (§10 [QUIRK 22]) |

- [x] `0x48` Reset Selector flag decode (§5.3's sub-table): `CR1&0xFF == 0` resets **only**
      partition `CR3>>8` and raises `CMOK|ESEL` immediately with no deferred IRQ; bit 7 `0x80`
      all filters `condfalse = 0xFF`; bit 6 `0x40` all filters `condtrue = i`; bit 4 `0x10` all
      filter conditions reset; bit 3 `0x08` empty ("reset partition output connectors");
      bit 2 `0x04` clears every partition and block; bits 5/1/0 not decoded.
      §10 [QUIRK 43]: bit 2 does **not** restore `blockfreespace` to 200 — that is a real
      accounting bug; restore it and record the deviation.
- [x] `CalcSectorOffsetNumber` shorthand for `0x61`/`0x62`/`0x63` (§5.4): `sectoffset == 0xFFFF`
      → `sectoffset = numblocks - 1` (count left as given); **else if** `sectnum == 0xFFFF` →
      `sectnum = numblocks - sectoffset`. The `else if` chain means `0xFFFF/0xFFFF` resolves only
      the offset — reproduce the chaining exactly.
- [x] Do **not** port §10 [BUG 13] (`Cs2CalculateActualSize` never advancing its loop index, so
      the result is count × one sector's size instead of the sum; plus testing
      `partition.size != 0` where unused is `-1`). Compute the real sum.
- [x] Do **not** port §10 [BUG 14] (`0x65`/`0x66` masking count with `0xFF` then testing
      `== 0xFFFF`, hardcoding 2352-byte blocks, unchecked `AllocateBlock`, and `MoveSectorData`
      mutating the source partition inside the copy loop).
- [x] Do **not** port §10 [BUG 15] (`0x64` zeroing `putpartition.size` before appending) or
      §10 [BUG 17] (get/put using inconsistent block indexing; `0x63` never setting
      `datatranspartitionnum`).

### 4.7 Getting the bytes into Work RAM — see architectural call-out A

- [x] Verify the **CPU polling path** first: guest polls `DRDY`, then reads `0x2581_8000` with
      `MOV.L` in a loop. This must work with no SCU involvement at all.
- [x] Then the **SCU DMA path**, which is what real BIOS and games actually use. This requires
      changes in `sh2.rs`'s `execute_scu_dma` (`sh2.rs:1536-1639`) that belong to
      `docs/implementation-plans/scu.md`; enumerate them there and cross-link. See call-out A.

### 4.8 Tests for Phase 4

- [x] `cs2_allocate_block_sets_bful_at_exhaustion`: allocate 200 blocks, assert `BFUL` raised on
      the 200th and `blockfreespace == 0`; free one, assert `isbufferfull` clears (§4.1,
      §10 [QUIRK 46]).
- [x] `cs2_get_buffer_size_reports_constants`: `CR3 == 0x1800`, `CR4 == 0x00C8` — derived from
      `MAX_SELECTORS = 24` and `MAX_BLOCKS = 200` in §9, i.e. `24 << 8` and `200 = 0xC8`.
- [x] `cs2_set_sector_length_code_table`: all four codes plus an out-of-range code leaving the
      value unchanged (§5.4).
- [x] `cs2_filter_fad_range_accepts_and_rejects`: filter with `mode = 0x40`, `fad = 200`,
      `range = 10`; feed synthetic workblocks at FAD 199 / 200 / 209 / 210 and assert
      accept/reject per `fad <= block.fad < fad + range` (§4.3). Values derived from the
      inequality, not from running the filter.
- [x] `cs2_filter_chain_falls_through_condfalse_then_drops`: filter 0 fails → `condfalse = 1`;
      filter 1 fails → `condfalse = 0xFF` → sector dropped, `lastbuffer == 0xFF` (§4.5).
- [x] `cs2_stripping_table`: for each of `workblock.size` ∈ {2048 mode 1, 2048 mode 2, 2324,
      2336, 2340, 2352}, build a synthetic 2352-byte sector with a known byte pattern and assert
      the stripped block starts at offset {16, 24, 24, 16, 12, 0} respectively (§4.5).
- [x] **`cs2_get_sector_data_round_trip_against_the_fixture`** — the headline test of this phase.
      Against the fixture CHD whose LBA 0 contains a known 2048-byte payload: force one sector
      into partition 0 via the periodic engine, issue `0x60` (code 0 → 2048), `0x61` with
      offset 0 / partition 0 / count 1, assert `DRDY`, then read `0x2581_8000` 512 times and
      compare against the fixture bytes *as written by the fixture generator*, big-endian per
      32-bit word. Assert the 513th read returns 0.
- [x] `cs2_get_then_delete_frees_on_readout`: same, with `0x63`; assert `blockfreespace` returns
      to 200 and `partition.size` drops only after the final FIFO read (§1.6).
- [x] `cs2_end_data_transfer_reports_word_count`: after transferring N 32-bit words, `0x06`
      reports `CR2 == cdwnum >> 1` and `CR1 low byte == (cdwnum >> 17) & 0xFF`; with no transfer,
      `CR1 low byte == 0xFF` and `CR2 == 0xFFFF` (§5.1).
- [x] `cs2_reset_selector_defers_esel_by_450us`: assert `CMOK` immediately, `ESEL` not set at
      449 µs, set at 451 µs (§2, §7).
- [x] `cs2_filter_index_out_of_range_is_rejected`: filter 24 and filter 35 both rejected — the
      regression test for §10 [BUG 10].
- [x] `scu_dma_from_cs2_fifo_lands_in_work_ram` (in `e2e-tests`): configure a level-0 SCU DMA
      with read address `0x2581_8000`, read-add = hold, write address in High Work RAM, count =
      2048; trigger; assert Work RAM matches the fixture payload byte-for-byte. This is the
      end-to-end proof that call-out A actually works.

---

## Phase 5 — Playback, seek, scan, subcode, CDDA

### 5.1 Commands

| Opcode | Name | Ref | Notes |
|---|---|---|---|
| `0x10` | Play Disc | §5.2 `:1734` | full argument decode below |
| `0x11` | Seek Disc | §5.2 `:1846` | four mutually exclusive forms |
| `0x12` | Scan Disc | §5.1 `:1914` | sets `SCAN` and nothing else; §10 [QUIRK 21] — one-way door |
| `0x20` | Get Subcode Q/RW | §5.1 `:1923` | `CR1&0xFF`: 0 = Q (`CR2 = 5`, `infotranstype = 3`), 1 = RW (`CR2 = 12`, `CR4 = group`, `infotranstype = 4`); `CMOK\|DRDY` |

- [x] `0x10` arguments (§5.2): `pdspos = ((CR1&0xFF)<<16)|CR2`, `pdepos = ((CR3&0xFF)<<16)|CR4`,
      `pdpmode = CR3>>8`.
      Start decode: `pdspos == 0xFFFFFF || pdpmode == 0xFF` → no change;
      `pdspos & 0x800000` → FAD mode, `playFAD = pdspos & 0xFFFFF`;
      otherwise track mode, `pdspos == 0` promoted to `0x0100`.
      `pdpmode & 0x80` = "preserve pickup position".
      End decode: `0xFFFFFF` → unchanged; `& 0x800000` → `playFAD + (pdepos & 0xFFFFF)` (a
      *length*); non-zero → `TrackToFAD(pdepos | 0x0063)`; zero → `TrackToFAD(0xFFFF)`
      (lead-out).
- [x] `0x11` forms (§5.2): `(CR1&0xFF)==0 && CR2==0` → **Stop**
      (`STANDBY`, all report fields `0xFF`, `FAD = 0xFFFFFFFF`);
      `(CR1&0xFF)==0xFF && CR2==0xFFFF` → **Pause**;
      `CR1 & 0x80` → **Seek by FAD** (`sdFAD = ((CR1&0x0F)<<16)|CR2`);
      `CR2>>8 != 0` → **Seek by track/index**; else **Error** (same wipe as Stop).
      All forms end with `set_timing(0)`, `report(status)`, `CMOK`.
- [x] Helpers (§5.2): `fad_to_track` (scan `TOC[0..98]`, `0xFF` on the first `0xFFFFFFFF`,
      `i+1` when `TOC[i] <= v < TOC[i+1]` masked to 24 bits, `0` off the end);
      `track_to_fad` (`0xFFFF` → `TOC[101] & 0xFFFFFF`; index byte `0x01` → `TOC[track-1] &
      0xFFFFFF`; index byte `0x63` → `(TOC[track] & 0xFFFFFF) - 1`; else `0`);
      `setup_default_play_stats(track, write_fad)` (no-op for `0xFF`; else `options = 8`,
      `repcnt = 0`, `ctrladdr = TOC[track-1] >> 24`, `index = 1`, `track = track`, and if
      `write_fad` also `FAD = TOC[track-1] & 0xFFFFFF`).
- [x] `0x20` Q-channel payload (§5.1): `[0] ctrladdr, [1] BCD(track), [2] BCD(index),
      [3..5] BCD(relative M,S,F), [6] 0, [7..9] BCD(absolute M,S,F)`.
      `ToBCD(v) = (v % 10) | ((v / 10) << 4)` (§9); `fad_to_msf`: `m = v/4500, s = (v%4500)/75,
      f = v%75`. Relative position is `FAD - (TOC[track-1] & 0xFFFFFF)`.
- [x] Do **not** port §10 [BUG 20] (`TOC[track-1]` with no validity check — `track == 0xFF` after
      a Stop gives `TOC[254]`; and the RW channel's unbounded `static group` counter reading past
      `workblock.data`'s 2448 bytes from `group == 4` on).
- [x] Do **not** port §10 [BUG 24] (`0x11`'s seek-by-FAD scanning only `TOC[0..15]` and passing
      the loop index `i` rather than `i+1` to `setup_default_play_stats`, which indexes
      `TOC[track-1]` → `TOC[-1]` for `i == 0`; and selecting the track *after* the one containing
      the target FAD).
- [x] Do **not** port §10 [BUG 26] (`0x10`'s track-mode end position truncating a 24-bit argument
      to 16 bits and OR-ing `0x63` into the index byte instead of replacing it).
- [x] Faithfully reproduce, with comments: §10 [QUIRK 25] (the fake seek delay assigns a FAD
      *difference* to `periodic_timing`, whose units are 1/3 µs, clamped to
      `[40000, SEEK_TIME=300000]` — unrelated units, but the resulting delay is what real
      software was tuned against), §10 [QUIRK 27] (play modes parsed but unimplemented; only the
      repeat count survives), §10 [QUIRK 44] (buffer-full back-pressure pushing the drive to
      `SEEK` with `options = 0` and back to `PLAY` when space frees).
- [x] Do **not** port §10 [QUIRK 47] (read errors silently swallowed: `-1`/`-2` returns handled
      by empty cases, so playback stalls at the same FAD forever with no status change and no
      HIRQ). Set `CDB_STAT_ERROR` and log; record the deviation.

### 5.2 End-of-play (§3.3)

- [x] On `FAD >= playendFAD`: either finish (`status = PAUSE`, `options = 0x8`, `set_timing(0)`,
      raise `PEND`, and if `playtype == FILE` also raise `EFLS` **and** `EHST`) or repeat
      (`FAD = playFAD`, `repcnt += 1` up to `0xE`).
- [x] §10 [HACK 35]: the `EHST` on the file path carries the inline comment *"Need for Assault
      Leynos 2"*, and the same block on the "sector filtered out" path raises neither `EHST` nor
      `options = 0x8`. Reproduce both branches exactly as written and comment the asymmetry — do
      not "clean it up" into one path.

### 5.3 CDDA → SCSP (§6.6)

- [x] Audio sectors never enter a partition. On sync-header mismatch, forward the raw 2352 bytes
      to the SCSP and return with no partition.
- [x] Mimas has no `ScspReceiveCDDA` equivalent. `Scsp` lives behind `Arc<Mutex<Scsp>>` on
      `SaturnSystem` (`lib.rs:74`) and is stepped by Core 5 (`lib.rs:298-313`). Add an
      `Scsp::receive_cdda(&mut self, &[u8; 2352])` feeding a ring buffer; give `Cs2` an
      `Option<Arc<Mutex<Scsp>>>` handle wired in `SaturnSystem::start`. **Cross-plan
      dependency** — coordinate the mixing side with `docs/implementation-plans/scsp.md`.
- [x] Note that `isaudio` forces `set_timing(1)`, which selects the 1× (75 sectors/s) rate — the
      correct real-time rate for audio playback (§3.3).

### 5.4 Tests for Phase 5

- [x] `cs2_fad_to_msf`: FAD 150 → `(0, 2, 0)` (150/4500 = 0, (150%4500)/75 = 2, 150%75 = 0);
      FAD 4650 → `(1, 2, 0)`; FAD 4649 → `(1, 1, 74)`. Derived arithmetically from §5.1's
      formula.
- [x] `cs2_to_bcd`: 0→0x00, 9→0x09, 10→0x10, 59→0x59, 99→0x99 — from §9's
      `ToBCD(v) = (v%10) + ((v/10)<<4)`.
- [x] `cs2_track_to_fad_lead_out_and_track_start`: against the 2-track fixture, `0xFFFF` returns
      the lead-out FAD and `(track<<8)|0x01` returns that track's start — values taken from the
      fixture generator's own track table.
- [x] `cs2_fad_to_track_boundaries`: FAD exactly at track 2's start returns 2, one below returns
      1, above the lead-out returns 0.
- [x] `cs2_seek_stop_form_wipes_report_fields`: `0x11` with `CR1&0xFF == 0, CR2 == 0` →
      `status == STANDBY`, `CR2 == 0xFFFF`, `CR4 == 0xFFFF`.
- [x] `cs2_play_reaches_end_and_raises_pend`: short playFAD..playendFAD range; drive `exec`
      enough 13 333 µs ticks; assert `PEND` raised, `status == PAUSE`, `options == 0x8`.
- [x] `cs2_play_repeats_and_increments_repcnt_up_to_0xE`.
- [x] `cs2_audio_track_bypasses_partitions`: playing over the fixture's AUDIO track produces no
      allocated blocks and `blockfreespace` stays 200, while the CDDA sink receives 2352 bytes
      per tick.

---

## Phase 6 — Filesystem commands and IP.BIN

### 6.1 ISO-9660 directory parsing (§5.5)

- [x] `DirRec` filled from a directory record: walk the *little-endian half* of each both-endian
      ISO field, handle the name-length padding byte, and detect an XA record by a trailing 14
      bytes (§5.5 — the reference calls this *"the best way I can think of"*; keep the same
      heuristic and the same honest comment).
- [x] `read_file_system`:
      - change-directory with `fid == 0xFFFFFF`: read **FAD 166** (= LBA 16, the PVD) via
        `read_unfiltered_sector`, parse the root directory record at offset `0x9C`, set
        `curdirsect = dirrec.lba`, `curdirsize = (dirrec.size / blocksectsize) - 1`,
        `curdirfidoffset = 0`;
      - change-directory with a real fid: `curdirsect = fileinfo[fid - curdirfidoffset].lba - 150`;
      - read-directory: `curdirfidoffset = fid - 2`, then skip forward to entry `fid`;
      - fill `fileinfo[0..1]` from the first two records and `fileinfo[2..255]` from the rest,
        **adding 150 to every `lba`** to convert LBA → FAD; spill into the next directory sector
        when a record length of 0 is hit and sectors remain;
      - free every intermediate sector and re-sort, so the walk leaves the partition as it found
        it.
- [x] `MAX_FILES = 256` (§9).

### 6.2 Commands

| Opcode | Name | Ref | Notes |
|---|---|---|---|
| `0x70` | Change Directory | §5.5 `:2745` | `CR3>>8` filter; `((CR3&0xFF)<<16)\|CR4` fid; `CMOK\|EFLS` |
| `0x71` | Read Directory | §5.5 `:2773` | `((CR3&0xFF)<<8)\|CR4` fid offset — note `<<8`, not `<<16` |
| `0x72` | Get File System Scope | §5.5 `:2801` | `CR2 = numfiles - 2`, `CR3 = 0x0100`, `CR4 = 0x0002` |
| `0x73` | Get File Info | §5.5 `:2813` | `0xFFFFFF` → `infotranstype = 2`, `CR2 = 0x05F4`; else `infotranstype = 1`, `CR2 = 0x06`; `CMOK\|DRDY` |
| `0x74` | Read File | §5.5 `:2846` | sets `playFAD = FAD = fileinfo[fid].lba + offset`, `playendFAD = playFAD + ceil(size/getsectsize) - offset`, `maxrepeat = 0`, `options = 0x8`, `status = PLAY`, `playtype = FILE`, `set_timing(1)` |
| `0x75` | Abort File | §5.5 `:2877` | `status = PAUSE` unless `OPEN`/`NODISC`; `isonesectorstored = 0`; `datatranstype = INVALID`; `cdwnum = 0`; `CMOK\|EFLS` |

- [x] `setup_file_info_transfer(fid)` builds the 12-byte record read through the info port
      (§5.5): `[0..3]` lba BE, `[4..7]` size BE, `[8]` interleavegapsize, `[9]` fileunitsize,
      `[10]` fid truncated to 8 bits, `[11]` flags.
- [x] Info port modes 1 and 2 (§1.7): mode 1 = single record, declared length `0x06` words
      = 12 bytes; mode 2 = all records, declared length `0x05F4` words = 1524, refilled from
      `fileinfo[2 + transfercount/12]` every 12 bytes. Both use `>=` terminators, not the
      reference's `>` (§10 [BUG 19]).
- [x] **Resolve §10 [BUG 37] with evidence, don't guess.** `0x71`/`0x74` build the file ID as
      `((CR3&0xFF)<<8)|CR4` while `0x70`/`0x73` use `((CR3&0xFF)<<16)|CR4`. At most one matches
      hardware. Pick `<<16` (consistent with the majority and with a 24-bit fid field), record
      the choice as a deviation, and re-verify against a boot/game trace before trusting it.
      Also bound-check `fileinfo[fid]` — the reference indexes a 256-entry array with a value up
      to `0xFFFF` and skips the `curdirfidoffset` subtraction that `read_file_system` applies.

### 6.3 IP.BIN (§6.7)

- [x] `get_ip(autoregion)`: force `outconcddev = filter[0]`, read **FAD 150** (LBA 0)
      unfiltered, and if the sector begins with `"SEGA SEGASATURN"` fill the header struct from
      §6.7's offset table: `system @0x00 (16)`, `company @0x10 (16)`, `itemnum @0x20 (10)`,
      `version @0x2A (6)`, `date @0x30..0x37` (reformatted `DD/MM/YYYY` from `buf[0x34..0x37]` +
      `buf[0x30..0x33]`), `cdinfo @0x38 (8)`, `region @0x40 (10)`,
      `peripheral @0x50 (16)`, `gamename @0x60 (112)`, `ipsize @0xE0 u32 BE`,
      `msh2stack @0xE8`, `ssh2stack @0xEC`, `firstprogaddr @0xF0`, `firstprogsize @0xF4`.
- [x] Region autodetect from `region[0]`: `J`→1, `T`→2, `U`→4, `B`→5, `K`→6, `A`→0xA, `E`→0xC,
      `L`→0xD, else 0. **Cross-plan dependency**: the reference calls `SmpcRecheckRegion()` on a
      disc change; coordinate with `docs/implementation-plans/smpc-peripheral.md`, since Mimas's
      SMPC logic currently lives inline in `sh2.rs`.
- [x] Free the sector's block and re-sort the partition before returning.
- [x] Do **not** port §10 [HACK 36] (the Panzer Dragoon Zwei stack-pointer rewrites, applied only
      in the little-endian branch) without a specific reproducible failure justifying it.

### 6.4 Tests for Phase 6

- [x] `cs2_ip_bin_parse_against_fixture`: the fixture's LBA 0 carries a hand-written
      `"SEGA SEGASATURN"` header with known company/itemnum/date/region/gamename and known
      `firstprogaddr`/`firstprogsize`. Assert every field against the generator's own inputs.
- [x] `cs2_region_autodetect_table`: all eight letters plus one unknown → 0.
- [x] `cs2_change_directory_root_reads_fad_166`: assert the backend was asked for FAD 166
      exactly, and that `curdirsect` matches the fixture's root-directory LBA.
- [x] `cs2_read_directory_converts_lba_to_fad`: every `fileinfo[i].lba` is the ISO record's LBA
      **plus 150** (§5.5, §9).
- [x] `cs2_get_file_info_single_record_layout`: read 6 words from the info port and reassemble
      the 12 bytes; assert lba/size big-endian against the fixture, and that the 7th read
      terminates (`infotranstype == -1`) rather than returning `transfileinfo[12..13]` — the
      regression test for §10 [BUG 19].
- [x] `cs2_filesystem_walk_leaves_partition_unchanged`: `blockfreespace == 200` before and after
      a `0x70` + `0x71` sequence (§5.5).

---

## Phase 7 — Remaining commands: MPEG stubs, FAD search, MPEG ROM

Low priority — none of this is on the BIOS boot path, and §10 [QUIRK 34] notes every MPEG field
is written and never read. Implement so unimplemented opcodes stop hanging the guest (§10
[QUIRK 6]), not because the functionality matters.

- [x] `doMPEGReport` (§2.2): `CR1 = (status<<8)|actionstatus`, `CR2 = vcounter`,
      `CR3 = (pictureinfo<<8)|mpegaudiostatus`, `CR4 = mpegvideostatus`. All four MPEG state
      variables are zero-initialised and never written, so the report is always
      `status<<8 / 0 / 0 / 0`.
- [x] `0x90` MPEG Get Status — `mpeg`; `CMOK|MPCM` (§5.6 `:2890`)
- [x] `0x91` MPEG Get Interrupt — `CR1 = (status<<8)|((int>>16)&0xFF)`, `CR2 = int & 0xFFFF`;
      `int` hardcoded 0 then ANDed with `mpegintmask`; `CMOK|MPCM` (`:2897`)
- [x] `0x92` MPEG Set Interrupt Mask — `((CR1&0xFF)<<16)|CR2`; `CMOK|MPCM` (`:2916`)
- [x] `0x93` MPEG Init — `CR1 = mpgauth ? status<<8 : 0xFF00`; `CR2==1` →
      `CMOK|MPCM|MPED|MPST`, else `CMOK|MPED|MPST` (`:2926`)
- [x] `0x94` MPEG Set Mode — `CR1&0xFF` vidplaymode, `CR2>>8` dectimingmode, `CR2&0xFF` outmode,
      `CR3>>8` slmode; `0xFF` means "leave alone" (`:2948`)
- [x] `0x95` MPEG Play (`:2972`), `0x96` MPEG Set Decoding Method (`:2981`) — report-only stubs
- [x] `0x9A` MPEG Set Connection / `0x9B` Get Connection — `CR3>>8` selects current/next;
      `CR1&0xFF` audcon, `CR2>>8` audlay, `CR2&0xFF` audbufnum, `CR3&0xFF` vidcon, `CR4>>8`
      vidlay, `CR4&0xFF` vidbufnum (`:2990`, `:3020`)
- [x] `0x9D` MPEG Set Stream / `0x9E` Get Stream — same shape with
      `audstm/audstmid/audchannum/vidstm/vidstmid/vidchannum` (`:3045`, `:3075`)
- [x] `0xA0`-`0xA4` MPEG Display / Set Window / Set Border Color / Set Fade / Set Video Effects —
      report-only stubs (`:3100`-`:3137`)
- [x] `0xAF` MPEG Set LSI — no CR update, `CMOK|MPCM` (`:3146`)
- [x] `0x55` Exec FAD Search / `0x56` Get FAD Search Results — §10 [QUIRK 22], pure stubs marked
      *"finish me"*; implement as stubs that at least raise `CMOK` so the guest doesn't hang
      (§5.4 `:2390`, `:2398`)
- [x] `0xE2` Get MPEG ROM (§5.7 `:3217`) — **do not port as written**. §10 [QUIRK 33]: the
      reference opens an arbitrary **host file** at `Cs2Area->mpegpath` and streams it into
      partition buffers. Reject the command (`report(REJECT)`, `CMOK|MPED`) unless and until a
      real MPEG cart is modelled.
- [x] Opcodes named only in `cs2.h` and absent from the dispatch switch (§5.6 last paragraph):
      `0x97` Out Decoding Sync, `0x98` Get Timecode, `0x99` Get Pts, `0x9C` Change Connection,
      `0x9F` Get Picture Size, `0xA5`-`0xAA` Get/Set/Read/Write Image and Read/Write Sector,
      `0xAE` Get LSI. Leave unimplemented (they hang, matching §10 [QUIRK 6]), but log the opcode
      by name so a trace identifies them instantly.
- [x] Info port modes 3 and 4 (§1.7) if not already done in Phase 5: Q = 5 words / 10 bytes,
      RW = 12 words / 24 bytes, both with `>=` terminators.

### Tests for Phase 7

- [x] `cs2_mpeg_commands_all_complete_with_cmok`: loop over every opcode in the table above,
      issue it, assert `CMOK` is raised and `CR1 >> 8 == status`. Guards against a guest hanging
      on a stub.
- [x] `cs2_get_mpeg_rom_is_rejected`: `CR1 >> 8 == 0xFF` — the deliberate deviation from
      §10 [QUIRK 33].
- [x] `cs2_named_but_undispatched_opcodes_do_not_complete`: `0x97`, `0x9C`, `0xA5` leave CR1-CR4
      untouched (§10 [QUIRK 6]).

---

## Architectural call-outs

### A. Sector data reaches Work RAM through SCU DMA — state it, don't assume a memcpy

Real hardware has **no** path from a CD sector buffer to Work RAM other than the CPU reading
`0x25818000` word by word, or a DMA controller doing the same reads on its behalf. §4.5's flow
diagram ends at *"guest reads `0x05818000` (32-bit) repeatedly, **or SCU-DMAs from it**"*. There
is no "write the sector into RAM" command anywhere in the §5.8 opcode index.

The CD block must therefore expose exactly one egress: a 32-bit FIFO read that advances transfer
state. `Cs2` must **never** write into `WorkRam` itself.

`Sh2::execute_scu_dma` (`sh2.rs:1536-1639`) cannot drive that FIFO correctly today. Three defects
must be fixed — all of them properly belong to `docs/implementation-plans/scu.md`, listed here so
the dependency is explicit and neither plan assumes the other handled it:

1. **Byte-granular reads.** The engine reads via `raw_read_byte(src)` (`sh2.rs:1610-1611`,
   `:1622`). Byte access to CS2 returns `0xFF` (§1.1) and, worse, would advance the FIFO by four
   separate transactions. The DMA engine needs a 32-bit read path when the source resolves to
   `MemRegion::Cs2Regs`.
2. **Unconditional source increment.** `src += 2` / `src += 1` regardless of `DnAD`'s read-add
   field (`sh2.rs:1614`, `:1624`). A FIFO port needs the source address held. Note
   `hardware-reference/scu.md` §1.2's warning that `ReadAdd == 0` does not mean "hold" in the
   ordinary sense — it switches the engine into *fill mode*; the correct hold behavior for a
   memory-mapped FIFO comes from the port itself not advancing, not from the address arithmetic.
3. **Wrong indirect-mode bit.** `sh2.rs:1546` tests `mode & 0x01_0000` (bit 16);
   `hardware-reference/scu.md` §1.2 puts indirect mode at **bit 24** (`0x0100_0000`). Indirect
   descriptor lists are how games queue multi-sector CD transfers, so this must be right before
   Phase 4's DMA test can pass.

Also note the interrupt side: §4.1 of `hardware-reference/scu.md` gives Level 0/1/2 DMA End
vectors `0x4B`/`0x4A`/`0x49`. Mimas raises none of them today; a game waiting on DMA-end after a
CD read will hang even with a perfect CD block.

### B. Core 7's loop — parked, but not *only* command-driven

Core 7 (`lib.rs:347-360`) is one of the five cores that never park (CLAUDE.md, "Known
architecture debt"). Only Core 1 and Core 6 use `park_while_inactive` (`sync.rs:147-156`).

**Phase 2 shape** — pure command-driven, identical to Core 6's SCU DSP pattern
(`lib.rs:320-341`):

```
set_thread_active(7, false)
loop {
    park_while_inactive(7)            // 0% CPU
    while cs2.has_work() {
        cs2.exec(elapsed_us);
        sync_core(7, cycles);
    }
    set_thread_active(7, false)
}
```

Woken by `set_thread_active(7, true)` from Core 0's CR4-write handler — the same call
`write_scu_dsp_port` already makes for the DSP's `EX` bit (`sh2.rs:732-736`).

**Phase 3 breaks that**, and the plan must say so up front rather than discovering it mid-phase.
`Cs2Exec` has *three* timers (§7), only one of which is command-driven:

| Timer | Period | Command-driven? |
|---|---|---|
| `_commandtiming` | 50 / 250 µs after a CR4 write | yes |
| `_statuscycles` | 333 333 µs (3 Hz backend poll, §3.2) | **no** — free-running |
| `_periodiccycles` | 16 667 µs idle / 13 333 µs 1× / 6 667 µs 2× (§3.3) | **no** — free-running |

A drive in `PAUSE` with no command outstanding still issues a `doCDReport` + `SCDQ` every
16.7 ms, and still polls the backend every 333 ms. So:

- [x] Add `LockStepSync::park_until(core_id, deadline) -> ParkResult` — a `Condvar::wait_timeout`
      sibling of `park_while_inactive` on the same `park_condvar`. It must return distinguishable
      "reactivated" / "timed out" / "shutdown" outcomes. There is no timeout variant today.
- [x] Core 7 computes its next deadline as `min(next_status_poll, next_periodic_tick)` and parks
      until then, waking early if a CR4 write reactivates it. This is a real timed wait, not a
      spin — it keeps the "park when idle" property while honoring free-running hardware timers.
- [x] Feed `exec` **wall-clock elapsed microseconds**, not `LockStepSync` cycles. §0.1 of the
      reference establishes that `Cs2Exec`'s `timing` argument is elapsed µs; Mimas's cycle
      counters are per-core abstractions with no fixed µs relationship. Use `Instant` deltas,
      matching how `ClockThrottle` (`throttle.rs`) and Core 3's frame pacing
      (`lib.rs:207-208`, 16 666 µs) already work.
- [x] While `status` is `PLAY`/`SEEK`, Core 7 is genuinely busy at 75-150 Hz. That is real work,
      exempt from the "zero-polling" rule the same way Core 6's DSP stepping is.
- [x] Core 7 is named `smpc-cd-block`. This plan gives it CD-block work only; SMPC command
      handling stays inline in `sh2.rs` until `docs/implementation-plans/smpc-peripheral.md`
      moves it. Note that whichever plan lands second inherits a thread that is already doing
      timed work, so the deadline computation above must be written to accommodate a second
      timer source.

### C. Ownership and lock ordering

- `Cs2` lives behind `Arc<Mutex<Cs2>>` on `SaturnSystem`, shared between Core 0 (register access,
  FIFO reads, SCU DMA reads) and Core 7 (the `exec` state machine) — the exact `scu_dsp` shape
  (`lib.rs:73`).
- **Never acquire a `WorkRam` region lock while holding the `Cs2` mutex.** `WorkRam`'s per-region
  locks have a documented ordering rule (`shared_buffers.rs:17-24`: acquire in field-declaration
  order); `Cs2`'s mutex is outside that hierarchy. The SCU DMA path naturally satisfies this —
  it reads 4 bytes from the FIFO (taking and releasing the `Cs2` lock), then writes to Work RAM
  (taking that region's lock) — as long as the two are never nested. Write it that way and say so
  in the code.
- `Cs2` owns its own 200 sector blocks; nothing about them belongs in `WorkRam`.
- Hold the `Cs2` lock for the shortest possible span in `Sh2`'s register accessors. Core 0 taking
  it per-register while Core 7 holds it for an entire `exec` tick is the obvious contention
  point; if it shows up in telemetry, split `exec`'s long-running work (sector reads) outside the
  lock rather than reaching for a different primitive.

### D. Interrupt delivery — the A-bus external interrupt

§1.3: `Cs2SetIRQ` calls `ScuSendExternalInterrupt00()` whenever `HIRQ & HIRQMASK != 0`.
`hardware-reference/scu.md` §4.1 gives that source **vector `0x50`, level `7`**, `IMS` mask bit
15 (`0x8000`, shared by all 16 external interrupts), `IST` bit 16 (`0x00010000`).

Mimas has no SCU interrupt controller — no `IMS`, no `IST`, no `AIACK`. Interrupts are plain
flags on `Sh2`, dispatched by `service_pending_interrupt` (`sh2.rs:908-951`) against per-source
level/vector constants (`sh2.rs:193-222`).

- [x] Add `const ABUS_IRQ_LEVEL: u32 = 7; const ABUS_IRQ_VECTOR: u32 = 0x50;` alongside the
      existing four.
- [x] Add `Sh2.abus_irq_pending: Option<Arc<AtomicBool>>`, following `sound_req_irq`
      (`sh2.rs:139-148`) exactly, **including its Release/Acquire ordering contract**
      (`sh2.rs:96-108`): Core 7 stores with `Release` after the register writes that produced the
      response; Core 0 loads with `Acquire` before reading CR1-CR4. `Relaxed` was already
      measured as a real bug once in this codebase for the M68K handshake; do not repeat it.
- [x] Insert it into `service_pending_interrupt`'s priority chain at level 7 — below Sound
      Request (9) and SMPC (8), above nothing currently modelled.
- [x] **Known gap, record it**: `hardware-reference/scu.md` §4.5 / §10 item 16 — external
      interrupts are *dropped, not queued*, when `AIACK` (SCU register `0xA8`) is 0, and `AIACK`
      is a one-shot gate cleared on each delivery. Mimas models neither `AIACK` nor `IMS`, so
      Phase 2 delivers every CD interrupt unconditionally. That is more permissive than hardware
      and could plausibly cause spurious re-entry in a BIOS handler. Flag it in
      `.development/current_bugs.md` and cross-link `docs/implementation-plans/scu.md`.
- [x] The interrupt goes to the **master SH-2 only** (`SendInterrupt` targets `MSH2`), even
      though the slave also sees the CS2 window.

### E. Access-width decode must live above `raw_read_byte`

`raw_read_byte` (`sh2.rs:396-403`) takes `&self` and is called 2× by `read_word` and 4× by
`read_long`. Every CS2 port with a read side effect — CR4 (clears `command_pending`), the info
port (advances `transfercount`), the data FIFO (advances `datatransoffset`) — would fire that
side effect once per byte.

The precedent is already in the tree: the SCU DSP's four 32-bit-only ports are intercepted at
`read_long`/`write_long` (`sh2.rs:645-649`, `:670-698` → `:718-744`) with the doc comment
explaining exactly this reasoning (`sh2.rs:709-717`). CS2 is the second such register group, and
the first needing it at **word** width too — `read_word`/`write_word` (`sh2.rs:613-636`) have no
interception hook at all today and need one added.

### F. Memory footprint

200 blocks × 2352 bytes ≈ 470 KB, plus the 2448-byte workblock, plus 24 partitions holding index
lists rather than the reference's 24 × 200 pointer arrays. Allocate the block array boxed at
`Cs2::new` (matching `WorkRam`'s `vec![...].into_boxed_slice().try_into()` idiom,
`shared_buffers.rs:66-84`) rather than inline in the struct, so `Cs2` stays cheap to move. The
R36S target this project aims at (see `README.md`) has memory to spare for 470 KB, but not for a
naive 24 × 200 × 2352 partition model.

---

## Test fixtures and conventions

Existing precedent: `saturn-core` unit tests live in the module's own `#[cfg(test)] mod tests`;
cross-crate tests live in `e2e-tests/src/lib.rs` with `create_temp_file`/`delete_temp_file`
helpers (`e2e-tests/src/lib.rs:293-302`); `milestone-tests/` is a separate workspace requiring a
real `MIMAS_BIOS_PATH` and a downloaded CLIP model, deliberately excluded from
`cargo test --workspace` (CLAUDE.md). `milestone-tests/fixtures/` currently holds exactly one
file, `cd_player_screen.jpg` — the BIOS CD-player screen, i.e. the visual milestone this entire
plan unlocks.

`cargo test --workspace` must stay fast, deterministic and offline, so the CD fixtures must be
tiny and committed.

- [x] **`tools/make_test_chd.py`** (new, alongside `tools/sh2dis.py`): generates deterministic
      CHD fixtures from an explicit track table. Committed so the expected values in every test
      are traceable to the generator's inputs, not to emulator output. It must emit, alongside
      each `.chd`, a `.expected.json` recording the track table, the derived 102-entry TOC, and
      the payload bytes — so a test asserts against numbers the generator computed, satisfying
      CLAUDE.md's "independently-derived values" rule.
- [x] **Fixture 1 — `single_data_track.chd`**: one `MODE1/2048` track, ~16 sectors.
      LBA 0 (FAD 150) carries a hand-written IP.BIN header beginning `"SEGA SEGASATURN"` with
      known company/date/region/gamename. LBA 16 (FAD 166) carries a minimal ISO-9660 PVD with a
      root directory record at offset `0x9C`. Remaining sectors carry a known LCG byte pattern.
      Derived TOC, by hand from §6.3:
      `TOC[0] = (0x41 << 24) | 150 = 0x41000096`;
      `TOC[1..=98] = 0xFFFFFFFF`;
      `TOC[99]  = (0x41000096 & 0xFF000000) | 0x010000 = 0x41010000`;
      `TOC[100] = (0x41000096 & 0xFF000000) | (1 << 16) = 0x41010000`;
      `TOC[101] = 0x41000000 | lead_out_fad`.
- [x] **Fixture 2 — `data_plus_audio.chd`**: track 1 `MODE1/2048` (`ctl_addr 0x41`), track 2
      `AUDIO` (`ctl_addr 0x01`) with a known sine or ramp payload. Exercises `fad_to_track`
      boundaries, `TOC[100]`'s last-track number, `ctl_addr` in `doCDReport`'s CR2, and the CDDA
      bypass. Deliberately covers §10 [BUG 54]'s "last track of a CHD" case, which the reference
      gets wrong.
- [x] **Fixture 3 — `mode2_form1.chd`**: a `MODE2_RAW`/2352 track with real subheaders
      (`data[0x10..0x13]` = fn/cn/sm/ci), so the filter subheader conditions (§4.3 mode bits 0-3)
      and the form-2 detection (`data[0x12] & 0x20`, §9) can be tested against known values.
- [x] Keep every fixture under ~100 KB. CHD compresses zeroed sectors extremely well; 16-32
      sectors is enough for every test above.
- [x] Store them in `e2e-tests/fixtures/` (new directory) and reference by a path relative to
      `CARGO_MANIFEST_DIR`, not a `/tmp` copy.
- [x] Rule for every phase: **derive expected values from the reference's formulas in a
      throwaway script, then paste them as literals.** Never assert a value read out of the
      emulator. CLAUDE.md names two past regressions caused by breaking this rule
      (`bt_bf_no_delay_slot`, the first `DIV1` test).

---

## Deliberate deviations from the reference — one place, kept current

The reference documents 66 known bugs/quirks/hacks in Yabause (§10). Mimas ports *hardware
behavior*, not Yabause's defects. Every intentional divergence lands here; every one is a
candidate to be reverted if a real BIOS or game trace shows software depending on the buggy
behavior.

| # | Reference item | Mimas's choice |
|---|---|---|
| 1 | §10 [BUG 5] `0x00` Get Status raises no SCU interrupt | Raise it. Flag in `current_bugs.md` |
| 2 | §10 [BUG 9] `0x31` misrouted to the *setter* | Implement the real getter |
| 3 | §10 [BUG 10]/[BUG 11]/[BUG 12] filter and partition indices unchecked / checked against `0x24` | Bound-check against 24, reject otherwise |
| 4 | §10 [BUG 13] `CalculateActualSize` doesn't advance its loop index | Compute the real sum |
| 5 | §10 [BUG 14] copy/move mask count with `0xFF` then test `0xFFFF`; hardcode 2352 | Honor `getsectsize`/`putsectsize`, make "to end of partition" reachable |
| 6 | §10 [BUG 15] `PutSectorData` zeroes `partition.size` before appending | Don't |
| 7 | §10 [QUIRK 16] put-path `(putsectsize - getsectsize) / 24` offset discard | Plain offset |
| 8 | §10 [BUG 17] get/put use inconsistent block indexing | One consistent indexing rule |
| 9 | §10 [BUG 18] FIFO read dereferences before the NULL guard | Guard first (unrepresentable in safe Rust anyway) |
| 10 | §10 [BUG 19] all five info-port terminators use `>` not `>=` | Use `>=`; no out-of-bounds word |
| 11 | §10 [BUG 20] subcode Q indexes `TOC[track-1]` unchecked; RW's unbounded `group` | Bound both |
| 12 | §10 [BUG 24] seek-by-FAD scans only `TOC[0..15]`, passes `i` not `i+1`, picks the next track | Scan all tracks, pass `i+1`, pick the containing track |
| 13 | §10 [BUG 26] play end-position truncates to 16 bits and ORs `0x63` | Replace the index byte, keep 24 bits |
| 14 | §10 [BUG 37] `0x71`/`0x74` fid `<<8` vs `0x70`/`0x73` fid `<<16` | Use `<<16` everywhere; re-verify against a trace |
| 15 | §10 [BUG 39] `sort_blocks` desyncs the parallel `blocknum[]` | Single `Vec<u8>` of indices |
| 16 | §10 [BUG 40] unfiltered read leaks a block on read failure | Allocate after a successful read |
| 17 | §10 [QUIRK 43] ResetSelector bit 2 doesn't restore `blockfreespace` | Restore it |
| 18 | §10 [QUIRK 47] read errors silently swallowed; playback stalls forever | Set `CDB_STAT_ERROR` and log |
| 19 | §10 [QUIRK 33] `0xE2` reads an arbitrary host file | Reject the command |
| 20 | §10 [HACK 36] Panzer Dragoon Zwei stack-pointer rewrites | Not ported without a reproducible failure |
| 21 | §10 [QUIRK 55] 2048-byte tracks leave the mode byte zero | Synthesize MIN/SEC/FRAME/mode |
| 22 | §10 [BUG 52]/[53]/[54] `cdbase.c` track lookup: stale `currentTrack`, unconditional success, last track unreachable | Correct lookup; `Err` on a FAD outside every track |
| 23 | §10 [BUG 60]/[61]/[62] loader off-by-ones (BinCue/CCD/MDS) | N/A — Mimas is CHD-only |
| 24 | §10 [DEAD 44]/[45]/[49]/[50] `RapidCopy*`, `GetTimeToNextSector`, `Cs2Command`, `SetDelayIRQ` | Not implemented |
| 25 | §10 [BUG 51] `Cs2Exec` early-return skips the modem exec | N/A — no modem |

**Faithfully reproduced despite looking wrong** (do not "fix" these without evidence):
§10 [QUIRK 1] byte accesses return `0xFF`; [QUIRK 2] width asymmetry; [DEAD 3] HIRQ read does not
derive `BFUL`/`DCHG`/`CSCT`; [QUIRK 4] `CMOK` never auto-cleared; [QUIRK 6] unimplemented opcodes
hang; [QUIRK 21] scan is a one-way door; [QUIRK 23] `0x05` OpenTray undone by the backend poll;
[QUIRK 25] the fake seek delay's unit mismatch; [QUIRK 27] play modes parsed but unimplemented;
[QUIRK 29] init flags 1-3 are no-ops and flag 0 does not clear the TOC; [QUIRK 30] hardcoded
"MPEG card exists"; [QUIRK 31] single-session hardcode; [QUIRK 32] authentication always
succeeds; [QUIRK 38] `get_partition` evaluates no filter conditions; [QUIRK 41] filter mode bit 5
undecoded; [QUIRK 42] `SetFilterMode`'s init bit sets `range = 0`; [QUIRK 44] buffer-full
back-pressure; [QUIRK 46] exact-zero `BFUL` transitions; [HACK 35] the "Assault Leynos 2" `EHST`.

---

## Appendix — coverage matrices

### Opcode coverage (all 64 dispatched in the reference, §5.8)

| Phase | Opcodes |
|---|---|
| 2 | `0x00` `0x01` `0x02` `0x03` `0x04` `0x05` `0x06` `0xE0` `0xE1` |
| 4 | `0x30` `0x31` `0x32` `0x40` `0x41` `0x42` `0x43` `0x44` `0x45` `0x46` `0x47` `0x48` `0x50` `0x51` `0x52` `0x53` `0x54` `0x60` `0x61` `0x62` `0x63` `0x64` `0x65` `0x66` `0x67` |
| 5 | `0x10` `0x11` `0x12` `0x20` |
| 6 | `0x70` `0x71` `0x72` `0x73` `0x74` `0x75` |
| 7 | `0x55` `0x56` `0x90` `0x91` `0x92` `0x93` `0x94` `0x95` `0x96` `0x9A` `0x9B` `0x9D` `0x9E` `0xA0` `0xA1` `0xA2` `0xA3` `0xA4` `0xAF` `0xE2` |
| never | `0x07`-`0x0F`, `0x13`-`0x1F`, `0x21`-`0x2F`, `0x33`-`0x3F`, `0x49`-`0x4F`, `0x57`-`0x5F`, `0x68`-`0x6F`, `0x76`-`0x8F`, `0x97`-`0x99`, `0x9C`, `0x9F`, `0xA5`-`0xAE`, `0xB0`-`0xDF`, `0xE3`-`0xFF` — undispatched in the reference; leave hanging per §10 [QUIRK 6], but log the opcode |

### HIRQ bit coverage (§1.3)

| Bit | Name | First raised in |
|---|---|---|
| 0 `0x0001` | `CMOK` | Phase 2 — every command |
| 1 `0x0002` | `DRDY` | Phase 2 (`0x02` GetToc), Phase 4 (`0x61`/`0x63`/`0x64`), Phase 5 (`0x20`), Phase 6 (`0x73`) |
| 2 `0x0004` | `CSCT` | Phase 2 (`0xE0`), Phase 3/4 (periodic sector stored), Phase 7 (`0xE2`) |
| 3 `0x0008` | `BFUL` | Phase 4 (`allocate_block` exhaustion) |
| 4 `0x0010` | `PEND` | Phase 5 (end of play) |
| 5 `0x0020` | `DCHG` | Phase 2 (`0x04` with `isdiskchanged`, `0x05` OpenTray) |
| 6 `0x0040` | `ESEL` | Phase 2 (`0x04`), Phase 4 (`0x30`/`0x40`-`0x48`/`0x52`/`0x53`/`0x54`/`0x60`) |
| 7 `0x0080` | `EHST` | Phase 2 (`0x06`), Phase 4 (FIFO put completion, `0x61`/`0x62`/`0x63` errors), Phase 5 (file end-of-play) |
| 8 `0x0100` | `ECPY` | Phase 4 (`0x65`/`0x66`) |
| 9 `0x0200` | `EFLS` | Phase 2 (`0xE0`), Phase 5 (file end-of-play), Phase 6 (`0x70`-`0x72`, `0x75`) |
| 10 `0x0400` | `SCDQ` | Phase 3 (periodic report) |
| 11 `0x0800` | `MPED` | Phase 7 (`0x93`, `0xE2`) |
| 12 `0x1000` | `MPCM` | Phase 7 (every MPEG command) |
| 13 `0x2000` | `MPST` | Phase 7 (`0x93`) |
| 14-15 | undefined | never set (§1.3) |

### Register coverage (§1.2)

| Offset | Absolute (cache-through) | Register | Phase |
|---|---|---|---|
| `0x18000` | `0x25818000` | Data transfer FIFO (R32/W32 only) | 1 decode, 4 semantics |
| `0x90008` / `0x9000A` | `0x25890008` / `…0A` | `HIRQ` (R16/W16/R32) | 1 decode, 2 semantics |
| `0x9000C` / `0x9000E` | `0x2589000C` / `…0E` | `HIRQMASK` | 1 decode, 2 semantics |
| `0x90018` / `0x9001A` | `0x25890018` / `…1A` | `CR1` (write clears `PERI`, sets `command_pending`) | 1 decode, 2 semantics |
| `0x9001C` / `0x9001E` | `0x2589001C` / `…1E` | `CR2` | 1 |
| `0x90020` / `0x90022` | `0x25890020` / `…22` | `CR3` | 1 |
| `0x90024` / `0x90026` | `0x25890024` / `…26` | `CR4` (write launches, read clears `command_pending`) | 1 decode, 2 semantics |
| `0x90028` / `0x9002A` | `0x25890028` / `…2A` | `MPEGRGB` (plain scratch; nothing reads it) | 1 |
| `0x98000` | `0x25898000` | Info transfer port (R16 only, 5 modes) | 1 decode; mode 0 in 2, modes 1-2 in 6, modes 3-4 in 5/7 |
| everything else in `0x05800000`-`0x058FFFFF` | — | Undocumented: read 0, write dropped, log once | 1 |
| any byte-width access | — | Read `0xFF`, write dropped (cartridge forward) | 1 |

---

## Tracking-doc updates this plan implies

Per CLAUDE.md, these are not end-of-session chores:

- [x] `.development/current_blocker.md` — currently **0 bytes**. Populate it from §0.5's
      `[REGACCESS]` sweep before starting Phase 1, and rewrite it at each phase exit.
- [x] `.development/current_bugs.md` — currently **0 bytes**. Add: the `0x00`-Get-Status
      interrupt divergence (call-out D / deviation #1), the missing `AIACK`/`IMS` gate
      (call-out D), and the three `execute_scu_dma` defects (call-out A) with a pointer to
      `docs/implementation-plans/scu.md`.
- [x] `.development/phased_development_plan.md:115-128` — flip "Phase 6 … CD Block / CS2
      Subsystem" from ✅ Completed to ⬜, per §0.3.
- [x] `.development/ROADMAP.md` / `TASKS.md` — currently **0 bytes**; seed them with these
      phases.
- [x] `CLAUDE.md`'s "Known architecture debt" bullet on CD-ROM (line 66) and the Core 7 row of
      the thread table (line 59) — update as each phase lands.
- [x] `docs/mimas_emu_engineering_draft.md` §8 (line 318-322) — update the "Current implementation
      status" paragraph as each phase lands.
- [x] `history.md` — a chapter per phase explaining *why* each deliberate deviation in the table
      above was chosen, since the diff only shows what changed.
