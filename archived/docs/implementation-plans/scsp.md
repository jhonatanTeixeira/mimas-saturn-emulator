# SCSP — implementation plan

Diffs `saturn-core/src/scsp.rs` (+ the SCSP-facing parts of `m68k.rs`, `sh2.rs`, `lib.rs`,
`shared_buffers.rs`) against `docs/hardware-reference/scsp.md`, and lays out the ordered work to
close the gap.

Every hardware claim below is a pointer into `docs/hardware-reference/scsp.md` (cited as
"§N" — that document in turn cites `yabause/src/<file>:<line>` for each claim). Nothing here is
sourced from memory or from a Saturn data sheet. Where the reference itself cannot answer a
question, that is stated as a gap rather than guessed at (see §"Questions the reference cannot
answer").

---

## 0. Which Yabause SCSP model Mimas mirrors

`docs/hardware-reference/scsp.md` §0 establishes that Yabause contains **three** SCSP models:

| Model | Where | Selected by |
|---|---|---|
| "old" engine | `scsp.c`, `slot_t` / `scsp_update()` | `use_new_scsp == 0` — the compiled **default** |
| "new" engine | `scsp.c`, `struct Slot` / `generate_sample()` | `use_new_scsp == 1` |
| `scsp2.c` | separate file, Andrew Church rewrite | `-DUSE_SCSP2=1` at build time, not default |

**Decision: Mimas mirrors the "new" engine as its primary reference model.** Rationale, straight
from §0.1: it is the only one of the three with a sound DSP (`scspdsp.c` is dead code for the
other two), the only one implementing slot modulation (`MDL`/`MDXSL`/`MDYSL`), the only one that
writes the sound stack `$600-$67E`, the only one that applies `MVOL`, and the only one that
routes CDDA through `EXTS0/1` into the DSP rather than bolting it onto the output post-hoc. The
reference states this conclusion outright: *"it is the more complete hardware model and is the one
Mimas should mirror"* (§0.1). The old engine and `scsp2.c` substitute *"play the effect send dry"*
for the missing DSP (§4.5, deviation #59) — a hack Mimas must not inherit.

**But not slavishly.** CLAUDE.md's rule is "port *what the hardware does*, never transliterate."
The reference flags several places where the new engine is demonstrably *worse* than the other
two. In each of these, Mimas takes the corrected behaviour and records the deviation:

| Ref. deviation | New engine does | Mimas does instead | Corroborated by |
|---|---|---|---|
| #23 (§4.4) | pan magnitude used as a shift → 6 dB/step | ~3 dB/step | old engine `(d>>1)&7`; `scsp2.c`'s own comment; `AddSoundPan`'s 3 dB debug print |
| #22 (§4.1, §6.1) | `PLFOS==0` / `ALFOS==0` still modulate | treat 0 as "off" | old engine's `31` sentinel; `scsp2.c`'s `lfo_fm_shift = -1` |
| #48 (§3.3) | key-off during attack leaves attenuation as-is | convert attack→release | old engine `ecnt = SCSP_ENV_DE - ecnt`; `scsp2.c:3002` |
| #46 (§4.2) | `LPCTL==1` wrap is `sample_offset = LSA`, discarding overshoot | modulo wrap | `scsp2.c:679-681` |
| #65 (§14.4) | `if (ar < 0x10) ar = 0x10;` for Darius Gaiden, applied to every game and mutating the stored register | never | — game hack |
| #17 (§8.1) | DMA `DDIR==1` swaps `DMEA`/`DRGA` roles | `DMEA` is always the sound-RAM side | `scsp.c`'s own header comment; `scsp2.c` |
| #19 (§6.1) | `lfo_step_table[9..0xB]` breaks its own geometric progression | use the progression | the table's own pattern |
| #27 (§9.3) | asserts M68K IPL 0 for a source with no `SCILVn` bits | max-priority scan | the `#if 0`'d correct body at `scsp.c:1888-1914` |
| #26, #28, #29, #33, #37, #38 | assorted decode/masking defects | fixed | — |

Everything else follows the new engine. Where the new engine's behaviour is *unknown* to be right
or wrong (e.g. `MIXS` even-word readback, #32; `DDIR` polarity, #18), Mimas follows it and marks
the site with a comment naming the reference deviation number.

---

## 1. Current-state assessment

### 1.1 What exists and is live

| Piece | File | Status |
|---|---|---|
| `Scsp::synthesize` | `scsp.rs:37-107` | **Live** — called from Core 5 (`scsp-synth`) every loop iteration as `scsp_c5.lock().unwrap().synthesize(&work_ram_c5, 128)` (`lib.rs:306`) |
| 32 `VoiceState` slots | `scsp.rs:8`, `11-20` | Present: `active`, `sample_addr`, `current_offset`, `loop_start`, `loop_end`, `step`, `volume` |
| `scsp_regs` backing store | `shared_buffers.rs:32` | `RwLock<Box<[u8; 0x1000]>>` — **size is correct** (§0.3: the block is 12 bits wide, `a &= 0xFFF` at every entry point) |
| `sound_ram` backing store | `shared_buffers.rs:29` | `RwLock<Box<[u8; 0x80000]>>` — **size is correct** (§10.1: 512 KB) |
| M68K memory map | `m68k.rs:95-147` | Sound RAM `0x000000-0x07FFFF`, dead hole `0x080000-0x0FFFFF`, registers `>= 0x100000` mirrored every `0x1000`. **Matches §10.1 exactly** |
| M68K `MCIEB`/`MCIPD` handshake | `m68k.rs:43-44`, `108-147` | Correct offsets, correct enable-gating, correct single-held-lock atomicity. See §1.4 |
| SMPC `SNDON`/`SNDOFF` gating | `sh2.rs:56-57`, `839-855`; `lib.rs:264-284` | Live: sets `m68k_control`; Core 4 resets/stops the M68K on the edge |
| SH-2 Sound Request IRQ | `sh2.rs:222`, `908-940` | Live: vector `0x46`, level 9, priority-ordered against VBLANK-IN/OUT and SMPC |

### 1.2 The byte-layout derivation everything below depends on

`scsp_regs` is a **flat, byte-addressed, big-endian** array. Derivation (not assumed):

- `Sh2::raw_write_byte`'s `ScspRegs` arm (`sh2.rs:522-526`) stores one byte at `off & 0xFFF`; the
  SH-2 is big-endian and word writes decompose hi-byte-first.
- `M68k::write_word` (`m68k.rs:153-156`) writes `(val >> 8)` at `addr` and `val` at `addr+1`.
- `M68k::write_byte` reads `MCIEB` back as `u16::from_be_bytes([ram[0x2A], ram[0x2B]])`
  (`m68k.rs:132-135`) — i.e. the existing, working code already treats the array as big-endian.

Therefore, for register word `$XX`: **byte `$XX` holds word bits 15:8, byte `$XX+1` holds word
bits 7:0.** Slot `n`'s base is `n * 0x20` (§3: 32 slots × `$20` bytes, slot number =
`(addr >> 5) & 0x1F`).

Full per-slot byte map, mechanically derived from §3.1's word-bit table:

| Byte | Byte bits | Field | Word bits (§3.1) |
|---|---|---|---|
| `+0x00` | 4 | `KYONEX` | `$00` 12 |
| `+0x00` | 3 | `KYONB` | `$00` 11 |
| `+0x00` | 2:1 | `SBCTL` | `$00` 10:9 |
| `+0x00` | 0 | `SSCTL[1]` | `$00` 8 |
| `+0x01` | 7 | `SSCTL[0]` | `$00` 7 |
| `+0x01` | 6:5 | `LPCTL` | `$00` 6:5 |
| `+0x01` | 4 | `PCM8B` | `$00` 4 |
| `+0x01` | 3:0 | `SA[19:16]` | `$00` 3:0 |
| `+0x02..+0x03` | — | `SA[15:0]` | `$02` 15:0 |
| `+0x04..+0x05` | — | `LSA` | `$04` 15:0 |
| `+0x06..+0x07` | — | `LEA` | `$06` 15:0 |
| `+0x08` | 7:3 | `D2R` | `$08` 15:11 |
| `+0x08` | 2:0 | `D1R[4:2]` | `$08` 10:8 |
| `+0x09` | 7:6 | `D1R[1:0]` | `$08` 7:6 |
| `+0x09` | 5 | `EGHOLD` | `$08` 5 |
| `+0x09` | 4:0 | `AR` | `$08` 4:0 |
| `+0x0A` | 6 | `LPSLNK` | `$0A` 14 |
| `+0x0A` | 5:2 | `KRS` | `$0A` 13:10 |
| `+0x0A` | 1:0 | `DL[4:3]` | `$0A` 9:8 |
| `+0x0B` | 7:5 | `DL[2:0]` | `$0A` 7:5 |
| `+0x0B` | 4:0 | `RR` | `$0A` 4:0 |
| `+0x0C` | 1 | `STWINH` | `$0C` 9 |
| `+0x0C` | 0 | `SDIR` | `$0C` 8 |
| `+0x0D` | 7:0 | `TL` | `$0C` 7:0 |
| `+0x0E` | 7:4 | `MDL` | `$0E` 15:12 |
| `+0x0E` | 3:0 | `MDXSL[5:2]` | `$0E` 11:8 |
| `+0x0F` | 7:6 | `MDXSL[1:0]` | `$0E` 7:6 |
| `+0x0F` | 5:0 | `MDYSL` | `$0E` 5:0 |
| `+0x10` | 6:3 | `OCT` (signed −8…+7) | `$10` 14:11 |
| `+0x10` | 1:0 | `FNS[9:8]` | `$10` 9:8 |
| `+0x11` | 7:0 | `FNS[7:0]` | `$10` 7:0 |
| `+0x12` | 7 | `LFORE` | `$12` 15 |
| `+0x12` | 6:2 | `LFOF` | `$12` 14:10 |
| `+0x12` | 1:0 | `PLFOWS` | `$12` 9:8 |
| `+0x13` | 7:5 | `PLFOS` | `$12` 7:5 |
| `+0x13` | 4:3 | `ALFOWS` | `$12` 4:3 |
| `+0x13` | 2:0 | `ALFOS` | `$12` 2:0 |
| `+0x14` | — | *(no fields)* | `$14` 15:8 |
| `+0x15` | 6:3 | `ISEL` | `$14` 6:3 |
| `+0x15` | 2:0 | `IMXL` | `$14` 2:0 |
| `+0x16` | 7:5 | `DISDL` | `$16` 15:13 |
| `+0x16` | 4:0 | `DIPAN` | `$16` 12:8 |
| `+0x17` | 7:5 | `EFSDL` | `$16` 7:5 |
| `+0x17` | 4:0 | `EFPAN` | `$16` 4:0 |
| `+0x18..+0x1F` | — | not decoded by any engine (§3.1) | — |

### 1.3 Register-layout correctness issues in `scsp.rs` — the blocking bugs

Checked field by field against the table above. **Four of the six decodes in `synthesize` read
the wrong bytes.** Building an envelope generator on top of these would compound the error, so
Phase 1 fixes all of them before anything new is added.

| # | Code | Reads | Should read | Verdict |
|---|---|---|---|---|
| **B1** | `scsp.rs:47` `(regs[base] & 0x08) != 0` | byte `+0x00` bit 3 = word `$00` **bit 11** = `KYONB` | — | **Bit position is CORRECT. The comment is correct too.** See below. |
| **B2** | `scsp.rs:54-56` `((regs[base] & 7) << 16) \| (regs[base+1] << 8) \| regs[base+2]` | word `$00` bits 10:8 (`SBCTL`+`SSCTL[1]`) as `SA[19:16]`; word `$00` bits 7:0 as `SA[15:8]`; word `$02` bits 15:8 as `SA[7:0]` | `((regs[base+1] & 0x0F) << 16) \| (regs[base+2] << 8) \| regs[base+3]` | **WRONG** — off by one byte *and* the wrong nibble width (`& 7`, should be `& 0x0F`) |
| **B3** | `scsp.rs:60` `LSA = regs[base+4]<<8 \| regs[base+5]` | word `$04` | word `$04` | offset **correct**; *units* wrong — see B7 |
| **B4** | `scsp.rs:63` `LEA = regs[base+6]<<8 \| regs[base+7]` | word `$06` | word `$06` | offset **correct**; units wrong — see B7 |
| **B5** | `scsp.rs:67` `FNS = regs[base+10]<<8 \| regs[base+11]` | word **`$0A`** = `LPSLNK`/`KRS`/`DL`/`RR` | word **`$10`**: `FNS = ((regs[base+0x10] & 0x03) << 8) \| regs[base+0x11]`, `OCT = (regs[base+0x10] >> 3) & 0x0F` | **WRONG register entirely** — decimal `10` was used where hex `$10` was meant |
| **B6** | `scsp.rs:70` `TL = regs[base+12]` | byte `+0x0C` = `STWINH`/`SDIR`/unknown bits | byte `+0x0D` (`regs[base+0x0D]`) | **WRONG byte** — same decimal-for-hex slip (`12` vs `$0C`… the code wants word `$0C`'s *low* byte, which is `+0x0D`) |

**On B1 — the KYON bit.** The task framing for this plan asserted a contradiction between
`scsp.rs:46`'s comment ("KYON is bit 11 of the first word") and `scsp.rs:47`'s mask (`& 0x08`).
**There is no contradiction, and this plan does not propose "fixing" the mask.** Derivation:
`regs[base]` is byte `+0x00`, which by §1.2 holds word `$00` bits **15:8**; bit 3 of that byte is
therefore word bit **11**; §3.1 gives word `$00` bit 11 as `KYONB`. Comment and code agree, and
both name the right bit. Asserting otherwise, or "correcting" `0x08` to something else, would
introduce a bug rather than remove one.

The *real* key-on defect is a different one, and it is more serious than a wrong mask:

| # | Defect | Reference |
|---|---|---|
| **B1a** | The code treats `KYONB` as a **level-triggered** key-on. On real hardware `KYONB` is only *latched desired state*; the actual key-on/key-off transition happens for **all 32 slots simultaneously** when a write sets `KYONEX` (word `$00` **bit 12** = byte `+0x00` bit 4 = mask `0x10`). `KYONEX` is never stored and reads back as 0. | §3.1 (`$00` bit 12), §3.3 (`keyonex()` loops all 32 channels) |
| **B1b** | Key-off (`else` branch, `scsp.rs:73-75`) sets `voice.active = false` — instant silence. Real key-off enters the **RELEASE** envelope phase and keeps producing (decaying) output. | §3.3 (`keyoff` = `change_envelope_state(slot, RELEASE)`) |
| **B1c** | `keyon` is a **no-op unless the slot is currently in `RELEASE`** (§3.3, `scsp.c:929-994`). The code has no such guard, so a re-assert of `KYONB` restarts an already-playing note. | §3.3 |

`KYONEX` also cannot be implemented by the current architecture at all — see §1.5 and Phase 1's
architectural item.

Further per-field defects in the same function (all Phase 1 or Phase 2):

| # | Defect | Correct behaviour | Reference |
|---|---|---|---|
| **B7** | `LSA`/`LEA` compared directly against a byte offset (`scsp.rs:95-97`), and `current_offset` is added to `sample_addr` as a byte count | `SA` is a **byte** address; `LSA`/`LEA`/`sample_offset` are **sample** counts. Address = `SA + offset*2` for 16-bit, `SA + offset` for 8-bit | §4.2 (`scsp.c:620-623`) |
| **B8** | `PCM8B` (word `$00` bit 4) never read; every sample is read as one byte | `PCM8B=0` → 16-bit: `wave = read_word(addr)`; `PCM8B=1` → 8-bit: `wave = read_byte(addr) << 8` | §4.3 |
| **B9** | Sample decoded as **unsigned** centred on 128: `(byte as f32 - 128.0)/128.0` (`scsp.rs:89`) | Samples are **signed**. 8-bit samples become the *high byte* of an `s16` | §4.3 (`wave` is `u16`, `output` is `s16`, `<< 8` for 8-bit) |
| **B10** | `TL` treated as a *gain*: `level as f32 / 255.0` (`scsp.rs:71`) | `TL` is **attenuation**: 0 = loudest, 0xFF = quietest. `apply_volume`: `v = tl*4 + att; sample = (s * ((v & 0x3F) ^ 0x7F)) >> ((v >> 6) + 7)` | §5.1 |
| **B11** | Pitch: `step = fns / 1024.0` (`scsp.rs:68`), `OCT` ignored | 18-bit fixed-point phase accumulator; `phase_increment = (FNS \| 0x400) << (OCT ^ 8)`; `sample_delta = phase >> 18`, fraction retained | §4.1 |
| **B12** | Loop: single mode, only when `loop_end > loop_start` (`scsp.rs:95`) | `LPCTL` selects 4 modes: 0 off (slot dies at LEA), 1 normal, 2 reverse, 3 ping-pong | §4.2 |
| **B13** | Output is mono — the same value goes to L and R (`scsp.rs:91-92`) | `DISDL` send level (6 dB/step, `get_sdl_shift`) + `DIPAN` pan (bit 4 = which side, bits 3:0 = magnitude) | §4.4 |
| **B14** | `Scsp::volume` / `set_volume` (`scsp.rs:5`, `33-35`) is stored and **never used** | `MVOL` = `$400` bits 3:0 (byte `$401` bits 3:0); output shift is `0xF - MVOL` | §2.1, §4.4 |
| **B15** | Slot parameters are latched **once, on key-on** (`scsp.rs:51-72`) and never refreshed | Hardware reads the register file continuously; a mid-note `TL`/`FNS`/`DISDL` change takes effect immediately | §4 (the pipeline reads `slot->regs.*` every step) |
| **B16** | Bound check `base + 30 >= regs.len()` (`scsp.rs:43`) guards 31 of 32 bytes and is dead anyway (32 × 0x20 = 0x400 ≤ 0x1000) | Remove, or make it `base + 0x20 <= regs.len()` | — |
| **B17** | `sample_addr` masked `& 0x7FFFF` (`scsp.rs:57`); no mask on the *running* pointer beyond a `< sound_ram.len()` check that silently kills the voice (`scsp.rs:98-100`) | Mask the final address `& 0x7FFFF` each read (§4.3 does this unconditionally, deviation #50) | §4.3 |

### 1.4 M68K side: confirmed correct, and what's still missing

The `MCIEB`/`MCIPD` handshake in `m68k.rs:108-147` was cross-checked against Yabause before this
plan and **holds up against `hardware-reference/scsp.md`**:

- `SCSP_MCIEB_OFFSET = 0x2A`, `SCSP_MCIPD_OFFSET = 0x2C` (`m68k.rs:43-44`) match §2.1's `$42A`
  `MCIEB` / `$42C` `MCIPD` exactly (the block base `$400` is folded into the M68K's
  `addr - 0x100000` decode, so `0x42A & 0xFFF` addressed as `0x2A` within the CCR area is the
  same byte — note the code indexes `ram[0x2A]`, i.e. **offset `$02A`, not `$42A`**; see M2 below).
- Enable-gating (`if mcieb & mcipd != 0`) matches §9.4's `scsp_main_interrupt`: *"the SCSP asserts
  the SCU's Sound Request line whenever any bit is set in `MCIPD` that is also set in `MCIEB`"*.
- The single held write-guard spanning the store and the read-back (`m68k.rs:114-125`) is the
  right call and has no Yabause equivalent to check against — it is Mimas-specific correctness.
- There is **no** `check_main_interrupt` method; the logic is inline in `write_byte`. Extracting it
  is optional.

Missing / wrong on the M68K side, per the reference:

| # | Gap | Reference |
|---|---|---|
| **M1** | Interrupts only fire on a **write to `MCIPD`**. Real hardware raises them from *sources* — timers A/B/C (bits 6/7/8), DMA-complete (bit 4), once-per-sample (bit 10), MIDI (bits 3, 9) — none of which exist | §9.1, §9.2 |
| **M2** | The handler indexes the CCR at `0x2A`/`0x2C`, but the CCR block starts at `$400`. A driver writing the documented `0x10042A` lands at array offset `0x42A` and is **not** seen; only a write to `0x10002A` triggers the handshake. The register block is mirrored every `0x1000` from the M68K (§10.1), so `0x10002A` is *not* a mirror of `0x10042A` — these are different registers | §0.3, §2.1, §10.1 |
| **M3** | `MCIRE` (`$42E`, write-1-to-clear) unimplemented — nothing ever clears `MCIPD` | §2.1, §9.2 |
| **M4** | `MCIPD` accepts arbitrary writes. §2.1: *"R; W only bit 5"* — only bit 5 (`0x20`, the manual interrupt) is writable | §2.1 |
| **M5** | No sound-CPU interrupt path at all: `SCIEB` `$41E`, `SCIPD` `$420`, `SCIRE` `$422`, `SCILV0/1/2` `$424/$426/$428`. The M68K has no IRQ input — `SR_IMASK_SHIFT` is written at reset (`m68k.rs:73`) and never consulted; there is no `SetIRQ`, no autovector exception entry, no `STOP` | §9.2, §9.3 |
| **M6** | High-byte writes to `MCIEB` don't rescan for already-pending interrupts. Note §9.4 deviation #28 says Yabause's *byte* path has this bug and its *word* path doesn't — Mimas should do the rescan on **both** halves (as `scsp2.c` does) | §9.4 |
| **M7** | `MEM4MB` (`$400` bit 9) unimplemented. The M68K *data* path ignoring it actually matches `scsp.c` (§10.1, deviation #39), but the SH-2 side does not — see M8 | §2.2, §10.1 |
| **M8** | SH-2 sound-RAM window (`sh2.rs:358-359`, `421-424`, `517-521`) maps `0x05A00000-0x05AFFFFF` and masks `& 0x7FFFF` — mirroring every 512 KB unconditionally. §10.2: mask `& 0xFFFFF`, then if `MEM4MB == 0` mask `& 0x3FFFF` (mirror every 256 KB), else return all-ones / discard above `0x7FFFF` | §10.2 |
| **M9** | `M68K->WriteNotify` equivalent: not needed (Mimas has no M68K recompiler cache). Record as N/A, don't port | §8.2, §10.2 |
| **M10** | The dummy-M68K sound-RAM-zeroing hack (`0x700`-`0x770`, `0x790`, `0x792`) is a Yabause workaround for having no M68K core. Mimas has a real one — **do not port** | §0.5, deviation #67 |

The SH-2-side SCSP register window (`sh2.rs:360-361`, `& 0xFFF`) mirrors every 4 KB across
`0x05B00000-0x05BFFFFF` — **correct** per §1.1/§0.3.

### 1.5 Architectural gaps

| # | Gap | Detail |
|---|---|---|
| **A1** | **No sample clock.** Core 5 (`lib.rs:298-313`) calls `synthesize(&work_ram, 128)` in a bare `loop { … thread::yield_now() }` with no `ClockThrottle`. The SCSP therefore runs at *host* speed, unrelated to 44.1 kHz. Anything defined "per output sample" (timers, interrupt bit 10, the DSP's `MDEC_CT`, every envelope rate) would tick at an arbitrary rate | §11: 256 M68K cycles = 1 sample; `throttle.rs:27` already defines `M68K_CLOCK_HZ = 44_100.0 * 256.0`, so the anchor exists |
| **A2** | **`KYONEX` is an edge, and the register block is polled.** `KYONEX` is never stored (§3.2) and is a pulse. A poller reading a shadow array can neither see it nor know how many times it fired. Same problem for `$416` `DEXE` (DMA start), `$422`/`$42E` (`SCIRE`/`MCIRE`, write-1-to-clear), `$406` (`MOBUF` push), and `MPRO` byte writes (the `updated` flag) | §3.3, §8.1, §9.2, §7.1 |
| **A3** | **Audio sink is a dead end.** `audio_tx`/`audio_rx` (`scsp.rs:6-7`, `bounded(44100*2)`) has no consumer anywhere in the workspace; `try_send` silently drops once full (`scsp.rs:104-105`). `SoundRingBuffer` (`scsp.rs:116-126`) is constructed only by one e2e test | — |
| **A4** | **Lock-order violation.** `synthesize` takes `scsp_regs.read()` then `sound_ram.read()` (`scsp.rs:38-39`) — the **reverse** of `shared_buffers.rs`'s declaration order (`sound_ram` is declared at :29, `scsp_regs` at :32), which its own doc comment (:17-24) designates as the required acquisition order. That comment also claims *"No call site needs more than one of these locks at once today"* — `synthesize` is a counter-example and is the only one in the workspace. With `std::sync::RwLock` (writer-preferring on Linux), two readers acquiring in opposite orders with a writer queued between them can deadlock | `shared_buffers.rs:17-24`, `scsp.rs:38-39` |
| **A5** | **Locks held for 128 samples.** Both read guards span the entire block (`scsp.rs:38-107`), blocking the M68K's `sound_ram.write()` (its driver's own memory-clear loop) and `scsp_regs.write()` for the duration | `scsp.rs:37-107` |
| **A6** | `lib.rs:50-54`'s doc comment says Core 3 owns the `M68k` instance. It is actually created and stepped on **Core 4** (`lib.rs:250-289`). Stale comment from before the split | `lib.rs:53` |

### 1.6 Complete inventory of what does not exist

Registers, in reference order. Everything unchecked is unimplemented.

**CCR `$400-$42F` (§2.1)** — *none* of these are decoded; the block is inert backing store:
`MEM4MB`, `DAC18B`, `VER`, `MVOL`, `RBL`, `RBP`, `MOFULL`, `MOEMP`, `MIOVF`, `MIFULL`, `MIEMP`,
`MIBUF`, `MOBUF`, `MSLC`, `CA`, `SGC`, `EG`, `DMEAL`, `DMEAH`, `DRGA`, `DGATE`, `DDIR`, `DEXE`,
`DTLG`, `TACTL`, `TIMA`, `TBCTL`, `TIMB`, `TCCTL`, `TIMC`, `SCIEB`, `SCIPD`, `SCIRE`, `SCILV0`,
`SCILV1`, `SCILV2`, `MCIEB` (read-only use by `m68k.rs`), `MCIPD` (partial), `MCIRE`.

**ISR `$000-$3FF` (§3.1)** — decoded today: `KYONB` (mis-scoped), `SA` (wrong bytes), `LSA`,
`LEA`, `FNS` (wrong register), `TL` (wrong byte). Not decoded at all: `KYONEX`, `SBCTL`, `SSCTL`,
`LPCTL`, `PCM8B`, `D2R`, `D1R`, `EGHOLD`, `AR`, `LPSLNK`, `KRS`, `DL`, `RR`, `STWINH`, `SDIR`,
`MDL`, `MDXSL`, `MDYSL`, `OCT`, `LFORE`, `LFOF`, `PLFOWS`, `PLFOS`, `ALFOWS`, `ALFOS`, `ISEL`,
`IMXL`, `DISDL`, `DIPAN`, `EFSDL`, `EFPAN`.

**Envelope generator (§5.1)** — nothing: no `attenuation`, no `ATTACK`/`DECAY1`/`DECAY2`/`RELEASE`
phase, no `get_rate()`, no `need_envelope_step()`, no `envelope_table`, no `attack_rate_table`,
no `decay_rate_table`, no `apply_volume`.

**LFO (§6.1)** — nothing: no PLFO/ALFO tables (saw/square/triangle/noise × pitch/amplitude), no
`lfo_step_table`, no `lfo_counter`/`lfo_pos`.

**Sound DSP (§7)** — nothing: no `COEF[64]`, `MADRS[32]`, `MPRO[128]`, `TEMP[128]`, `MEMS[32]`,
`MIXS[16]`, `EFREG[16]`, `EXTS[2]`; no instruction decode; no `MDEC_CT`; no ring buffer; no
float/int conversion.

**Other** — sound stack `$600-$67E` (§4, `op7`), modulation (§4.2), sound-RAM DMA (§8), timers
(§9.1), interrupt controller (§9.2-9.4), MIDI (§9.5), CDDA/`EXTS` (§12), monitor register `$408`
(§2.4), savestates (§13).

---

## Phase 1 — Fix the register decode in the existing playback path

**Nothing new gets built until this lands.** Every later phase indexes the same slot bytes.

- [ ] **1.1** Add a `SlotRegs` decoder: one function `decode_slot(regs: &[u8], slot: usize) ->
      SlotRegs` implementing the byte map in §1.2 in full — all 31 named fields, including the
      ones no phase uses yet (`SBCTL`, `STWINH`, `SDIR`, `LPSLNK`, `ISEL`), so the table exists in
      exactly one place. Reference: §3.1. Replaces the ad-hoc reads at `scsp.rs:47`, `54-56`,
      `60`, `63`, `67`, `70`.
- [ ] **1.2** Fix **B2** — `SA`: `((regs[base+1] & 0x0F) as u32) << 16 | (regs[base+2] as u32) << 8
      | regs[base+3] as u32`. §3.1 rows `$00` 3:0 and `$02` 15:0.
- [ ] **1.3** Fix **B5** — `OCT`/`FNS` move from word `$0A` to word `$10`:
      `OCT = (regs[base+0x10] >> 3) & 0x0F` (signed −8…+7), `FNS = ((regs[base+0x10] & 0x03) as
      u16) << 8 | regs[base+0x11] as u16`. §3.1 rows `$10` 14:11 and 9:0.
- [ ] **1.4** Fix **B6** — `TL = regs[base+0x0D]`. §3.1 row `$0C` 7:0.
- [ ] **1.5** Fix **B10** — invert the sense: `TL` is attenuation. Interim (pre-Phase 3) volume:
      `apply_volume(tl, 0, sample)` per §5.1's formula, so the exponent/mantissa path is in place
      before the EG feeds it a real `attenuation`.
- [ ] **1.6** Fix **B11** — replace `step: f64` with an 18-bit fixed-point `u32` phase accumulator:
      `phase_increment = ((FNS as u32) | 0x400) << (OCT ^ 8)`; per step
      `phase += increment; delta = phase >> 18; phase &= (1<<18)-1`. §4.1.
- [ ] **1.7** Fix **B7** — `sample_offset` is a **sample** count; address is
      `SA + sample_offset*2` (16-bit) or `SA + sample_offset` (8-bit). §4.2 (`scsp.c:620-623`).
- [ ] **1.8** Fix **B8**/**B9** — honour `PCM8B` and read **signed** samples: 16-bit →
      `i16::from_be_bytes` at `addr & 0x7FFFF`; 8-bit → `(byte as i16) << 8`. §4.3.
- [ ] **1.9** Fix **B16** — drop the dead `base + 30` guard; assert `32 * 0x20 <= 0x1000` once.
- [ ] **1.10** Fix **B17** — mask every sample fetch `& 0x7FFFF` instead of deactivating the voice
      on an out-of-range pointer. §4.3 (deviation #50: the mask is unconditional, `MEM4MB` is not
      consulted here).
- [ ] **1.11** Fix **B15** — re-decode slot registers every sample block rather than latching at
      key-on. Only *state* (`phase`, `sample_offset`, `backwards`, `attenuation`, `envelope`,
      `lfo_pos`) persists in `VoiceState`; everything register-derived is read fresh. §4.
- [ ] **1.12** **Architectural (A2/A4/A5).** Restructure how `synthesize` sees the register file:
      - snapshot the 0x1000-byte register block into a local buffer under one short
        `scsp_regs.read()` per block, then release — fixes **A5**;
      - acquire `sound_ram` *before* `scsp_regs` (declaration order), or hold neither across the
        sample loop — fixes **A4**;
      - correct the now-false claim in `shared_buffers.rs:20-22` either way.
- [ ] **1.13** **Architectural (A2), `KYONEX`.** Add an edge-event channel from the register
      writers to Core 5. Concretely: a `crossbeam::channel::Sender<ScspEvent>` stored on `Sh2` and
      `M68k` (via a setter, per CLAUDE.md's `Sh2::new()` signature-stability rule), pushed from
      the `ScspRegs` write arms (`sh2.rs:522-526`, `m68k.rs:113-146`) when a write sets a
      side-effecting bit. Phase 1 needs one variant: `KeyOnEx`. Later phases add `DmaStart`,
      `IntReset`, `MproUpdated`, `MidiOut`. Core 5 drains the channel before each sample block.
      Rationale in §"Architectural call-outs" below.
- [ ] **1.14** Fix **B1a/B1b/B1c** — key-on/key-off semantics: on a `KeyOnEx` event, loop all 32
      slots; `KYONB` set → `keyon` (only if the slot is in `RELEASE`); clear → `keyoff` (enter
      `RELEASE`, don't silence). Key-on resets `sample_counter`, `step_count`, `sample_offset`,
      `envelope_steps_taken` to 0 and `attenuation` to `0x280`. §3.3.
      Deviate from the new engine on **#48**: convert an in-progress attack to release on key-off
      (old engine / `scsp2.c` behaviour), and comment the deviation.
- [ ] **1.15** Fix **B12** — implement all four `LPCTL` modes per §4.2's table, using a modulo wrap
      in mode 1 (deviation #46) rather than the hard `sample_offset = LSA` reset.
- [ ] **1.16** Fix **B13**/**B14** — stereo output: `get_sdl_shift(sdl) = if sdl==0 {16} else
      {7-sdl}`; `get_panning(pan)` → `if pan & 0x10 { (0, pan & 0xF) } else { (pan & 0xF, 0) }`;
      accumulate `(sample >> disdl >> pan_l) >> 1` / `… >> pan_r`; final
      `out >>= 0xF - MVOL`, clamp to `i16`. §4.4. **Deviate on #23**: use ~3 dB per pan step
      (`pan >> 1` as the shift, per the old engine and `scsp2.c`), not 6 dB. Wire `MVOL` from
      `$401 & 0x0F` and delete/repurpose `Scsp::set_volume`'s dead `volume` field (**B14**).
- [ ] **1.17** Fix **A1** — pace Core 5. Add `SCSP_CLOCK_HZ = 22_579_200.0` to `throttle.rs`
      (§4: *"512 clock cycles at 22579200hz"* per sample; consistent with the existing
      `M68K_CLOCK_HZ = 44_100.0 * 256.0` at `throttle.rs:27`) and give Core 5 a `ClockThrottle`
      advancing 512 per generated sample. Reduce the block size from 128 to something that keeps
      key-on latency below one sample-period boundary once timers exist (Phase 5 will need
      `scsp_update_timer(1)` per sample — §9.1).
- [ ] **1.18** Fix **A6** — correct the stale "Core 3 owns the M68k" comment at `lib.rs:53`.

**Exit criteria:** a hand-built register image produces the exact sample stream computed
independently for it; `cargo test --workspace` green.

### Phase 1 testing

Every expected value below is derived from `hardware-reference/scsp.md`, never from the code.

- [ ] **T1.1 — byte map.** For a synthetic slot image, assert each decoded field. Worked example
      (slot 3, base `0x60`): to encode `SA = 0x12345`, §3.1 puts `SA[19:16]=0x1` in word `$00`
      bits 3:0 and `SA[15:0]=0x2345` in word `$02`, so the bytes are
      `[0x60]=0x00, [0x61]=0x01, [0x62]=0x23, [0x63]=0x45`. Assert `decode_slot(...).sa ==
      0x12345`. Add a comment recording that the pre-fix code decoded `0x000123` from these
      same bytes, so a regression is recognisable.
- [ ] **T1.2 — `KYONEX` vs `KYONB` (guards against "fixing" B1 the wrong way).** Three cases,
      bit positions taken from §3.1 (`$00` bit 12 = `KYONEX`, bit 11 = `KYONB`) and converted to
      bytes by §1.2's rule:
      - word `$00 = 0x0800` (bytes `0x08, 0x00`): `KYONB` set, `KYONEX` clear → **no** key-on;
      - word `$00 = 0x1800` (bytes `0x18, 0x00`): both set → key-on;
      - word `$00 = 0x1000` (bytes `0x10, 0x00`) after a key-on: `KYONEX` with `KYONB` clear →
        key-**off** (slot enters `RELEASE`, still audible).
      Also assert a `KYONEX` on slot 0 keys on slot 7 if slot 7's `KYONB` is set (§3.3's
      all-32-slots loop), and that reading `$00` back returns `KYONEX` as 0 (§3.2).
- [ ] **T1.3 — pitch.** Hand-computed from §4.1's `phase_increment = (FNS ^ 0x400) << (OCT ^ 8)`
      with an 18-bit fraction:
      | `OCT` | `FNS` | increment | samples/step |
      |---|---|---|---|
      | 0 | 0x000 | `0x400 << 8` = `0x40000` | 1.0 |
      | 0 | 0x200 | `0x600 << 8` = `0x60000` | 1.5 |
      | +1 | 0x000 | `0x400 << 9` = `0x80000` | 2.0 |
      | −1 (0xF) | 0x000 | `0x400 << 7` = `0x20000` | 0.5 |
      | −8 (0x8) | 0x000 | `0x400 << 0` = `0x400` | 1/256 |
      (`OCT ^ 8` maps the signed field to 0…15; −8 encodes as `0x8`, +7 as `0x7`.)
- [ ] **T1.4 — `TL` polarity.** Word `$0C = 0x0000` → `TL = 0` → **full** volume; word
      `$0C = 0x00FF` → `TL = 255` → `v = 1020`, so `apply_volume` gives
      `(s * ((1020 & 0x3F) ^ 0x7F)) >> ((1020 >> 6) + 7)` = `(s * 0x40) >> 22` ≈ silence
      (§5.1). Assert both, hand-computing the second from the formula, not from a run.
- [ ] **T1.5 — sample format.** Sound RAM byte `0x80` at the slot's `SA` with `PCM8B=1` decodes to
      `-128 << 8 = -32768`, not `0.0` (which is what the current `(0x80 - 128)/128` gives).
      With `PCM8B=0`, bytes `0x80 0x00` decode to `-32768`. §4.3.
- [ ] **T1.6 — loop modes.** For `LSA=4`, `LEA=8`, increment 1.0, run 16 steps and assert the
      offset sequence for each `LPCTL` against a hand-written expected list built from §4.2's
      table (mode 0 → dies at 8; mode 1 → `…7,8→4,5…`; mode 2 → clamp at 8 then descend to 4 then
      jump to 8; mode 3 → 4↔8 ping-pong).
- [ ] **T1.7 — pan/send.** `DISDL=7, DIPAN=0x00` → both channels at `>> 0 >> 0 >> 1`.
      `DISDL=7, DIPAN=0x1F` → §4.4's `get_panning` sets `l=0, r=0xF`, i.e. right is attenuated.
      `DISDL=0` → `get_sdl_shift` returns 16 → silent. `MVOL=0` → `>> 15`; `MVOL=15` → `>> 0`.
- [ ] **T1.8 — lock order.** A test that spawns a writer thread hammering
      `sound_ram.write()`/`scsp_regs.write()` while `synthesize` runs, to catch a reintroduced
      double-hold. (Won't prove absence of deadlock, but pins the intent.)

---

## Phase 2 — Slot pipeline structure

Phase 1 fixes decode; Phase 2 puts the per-sample work into the shape the EG, LFO and DSP plug
into. Reference: §4 (`generate_sample()`'s 32-step, 7-stage pipeline).

Mimas need not reproduce the 7-deep pipeline *delay* (an artefact of the hardware's slot
time-multiplexing that Yabause models literally); it does need the same per-sample ordering of
operations, because `op7`'s sound-stack write is what `op2`'s modulation reads a generation later.

- [ ] **2.1** Restructure `synthesize` into explicit stages matching §4: phase/pitch (`op1`),
      address+loop+modulation (`op2`), waveform read (`op3`), interpolation+EG (`op4`),
      level calc (`op5`), sound-stack write (`op7`). `op6` is empty (deviation #68) — omit it and
      say why.
- [ ] **2.2** Implement the `attenuation >= 0x3bf` early-out for every stage except `op7`, with
      `op5` forcing `output = 0` in that case. §4.
- [ ] **2.3** Implement the 64-entry sound stack (`$600-$67E`, mirrored at `$680-$6FE`), written
      by `op7`, word-readable from the register block. §1.2, §4.
- [ ] **2.4** Implement modulation: `MDL`/`MDXSL`/`MDYSL` per §4.2 —
      `x_sel = (mdxsl + slot_num) & 0x1f`, `zd = (xd + yd) / 2`, `zd >>= 0xF - MDL`, result added
      to the **sample offset**, not the phase. `MDL == 0` = off. Keep the 5-bit mask
      (deviation #51) and comment it.
- [ ] **2.5** Implement `SSCTL` as the reference has it: `SSCTL != 0` silences the slot while
      still advancing the phase counter (§4.5). Do **not** invent a noise generator —
      deviation #1 records that no engine implements sources 1 and 2.
- [ ] **2.6** Route each slot into `MIXS[ISEL]` at level `IMXL`: `mixs[isel] += (output >>
      get_sdl_shift(imxl)) << 4`. §4.4. (Consumed in Phase 6; harmless before that.)
- [ ] **2.7** Fix **A3** — give the audio channel a real consumer, or make dropping explicit and
      counted. Wire `saturn-frontend-native` to drain `audio_rx`, even to `/dev/null`, so the
      producer's back-pressure behaviour is exercised.

**Exit criteria:** a single 16-bit PCM loop in sound RAM, keyed on, produces a bit-exact expected
sample sequence; two slots at different pans produce the expected stereo difference.

### Phase 2 testing
- [ ] **T2.1** Modulation: with `MDL=15`, stack entries `x=1000`, `y=2000`, expect
      `zd = (1000+2000)/2 >> 0 = 1500` added to the sample offset; `MDL=14` → `>> 1` = 750;
      `MDL=0` → 0. Hand-derived from §4.2's snippet.
- [ ] **T2.2** `get_slot` wrap: slot 30 with `MDXSL=5` selects stack entry `(5+30)&0x1f = 3`.
- [ ] **T2.3** Silence threshold: force `attenuation = 0x3bf` and assert the slot contributes
      exactly 0 while its phase counter still advances (§4.5's "Call Address" rationale).

---

## Phase 3 — Envelope generator

Reference: §5.1 (new engine). The old engine's model (§5.2, fixed-point counter + `x^7` table) and
`scsp2.c`'s (§5.3, `x^4`) are **not** used — but §5.2's ASCII diagram (`scsp.c:2069-2077`) is the
clearest statement of the intended 4-segment shape and is worth keeping in a comment.

State per slot: `attenuation: u16` (10-bit, **0 = full volume, 0x3FF = silence** — opposite
polarity to the old engine), `envelope: {Attack, Decay1, Decay2, Release}`, `sample_counter`,
`step_count`, `envelope_steps_taken`.

- [ ] **3.1 Effective rate — `get_rate()`** (§5.1, `scsp.c:692-711`), exactly:
      ```
      if KRS == 0xF { r = rate * 2 }
      else { r = KRS*2 + rate*2 + ((FNS >> 9) & 1); r = (8 ^ OCT) + (r - 8) }
      clamp to [0, 0x3C]
      ```
      where `rate` is `AR`, `D1R`, `D2R` or `RR` depending on phase.
- [ ] **3.2 `envelope_table[52][8]`** (§5.1, `scsp.c:241-268`) — generated, not transcribed. The
      reference gives the generator in full:
      ```
      MAKE_TABLE(S) emits 4 rows:
        { 8192>>S, 4096>>S, 4096>>S, END, END, END, END, END }
        { 8192>>S, 4096>>S, 4096>>S, 4096>>S, 4096>>S, 4096>>S, 4096>>S, END }
        { 4096>>S, END, END, END, END, END, END, END }
        { 4096>>S, 4096>>S, 4096>>S, 2048>>S, 2048>>S, END, END, END }
      for S = 0..12, END = 0xFFFF
      ```
      Build it with a `const fn` or `LazyLock`; do not hand-copy 416 numbers.
- [ ] **3.3 `need_envelope_step()`** (§5.1, `scsp.c:649-690`): never when `sample_counter == 0`;
      never for effective rate 0 or 1; for rate `>= 0x30`, step on every even `sample_counter`;
      otherwise step when `sample_counter % envelope_table[rate-2][step_count] == 0`, then
      advance `step_count`, wrapping when the next entry is `EFFECTIVE_RATE_END`.
- [ ] **3.4 `attack_rate_table[16][4]` and `decay_rate_table[16][4]`** — **reference gap.** §5.1
      describes them (*"indexed by `[effective_rate - 0x30]` (clamped to row 0 for rates
      `<= 0x30`) and `[envelope_steps_taken & 3]`; attack entries are shift counts 1-4, decay
      entries are linear increments 1, 2, 4, 8"*) but does not reproduce the 128 values. **Read
      `../yabause/src/scsp.c:195-239` and transcribe**, then extend
      `docs/hardware-reference/scsp.md` §5.1 with the tables so the next session doesn't repeat
      the lookup.
- [ ] **3.5 Phase state machine** (§5.1's `op4` table):
      | Phase | Action | Transition |
      |---|---|---|
      | `Attack` | `attenuation -= (attenuation >> attack_rate_table[r][n]) + 1` | `attenuation == 0` → `Decay1` |
      | `Decay1` | `attenuation += decay_rate_table[r][n]`, capped below `0x3bf`, rate `D1R` | `(attenuation >> 5) >= DL` → `Decay2` |
      | `Decay2` | same, rate `D2R` | none (runs to silence) |
      | `Release` | same, rate `RR` | none |
      `change_envelope_state()` resets `step_count` on **every** transition (§5.1).
- [ ] **3.6 `apply_volume`** (§5.1, `scsp.c:773-786`) — already stubbed in Phase 1.5; now fed the
      live `attenuation` plus the Phase 4 amplitude-LFO term:
      `v = TL*4 + slot_att; if v > 0x3FF { v = 0x3FF }; out = (s * ((v & 0x3F) ^ 0x7F)) >>
      ((v >> 6) + 7)`.
- [ ] **3.7 Key-on/key-off wiring** — already in Phase 1.14; verify against §3.3's exact reset
      table (`envelope = ATTACK`, `attenuation = 0x280`, four counters zeroed, `sa &= !1` when
      `!PCM8B`). Use `scsp2.c`'s clean `sa &= ~1` rather than the new engine's 24-bit
      `0xFFFFFE` mask (§3.3 [QUIRK]).
- [ ] **3.8 One-shot end-of-sample** (§4.2, `LPCTL == 0`): force `attenuation = 0x3FF` when the
      offset passes `LEA`. §3.3's [QUIRK] notes the new engine leaves `envelope` untouched here,
      which means the slot can't be re-keyed until a key-off arrives; since `keyonex` supplies
      one, replicate the behaviour but comment it.
- [ ] **3.9 `EGHOLD`** — §5.1 [QUIRK] / deviation #6: the bit only forces `AR = 0x1F` at write
      time and there is no hold-at-peak state anywhere in the source set. Implement the `AR`
      forcing; record hold-at-peak as unknown (see §"Questions the reference cannot answer").
- [ ] **3.10 Do not port** deviation #65 (the Darius Gaiden `AR < 0x10 → 0x10` clamp).
- [ ] **3.11 `SGC`/`EG` monitor fields** (§2.4) for `$408` — phase enum and `attenuation >> 5`,
      *not* inverted (the new engine's polarity). Deferred to Phase 5 with the rest of `$408`.

### Phase 3 testing
Hand-traced expected values, each derived from the reference's formulas before any code runs.

- [ ] **T3.1 — `get_rate()` by hand.**
      | `AR` | `KRS` | `OCT` | `FNS` bit 9 | expected |
      |---|---|---|---|---|
      | 16 | 0xF | any | any | `16*2 = 32` (`0x20`) |
      | 16 | 0 | 0 | 0 | `0 + 32 + 0 = 32`; `(8^0) + (32-8) = 8 + 24 = 32` |
      | 16 | 4 | 0 | 1 | `8 + 32 + 1 = 41`; `8 + 33 = 41` |
      | 31 | 0xF | any | any | `62` (`0x3E`) → clamped to `0x3C` |
      | 0 | 0xF | any | any | `0` |
- [ ] **T3.2 — `envelope_table` row selection by hand.** Effective rate `0x20` (32) indexes row
      `32 - 2 = 30`. Row 30: `30 / 4 = 7` → `SHIFT = 7`; `30 % 4 = 2` → the third `MAKE_TABLE`
      pattern → `{4096>>7, END, …}` = `{32, END, …}`. So the envelope steps once every 32
      samples, and `step_count` wraps immediately (next entry is `END`). Assert exactly that
      cadence over 128 simulated samples.
- [ ] **T3.3 — rate `>= 0x30`.** Effective rate `0x30`: steps on every even `sample_counter`, i.e.
      once per 2 samples (§5.1's table row). Assert 64 steps in 128 samples.
- [ ] **T3.4 — rates 0 and 1 never step.** Direct from §5.1's table.
- [ ] **T3.5 — attack curve shape.** From key-on, `attenuation = 0x280`. With a shift count of 4
      (from the table transcribed in 3.4), the first three steps are
      `0x280 - (0x280>>4) - 1 = 0x280 - 40 - 1 = 0x257`, then `0x257 - 37 - 1 = 0x231`, then
      `0x231 - 35 - 1 = 0x20D`. Compute the expected list in a throwaway Python script (per
      CLAUDE.md) and paste it into the test as a literal.
- [ ] **T3.6 — `DL` threshold.** `DL = 8` → transition `Decay1 → Decay2` when
      `attenuation >> 5 >= 8`, i.e. at `attenuation >= 0x100`. Assert the exact sample index at
      which the phase flips for a known `D1R`.
- [ ] **T3.7 — key-off during attack** converts to release (Mimas's deliberate deviation from
      #48): assert `envelope == Release` and that `attenuation` continues *upward* from where
      attack left it.
- [ ] **T3.8 — `apply_volume` spot values.** `TL=0, att=0` → `v=0` → `(s * 0x7F) >> 7` ≈ `s`.
      `TL=64, att=0` → `v=256` → `(s * ((0 )^0x7F)) >> ((4)+7)` = `(s*0x7F) >> 11` ≈ `s/16`
      (−24 dB, consistent with §5.3's −0.376 dB per `TL` step × 64 = −24.1 dB — an independent
      cross-check of the formula).

---

## Phase 4 — LFO

Reference: §6.1 (new engine). Two 256-entry table sets, regenerated on reset.

- [ ] **4.1 `fill_plfo_tables()`** (§6.1, `scsp.c:414-452`), `s8[4][256]`:
      - saw (`PLFOWS=0`): `i` for `i<128`, `-256+i` otherwise → −128…127 ramp;
      - square (1): `+127` for `i<128`, `-128` otherwise;
      - triangle (2): `i*2` for `i<64`, `255-i*2` for `i<192`, `i*2-512` otherwise;
      - noise (3): a fixed 256-entry pseudo-random sequence regenerated on reset (deviation #53 —
        replicate the *behaviour*, using a seeded PRNG rather than C `rand()`, and comment that
        real hardware presumably free-runs).
- [ ] **4.2 `fill_alfo_tables()`** (§6.1, `scsp.c:454-487`), `u8[4][256]`: saw `i` → 0…255;
      square `0`/`0xFF`; triangle `i*2` (`i<128`), `255-i*2` otherwise; noise as above.
- [ ] **4.3 `lfo_step_table[32]`** — **reference gap.** §6.1 gives the endpoints (`0x3FC` at
      `LFOF=0`, `0x001` at `LFOF=31`), the intended geometric grouping (−0x80, −0x40, −0x20,
      −0x10 …) and the three defective entries (9, 0xA transposed; 0xB is `0x08c`, should be
      `0x09c`). **Read `../yabause/src/scsp.c:355-389`** for the remaining 29 values, apply the
      corrected 9/0xA/0xB (deviation #19), and extend the hardware reference with the full table.
- [ ] **4.4 Counter advance** (§6.1, `scsp.c:501-508`): `lfo_counter` increments once per sample
      in `op7`; when `lfo_counter % lfo_step_table[LFOF] == 0`, reset it and increment `lfo_pos`
      (wrapping at 0xFF). LFO period = `256 * lfo_step_table[LFOF]` samples.
- [ ] **4.5 Pitch application** (§4.1): `plfo_shifted = (plfo_val << PLFOS) >> 2`, added to
      `phase_increment`. **Deviate on #22**: `PLFOS == 0` disables pitch modulation entirely
      (old engine's `31` sentinel / `scsp2.c`'s `-1`), rather than producing a non-zero term.
- [ ] **4.6 Amplitude application** (§6.1): `lfo_add = ((alfo_val + 1) >> (7 - ALFOS)) << 1`, added
      to `attenuation` before `apply_volume`. **Deviate on #22** for `ALFOS == 0` likewise.
- [ ] **4.7 `LFORE`** (word `$12` bit 15) — deviation #7: the new engine does not implement it at
      all; the old engine's `lfoinc = -1` is a sentinel abuse and its early-return discards the
      rest of the same register write (deviation #21). Implement `scsp2.c`'s behaviour instead
      (§6.2 [QUIRK]): zero the LFO counter, disable both modulation shifts, and **still decode
      every other field in the same write**. Comment the three-way divergence.

### Phase 4 testing
- [ ] **T4.1 — table values by hand** from §6.1's formulas: PLFO triangle at `i=0` → 0; `i=63` →
      126; `i=64` → `255-128 = 127`; `i=128` → `255-256 = -1`; `i=191` → `255-382 = -127`;
      `i=192` → `384-512 = -128`. ALFO saw at `i=255` → 255. PLFO square at `i=127` → +127,
      `i=128` → −128.
- [ ] **T4.2 — period.** `LFOF = 31` → `lfo_step_table[31] = 1` → `lfo_pos` advances every
      sample → full period 256 samples. `LFOF = 0` → `0x3FC = 1020` → period
      `256 * 1020 = 261,120` samples ≈ 5.92 s at 44.1 kHz. Assert both counts exactly.
- [ ] **T4.3 — sensitivity.** `PLFOS = 3`, saw LFO at `i = 127` (`plfo_val = 127`) →
      `(127 << 3) >> 2 = 254` added to the phase increment. `PLFOS = 0` → 0 (Mimas's deviation);
      assert this explicitly so a future "faithful port" doesn't silently reintroduce #22.
- [ ] **T4.4 — `LFORE`** written together with `LFOF` in one word write: assert `LFOF` survives
      (guards against reintroducing deviation #21).

---

## Phase 5 — Common Control Registers: MVOL/RBL/RBP, monitor, timers, interrupts, DMA

Everything in §2 and §9. Ordered before the DSP because the DSP needs `RBL`/`RBP`, and because
the interrupt controller is shared with Phase 7.

- [ ] **5.1 `$400`** — `MEM4MB` (bit 9 = byte `$400` bit 1), `DAC18B` (bit 8, inert —
      deviation #8), `VER` (bits 7:4, read-only, hardwired 0), `MVOL` (bits 3:0). §2.1, §2.2.
      `MEM4MB` also drives the SH-2 window's mirroring (Phase 7 / **M8**).
- [ ] **5.2 `$402`** — `RBL` (bits 8:7, `0x2000 << RBL` words) and `RBP` (bits 6:0). Store the
      **raw 7-bit** `RBP` and let the DSP shift it (`(addr + (rbp << 12)) & 0x3FFFF` in word
      units), i.e. the new engine's convention (§2.3), not the old engine's pre-multiplied form.
- [ ] **5.3 `$404`/`$406`** — MIDI FIFOs. §9.5: the Saturn has no MIDI port. Implement the flag
      bits (`MOFULL 0x10`, `MOEMP 0x08`, `MIOVF 0x04`, `MIFULL 0x02`, `MIEMP 0x01`) and the
      4-byte FIFOs so reads return the documented `0xFF`-on-empty and the flags are consistent;
      no host MIDI plumbing.
- [ ] **5.4 `$408`** — `MSLC` (bits 15:11, write), `CA` (10:7), `SGC` (6:5), `EG` (4:0), read-only,
      recomputed on every `MSLC` write and once per generated sample. §2.4. Note deviation #25:
      the byte and word reads disagree in Yabause. **Pick the word-read layout**
      (`(ca & 0x780) | (sgc << 5) | eg`) and make the byte reads consistent with it; comment that
      the source is self-contradictory here and that this is a Mimas choice.
- [ ] **5.5 `$412`/`$414`/`$416`** — DMA: `DMEAL` (15:1), `DMEAH` (15:12 — keep all four bits per
      `scsp2.c`, not `scsp.c`'s 3-bit mask, deviation "DMEA is only 19 bits"), `DRGA` (11:1),
      `DGATE` (14), `DDIR` (13), `DEXE` (12), `DTLG` (11:1). §8.
- [ ] **5.6 `scsp_dma()`** (§8): word-at-a-time, `DTLG >> 1` words, **instantaneous** (matching
      both implementations; deviation #15 notes no arbitration or timing exists in any of them).
      `DMEA` is the **sound-RAM** side in *both* directions (deviation #17 — do not port the
      swap). Implement `DGATE` zero-fill (deviation #13 — `scsp.c` has it commented out;
      `scsp2.c:3152-3191` has it). On completion clear `DEXE` and raise interrupt bit 4
      (`0x010`) to **both** CPUs.
      **`DDIR` polarity is unresolved** (deviation #18) — see §"Questions the reference cannot
      answer". Pick `scsp.c`'s (`DDIR==0` → sound-RAM → registers), mark it, and make it a
      one-line flip if a game disagrees.
      Triggering is edge-based (a write setting `DEXE`) → uses Phase 1.13's event channel.
- [ ] **5.7 Timers A/B/C** — `$418`/`$41A`/`$41C`, each `TxCTL` (bits 10:8) + `TIMx` (7:0). §9.1:
      counters are 8.8 fixed point; writing `TIMx` sets `cnt = d << 8`; per sample
      `cnt += 1 << (8 - TxCTL)`; on `cnt >= 0xFF00`, raise the interrupt and **subtract** `0xFF00`
      (don't reset). Interrupt bits: A `0x040`, B `0x080`, C `0x100`.
- [ ] **5.8 "Once per sample" interrupt** bit 10 (`0x400`), §9.1/§9.2. Because Mimas will drive
      `update_timer(1)` once per sample (Phase 1.17), deviation #54's batching problem does not
      arise — record that as a place Mimas is *more* accurate than the reference.
- [ ] **5.9 Interrupt registers** — `SCIEB $41E`, `SCIPD $420`, `SCIRE $422`, `SCILV0/1/2
      $424/$426/$428`, `MCIEB $42A`, `MCIPD $42C`, `MCIRE $42E`; all 11 bits wide. §2.1, §9.2.
      `SCIPD`/`MCIPD` are read-mostly: **only bit 5 is writable** (**M4**). `SCIRE`/`MCIRE` are
      write-1-to-clear → edge events (Phase 1.13).
      Mask incoming data to 11 bits on **both** byte halves (deviation #29) and rescan for
      already-pending interrupts on **both** halves of an enable-register write (deviation #28,
      **M6**).
- [ ] **5.10 Source→level mapping for the M68K** (§9.3): `SCILV0/1/2` are three parallel 8-bit
      registers forming a 3-bit IPL per source; sources 8-10 fold onto bit 7. Use the
      **maximum-priority scan** (`scsp_check_interrupt`, the `#if 0`'d correct body) for every
      delivery, never the unconditional `sintf(level)` path — deviation #27.
- [ ] **5.11 Main-CPU path** (§9.4): no levels, no vector — set the SCU Sound Request line
      whenever `MCIPD & MCIEB != 0`. This generalises `m68k.rs:131-145`'s write-triggered check
      into a function called from every source. Fixes **M1**.
- [ ] **5.12 Interrupt bit inventory** (§9.2), for the enum: 3 = MIDI in, 4 = DMA complete,
      5 = manual, 6 = Timer A, 7 = Timer B, 8 = Timer C, 9 = MIDI out empty, 10 = once per sample.
      Bits 0-2 are never raised by anything.
- [ ] **5.13 Reset values** (§2.5): `mem4b`, `mvol`, `rbl`, `rbp`, `mslc`, `ca`, `dmea`, `drga`,
      `dmfl`, `dmlen`, `mcieb`, `mcipd`, `scieb`, `scipd`, `scilv0/1/2` → 0; timer counters →
      `0xFF00` (already expired); prescalers → 0; MIDI flags → `IN_EMP | OUT_EMP`; the whole
      `scsp_regs` shadow zeroed; every slot `attenuation = 0x3FF`, `envelope = RELEASE`.

### Phase 5 testing
- [ ] **T5.1 — timer arithmetic by hand.** `TIMA = 0x80`, `TACTL = 0` → `timacnt = 0x8000`,
      `+0x100` per sample → crosses `0xFF00` after `(0xFF00 - 0x8000) / 0x100 = 127` samples.
      `TACTL = 7` → `+0x02` per sample → `(0xFF00-0x8000)/2 = 16,256` samples. Assert both counts.
- [ ] **T5.2 — subtract-not-reset.** After the first Timer A interrupt with `TIMA = 0`
      (`timacnt = 0`), the counter continues from `timacnt - 0xFF00`, so the *second* interrupt
      arrives at the same interval, not one sample later.
- [ ] **T5.3 — IPL composition.** Source = Timer A (bit 6, mask `0x40`). `SCILV0 = 0x40`,
      `SCILV1 = 0x00`, `SCILV2 = 0x40` → level `1 | 4 = 5`. With `SCILV*` all clear, the
      max-priority scan must deliver **nothing** (not IPL 0) — deviation #27.
- [ ] **T5.4 — source folding.** Source bit 9 (MIDI out) and bit 10 (per-sample) both consult
      `SCILVn` bit 7 (§9.3's `if (id > 0x80) id = 0x80`).
- [ ] **T5.5 — `MCIPD` write mask.** Writing `0x40` to `MCIPD` must **not** set bit 6; writing
      `0x20` must set bit 5 (§2.1). This is a behaviour change from `m68k.rs`'s current raw store,
      so update `mcipd_write_fires_sound_req_irq_only_when_mcieb_enables_it`
      (`m68k.rs:1177-1198`) — which happens to use bit 5, so it stays valid.
- [ ] **T5.6 — `MCIRE`.** Set `MCIPD` bit 5, assert the SH-2 line goes high; write `0x20` to
      `MCIRE`; assert `MCIPD` bit 5 clears.
- [ ] **T5.7 — DMA.** 16 bytes sound RAM → registers and back, `DGATE` zero-fill, `DEXE`
      self-clearing, and interrupt bit 4 raised on both CPU paths (§8.2's table is the expected
      matrix).
- [ ] **T5.8 — `$408` monitor.** `MSLC = 5`, slot 5 in `Decay1` with `attenuation = 0x1E0` →
      `SGC = 1`, `EG = 0x1E0 >> 5 = 0x0F`; word read = `(ca & 0x780) | (1 << 5) | 0x0F`.

---

## Phase 6 — Sound DSP (`scspdsp.c` model)

Reference: §7. 128-step microprogram, executed in full once per output sample.

- [ ] **6.1 Register banks and their address decode** (§7.1):
      | Bank | Type | Block address | Notes |
      |---|---|---|---|
      | `COEF[64]` | `u16`, 13-bit | `$700-$77F` | stored **pre-shifted** (`>> 3` on write, `<< 3` on read) |
      | `MADRS[32]` | `u16` | `$780-$7BF` | `$7A0-$7BF` **writes** hit 16-31 but **reads** mirror 0-15 (deviation #31 — fix: make the upper half readable, comment the divergence) |
      | `MPRO[128]` | `u64` | `$800-$BFF` | big-endian within each 8-byte slot: `$800`=63:48, `$802`=47:32, `$804`=31:16, `$806`=15:0 |
      | `TEMP[128]` | `s32`, 24-bit | `$C00-$DFF` | **no decode in the reference** (deviation #14) — Mimas should map it properly; note the divergence |
      | `MEMS[32]` | `s32`, 24-bit | `$E00-$E7F` | same |
      | `MIXS[16]` | `s32`, 20-bit | `$E80-$EBF` | read-only; payload in bits 19:4 (deviation #32) |
      | `EFREG[16]` | `s16` | `$EC0-$EDF` | |
      | `EXTS[2]` | `s16` | `$EE0`, `$EE2` | CD audio in |
      Do **not** replicate deviations #34-#38 (dead 32-bit paths, byte reads returning 0, the
      `a > 0xC00` off-by-one, the missing `return`): implement every width uniformly.
- [ ] **6.2 MPRO instruction decode** — all 25 fields, exactly per §7.2's table: `TRA` 62:56,
      `TWT` 55, `TWA` 54:48, `XSEL` 47, `YSEL` 46:45, `IRA` 43:38, `IWT` 37, `IWA` 36:32,
      `TABLE` 31, `MWT` 30, `MRD` 29, `EWT` 28, `EWA` 27:24, `ADRL` 23, `FRCL` 22, `SHIFT1` 21,
      `SHIFT0` 20, `YRL` 19, `NEGB` 18, `ZERO` 17, `BSEL` 16, `NOFL` 15, `COEF` 14:9, `MASA` 6:2,
      `ADREB` 1, `NXADR` 0. Bits 63, 44, 8:7 unused.
- [ ] **6.3 SHIFT decode** — *not* a plain 2-bit field (§7.2): amount is `SHIFT0 ^ SHIFT1`;
      saturation only when `SHIFT1 == 0`; `SHIFT == 3` additionally selects the alternate
      `FRC_REG` source (`ShifterOutput & 0xFFF`) and the alternate `ADRS_REG` source
      (`ShifterOutput >> 12`).
- [ ] **6.4 `IRA` decode** (§7.2): `0x00-0x1F` → `MEMS[IRA & 0x1F]`; `0x20-0x2F` →
      `MIXS[IRA & 0xF] << 4`; `0x30-0x31` → `EXTS[IRA & 1] << 8`; `0x32-0x3F` → **leave `INPUTS`
      unchanged**.
- [ ] **6.5 `YSEL` decode** (§7.2): 0 = `FRC_REG`, 1 = `COEF[COEF]`, 2 = `(Y_REG >> 11) & 0x1FFF`,
      3 = `(Y_REG >> 4) & 0x0FFF`.
- [ ] **6.6 Step ordering** — the 13 numbered steps of §7.3, in order, including: the shifter
      operating on the **previous** step's `SHIFT_REG`; the multiplier's
      `product = (sign13(Y) * X) >> 12` (Y always treated as 13-bit signed regardless of source);
      the adder's `SHIFT_REG = (product + B) & 0x3FFFFFF`; `MEMS` writes landing **two steps**
      after the read is scheduled; at most one sound-RAM read *or* write serviced per step.
- [ ] **6.7 Address generator** (§7.3 step 11): `addr = MADRS[MASA] + NXADR (+ sign12(ADRS_REG) if
      ADREB)`; unless `TABLE`, `addr += MDEC_CT` and `addr &= (0x2000 << RBL) - 1`; then
      `io_addr = (addr + (RBP << 12)) & 0x3FFFF`, in **word** units.
- [ ] **6.8 Float format** — **reference gap.** §7.3 describes `float_to_int` as
      sign(15):exponent(14:11):mantissa(10:0) with exponents above 11 clamped and the implicit
      leading bit synthesised as `sign` vs `!sign`, and `int_to_float` as the inverse. The exact
      code is not reproduced. **Read `../yabause/src/scspdsp.c:84-151`** and extend the hardware
      reference. `NOFL = 1` bypasses both (`<< 8` / `>> 8`).
- [ ] **6.9 Driving the DSP** (§7.4): once per output sample — publish `rbp`/`rbl`/`exts`,
      recompute `last_step` when the program changed, execute steps `0..last_step`, decrement
      `MDEC_CT` (reloading `0x2000 << rbl` at zero), then **clear all 16 `MIXS` entries**.
      Program length is inferred from trailing zeros (deviation #52) — replicate, and set the
      `updated` flag on byte writes too (deviation #33).
- [ ] **6.10 Effect return path** (§4.4): for `i` in `0..18`, `EFREG[i]` (or `EXTS[i-16]`)
      attenuated by slot `i`'s `EFSDL` and panned by its `EFPAN`, summed into the output before
      `MVOL`.
- [ ] **6.11 Do not port** the assembler/disassembler defects (deviation #43) or the dead
      `ScspDsp` fields (#72, #73).

### Phase 6 testing
- [ ] **T6.1 — SHIFT table**, all four cases, directly from §7.2's table (00 → shift 0 +
      saturate; 01 → shift 1 + saturate; 10 → shift 1 no saturate; 11 → shift 0 no saturate).
      Include a value that saturates in cases 0/1 and wraps in cases 2/3.
- [ ] **T6.2 — bitfield extraction.** Build one `u64` with every field set to a distinct value
      and assert all 25 come back. Derive the packing by hand from §7.2's bit ranges, then
      cross-check by writing the same instruction through the `$800`-`$806` word path and
      asserting the reassembled `u64` matches (two independent derivations of the same value).
- [ ] **T6.3 — multiplier.** `Y = 0x1FFF` (13-bit signed = −1), `X = 0x1000` → `product =
      (-1 * 0x1000) >> 12 = -1`. `Y = 0x0FFF` (+4095), `X = 0x1000` → `(4095*4096)>>12 = 4095`.
- [ ] **T6.4 — a two-instruction program.** Instruction 0: `IRA` selects `MIXS[0]`, `YRL`, `TWT`
      to `TEMP[0]`. Instruction 1: read `TEMP[0]`, `EWT` to `EFREG[0]`. Hand-compute the expected
      `EFREG[0]` from §7.3's ordering (remembering the shifter's one-step lag) and assert it.
- [ ] **T6.5 — `MDEC_CT` wrap.** `RBL = 0` → period `0x2000` samples; assert `TEMP` ring
      addressing (`(TRA + MDEC_CT) & 0x7F`) walks correctly across the wrap.
- [ ] **T6.6 — `last_step` inference.** A program with a zero word at index 4 and nonzero at 5
      runs 6 steps; a program ending in `nop` runs short (deviation #52) — assert the documented
      behaviour so it isn't mistaken for a bug later.
- [ ] **T6.7 — float round-trip.** Once 6.8's tables are transcribed: `int_to_float` then
      `float_to_int` on a set of hand-picked 24-bit values, with expected results computed in a
      throwaway script from the documented field layout.

---

## Phase 7 — M68K-side memory map and interrupt handshake completion

Reference: §10, §9.3, §2.2.

- [ ] **7.1** Fix **M2** — the CCR lives at block offset `$400`, not `$000`.
      `SCSP_MCIEB_OFFSET`/`SCSP_MCIPD_OFFSET` (`m68k.rs:43-44`) should be `0x42A`/`0x42C`.
      **Verify against a real boot trace before changing**: if BIOS/driver code is observed
      writing `0x10002A`, that is itself a finding worth recording (it would mean Mimas's current
      constant is compensating for something else). Grep a `[REGACCESS]`-style log of M68K SCSP
      writes first, per CLAUDE.md's diagnostic recipes.
- [ ] **7.2** Fix **M5** — give `M68k` an IRQ input: `set_irq(level)`, an `ipl: u8` field, an
      interrupt check in `step()` gated on `(sr >> 8) & 7`, autovector exception entry (push PC
      and SR, load the vector from `sound_ram[0x60 + level*4]`, set the mask to `level`), and the
      `STOP` opcode (`0x4E72`). The existing `RTE` handler (`m68k.rs:570`) already pops SR then
      PC — confirm the push order matches.
- [ ] **7.3** Fix **M3**/**M4** — `MCIRE` write-1-to-clear; `MCIPD` writable only at bit 5.
- [ ] **7.4** Fix **M6** — rescan on both byte halves of an `MCIEB`/`SCIEB` write.
- [ ] **7.5** Fix **M8** — SH-2 sound-RAM window honours `MEM4MB`: mask `& 0xFFFFF`; if
      `MEM4MB == 0` mask `& 0x3FFFF`; else return all-ones / discard above `0x7FFFF`
      (`sh2.rs:421-424`, `517-521`). Do **not** replicate deviation #26's out-of-bounds read.
- [ ] **7.6** Record **M7** explicitly: the M68K *data* path deliberately ignores `MEM4MB`,
      matching `scsp.c` (deviation #39). If `MEM4MB` handling is ever added there, the *fetch*
      path must change with it (deviation #40's stale-bank bug is a warning, not a model).
- [ ] **7.7** Confirm and keep: sound RAM `0x000000-0x07FFFF`, dead hole
      `0x080000-0x0FFFFF` (reads 0, writes discarded), registers `>= 0x100000` mirrored every
      `0x1000` (`m68k.rs:95-147`). §10.1. Note that `scsp2.c` mirrors sound RAM across the whole
      first megabyte instead — the two implementations disagree; Mimas keeps `scsp.c`'s hole.
- [ ] **7.8** Do **not** port `SyncSh2And68k`'s every-512th-read thread signal (deviation #55) —
      Mimas's `LockStepSync` already provides bounded-slack pacing between Core 0 and Core 4.
- [ ] **7.9** Extract the inline handshake in `m68k.rs:113-146` into a named
      `check_main_interrupt` once Phase 5.11's source-driven path exists, so both callers share it.

### Phase 7 testing
- [ ] **T7.1** Extend `scsp_register_window_is_shared_with_the_sh2_side` (`m68k.rs:1157`) to cover
      the `$400`-relative offsets after 7.1.
- [ ] **T7.2** M68K address decode boundaries: `0x07FFFF` → sound RAM last byte; `0x080000` →
      0/discard; `0x0FFFFF` → 0/discard; `0x100000` → register `$000`; `0x1004 2A` → `$42A`;
      `0x101000` → mirrors `$000`.
- [ ] **T7.3** IPL delivery: `set_irq(5)` with `SR` mask 7 → not taken; mask 4 → taken, vector
      read from `sound_ram[0x60 + 5*4] = 0x74`, mask raised to 5. Derive the autovector base from
      the 68000 vector table (vectors 25-31 at `0x64`-`0x7C` for autovectors 1-7 → level 5 is
      vector 29 at `0x74`).
- [ ] **T7.4** `MEM4MB` mirroring: with `MEM4MB=0`, an SH-2 write at `0x05A40000` reads back at
      `0x05A00000`; with `MEM4MB=1`, `0x05A90000` reads all-ones and the write is discarded.

---

## Phase 8 — Remaining (deferrable)

- [ ] **8.1** CDDA into `EXTS0`/`EXTS1`, one stereo frame per generated sample (§12, new-engine
      path). Blocked on the CD block being wired up at all (`docs/implementation-plans/cs2-cdblock.md`).
- [ ] **8.2** Savestates (§13). The reference's most useful hint: on load, every slot register is
      replayed through the word-write path to regenerate derived state — i.e. **all slot-derived
      caches must be reconstructible from the raw register words alone**. Design for that now;
      it is also a good invariant test.
- [ ] **8.3** Host audio output — a real sink for `audio_tx`, replacing Phase 2.7's drain.
- [ ] **8.4** Slot-level debug/mute tooling equivalent to §14.5 #76, if useful for bring-up.

---

## Architectural call-outs

### Does a key-on from the M68K need to be deterministically visible to the next `synthesize()`?

**Yes — and the current relaxed read-every-tick approach cannot express it at all.** This is not
a tuning question; it is a representability question.

`KYONEX` (word `$00` bit 12) is a **write-triggered pulse**: §3.2 states it is never stored and
reads back as 0. A poller that samples the register shadow once per block sees only *state*, so:

- if the driver writes `KYONEX=1` and the shadow keeps the bit, every subsequent poll re-triggers
  the key-on — 32 slots restarted, forever;
- if the shadow doesn't keep it (which is what real read-back does), the poller never sees it and
  no note ever starts;
- there is no third option, because the writer (`Sh2`/`M68k`) and the reader (Core 5) are
  different threads and nothing tells the reader "a write happened."

The same argument applies to `$416` `DEXE` (DMA start), `$422`/`$42E` (`SCIRE`/`MCIRE`,
write-1-to-clear), `$406` (`MOBUF` FIFO push) and byte writes to `MPRO` (the `updated` flag).
**Roughly eight of the SCSP's registers have write side effects; "plain byte array + poll" can
only model the level-triggered ones.**

**Recommendation: a bounded `crossbeam` channel of `ScspEvent`s from the writers to Core 5**
(Phase 1.13), drained at the top of each sample block, with the shadow array retained as the
register file for every level-triggered field. Properties:

- **Deterministic ordering** with respect to the register store, because the event is pushed
  while the writer still holds `scsp_regs.write()` — the same "acquire once, hold across related
  operations" pattern `m68k.rs:114-125` already uses for the `MCIEB`/`MCIPD` read-back, and which
  `shared_buffers.rs:17-24` explicitly sanctions.
- **No new lock ordering**, since the channel is not a `WorkRam` field.
- **Bounded latency**: at most one sample block. With Phase 1.17's throttle that is a known
  number of emulated microseconds rather than today's unbounded, host-speed-dependent window.

**Rejected alternative:** move all SCSP register decoding into the `Arc<Mutex<Scsp>>` and have
SH-2/M68K writes call `scsp.lock().write_reg()`. Strictly correct, and it removes the shadow
entirely — but Core 5 holds that mutex for a whole `synthesize` call, so every M68K register write
would block on the audio thread. It becomes viable only if `synthesize` is restructured to one
sample per lock acquisition; noted as a fallback, not the plan.

**Also required regardless:** stop holding `scsp_regs` and `sound_ram` read guards across a
128-sample block (**A5**), and stop acquiring them in reverse declaration order (**A4**). Both are
Phase 1.12.

### Sample-clock ownership

Timers, the per-sample interrupt, the DSP's `MDEC_CT` and every envelope/LFO rate are defined per
**output sample**. Mimas needs one authoritative sample clock. Two candidates:

1. **Wall-clock throttle on Core 5** — `ClockThrottle` at 22.5792 MHz, 512 units per sample
   (§4, §11's `ScspAsynMainRealtime` equivalent). Simple; independent of the M68K.
2. **Derive from the M68K cycle counter** — 256 M68K cycles per sample (§11's mode-0 equivalent);
   `throttle.rs:27` already has `M68K_CLOCK_HZ = 44_100.0 * 256.0`, so the ratio is exact.

Option 1 is the recommendation: it keeps Core 5 self-paced like every other Mimas thread, works
when the M68K is stopped (`SNDOFF` — the SCSP keeps running on real hardware), and doesn't create
a cross-thread dependency `LockStepSync` would then have to arbitrate. Option 2's advantage —
exact M68K/SCSP phase alignment — matters only for drivers that count timer ticks against their
own instruction stream, which is worth revisiting if such a bug appears.

### Deliberate simplifications to state honestly (CLAUDE.md's rule)

- No bus arbitration or contention for sound-RAM access; DMA is instantaneous (matches every
  Yabause model — deviation #15).
- No sample interpolation; every engine point-samples (deviation #9).
- `SSCTL` sources 1 (noise) and 2 (zeros) silence the slot rather than synthesising
  (deviation #1) — no source in the reference set implements them.
- MIDI is modelled but unreachable; the Saturn has no MIDI port (§9.5).

---

## Questions the reference cannot answer

Per CLAUDE.md, these are recorded rather than guessed. Each needs a second source (real hardware
docs, a different emulator, or a test ROM) before being implemented.

1. **`DDIR` polarity** — `scsp.c` and `scsp2.c` are opposite and nothing resolves it
   (deviation #18).
2. **`MIXS` word-pair split** — the even word of every pair always reads back 0; unexplained
   (deviation #32).
3. **`EGHOLD` hold-at-peak** — no engine implements it beyond forcing `AR = 0x1F`
   (deviation #6).
4. **`LFORE` exact semantics** — three implementations, three behaviours, one of them a sentinel
   abuse (deviation #7, #21).
5. **`SBCTL` bit reversal**, **`STWINH`**, **`SDIR`**, **`LPSLNK`** — decoded by all three
   engines, applied by none (deviations #2-#5). Their *encodings* are known; their *effects* are
   not.
6. **`$408` `CA` bit position** — byte and word reads disagree (`0xE0` vs `0x780`) and the source
   calls the register "still not correct" (deviation #25).
7. **Pan step size** — 3 dB vs 6 dB; three implementations, two answers, with the debug printer
   siding with 3 dB (deviation #23). Mimas picks 3 dB; a hardware recording would settle it.

## Reference-document gaps to fill while implementing

`docs/hardware-reference/scsp.md` describes these but does not reproduce them. Each phase that
needs one should read the cited Yabause lines and **extend the reference**, so the lookup is done
once:

- [ ] `attack_rate_table[16][4]` and `decay_rate_table[16][4]` — `scsp.c:195-239` (Phase 3.4)
- [ ] `lfo_step_table[32]`, all 32 entries — `scsp.c:355-389` (Phase 4.3)
- [ ] `float_to_int` / `int_to_float` bodies — `scspdsp.c:84-106`, `108-151` (Phase 6.8)

---

## Tracking-doc updates this plan implies

Per CLAUDE.md's "update these as you go":

- `.development/current_bugs.md` — currently empty. Add **B1a/B1b/B1c**, **B2**, **B5**, **B6**,
  **B10**, **A1**, **A4**, **M2** as discovered-and-open.
- `.development/TASKS.md` / `ROADMAP.md` — currently empty. `phased_development_plan.md` marks
  Phase 4 (*"Sound Subsystem (MC68000 & SCSP) … envelope shaping (Attack Rate, Decay Rate,
  Sustain Rate, Release Rate)"*) as **✅ Completed**. That is **wrong**: there is no envelope
  generator, no LFO, and no DSP. Downgrade it to partial, naming this plan's Phase 3-6 as the
  remaining work.
- `history.md` — add a chapter when Phase 1 lands, explaining why the `KYONEX`-edge event channel
  exists rather than the obvious poll-the-shadow approach.
- `docs/mimas_emu_engineering_draft.md` §7 already points here and describes the status
  accurately; no change needed until Phase 3 lands.
