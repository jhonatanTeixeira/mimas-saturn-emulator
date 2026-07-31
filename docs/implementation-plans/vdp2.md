# VDP2 — Implementation Plan

**What this document is.** The phased roadmap from Mimas's current VDP2 implementation
(`saturn-core/src/vdp.rs`, 253 lines, of which ~33 are VDP2) to the behaviour catalogued in
`docs/hardware-reference/vdp2.md`. Every phase names the exact registers, pixel formats,
hardware-reference section (`§A.n` = Section A, Registers; `§B.n` = Section B, Rendering) and
current-code location it touches.

**This is the single largest subsystem gap in the project.** Not "largest remaining feature" —
largest *gap*. Of ~142 addressable VDP2 registers, Mimas decodes **two**. Of the six compositing
layers, Mimas renders **zero**. The reference document is ~2300 lines; the code it is being
diffed against is 33.

**Sibling documents.** `docs/implementation-plans/vdp1.md` covers the drawing engine and the
framebuffer bank model; the two plans share a Core 2 / Core 3 thread-topology decision, resolved
identically in both (Phase 11 here). `docs/hardware-reference/vdp1.md` §11 is the normative
description of the VDP1→VDP2 boundary and is the input to Phase 8.

**Rule inherited from `CLAUDE.md`.** Every expected value asserted in a test in this plan must be
derived independently of the renderer under test — by hand from the hardware reference, or by a
throwaway script written from the reference. A self-consistent-but-wrong test is worse than no
test, and Phase 0 documents a case where this project already shipped one in this exact file.

---

## Phase 0 — Current-state assessment

This phase writes no code. It exists so that no later phase has to re-derive what is and is not
there, and so the scope claim is honest rather than optimistic.

### 0.1 What VDP2 actually does today

`saturn-core/src/vdp.rs`, in full:

| Location | What it does |
|---|---|
| `vdp.rs:43` | `const REG_TVMD: usize = 0x000` |
| `vdp.rs:44` | `const REG_BKTAL: usize = 0x0AE` |
| `vdp.rs:46-48` | `read_reg_word` — big-endian word read out of the `vdp2_regs` byte array |
| `vdp.rs:50-63` | `resolution_from_tvmd` — HRESO bits 0-2 → 320/352/640/704; VRESO bits 4-5 → 224/240/256 |
| `vdp.rs:65-73` | `rgb555_to_xrgb8888` — 5:5:5 → XRGB8888 with bit replication |
| `vdp.rs:127-159` | `render_backdrop` — the entire VDP2 renderer |
| `vdp.rs:128-131` | acquires `vdp2_regs.read()`, decodes TVMD for resolution and DISP |
| `vdp.rs:133-136` | allocates a fresh `Framebuffer`; returns it all-black if DISP is clear |
| `vdp.rs:137-139` | reads the word at offset `0x0AE` and `fill`s the whole frame with it |
| `vdp.rs:142-156` | overlays VDP1's framebuffer, skipping words equal to `0x0000` |

It is invoked from exactly one place: `lib.rs:217`, inside Core 3's (`vdp2-composite`) loop, one
line after `vdp::execute_vdp1(&work_ram_c3)` at `lib.rs:216`, both gated on a wall-clock
`frame_interval` of 16,666 µs (`lib.rs:208`, `lib.rs:214-215`). The returned frame is published
lock-free via `vdp2_frame.store(Arc::new(frame))` (`lib.rs:231`) and consumed by
`saturn-frontend-native/src/bin/mimas_window.rs:70` and `main.rs:203`.

### 0.2 What does not exist — verified, not assumed

Confirmed by grepping the whole workspace for `nbg|rbg|cram|cycle_pattern|CYCA|CYCB|priority|
window|mosaic|BGON|RAMCTL|SCXIN|PNCN|CHCTL|PLSZ|MPOFN` across all `*.rs`. The only hits outside
comments and unrelated code (`minifb`'s `Window`, SH-2 interrupt *priority*, the SMPC register
*window*) are `shared_buffers.rs:43` and `:78` declaring the `vdp2_cram` buffer, and
`sh2.rs`/`scu_dsp.rs` treating VDP2 VRAM/CRAM as plain byte storage. There is no VDP2 layer
rendering anywhere in this codebase.

**Absent, in full:**

- [ ] **NBG0, NBG1, NBG2, NBG3** — no tile layer of any kind. No `BGON` decode (§A.4).
- [ ] **RBG0, RBG1** — no rotation layer, no rotation parameter table reader, no coefficient
      table, no matrix math (§A.10, §B.7).
- [ ] **Character pattern name decode** — no `Vdp2PatternAddr` equivalent; `PNCN0`-`PNCN3`,
      `PNCR` are never read (§A.6, §B.2).
- [ ] **Plane / page / cell addressing** — no `CalcPlaneAddr`, no plane address table, no
      `Vdp2MapCalcXY`. `MPOFN`, `MPOFR`, `MPABN0`…`MPOPRB` (20 registers) never read (§A.7, §B.3).
- [ ] **Pixel format decode** — none of the five `colornumber` formats (4bpp palette, 8bpp
      palette, 16bpp palette, RGB555, RGB888). `CHCTLA`/`CHCTLB` never read (§A.5, §B.4).
- [ ] **Bitmap mode** — no bitmap layers, no `ReadBitmapSize`, `BMPNA`/`BMPNB` never read (§A.5).
- [ ] **CRAM palette lookup** — `vdp2_cram` is allocated (`shared_buffers.rs:43`) and readable by
      the SH-2 (`sh2.rs:445-448`, `sh2.rs:547-551`) and the SCU DSP (`scu_dsp.rs:673`, `:690`),
      but **no renderer has ever read a colour out of it**. No colour-mode decode from `RAMCTL`
      bits 12-13, no mode-0 write mirroring, no `CRAOFA`/`CRAOFB` offsets (§0.1, §A.13).
- [ ] **Priority resolution** — `PRINA`, `PRINB`, `PRIR`, `PRISA`-`PRISD` never read. No
      `TitanDigPixel` equivalent, no per-pixel priority storage, no tie-break order (§A.14, §B.9).
- [ ] **Colour calculation / blending** — `CCCTL`, `CCRNA`, `CCRNB`, `CCRR`, `CCRLB`,
      `CCRSA`-`CCRSD`, `SFCCMD` never read. No alpha, no top/bottom/add blend modes (§A.14, §B.10).
- [ ] **Windows** — `WPSX0`…`WPEY1`, `WCTLA`-`WCTLD`, `LWTA0`, `LWTA1` never read. No `TestWindow`,
      no line windows, no sprite window mask (§A.11, §B.8).
- [ ] **Mosaic** — `MZCTL` never read (§A.4).
- [ ] **Scroll and zoom** — `SCXIN0`…`SCYN3`, `ZMXN0`/`ZMYN0`/`ZMXN1`/`ZMYN1`, `ZMCTL` never read.
      There is no scroll offset applied to anything (§A.8).
- [ ] **Line scroll / vertical cell scroll** — `SCRCTL`, `VCSTA`, `LSTA0`, `LSTA1` never read
      (§A.8, §B.6).
- [ ] **Back screen** — `BKTAU` never read; `BKTAL` is misused (see §0.4 below). No VRAM fetch, no
      per-line mode (§A.9, §B.12).
- [ ] **Line colour screen** — `LCTA`, `LNCLEN` never read (§A.8, §B.12).
- [ ] **Colour offset** — `CLOFEN`, `CLOFSL`, `COAR`/`COAG`/`COAB`, `COBR`/`COBG`/`COBB` never
      read (§A.15).
- [ ] **Special function / special priority / special colour calc** — `SFSEL`, `SFCODE`, `SFPRMD`
      never read (§A.4, §A.13).
- [ ] **Shadows** — `SDCTL` never read; no shadow types, no `shadow_enabled` propagation (§A.12,
      §B.11).
- [ ] **Sprite layer read-out** — the VDP1 framebuffer overlay at `vdp.rs:142-156` is not the
      sprite layer. `SPCTL` is never read, so there is no sprite-type decode, no priority field
      extraction, no `CRAOFB` colour-bank resolution, no resolution matching (§A.12, §B.13).
- [ ] **VRAM cycle patterns** — `CYCA0L`/`U`, `CYCA1L`/`U`, `CYCB0L`/`U`, `CYCB1L`/`U` never read.
      No timeslot decode, no CPU stall factor, no bank-partition model (§A.2, §A.3).
- [ ] **Per-line register latching** — no `Vdp2Lines`-equivalent snapshot. Registers are sampled
      once, mid-frame, whenever Core 3's wall clock happens to fire (§A.16).
- [ ] **Interlace** — `TVMD` bits 6-7 (LSMD) never read; no field parity, no line skipping (§A.1).
- [ ] **`VRSIZE`** — never read, so the 4 Mbit / 8 Mbit distinction that changes character address
      width, plane address masking, back screen address width and line colour screen address width
      is absent in all four places (§A.1).
- [ ] **`EXTEN` / `HCNT` / `VCNT`** — no V-counter latch side effect on read, no counters at all.
      `VCNT` reads whatever the CPU last wrote there, which is 0 (§A.1).
- [ ] **`TVSTAT` HBLANK, ODD, PAL, EXSYFG, EXLTFG** — `sh2.rs:897-905`'s `tvstat_word()` computes
      only `TVSTAT_VBLANK_BIT` (`0x0008`). The other five defined bits are permanently clear, the
      read-clears-bits-8-9 behaviour is absent, and the "when DISP is clear, reads always report
      VBLANK set" rule (§A.1) is absent (`sh2.rs:449-460`).

**Register coverage: 2 of ~142.** The addressable window is `0x000`-`0x11E` = 144 words, minus the
two reserved slots at `0x00C` and `0x0FE` (§A.0). Mimas decodes `TVMD` (`0x000`) and misreads
`BKTAL` (`0x0AE`). Every other register is plain read/write byte storage in
`shared_buffers.rs:45`, which is enough for the BIOS's write-then-read-back verification pattern
and nothing else.

### 0.3 Cross-cutting infrastructure that does not exist

These are not registers; they are machinery every later phase depends on.

- [ ] **No scanline / SPG (video timing generator).** VBLANK is generated inside *Core 0's* CPU
      loop (`sh2.rs:1674-1700`) from `VBLANK_INTERVAL` (`sh2.rs:223`) and `VBLANK_DURATION`
      (`sh2.rs:229`) against `std::time::Instant`. Core 3's render loop has its own, separate
      16,666 µs wall clock (`lib.rs:207-208`). **These are two independent ~60 Hz clocks that
      drift against each other**, and neither is a line counter. Per-line register latching
      (§A.16), line scroll (§A.8), per-line back screen (§A.9), per-line coefficients (§B.7) and
      `HBLANK`/`VCNT` all require a real line counter that does not exist.
- [ ] **No layer buffers.** §B.1's model is six independent full-screen `PixelData` buffers
      (`{pixel: u32, priority: u8, linescreen: u8, shadow_type: u8, shadow_enabled: u8}`), written
      independently and resolved once per output pixel. Mimas has one `Vec<u32>` written directly
      (`vdp.rs:8`, `vdp.rs:133`).
- [ ] **No intermediate pixel representation.** §0.2 defines a 32-bit intermediate with a 6-bit
      alpha in bits 24-29 and a colour-calc flag in bit 31. Mimas's `rgb555_to_xrgb8888`
      (`vdp.rs:65-73`) goes straight to final XRGB8888 with no alpha channel at all, which cannot
      express colour calculation.
- [ ] **No register snapshot.** A threaded renderer needs a consistent view of VRAM + CRAM +
      registers for the duration of a frame. Today `render_backdrop` reads live locked memory
      while Core 0 is free to write it mid-frame.
- [ ] **`vdp2_regs` mirror mask is wrong.** Real hardware masks register offsets with `& 0x1FF`
      (§0); Mimas allocates `0x1000` bytes and masks `& (len-1)` = `& 0xFFF`
      (`sh2.rs:461-464`, `sh2.rs:552-556`). A write to physical `0x05F80200` should alias
      `TVMD`; today it lands on a distinct, unread byte.

### 0.4 Confirmed defects in the 33 lines that do exist

Found by diffing the current code against the reference during this plan's research. These are
real bugs, not stylistic notes.

- [ ] **`BKTAL` is not a colour — it is half of a VRAM address.** `vdp.rs:137` does
      `read_reg_word(&regs, 0x0AE)` and `vdp.rs:139` fills the frame with
      `rgb555_to_xrgb8888(that_word)`. Per §A.9, `BKTAU`/`BKTAL` form a VRAM byte address
      `((BKTAU & 0x7) << 16 | BKTAL) * 2` (8 Mbit) or `((BKTAU & 0x3) << 16 | BKTAL) * 2`
      (4 Mbit), and the *back screen colour* is the RGB 5:5:5 word **read from VDP2 VRAM at that
      address** — not through CRAM. `BKTAU` bit 15 additionally selects per-line mode, where the
      address advances by 2 bytes per scanline. Today's renderer paints the screen with a
      pointer. It has never been correct; it produced plausible output only because an address of
      0 and a colour of 0 both render black.
- [ ] **The test that "verifies" the backdrop asserts the bug.**
      `render_backdrop_reads_real_registers` (`vdp.rs:181-197`) writes `0x7C00` into `BKTAL` and
      asserts every pixel is `0x0000FF`. That is exactly the self-consistent-but-wrong test
      `CLAUDE.md` warns about — it was derived from the implementation, not from hardware. It must
      be rewritten in Phase 1 to place a colour word in *VRAM* and an address in `BKTAU`/`BKTAL`.
- [ ] **`DISP == 0` is over-strict.** `vdp.rs:131,134-136` returns an all-black frame whenever
      TVMD bit 15 is clear. Per §A.1 + §A.9 the correct rule is: layers are not drawn (§A.1,
      `Vdp2DrawScreens` gated on `TVMD & 0x8000`), but the back screen is still drawn from VRAM
      when `BDCLMD` (TVMD bit 8) is set; it is forced black only when `DISP == 0 && BDCLMD == 0`.
- [ ] **VRESO code 3 is mishandled.** `vdp.rs:57-61` maps `_ => 256`, folding code 3 into 256.
      Per §A.1, code 3 **leaves the previous height value unchanged**. Needs stateful resolution
      decode, not a pure function — or an explicit documented simplification.
- [ ] **Interlace is missing from resolution decode.** Per §A.1, `LSMD == 3` (TVMD bits 6-7)
      doubles the vertical resolution. `resolution_from_tvmd` never looks at bits 6-7.
- [ ] **The VDP1 overlay hard-codes a 320-pixel stride.** `vdp.rs:145` computes
      `offset = (y * 320 + x) * 2` regardless of the frame's actual width, so at 352/640/704 the
      overlay samples the wrong framebuffer pixels. §B.13 specifies real resolution matching
      (1:1 / pixel-doubling / half-rate) driven by VDP1's width and `vdp2_x_hires`.
- [ ] **The zero-transparency test is *right*, but everything after it is wrong.** §B.13 confirms
      framebuffer word `0x0000` is transparent in 16-bit mode, so `vdp.rs:148`'s `color16 != 0`
      matches hardware for that one case. The defect is that every *nonzero* word is then treated
      as direct RGB555, when per §A.12/§B.13 a nonzero word is a colour-bank index resolved
      through CRAM at `(CRAOFB & 0x70) << 4` **unless** bit 15 is set *and* `SPCTL & 0x20`
      (`SPCLMD`) is set — and even then the exact-`0x8000` case has its own rule. Sprite-type
      decode (§A.12, 16 layouts) also strips priority and colour-calc bits out of the word before
      it is used as an index; today those bits are rendered as colour.
- [ ] **5→8 bit channel expansion diverges from Yabause deliberately.** `vdp.rs:69-71` replicates
      the high bits (`(r5 << 3) | (r5 >> 2)`), so `0x1F → 0xFF`. Yabause shifts only (§0.1), so
      `0x1F → 0xF8`. Mimas's is the better analogue model and the existing test
      (`vdp.rs:174`) has committed to it. **Keep it, but record it** as a knowing divergence so
      that a future pixel-for-pixel comparison against Yabause output is not mistaken for a bug.

### 0.5 Status-tracking correction

`.development/phased_development_plan.md` Phase 5 ("Display Composition & Video Subsystems") is
marked **`✅ Completed`** and claims NBG0-3, RBG0, cycle-pattern parsing, CRAM indexing, priority
overlays, the full VDP1 primitive set and `FBCR` bank swapping. None of that exists.
`.development/ROADMAP.md`'s M4 (`VDP2 tile rendering ⬜`) is the accurate record. Fix the phased
plan's status when this plan's Phase 1 starts.

### 0.6 Why this is the next milestone

`.development/current_blocker.md` names it directly: the current SH-2 wall at `0x060131A8` is
"the single thing standing between 'BIOS reaches new territory' and **'BIOS reaches VDP2 setup and
we see something on screen'**". `ROADMAP.md` M4 adds that the Saturn splash is "**likely required
for the Saturn logo itself** — the BIOS splash is probably an NBG bitmap/tile layer, not just a
backdrop color", and `milestone-tests/` (the CLIP-similarity BIOS-screen check) has nothing to
pass until at least Phases 1-4 land.

**Consequence for phase ordering:** the ordering below is optimised for *first real BIOS pixels*,
not for reference-document order. Rotation (§B.7) and cycle patterns (§A.3) are the most
intellectually demanding sections and come last, because nothing in a BIOS splash needs them.

---

## Phase 1 — Foundations: register file, CRAM, the real back screen, and a line counter

Nothing renders a layer in this phase. It builds the four things every later phase reads from, and
it fixes the four defects in §0.4 that are cheap to fix now and expensive to fix later.

**Depends on:** nothing.
**Unblocks:** every subsequent phase.

### 1.1 A typed register file

- [ ] Add `saturn-core/src/vdp2_regs.rs` (or a `mod regs` inside `vdp.rs`) with a
      `Vdp2Registers` struct: 144 `u16`s indexed by `offset >> 1`, plus named accessors for every
      register in §A.0's table. Do **not** transliterate Yabause's `Vdp2` C struct — build it from
      the offset table, so the offsets are the single source of truth.
- [ ] `Vdp2Registers::snapshot(&WorkRam) -> Self`: one `vdp2_regs.read()` acquisition, copy
      `0x000..0x120`, release. Big-endian word decode, matching `read_reg_word` (`vdp.rs:46-48`).
- [ ] Field accessors return decoded values, not raw words, wherever §A specifies a decode:
      `hreso()`, `vreso()`, `lsmd()`, `bdclmd()`, `disp()`, `color_mode()` (RAMCTL 12-13),
      `vram_a_partitioned()` (RAMCTL bit 8), `vram_b_partitioned()` (RAMCTL bit 9),
      `vram_8mbit()` (VRSIZE bit 15).
- [ ] Fix the mirror mask: change `sh2.rs`'s `MemRegion::Vdp2Regs` arms (`sh2.rs:461-464`,
      `sh2.rs:552-556`) from `& (ram.len() - 1)` to `& 0x1FF` per §0, keeping the `0x1000`
      allocation. Add a test that a write to physical `0x05F80200` is visible at `0x05F80000`.
- [ ] **Deliberate divergence from Yabause, recorded:** Yabause's `Vdp2ReadByte`/`Vdp2ReadLong`
      return 0 and `Vdp2WriteByte` is a no-op (§0, §B.14 item 10). Mimas's plain byte-granular
      storage is kept — it is what makes the BIOS's byte-wise register probing work, and Yabause's
      behaviour there is a gap, not hardware.

### 1.2 RAMCTL and the Colour RAM reader

The `RAMCTL` bank-partition bit position is one of the reference's flagged contradictions (§A.2):
`Vdp2GetBank` reads bits 4-5, while `VDP2genVRamCyclePattern` and `Rbg0CheckRam` read bits 8-9 for
the same question, and bits 4-5 are simultaneously VRAM-B0's usage field.

- [ ] **Resolve as bits 8-9.** Three call sites agree on 8-9, it is consistent with bits 0-7 being
      four 2-bit per-bank usage fields, and the reference notes `Vdp2GetBank`'s 8 Mbit branch
      assumes an address space twice the size actually allocated (i.e. untested). Record the
      decision and the reasoning in the code, with a pointer to §A.2.
- [ ] `cram_lookup(index: u16, mode: u8, cram: &[u8]) -> u32` per §0.1:
      - mode 0 and mode 1: 16-bit entry at `(index << 1) & 0xFFF`
      - mode 2: 32-bit entry at `(index << 2) & 0xFFF`, returned verbatim
      - CRAM bit 15 relocated to bit 31 of the result (the flag special colour-calculation mode 3
        and `TitanTransBit` consume — §A.14, §B.10)
- [ ] **Channel expansion decision:** use Mimas's existing bit-replication
      (`rgb555_to_xrgb8888`, `vdp.rs:65-73`), not Yabause's shift-only `<< 3 / << 6 / << 9`.
      Record it as a knowing divergence (§0.4).
- [ ] Mode-0 CRAM write mirroring: in `ColorMode == 0`, word/long CRAM writes below `0x800` must
      also be written to `addr + 0x800` (§0.1). This is a *write-path* behaviour, so it belongs in
      `sh2.rs`'s `MemRegion::Vdp2Cram` arm (`sh2.rs:547-551`) and `scu_dsp.rs:690`, not in the
      renderer — which means the renderer must not assume it and must go through
      `cram_lookup` regardless.
- [ ] Note the mode 0/1 ambiguity honestly: §0.1 records that modes 0 and 1 have byte-identical
      decode, and the 1024-vs-2048-entry distinction is expressed *only* through mode 0's write
      mirroring. Implement exactly that; do not invent a difference.

### 1.3 The real back screen

- [ ] Replace `vdp.rs:137-139` entirely. Per §A.9 and §B.12:
      `addr = ((BKTAU & mask) << 16 | BKTAL) * 2` where `mask` is `0x7` (8 Mbit) or `0x3`
      (4 Mbit, from `VRSIZE` bit 15); read an RGB 5:5:5 **word from VDP2 VRAM** at `addr`;
      alpha `0x3F`.
- [ ] Per-line mode: when `BKTAU & 0x8000`, advance `addr` by 2 per scanline and produce one
      colour per line. Requires the line counter from §1.5; until then, use line 0's colour for
      the whole frame and flag it.
- [ ] Correct the `DISP` rule: force black only when `DISP == 0 && BDCLMD == 0`; otherwise the
      back screen is drawn from VRAM even with `DISP` clear. Layers stay unrendered when `DISP`
      is clear (§A.1).

### 1.4 Resolution decode, corrected

- [ ] Extend `resolution_from_tvmd` (`vdp.rs:50-63`) to return the full §A.1 tuple:
      `(vdp2width, vdp2height, rbg0width, rbg0height, x_hires, interlace)`.
      HRESO decode is already correct; keep it.
- [ ] VRESO code 3 must retain the previous height. Make the decoder a method on a small
      `DisplayState` that carries the last committed height, rather than a pure function.
- [ ] LSMD: `(TVMD >> 6) & 3 == 3` → double-density interlace, `vdp2height` doubled.
      Record §A.1's note that `rbg0height` is *not* doubled (so RBG0 covers only the top half of
      an interlaced field buffer) as a hardware-reference-faithful behaviour, not a bug to fix.
- [ ] `VBlankLineCount = 225 + (TVMD & 0x30)` on every TVMD write (§A.1) → 225 / 241 / 257 active
      lines. This is the input to §1.5.

### 1.5 A real line counter (SPG)

The most important structural change in this phase, and the one with the widest blast radius.

- [ ] Introduce a `Spg` (or `VideoTiming`) owned by Core 3: a free-running line counter derived
      from the frame clock, exposing `line_count`, `in_vblank`, `in_hblank`, `is_odd_frame`.
- [ ] **Move VBLANK-IN / VBLANK-OUT generation out of `Sh2::run_loop`** (`sh2.rs:1674-1700`) and
      into the SPG. Today Core 0 and Core 3 each keep their own ~60 Hz wall clock and drift
      independently; the CPU's notion of "we are in VBLANK" and the renderer's notion of "this
      frame is done" have no defined relationship. Preserve the exact existing behaviour of both
      interrupts (`sh2.rs:193-234`) — this is a *relocation*, verified by the existing interrupt
      tests staying green, not a semantic change.
- [ ] Drive `TVSTAT` from the SPG rather than from `tvstat_word()`'s wall-clock arithmetic
      (`sh2.rs:897-905`). Add the missing bits per §A.1: HBLANK (bit 2), ODD (bit 1, toggling
      only in interlace — forced to 1 every frame in non-interlace), PAL (bit 0, preserved across
      reset), EXSYFG/EXLTFG (bits 8-9, **cleared on read**), and the rule that a read with `DISP`
      clear always reports VBLANK set.
- [ ] Implement `VCNT` (`0x00A`) as a real readable line counter and `EXTEN`'s (`0x002`) read side
      effect: when bit 9 is clear, a read latches `VCNT = line_count` and sets `TVSTAT |= 0x200`
      (§A.1). `HCNT` (`0x008`) stays externally-latched-only, matching the reference — and this
      is a *hardware-reference-documented* Yabause gap (§B.14 item 9), so mark it as such rather
      than as a Mimas simplification.
- [ ] Per-line register snapshot (§A.16): a `Vec<Vdp2Registers>` of `VBlankLineCount` entries,
      captured at each HBLANK-OUT. Start by capturing it and having the renderer read only line
      0 from it — the machinery lands now, the consumers arrive in Phases 5 and 9.

### 1.6 Testing — Phase 1

- [ ] `back_screen_reads_the_colour_from_vram_not_the_register` — the replacement for
      `render_backdrop_reads_real_registers` (`vdp.rs:181-197`). Set `BKTAU = 0x0001`,
      `BKTAL = 0x2345`, `VRSIZE` bit 15 set; hand-compute the address:
      `(0x1 << 16 | 0x2345) * 2 = 0x12345 * 2 = 0x2468A`. Write a chosen RGB555 word there in
      `vdp2_vram`, put a *different* word at register offset `0x0AE`'s old misread position, and
      assert the frame takes the VRAM colour. The second half of that assertion is what proves
      the old bug is gone.
- [ ] `back_screen_per_line_advances_two_bytes_per_line` — `BKTAU & 0x8000` set, three distinct
      colours at `addr`, `addr+2`, `addr+4`; assert rows 0, 1, 2.
- [ ] `back_screen_is_drawn_with_disp_clear_when_bdclmd_set` and its complement
      `..._is_black_when_both_clear` — replaces `render_backdrop_is_black_when_display_disabled`
      (`vdp.rs:199-204`), which asserts the over-strict rule.
- [ ] `cram_mode0_and_mode1_decode_identically` and `cram_mode2_returns_the_long_verbatim`
      — derive the expected values by hand from §0.1's formulas, not from the implementation.
- [ ] `cram_bit15_lands_at_bit31` — the colour-calc flag survives the lookup.
- [ ] `cram_mode0_write_below_0x800_mirrors_to_plus_0x800` — a write-path test through
      `Sh2::write_word`, reading back at both addresses.
- [ ] `vreso_code_3_keeps_the_previous_height` — set VRESO 1 (240), then VRESO 3, assert 240.
- [ ] `lsmd_3_doubles_height_but_not_rbg0_height`.
- [ ] `vdp2_register_window_mirrors_every_512_bytes` — mirrors the existing
      `smpc_register_window_mirrors_every_512kb` test's shape (`sh2.rs:2306-2313`).
- [ ] Keep `resolution_decodes_common_ntsc_modes` (`vdp.rs:165-170`) and
      `rgb555_conversion_hits_full_white_and_pure_channels` (`vdp.rs:172-179`) — both assert
      independently-correct facts and should survive unchanged apart from the widened return type.

---

## Phase 2 — One NBG layer, the simplest format, pixel-exact

The goal is a single, fully hand-verifiable path from a hand-built VRAM/register fixture to a
known image. Everything is constrained to the narrowest legal configuration so that exactly one
formula is under test at each step.

**Target configuration:** NBG3, tile mode, `colornumber == 0` (16-colour palette, 4 bpp),
one-word pattern name (`PNCN3` bit 15 set), 1×1 cell characters (`CHCTLB` bit 4 clear),
`auxmode == 0`, plane size `1 × 1` (`PLSZ` bits 6-7 = 0), 4 Mbit VRAM, scroll `(0, 0)`,
no zoom (NBG3 has none — §A.8), no windows, no mosaic, no colour calc, priority whatever.

NBG3 rather than NBG0 deliberately: it is the layer with the fewest features (no bitmap mode, no
zoom, 1-bit colour-format field, no line scroll), so the first implementation has the smallest
possible surface.

**Depends on:** Phase 1 (register file, CRAM lookup).
**Unblocks:** Phase 3.

### 2.1 Registers to decode

- [ ] `BGON` `0x020` bit 3 (`N3ON`) and bit 11 (`N3TPON`, transparency-code *disable* —
      `transparencyenable = !(BGON & 0x800)`) (§A.4).
- [ ] `CHCTLB` `0x02A` bit 4 (`N3CHSZ`), bit 5 (`N3CHCN`) (§A.5).
- [ ] `PNCN3` `0x036`: bits 0-4 supplementary character number, bits 5-7 supplementary palette
      number, bit 8 special colour calc, bit 9 special priority, bit 14 `auxmode`, bit 15
      pattern-name data size (**1 = one word, 0 = two words** — note the inversion) (§A.6).
- [ ] `PLSZ` `0x03A` bits 6-7 → `ReadPlaneSize` (§A.6). Encoding 2 maps to 1×1; the reference
      records this as the source's own guesswork (`vidshared.h:499`) — carry the uncertainty
      forward as a comment, do not silently present it as fact.
- [ ] `MPOFN` `0x03C` bits 12-14 → `(MPOFN & 0x7000) >> 6` (§A.7).
- [ ] `MPABN3` `0x04C`, `MPCDN3` `0x04E` — planes A/B and C/D, **low byte first** (§A.7).
- [ ] `SCXN3` `0x094`, `SCYN3` `0x096` — `& 0x7FF`, integer only (§A.8).
- [ ] `CRAOFA` `0x0E4` bits 12-14 → `(CRAOFA & 0x7000) >> 4` (§A.13).
- [ ] `PRINB` `0x0FA` bits 8-10 → NBG3 priority (§A.14). Store it; Phase 4 uses it.
- [ ] `VRSIZE` `0x006` bit 15, already decoded in Phase 1, consumed here by `CalcPlaneAddr` and by
      the `charaddr &= 0x3FFF` clamp.

### 2.2 Algorithms to implement

- [ ] `read_pattern_data` — the `PNCN3` decode into `patterndatasize`, `patternwh`,
      `pagewh = 64 >> patternwh_bits`, `supplementdata = pnc & 0x3FF`, `auxmode` (§A.6).
- [ ] `calc_plane_addr` — the eight-row table in §A.7. Inputs `tmp = map_offset | plane_byte`
      (**ORed, not concatenated** — bits 6-7 overlap), `deca = planeh + planew - 2`,
      `multi = planeh * planew`. Only the 4 Mbit / 1-word / 1×1 row is exercised in this phase,
      but implement all eight now: the table is small, independently confirmed by
      `vdp2debug.c:259-272`, and splitting it across phases invites drift.
- [ ] `generate_plane_addr_table` — `planetbl[mapwh * mapwh]`, 4 entries for NBG layers (§A.7).
- [ ] `setup_screen_vars` — tile-mode geometry per §B.3: `pagepixelwh = 512`,
      `planepixelwidth = planew * 512`, `planepixelheight = planeh * 512`,
      `screenwidth = mapwh * planepixelwidth`, `xmask`/`ymask`.
- [ ] `map_calc_xy` — §B.3's cell-boundary cache. `cellwh = 2 + patternwh`;
      `check = ((y >> cellwh) << 16) | (x >> cellwh)`; on change, compute `planenum`, mask
      `x`/`y`, compute `info.addr` via the shift expression, fetch the pattern name, shift the
      one-cell pipeline. **The `oldcellcheck` comparison is the cache** — a pattern name is
      fetched once per cell, not per pixel. Implement the pipeline (`pipe[0]`/`pipe[1]`) now even
      though nothing sets `bad_cycle` yet (§B.5); adding it later means retrofitting the hot loop.
- [ ] `pattern_addr` one-word path (§B.2): for `colornumber == 0`,
      `paladdr = ((tmp & 0xF000) >> 8) | ((supplementdata & 0xE0) << 3)`; for `auxmode == 0`,
      `patternwh == 1`: `flipfunction = (tmp & 0xC00) >> 10`,
      `charaddr = (tmp & 0x3FF) | ((supplementdata & 0x1F) << 10)`.
      Common tail: `if !vram_8mbit { charaddr &= 0x3FFF }` then `charaddr *= 0x20`.
- [ ] Flip application for 8×8 cells (§B.3): after `x &= 7; y &= 7`, `flipfunction & 1` → `x = 7-x`,
      `& 2` → `y = 7-y`.
- [ ] `fetch_pixel`, `colornumber == 0` only (§B.4): byte at `charaddr + (y*cellw + x)/2` with
      `cellw = 8`; **even `x` is the high nibble** (`if !(x & 1) { dot >>= 4 }`); transparent when
      `(dot & 0xF) == 0` and `transparencyenable`; colour is
      `cram_lookup(coloroffset + (paladdr | (dot & 0xF)))`. All VRAM addresses masked `& 0x7FFFF`.
- [ ] A minimal `draw_scroll` (§B.6): per-line `y = info.y + mosaic_y[j]` (mosaic table is
      identity in this phase), `y &= ymask`; per-pixel `x = info.x + i`, `x &= xmask`, then
      `map_calc_xy` + `fetch_pixel`, writing into NBG3's layer buffer.
- [ ] Layer buffers (§B.1): the six `PixelData` arrays. In this phase only index 0 (NBG3) is
      written; compositing is "if NBG3 wrote a pixel, use it, else use the back screen". That is
      *not* priority resolution — label it as a stub that Phase 4 replaces, so it does not
      calcify.

### 2.3 Testing — Phase 2

This is the phase where the "independently derived" rule matters most, because everything later
builds on the addressing being right.

- [ ] **Build the fixture by hand, on paper, first.** Write out: a chosen `MPOFN` bits 12-14, a
      chosen plane byte in `MPABN3`, the resulting `tmp`, `deca`, `multi`, and the plane byte
      address from §A.7's table. Then the page/cell offset from §B.3's shift expression. Then the
      pattern-name word, its `charaddr` and `paladdr` per §B.2. Then the character byte and nibble
      per §B.4. Then the CRAM index and the RGB555 word. Put those intermediate values in the test
      as named constants with a comment naming the reference section each came from — so a future
      failure localises to one formula.
- [ ] `nbg3_plane_addr_matches_hand_computed_table` — table-driven over all eight rows of §A.7,
      values computed by hand or by a throwaway script written from the table, never from
      `calc_plane_addr`.
- [ ] `nbg3_pattern_name_decode_one_word_16colour` — assert `charaddr`, `paladdr` and
      `flipfunction` separately, for all four `auxmode`/`patternwh` combinations in §B.2's table.
- [ ] `nbg3_4bpp_even_x_is_the_high_nibble` — the single easiest thing to get backwards. Two
      adjacent pixels from one byte, asserting distinct colours in the right order.
- [ ] `nbg3_renders_one_hand_derived_8x8_cell` — the headline test. A single cell, 64 pixels,
      compared against a 64-entry expected array written out by hand. Every pixel, not a spot
      check.
- [ ] `nbg3_colour_index_zero_is_transparent_and_shows_the_back_screen` — and the complement with
      `BGON` bit 11 set, where index 0 must render as a real colour.
- [ ] `nbg3_horizontal_flip_mirrors_the_cell` / `nbg3_vertical_flip` / `nbg3_both` — three
      separate tests, each reusing the Phase-2 fixture and asserting the transposed expected array.
- [ ] `nbg3_scroll_wraps_at_the_plane_boundary` — `SCXN3` set so the layer wraps, asserting the
      `xmask` behaviour with a 2×2-cell fixture spanning the seam.
- [ ] `nbg3_disabled_in_bgon_renders_nothing`.
- [ ] Regression guard: the existing `test_vdp1_polygon_drawing` (`vdp.rs:206-252`) must stay
      green throughout; it is currently the only end-to-end proof that VDP1 output reaches the
      frame at all.

---

## Phase 3 — Remaining NBG layers and every character / bitmap format

Breadth over the same machinery. Each item is small; the volume is the work.

**Depends on:** Phase 2.
**Unblocks:** Phase 4 (needs ≥ 2 simultaneously-enabled layers to be meaningful).

### 3.1 NBG0, NBG1, NBG2

- [ ] NBG2: `BGON` bits 2/10, `CHCTLB` bits 0-1, `PNCN2` `0x034`, `PLSZ` bits 4-5,
      `MPOFN` bits 8-10 → `(MPOFN & 0x700) >> 2`, `MPABN2` `0x048` / `MPCDN2` `0x04A`,
      `SCXN2` `0x090` / `SCYN2` `0x092`, `CRAOFA` bits 8-10 → `CRAOFA & 0x700`,
      `PRINB` bits 0-2. `coordincx = coordincy = 1` unconditionally (§A.8).
- [ ] NBG1: `BGON` bits 1/9, `CHCTLA` bits 8-13 (character size, bitmap enable, bitmap size,
      **2-bit** colour format), `PNCN1` `0x032`, `PLSZ` bits 2-3, `MPOFN` bits 4-6 →
      `(MPOFN & 0x70) << 2`, `MPABN1` `0x044` / `MPCDN1` `0x046`, `SCXIN1`/`SCYIN1`
      `0x080`/`0x084` (`& 0x7FF`), `CRAOFA` bits 4-6 → `(CRAOFA & 0x70) << 4`, `PRINA` bits 8-10.
- [ ] NBG0: `BGON` bits 0/8, `CHCTLA` bits 0-6 (**3-bit** colour format), `PNCN0` `0x030`,
      `PLSZ` bits 0-1, `MPOFN` bits 0-2 → `(MPOFN & 0x7) << 6`, `MPABN0` `0x040` / `MPCDN0`
      `0x042`, `SCXIN0`/`SCYIN0` `0x070`/`0x074`, `CRAOFA` bits 0-2 → `(CRAOFA & 0x7) << 8`,
      `PRINA` bits 0-2.
- [ ] **Bandwidth exclusion rules** (§A.5) — the non-obvious cross-layer coupling that will
      otherwise show up as "NBG2 mysteriously vanished":
      - NBG1 suppressed when NBG0 enabled and `N0CHCN == 4`
      - NBG2 suppressed when NBG0 enabled and `N0CHCN >= 2`
      - NBG3 suppressed when (NBG0 enabled and `N0CHCN == 4`) or (NBG1 enabled and `N1CHCN >= 2`)
      The suppressed layer returns without drawing anything.

### 3.2 All five pixel formats

Per §B.4's table. Each needs its own transparency test and its own CRAM/direct path.

- [ ] `colornumber == 1` — 8 bpp palette. Byte at `charaddr + y*cellw + x`; transparent when
      `(dot & 0xFF) == 0`; `CRAM[coloroffset + (paladdr | (dot & 0xFF))]`.
- [ ] `colornumber == 2` — 16 bpp palette (2048 colours). Word; transparent when `dot == 0`;
      `CRAM[coloroffset + dot]` — **`paladdr` is deliberately not applied**. Easy to get wrong by
      symmetry with the other two palette modes.
- [ ] `colornumber == 3` — RGB 5:5:5 direct. Word; transparent when `!(dot & 0x8000)`; no CRAM.
- [ ] `colornumber == 4` — RGB 8:8:8 direct. Long; transparent when `!(dot & 0x80000000)`;
      `dot & 0xFFFFFF`. Record §B.4's channel-order caveat: the source does not establish that
      mode 4's byte order is compatible with mode 3's.
- [ ] Two-word pattern name decode (§B.2): `charaddr = tmp2 & 0x7FFF`,
      `flipfunction = (tmp1 & 0xC000) >> 14`,
      `paladdr = (colornumber == 0) ? ((tmp1 & 0x7F) << 4) : ((tmp1 & 0x70) << 4)`,
      `specialfunction = (tmp1 & 0x2000) >> 13`, `specialcolorfunction = (tmp1 & 0x1000) >> 12`.
- [ ] 16×16-cell characters (`patternwh == 2`): the `charaddr` merge variants in §B.2's table, and
      the four-sub-cell flip arithmetic in §B.3 (sub-cell order TL, TR, BL, BR laid out as four
      consecutive 8×8 blocks in `y`). This flip block is the single most error-prone piece of
      arithmetic in the phase — transcribe §B.3's exact conditional chain, do not re-derive it.

### 3.3 Bitmap mode

- [ ] `ReadBitmapSize` (§A.5): 0 → 512×256, 1 → 512×512, 2 → 1024×256, 3 → 1024×512.
      `cellw` doubles as the row stride, so bitmap addressing is
      `charaddr + (y * cellw + x) * bytes_per_pixel`.
- [ ] Bitmap base addresses (§A.7): NBG0 `(MPOFN & 0x7) * 0x20000`,
      NBG1 `((MPOFN & 0x70) >> 4) * 0x20000`. The *same* `MPOFN` fields that supply plane-address
      upper bits in tile mode, at a completely different scale.
- [ ] `BMPNA` `0x02C`: NBG0 palette `(BMPNA & 0x7) << 8`, NBG0 special-colour-calc bit 4,
      NBG1 palette `BMPNA & 0x700`, NBG1 special-colour-calc bit 12.
- [ ] **Flag the `<< 8` vs `<< 4` discrepancy** (§A.5): `vidsoft.c` scales by `<< 8`,
      `vdp2debug.c` prints `<< 4`, a factor of 16 apart. Implement `<< 8` (self-consistent with
      `paladdr | (dot & 0xFF)` for 8 bpp bitmaps) and add a code comment saying the source does not
      settle it, so a future "bitmap palettes are off by 16 banks" bug report has a starting point.
- [ ] Bitmap mode zeroes the plane geometry: `xmask = cellw - 1`, `ymask = cellh - 1`, and
      `map_calc_xy` is skipped entirely (§B.3, §B.6's `if !info.isbitmap`).

### 3.4 Testing — Phase 3

- [ ] One `<format>_renders_a_hand_derived_cell` test per `colornumber` value (five tests), each
      with its own hand-computed expected array. Reuse the Phase-2 fixture shape so only the
      format varies.
- [ ] `colornumber_2_ignores_paladdr` — set a nonzero `paladdr` and assert the colour is unchanged.
      This is the format-specific rule most likely to be "fixed" into a bug later.
- [ ] `two_word_pattern_name_decode` — separate assertions for each of the five decoded fields.
- [ ] `sixteen_by_sixteen_flip_selects_the_right_subcell` — all four `flipfunction` values against
      a fixture whose four sub-cells are four distinct solid colours, so a wrong sub-cell is
      unmistakable.
- [ ] `bitmap_mode_addressing_uses_cellw_as_stride` — a 512×256 bitmap with a known pixel at a
      non-trivial `(x, y)`, hand-computed offset.
- [ ] `bandwidth_exclusion_suppresses_nbg2_when_nbg0_is_high_colour` (and the other two rules) —
      three tests asserting a layer that *would* have drawn does not.
- [ ] `each_nbg_reads_its_own_registers` — a fixture enabling all four with four distinct solid
      colours and four distinct `CRAOFA` fields, asserting no cross-wiring. Cheap, and it catches
      the copy-paste errors this phase's structure invites.

---

## Phase 4 — Priority resolution and colour calculation

The first phase where "compositing" means anything. It cannot come earlier: priority resolution
across one layer is indistinguishable from no priority resolution, which is why Phase 2's stub is
explicitly labelled as one.

**Depends on:** Phase 3 (≥ 2 layers).
**Unblocks:** Phases 5, 8, 9 — all of which write into the same compositor.

### 4.1 Priority

- [ ] Read `PRINA` `0x0F8` (NBG0 bits 0-2, NBG1 bits 8-10), `PRINB` `0x0FA` (NBG2, NBG3),
      `PRIR` `0x0FC` (RBG0) (§A.14).
- [ ] **Priority 0 means "do not display."** A pixel with priority 0 is never stored; the
      resolution scan runs 7 down to 1 (§A.14, §B.9). Get this right at the *store* site, not the
      scan site, so an untouched buffer slot and a priority-0 pixel are indistinguishable — which
      is exactly the invariant §B.9 relies on.
- [ ] `dig_pixel` (§B.9): for `priority` 7→1, for `layer` from `SPRITE` down to `NBG3`, collect
      matching pixels until two are found. Layer indices are `NBG3=0, NBG2=1, NBG1=2, NBG0=3,
      RBG0=4, SPRITE=5`, so the equal-priority tie-break is
      **sprite > RBG0 > NBG0 > NBG1 > NBG2 > NBG3**. Only the top two are ever needed, because
      colour calculation blends exactly two.
- [ ] If fewer than two pixels are found, the back screen fills the remaining slot; if two are
      found the back screen is skipped entirely (§B.9).
- [ ] Layer buffers must be fully zeroed each frame (`TitanErase` equivalent), including the
      priority byte — §B.9 notes the scan matches on priority alone, not on the pixel being
      nonzero, so a stale priority from the previous frame resurrects a stale pixel.
- [ ] `SFPRMD` `0x0EA` special priority modes (§A.13): mode 0 verbatim; mode 1 (per tile) applies
      `priority = (priority & 0xE) | (specialfunction & 1)` **inside pattern decode**, so tile
      layers only; mode 2 (per pixel) applies in the scroll draw loop only, gated on
      `specialfunction & 1` **and** `PixelIsSpecialPriority`; mode 3 is undocumented and treated
      as mode 0.
- [ ] `SFSEL` `0x024` / `SFCODE` `0x026` (§A.4): `SFCODE` holds code A in bits 0-7 and code B in
      bits 8-15; each `SFSEL` bit picks one per layer. Each bit of the selected byte enables a
      *pair* of colour codes matched against `dot & 0xF`. Note §A.4's observation that
      `PixelIsSpecialPriority` and `GetAlpha` express the same pair mapping two different ways —
      implement one helper and use it for both, resolving the inconsistency in Mimas's favour.
- [ ] `SFPRMD & 0x3FF` nonzero also forces a layer to be drawn even when its priority register
      reads 0 (§A.13). Easy to miss; it changes which layers exist at all.

### 4.2 Colour calculation

- [ ] Introduce the intermediate pixel format (§0.2): 32-bit, **6-bit alpha in bits 24-29**, flag
      in bit 31. This replaces `rgb555_to_xrgb8888`'s direct-to-final output (`vdp.rs:65-73`) for
      everything inside the pipeline; the 5→8 expansion and the final conversion move to the end.
- [ ] `CCRNA` `0x108`, `CCRNB` `0x10A`, `CCRR` `0x10C` ratios: `alpha = ((~CCR & 0x1F) << 1) + 1`
      per §A.14's table — **inverted and doubled**, so register 0 → alpha `0x3F` (opaque) and
      register 31 → `0x01`.
- [ ] `CCRLB` `0x10E` is the documented odd one out: `alpha = (CCRLB & 0x1F) << 1`, not inverted,
      no `+1`, opposite direction (§A.14). Implement as documented and comment that the source
      does not establish whether it is deliberate.
- [ ] `CCCTL` `0x0EC`: per-layer enables (bits 0-4, bit 6 sprite), global bit 8 = additive,
      bit 9 = bottom-ratio. Bit 8 takes precedence over bit 9. Record §A.14's flagged conflict
      with `vdp2debug.c`'s "gradation" reading of bits 8-10 — implement the renderer's reading,
      note the other.
- [ ] The `0x80` alpha bit (§B.10): set when the global mode bit *and* the layer's own enable bit
      are both set; it lands at bit 31 and is what `TitanTransBit` tests. In ADD and BOTTOM modes
      participation requires both bits; in TOP mode participation is purely `alpha < 0x3F`.
- [ ] The three blend functions (§B.10), each with its exact arithmetic:
      - **Top**: `alpha = (alpha_of(top) << 2) + 3`; `out = (top*alpha + bottom*(0xFF-alpha))/0xFF`
        per channel; output alpha forced `0x3F`.
      - **Bottom**: returns `top` unchanged if bit 31 is clear; otherwise uses the *bottom* pixel's
        alpha as the ratio and **preserves** the top pixel's alpha rather than forcing `0x3F`.
      - **Add**: per-channel saturating addition, alpha forced `0x3F`.
- [ ] `SFCCMD` `0x0EE` (§A.14): mode 0 unconditional; mode 1 requires `specialcolorfunction & 1`;
      mode 2 additionally requires the SFCODE colour-code bit; mode 3 requires the preserved CRAM
      bit 15 (pixel bit 31).
- [ ] Final conversion (§B.10): `((pixel & 0x3F000000) << 2) + 0x03000000 | (pixel & 0x00FFFFFF)`
      — note the `+` rather than `|`, which is what makes alpha `0x3F` produce exactly `0xFF`.
      Then Mimas's own 5→8 expansion and XRGB8888 packing for `minifb`.
- [ ] **Do not implement the simplified compositing path** (`TitanRenderLinesSimplified`, §B.10).
      It is a performance specialisation whose four guard conditions (`CCCTL & 0x807F == 0`,
      `SFPRMD & 0x3FF == 0`, `LNCLEN & 0x1F == 0`, `SDCTL & 0x13F == 0`) exist to make it
      behaviourally equivalent to the general path. Adding a second implementation of the same
      semantics is exactly the kind of thing that drifts. Revisit only if profiling shows the
      general path is a real bottleneck, and only with a differential test against it.

### 4.3 Shadows

- [ ] `SDCTL` `0x0E2` per-layer bits 0-4 → `shadow_enabled`, stored on every written pixel.
      **It means "this layer accepts being shadowed", not "this layer casts a shadow"** — it is
      read from the pixel *below* (§A.12, §B.11).
- [ ] The three shadow paths (§B.11): transparent-MSB shadow, self-shadow (gated on
      `!(SPCTL & 0x10)`), normal shadow. All three blend with `0x20000000` — alpha `0x20`, RGB 0,
      i.e. roughly 50% toward black.
- [ ] §B.11 notes Yabause reads the *global* `SPCTL` here rather than a snapshot, which it calls a
      thread-safety wart. Mimas's snapshot model (§1.1) fixes this for free; note it as a
      deliberate improvement rather than silently diverging.

### 4.4 Testing — Phase 4

- [ ] `priority_zero_is_never_stored` — a layer with priority 0 must not appear even when it is
      the only layer with a pixel.
- [ ] `equal_priority_tie_break_order` — six layers all at the same priority, distinct colours,
      asserting the sprite > RBG0 > NBG0 > NBG1 > NBG2 > NBG3 order. One test, six sub-assertions
      built by disabling one layer at a time.
- [ ] `back_screen_fills_the_second_slot_when_only_one_layer_wrote`.
- [ ] `two_layers_found_skips_the_back_screen` — assert a back screen colour that would be
      visible if it leaked.
- [ ] `blend_top_ratio_matches_hand_computed_arithmetic` — pick `CCRNA = 15`, hand-compute
      `alpha = ((~15 & 0x1F) << 1) + 1 = 0x21`, then `(0x21 << 2) + 3 = 0x87`, then the per-channel
      mix for two chosen colours. Assert the exact byte. Repeat for `blend_add` (with a saturating
      case) and `blend_bottom` (including the pass-through when bit 31 is clear).
- [ ] `ccrlb_is_not_inverted` — asserts the documented asymmetry explicitly, so that a future
      "consistency fix" that inverts it fails a test instead of silently changing output.
- [ ] `sfprmd_mode_1_replaces_priority_bit_0_from_the_pattern_name` and
      `sfprmd_mode_2_uses_the_pixel_colour_code`.
- [ ] `sfprmd_nonzero_forces_a_priority_zero_layer_to_draw`.
- [ ] `shadow_enabled_is_read_from_the_layer_below` — two layers, `SDCTL` set on the *bottom* one
      only, asserting the shadow applies; and the inverse fixture asserting it does not.
- [ ] `stale_priority_does_not_survive_a_frame` — render frame 1 with a layer, frame 2 with it
      disabled, assert frame 2 shows the back screen. Catches an incomplete erase.

---

## Phase 5 — Scroll, zoom, line scroll, vertical cell scroll, mosaic, and the line colour screen

The remaining per-line and per-pixel coordinate machinery in §B.6, plus the second of the two
"screen" layers. Grouped because they all live in the same two loops and all depend on Phase 1.5's
line counter.

**Depends on:** Phase 3, Phase 1.5 (line counter + per-line snapshot).

- [ ] Zoom for NBG0/NBG1 (§A.8): `coordincx = (ZMXN.all & 0x7FF00) / 65536.0`. The `I` half is at
      the lower offset and therefore the **upper** 16 bits of the 32-bit pair, so the mask selects
      3 integer bits and 8 fractional bits. `coordincx` is a *coordinate increment* — a value > 1.0
      **reduces** the layer. NBG2/NBG3/RBG0 have no zoom (`coordincx = coordincy = 1`).
- [ ] `ZMCTL` `0x098` is never read by the software renderer (§A.8, §B.14 item 4). Do not
      implement it. Record it as a known unmodelled register with a pointer to §A.8's note that
      the reduction comes entirely from `ZMXN`/`ZMYN`.
- [ ] Fractional scroll `SCXDN*`/`SCYDN*` is discarded by the reference implementation (§B.14
      item 5). **Decision: implement it anyway** — it is a 3-line change (use the full
      `SCXIN:SCXDN` 32-bit pair as 16.16 rather than the integer half), it is unambiguous
      hardware behaviour, and this is one of the few places where doing better than Yabause costs
      nothing. Gate behind a test that shows sub-pixel scroll produces the expected stepping.
- [ ] `SCRCTL` `0x09A` line scroll (§A.8): two identical 8-bit halves. `islinescroll` from bits
      1-3, `linescrolltbl = (LSTA.all & 0x7FFFE) << 1`, `lineinc = 1 << ((mask >> 4) & 3)`
      → intervals 1, 2, 4, 8 lines.
- [ ] The line-scroll pre-pass (§B.6): iterates **every** line (not by `line_increment`), reading
      enabled components in the fixed order H, V, zoom as consecutive 4-byte entries, advancing
      only when `(j != 0) && ((j + 1) % lineinc == 0)`. Horizontal takes
      `(long >> 16) & 0x7FF`, vertical `(word & 0x7FF) + scrolly`, zoom `(long & 0x7FF00) / 65536`.
- [ ] Note §B.6's quirk: the post-line-scroll `x &= 0x3FF` uses a fixed 1024 mask rather than
      `xmask`, so line scroll can push `x` outside the virtual screen on narrow planes. Reproduce
      it (it is observable behaviour real software may depend on) and comment why.
- [ ] Vertical cell scroll (§A.8, §B.6): `VCSTA` `0x09C`/`0x09E`, the shared-table offset rules
      between NBG0 and NBG1, and the 88-longword-per-line snapshot (§A.16).
      **§B.6 records that Yabause's implementation is wrong by its own admission** — hardware
      applies a different value per *cell column*, advancing by `verticalscrollinc` at each cell
      boundary; Yabause applies one value per line, leaving `verticalscrollinc` dead.
      **Decision: implement the per-cell-column version.** The reference states plainly what
      hardware does; the per-line version is a known defect, and copying a known defect when the
      correct behaviour is documented one sentence away is not fidelity. Test both against a
      fixture where they visibly differ.
- [ ] Mosaic (§A.4, §B.6): `MZCTL` `0x022` per-layer enables bits 0-4,
      `mosaicxmask = ((MZCTL >> 8) & 0xF) + 1`, `mosaicymask = (MZCTL >> 12) + 1`. The lookup
      table is `mosaic_table[i][j] = j / (i+1) * (i+1)`. Vertical mosaic applies to the line index
      **before** scaling by `coordincy`, and the vertical line-scroll path bypasses mosaic and
      zoom entirely.
- [ ] Line colour screen (§A.8, §B.12): `LNCLEN` `0x0E8` per-layer enables, `LCTA` `0x0A8`/`0x0AA`
      with bit 31 as the per-line flag. Unlike the back screen, the VRAM value is masked to `0x7FF`
      and used as a **CRAM index**. Alpha from `CCRLB`. `linescreen[0]` is never allocated —
      index 0 means "no line screen".
- [ ] Colour offset (§A.15): `CLOFEN` `0x110` per-layer bits 0-6 (including bit 5 = back screen,
      bit 6 = sprite), `CLOFSL` `0x112` selecting set A or B, and the six 9-bit signed
      `COAR`..`COBB` values (`0x114`-`0x11E`) — bits 0-7 magnitude, **bit 8 is the sign**, applied
      with signed saturation to `[0, 0xFF]` per channel, leaving alpha untouched.
- [ ] Per-line register re-read (§A.16): each layer re-reads colour offset, its `SFPRMD` field,
      its `BGON` bit, and (for NBG0-3 only) **regenerates the entire plane address table** every
      line. RBG0 re-reads only colour offset and `SFPRMD` — no plane table, no enable.
- [ ] `perline_alpha` (§A.16) is produced for the GL renderer and never read by the software
      renderer. Do not implement it.

### 5.1 Testing — Phase 5

- [ ] `zoom_greater_than_one_reduces_the_layer` — the direction is the easy thing to invert.
      Hand-compute the source `x` for output columns 0, 1, 2 at `coordincx = 2.0`.
- [ ] `zoom_field_packing_selects_three_integer_and_eight_fraction_bits` — assert the decode of a
      chosen `ZMXIN`/`ZMXDN` pair directly, independent of rendering.
- [ ] `line_scroll_interval_advances_the_table_every_n_lines` — `lineinc = 4`, a table with four
      distinct values, asserting lines 0-3 share one and lines 4-7 share the next.
- [ ] `line_scroll_components_are_interleaved_in_h_v_zoom_order` — enable H and zoom but not V,
      assert the zoom value is read from the *second* 4-byte slot, not the third.
- [ ] `vertical_cell_scroll_differs_per_cell_column` — the test that distinguishes the corrected
      implementation from Yabause's. A fixture with distinct per-column scroll values and an
      expected image where columns are visibly offset differently.
- [ ] `mosaic_4x2_replicates_the_top_left_pixel_of_each_block` — full expected array for one
      8×4 region.
- [ ] `colour_offset_saturates_and_sign_extends` — four cases: positive to saturation, negative to
      zero, bit 8 set producing a real subtraction, and alpha unchanged.
- [ ] `line_colour_screen_indexes_cram_but_the_back_screen_does_not` — the single most confusable
      pair in §B.12, asserted side by side in one fixture.

---

## Phase 6 — Windows

Moved ahead of rotation (the task-suggested ordering pairs windows with cycle patterns) because
windows are consumed by *every* scroll layer, by the colour-calculation stage and by the rotation
parameter selector — Phase 9 cannot be written correctly without them, whereas cycle patterns are
consumed by nothing.

**Depends on:** Phase 4 (the colour-calculation window needs a colour-calculation stage to gate).
**Unblocks:** Phase 9's `RPMD == 3` path.

- [ ] Window coordinates (§A.11): `WPSX0`/`WPSY0`/`WPEX0`/`WPEY0` `0x0C0`-`0x0C6`,
      `WPSX1`..`WPEY1` `0x0C8`-`0x0CE`. Y masked to 9 bits and never rescaled; X rescaled by
      `(TVMD >> 1) & 3` per the four-row table (Normal `(x >> 1) & 0x1FF`, Hi-Res `x & 0x3FF`,
      Exclusive Normal `x & 0x1FF`, Exclusive Hi-Res `(x & 0x3FF) >> 1`). Note that this reuses
      TVMD bits 1-2, which are also part of HRESO (§A.1).
- [ ] `WCTLA` `0x0D0` / `WCTLB` `0x0D2` / `WCTLC` `0x0D4` / `WCTLD` `0x0D6` — eight-bit control
      byte per consumer, per §A.11's mapping table. Bit layout: bit 0 W0 area (1 = inside),
      bit 1 W0 enable, bit 2 W1 area, bit 3 W1 enable, bit 4 sprite window area, bit 5 sprite
      window enable, bit 7 logic (1 = AND, 0 = OR).
- [ ] `TestWindow` (§B.8) with its **two-bit return code**: bit 0 = pixel passes, bit 1 = window
      disabled. The three-valued result (`0`, `1`, `3`) is load-bearing in `TestBothWindow`'s
      dispatch — a `bool` will not work.
- [ ] The "outside" branch's extra vertical-overflow rule
      (`if yend > vdp2height && x >= xstart && x <= xend → fail`), recorded in §B.8 as an
      empirical hardware observation with no further justification. Implement it; comment that its
      provenance is an unexplained comment in the source.
- [ ] Bounds are **inclusive on both ends**.
- [ ] `TestBothWindow` (§B.8): the seven-case dispatch, transcribed exactly. §B.8 records that the
      source's own comments contradict its code here — `WindowLogic` is `(wctl & 0x80) ? (w0 || w1)
      : (w0 && w1)`, and the comments describe the opposite. **Treat the code as authoritative**
      (§B.8's explicit instruction) and do not carry the comments over.
- [ ] Line windows (§A.11): `LWTA0` `0x0D8`/`0x0DA`, `LWTA1` `0x0DC`/`0x0DE`. Bit 31 of the pair is
      the enable, **and it only takes effect if the corresponding window is also enabled in that
      layer's WCTL byte**. Each entry is two words (start X, end X) consumed sequentially per line.
      Special case: an end value of exactly `0xFFFF` forces both to 0, disabling the window for
      that line (§A.11 names 3D Baseball and Panzer Dragoon Saga). Then both are masked to `0x3FF`
      and rescaled by the same `(TVMD >> 1) & 3` table.
- [ ] Line windows supply **X bounds only**; `ystart`/`yend` keep whatever the static path left,
      which is 0 if the static path never ran (§A.11).
- [ ] Interlace: the line-window address is recomputed absolutely from `j` each line rather than
      advanced incrementally, because `ReadLineWindowClip` advances by 4 per call and the loop
      skips lines (§B.6).
- [ ] The colour-calculation window's **inverted polarity** (§B.6, §B.8): a *false* result from
      `TestBothWindow` on `WCTLD >> 8` means "force alpha to `0x3F`" (opaque), i.e. inside → no
      colour calculation, outside → colour calculation. This is backwards from every other window
      consumer and is the single most likely thing in this phase to be implemented upside down.
- [ ] Sprite window (`TestSpriteWindow`, §B.8): a `704 × 512` byte mask populated during the
      sprite pass. Defer the *population* to Phase 8; implement the *test* here returning "window
      disabled" until then, so the dispatch logic is complete and testable.

### 6.1 Testing — Phase 6

- [ ] `test_window_returns_three_when_disabled` — asserts the three-valued encoding directly,
      before any layer is involved.
- [ ] `window_bounds_are_inclusive` — pixels exactly at `xstart`, `xend`, `ystart`, `yend`.
- [ ] `window_x_coordinates_rescale_per_tvmd_mode` — all four modes, one chosen raw value,
      four hand-computed results.
- [ ] `and_logic_passes_when_either_window_passes` / `or_logic_requires_both` — named to match the
      *code's* behaviour, with a comment pointing at §B.8's comments-contradict-code note, so the
      test itself is the record of the resolution.
- [ ] `colour_calc_window_polarity_is_inverted` — a fixture where a blended pixel becomes opaque
      *inside* the window.
- [ ] `line_window_end_ffff_disables_the_window_for_that_line` — three lines, the middle one
      `0xFFFF`.
- [ ] `line_window_requires_both_bit_31_and_the_wctl_enable` — the two-condition gate, tested as
      three cases (neither, one, both).
- [ ] `three_window_case_matches_window_logic` — the inline duplication at §B.8 is a place a
      transcription error hides; assert it against the same expectations as the two-window case.

---

## Phase 7 — Sprite layer read-out (the VDP1 boundary)

Replaces `vdp.rs:142-156` — the current "overlay VDP1's framebuffer if the word is nonzero" — with
the real §B.13 sprite layer. This is a *VDP2* function that reads VDP1's front framebuffer using
VDP2's registers; `docs/hardware-reference/vdp1.md` §11 is the normative boundary description.

**Depends on:** Phase 4 (the sprite layer is layer 5 in the compositor), Phase 6 (`WCTLC >> 8`).
**Coordinates with:** `docs/implementation-plans/vdp1.md`, which owns the two-bank framebuffer
split. Until that lands, read the single flat buffer (`shared_buffers.rs:36-37`) and note the
tearing risk.

- [ ] `SPCTL` `0x0E0`: `SPTYPE` bits 0-3, `SPWINEN` bit 4, `SPCLMD` bit 5, `SPCCN` bits 8-10,
      `SPCCCS` bits 12-13 (§A.12).
- [ ] All 16 sprite-type layouts (§A.12's table): shadow bit, priority bit count and shift,
      colour-calc bit count and shift, colour data width, and the type-specific normal-shadow
      constant (`0x7FE`, `0x3FE`, `0x1FE`, `0x7E`, `0x3E`, `0xFE`). Types 0-7 are 16-bit
      framebuffer formats, types 8-F are 8-bit.
- [ ] `PRISA`-`PRISD` `0x0F0`-`0x0F6` → an 8-entry priority table indexed by the decoded priority
      field; narrow sprite types only reach the first 2, 4 or 8 entries (§A.14).
- [ ] `CCRSA`-`CCRSD` `0x100`-`0x106` → an 8-entry colour-calc ratio table, same
      invert-and-double derivation as the NBG ratios (§A.14).
- [ ] `CRAOFB` bits 4-6 → sprite CRAM offset `(CRAOFB & 0x70) << 4` (§A.13).
- [ ] 16-bit framebuffer path (§B.13): word `0x0000` is transparent (this is the one thing the
      current code gets right, `vdp.rs:148`); bit 15 set **and** `SPCTL & 0x20` → direct RGB via
      `COLSAT2YAB16` with priority `prioritytable[0]`; otherwise a colour-bank index resolved
      through CRAM. Plus §A.12's rule that a word of exactly `0x8000` is only drawn if
      `SPTYPE < 2`, or `SPTYPE < 8` with the sprite window disabled.
- [ ] 8-bit framebuffer path: colour-bank only, any nonzero byte decoded the same way.
- [ ] Resolution matching (§B.13) — replaces the hard-coded `* 320` stride at `vdp.rs:145`:
      VDP1 1024 + hires → 1:1; VDP1 512 + hires → 0.5 (pixel doubling); VDP1 1024 + lores → 2.0;
      otherwise `x = i`.
- [ ] VDP1 rotation-mode read-out (§B.13): when `Vdp1Regs->TVMR & 2`, sample coordinates come from
      **VDP2 rotation parameter A**. This creates a Phase-7 → Phase-8 dependency on the rotation
      parameter table reader; implement the non-rotated path here and the rotated path as a
      Phase-8 follow-up item.
- [ ] Sprite colour calculation (§B.13): gated on the colour-calculation window and `CCCTL & 0x40`;
      `SPCCCS` decides participation per §A.12's four conditions; bottom mode sets alpha
      unconditionally and only sets bit 31 when the sprite itself participates.
- [ ] Sprite window mask population (§B.13): when `SPCTL & 0x10` and the pixel carries
      `msbshadow`, set `spr_window_mask[y * vdp2width + x] = 1`; cleared at the start of the pass.
      **§B.13 flags a real inconsistency**: the mask is written in *framebuffer* coordinates and
      read in *screen* coordinates, which only coincide when the resolutions match. Decide
      explicitly: index the mask in screen coordinates on both sides (a deliberate correction),
      and record it.
- [ ] Window application: `WCTLC >> 8`, but only when the sprite window is disabled; when enabled,
      the test is deferred until after shadow processing (§B.8).

### 7.1 Testing — Phase 7

- [ ] `sprite_type_decode_table` — one table-driven test over all 16 types, asserting the
      priority/colour-calc/colour splits of a single chosen framebuffer word. Sixteen hand-computed
      triples; this is tedious and is exactly the kind of table that is wrong if not tested
      exhaustively.
- [ ] `sprite_word_0x8000_is_drawn_only_for_the_documented_types` — four cases spanning the
      `SPTYPE < 2` / `< 8` / window-enabled combinations.
- [ ] `sprite_colour_bank_index_strips_priority_bits_before_cram_lookup` — the specific bug the
      current code has (`vdp.rs:151` uses the whole word as a colour).
- [ ] `sprite_direct_rgb_requires_both_bit15_and_spclmd` — three cases.
- [ ] `sprite_resolution_matching_samples_the_right_framebuffer_pixel` — four cases from §B.13's
      table, each asserting which framebuffer coordinate output pixel 10 came from.
- [ ] Migrate `test_vdp1_polygon_drawing` (`vdp.rs:206-252`) to assert through the real sprite
      path rather than the overlay. Keep its independently-derived expectation (a red polygon at
      (15,15) over a blue backdrop) — the *values* were derived from the VDP1 command table, which
      remains valid; only the route from framebuffer to screen changes.

---

## Phase 8 — RBG0 / RBG1 rotation

The most mathematically demanding phase. Nothing before it depends on it, and it depends on almost
everything before it, which is why it is here rather than earlier.

**Depends on:** Phases 3 (pixel formats, plane addressing), 4 (compositing), 6 (rotation parameter
window). Phase 7's rotated sprite read-out depends on 8.1.

### 8.1 The rotation parameter table

- [ ] `RPTA` `0x0BC`/`0x0BE`: `addr = RPTA.all << 1`; parameter A at `addr & 0x000FFF7C`,
      parameter B at `(addr & 0x000FFFFC) | 0x00000080` — the two tables are 128 bytes apart and
      the register selects a 128-byte-aligned pair (§A.10).
- [ ] The 27-field table layout (§A.10): every field masked, sign-extended from the indicated bit,
      interpreted as signed 16.16 fixed point. Transcribe the table verbatim, including the two
      skipped 2-byte gaps after `Pz` (`+0x3A`) and `Cz` (`+0x42`) where the reader advances by 4.
- [ ] All masks clear the low 6 bits, so every field has at most a 10-bit fraction despite being
      stored as 16.16.
- [ ] **Fix the sign-extension bug** (§A.10's flagged inconsistency): the fixed-point reader
      sign-extends `Px`/`Py`/`Pz`/`Cx`/`Cy`/`Cz` (14-bit fields) with `0xFFF80000`, leaving bits
      14-18 clear and producing a large *positive* value for negative inputs. The float reader
      uses the correct `0xFFFFC000`. **Use `0xFFFFC000`.** This is a copy-paste error from the
      A-F fields, and porting it would mean porting a bug the reference already identified.
      Add a test with a negative `Px` that fails under the buggy mask.
- [ ] `RPMD` `0x0B0` bits 0-1 (§A.10): 0 = parameter A only, 1 = B only, 2 = switched per pixel by
      the coefficient MSB, 3 = switched by the rotation parameter window.
- [ ] §A.10 notes the float reader forces `deltaKAx = 0` for parameter B when `RPMD == 0x02`,
      citing hardware documentation, while the FP reader used by the renderer does not.
      **Flag as an open question** rather than picking silently; the two readers disagree and only
      one cites a source.
- [ ] `RPRCTL` `0x0B2` and `OVPNRA`/`OVPNRB` `0x0B8`/`0x0BA` are never read by any renderer
      (§B.14 items 2-3). Do not implement. Record them as known-unmodelled with a pointer.

### 8.2 Coefficients

- [ ] `KTCTL` `0x0B4` per §A.10: enable, data size (2 or 4 bytes), mode (0-3), line-colour enable,
      separately for parameters A (bits 0-4) and B (bits 8-12).
- [ ] `KTAOF` `0x0B6`: `coeftbladdr = (KTAOF_field * 0x10000 + touint(KAst)) * coefdatasize`,
      where `touint(KAst)` is `(u16)(KAst >> 16)` — the integer part **truncated to 16 bits**.
- [ ] Entry formats (§A.10): 2-byte (bit 15 = msb, value = sign-extended `i & 0x7FFF` from sign bit
      `0x4000`, × 64) and 4-byte (bit 31 = msb, bits 24-30 = a 7-bit `linescreen` field, value =
      sign-extended `i & 0x00FFFFFF` used directly as 16.16). Mode 3 (`Xp`) has its own two
      variants with different scales (× 16384 and × 256).
- [ ] Coefficient mode: 0 sets both `kx` and `ky`, 1 sets `kx` only, 2 sets `ky` only, 3 sets `Xp`.
- [ ] `msb` semantics: in single-parameter mode it makes the pixel transparent; in `RPMD == 2` it
      switches to the other parameter set (§A.10, §B.7).
- [ ] Fetch cadence (§B.7): once per line when `deltaKAx == 0`, once per pixel otherwise, with the
      integer and fractional address accumulators kept **separate** (`coefx`/`rcoefx`,
      `coefy`/`rcoefy`) so fractional increments accumulate without drift. Reset `coefx`/`rcoefx`
      at end of line; advance `coefy`/`rcoefy` by `deltaKAst`.
- [ ] `Rbg0CheckRam` (§B.7): if both VRAM banks are partitioned and no bank is designated the
      coefficient bank, force `deltaKAx = 0` for coefficient-mode-0 parameters. **This is a
      targeted game workaround** (Sonic R, All-Star Baseball '97) that demotes per-dot
      coefficients to per-line, not a general rule. **Decision: do not port it.** It is a
      compensation for Yabause's own lack of a VRAM allocation model, and Mimas has no such model
      either — adding a hack for a bug it does not have is strictly worse than not having it.
      Record in the simplification register.

### 8.3 The three rendering paths

- [ ] **Path 1** — `!coefenab && IsScreenRotatedFP(p)` (the identity-matrix test:
      `deltaXst==0, deltaYst==1, deltaX==1, deltaY==0, A==1, B==0, C==0, D==0, E==1, F==0`).
      The layer degenerates to a scroll layer: compute `info.x`/`info.y` from `kx`/`ky`/`Xst`/`Px`/
      `Mx`, set `coordincx`/`coordincy`, and fall through to `draw_scroll`. **Implement this path
      first** — it reuses Phase 3 wholesale and gets RBG0 on screen for the common
      "rotation layer used as a big scroll layer" case with almost no new math.
- [ ] **Path 2** — no coefficient table, screen rotated. The per-frame setup
      (`Xp`/`Yp`/`dX`/`dY`), the per-line `xmul`/`ymul` advance by `deltaXst`/`deltaYst`, and the
      per-pixel `Xsp`/`Ysp` → `x`/`y` projection (§B.7's "The rotation math").
- [ ] **Path 3** — coefficient table enabled. Second-parameter setup for `RPMD` 2 and 3, the
      per-pixel parameter selection cascade (§B.7), and the line-colour-screen-from-coefficients
      path where `lineColorAddr = (word & 0x780) | p->linescreen` — the coefficient supplies the
      low bits of a per-pixel line-colour index while `LCTA` supplies the high bits.
- [ ] `touint(v) = (u16)(v >> 16)` (§B.7) — truncated to 16 bits and treated as unsigned. This is
      *why* the screen-over tests never check for negative coordinates, and it means the rotated
      address space wraps at 65536 regardless of plane geometry. Implement it exactly; a "cleaner"
      signed version changes visible behaviour.
- [ ] All arithmetic in signed 16.16 (`mulfixed(a,b) = ((i64)a * (i64)b) >> 16`). Use a real
      fixed-point newtype, not raw `i32`, so the `>> 16` cannot be forgotten at one call site.
- [ ] Screen-over modes from `PLSZ` bits 10-11 / 14-15 (§A.6): 0 = repeat (mask), 1 = logged
      unimplemented and falls through to masking (`OVPNRA`/`OVPNRB` never read), 2 = transparent
      outside (`x > xmask || y > ymask`), 3 = clamp to 512×512. Modes 2 and 3 use `>` not `>=`,
      so the boundary pixel is included. Screen-over is **not applied on Path 2** — coordinates
      are unconditionally masked there (§B.7).
- [ ] §B.7's inconsistency: Path 2 iterates `j < vdp2height` while Path 3 iterates
      `j < rbg0height`, and `rbg0height` is not doubled for interlace. **Flag it; do not silently
      normalise.** Pick one (`rbg0height`, matching Path 3 and §A.1's statement that rotation
      layers rasterise at `rbg0width`/`rbg0height`), and record the choice with a pointer to
      §B.7.
- [ ] `Rbg0PutPixel` (§B.7): in hi-res the pixel is written twice, at `x*2` and `x*2+1`.
      Rotation layers **never apply special priority** — `info.priority` passes through directly.
- [ ] RBG1 (§B.7): when `BGON & 0x20`, RBG1 takes over NBG0's slot. It always uses rotation
      parameter B, plane size from `PLSZ >> 12`, plane addresses from parameter B, bitmap base from
      `MPOFR` bits 4-6 — but still uses NBG0's `PNCN0`, `CHCTLA`, `BMPNA`, `CRAOFA`, `PRINA`,
      `WCTLA`, `CCRNA` and `LNCLEN` bit 0. Per-line, NBG0's slot stays enabled if either
      `BGON & 0x1` or `BGON & 0x20`.
- [ ] `MPABRA`-`MPOPRA` `0x050`-`0x05E` and `MPABRB`-`MPOPRB` `0x060`-`0x06E`: 16 planes each,
      `mapwh = 4`, so `planetbl` has 16 entries for rotation layers (§A.7).
- [ ] `MPOFR` `0x03E` bits 0-2 (parameter A) and 4-6 (parameter B), plus the bitmap-base
      reinterpretation `(MPOFR & 0x7) * 0x20000` / `(MPOFR & 0x70) * 0x2000` (§A.7).

### 8.4 Testing — Phase 8

Rotation is the phase where a plausible-looking image is most likely to be subtly wrong, so the
tests deliberately avoid "does it look rotated".

- [ ] `rotation_table_field_offsets_and_masks` — one assertion per field in §A.10's 27-row table,
      against a fixture where every field holds a distinct recognisable value. This catches an
      off-by-one in the two skipped gaps, which is otherwise nearly invisible.
- [ ] `rotation_p_and_c_fields_sign_extend_from_bit_13` — the §A.10 bug fix. A negative `Px` must
      decode negative. Add a comment naming the buggy mask so the test's purpose survives.
- [ ] `identity_matrix_is_detected_as_unrotated` — assert `IsScreenRotatedFP`'s exact ten-condition
      test, then assert the degenerate path produces byte-identical output to an equivalent
      `draw_scroll` configuration. That equivalence is the strongest available check on Path 1.
- [ ] `rotation_projection_matches_hand_computed_fixed_point` — a 90° rotation matrix, three chosen
      output pixels, source coordinates computed independently in a throwaway script from §B.7's
      four formulas. Assert the *coordinates*, not the colours, so a failure localises to the math.
- [ ] `touint_truncates_to_sixteen_bits_unsigned` — a projection that overflows, asserting the
      wrap rather than a clamp or a negative.
- [ ] `screen_over_mode_2_boundary_pixel_is_included` — `x == xmask` must draw, `x == xmask + 1`
      must not (the `>` vs `>=` distinction).
- [ ] `coefficient_2byte_and_4byte_entries_decode_per_the_table` — four cases (two sizes × modes
      0-2 and mode 3).
- [ ] `coefficient_msb_makes_the_pixel_transparent_in_single_parameter_mode` and
      `..._switches_parameters_when_rpmd_is_2` — the same bit, two behaviours.
- [ ] `coefficient_fractional_accumulator_does_not_drift` — 200 lines with a fractional
      `deltaKAst`, asserting the address at line 200 against a hand-computed value. Separate
      integer/fraction accumulation is precisely the thing a "simplification" would break.
- [ ] `rbg1_uses_parameter_b_but_nbg0_pattern_registers` — the cross-wiring in §B.7, asserted
      explicitly because it is genuinely surprising.

---

## Phase 9 — VRAM access cycle patterns: what to actually build

The reference is unambiguous here, and the honest answer is uncomfortable: **faithfully porting
Yabause's cycle-pattern handling means porting something that barely does anything.**

### 9.1 What the reference establishes (§A.3, §B.14 item 1)

- The eight 4-bit timeslot codes per bank are decoded into `AC_VRAM[4][8]`, MSN = earliest slot.
- Codes `0x0`-`0x3` (pattern-name fetch) and `0x4`-`0x7` (character fetch) are only ever
  distinguished by the **OpenGL** renderer, at four call sites, to compute a per-bank boolean.
- Codes `0x8`-`0xD` — which on hardware encode vertical-cell-scroll and coefficient-table
  fetches — **are never tested anywhere in the source**.
- The only thing the software renderer computes from the pattern is `cpu_cycle_a`/`cpu_cycle_b`,
  a CPU stall factor consumed by `memory.c` as a fixed address bisection at `0x40000`,
  independent of `RAMCTL`.
- That computation contains a **bug the reference identifies**: both mapping blocks test
  `Vdp2External.cpu_cycle_a` (the previous frame's already-overwritten output) instead of the
  freshly computed local, making the 24-cycle case dead after the first frame.
- The software renderer's *only* other cycle-pattern behaviour is a hard-coded eight-register
  signature match for one game (Castlevania / Dracula X), which makes NBG3 render each cell with
  the previous cell's pattern data.
- **"The general behaviour on conflicting or insufficient timeslot allocation is not modelled by
  this code and cannot be derived from it."**

### 9.2 Decision

- [ ] **Decode the registers faithfully.** `CYCA0L`/`U`, `CYCA1L`/`U`, `CYCB0L`/`U`, `CYCB1L`/`U`
      (`0x010`-`0x01E`) into a `[[u8; 8]; 4]` with bank order A0, A1, B0, B1 and MSN-first slot
      order. This is cheap, exactly specified, independently testable, and is the prerequisite for
      everything else in this phase.
- [ ] **Implement the CPU stall factor, with the bug fixed.** Port §A.3's algorithm — including
      the `RAMCTL & 0x100` / `& 0x200` partition branches and the `fcnt` logic — but test the
      freshly computed local in the `== 1` branch, so the 24-cycle case is live. Add a comment
      naming §A.3's bug and stating that Mimas deliberately does not reproduce it.
- [ ] **But do not wire it up yet.** `Sh2`'s VDP2 VRAM access path (`sh2.rs:441-444`) has no cycle
      cost at all — it is a lock acquisition and an array index. There is nowhere for a stall
      factor to land until `Sh2` has a per-access memory cost model, which is an SH-2-plan concern
      (`docs/implementation-plans/sh2-cpu.md`), not a VDP2 one. Compute it, expose it, test it,
      and leave the consumer for whenever that model exists. Record the dependency in both plans.
- [ ] **Do not port the Castlevania signature match.** An eight-register equality test against one
      game's exact configuration is not hardware behaviour and does not generalise. The
      `pipe[0]`/`pipe[1]` machinery it drives is already built (Phase 2.2), so if a real
      one-cell-delay model is ever justified it has somewhere to go.
- [ ] **Do not port `Rbg0CheckRam`** (already decided in Phase 8.2, restated here for the same
      reason).
- [ ] **Build one thing Yabause does not: a cycle-pattern validator.** Not a renderer behaviour —
      a diagnostic. Once per frame, check whether each enabled layer has been allocated enough
      timeslots for its colour format and character size, and log a one-shot message when it has
      not, in the style of `sh2.rs`'s `REG_ACCESS_LOG`/`log_reg_access_once`. Cost in the render
      path: zero (it runs once per frame, off the pixel loop). Value: when a BIOS or game screen
      renders wrong, this immediately distinguishes "Mimas's fetch logic is broken" from "the
      software allocated slots Mimas is ignoring". That is the single most useful thing the cycle
      pattern can do for this project right now.
- [ ] **Explicitly document the simplification.** Mimas renders every enabled layer as though its
      VRAM fetches always succeed, regardless of timeslot allocation. Real hardware would drop or
      stale-repeat fetches. This is a *known* simplification, recorded in the register below and
      in `CLAUDE.md`'s simplification list alongside "VDP2 backdrop-only" — and unlike that one,
      it is a simplification the reference source *also* makes, which is why there is no oracle
      available for doing better.

### 9.3 Testing — Phase 9

- [ ] `cycle_pattern_nibble_order_is_msn_first` — one register value, eight asserted slots.
- [ ] `cpu_stall_factor_matches_hand_traced_algorithm` — three fixtures (all-free, all-busy,
      partitioned-with-fcnt) traced by hand through §A.3's pseudocode. Include one that
      distinguishes the fixed version from the buggy one, i.e. a case that should yield 24.
- [ ] `cycle_pattern_validator_flags_an_underallocated_layer` — a fixture enabling NBG0 at 8 bpp
      with a single character timeslot, asserting the diagnostic fires exactly once.
- [ ] `cycle_pattern_validator_is_silent_for_a_valid_allocation` — the false-positive guard, which
      matters more than the positive case for a diagnostic that is meant to be trusted.

---

## Phase 10 — Thread topology: does VDP2 get its own thread?

**Recommendation: yes, eventually — but not until three preconditions are met, and not as part of
any earlier phase.** This is the same conclusion `docs/implementation-plans/vdp1.md` reaches from
the other side; both plans should be changed together if either changes.

### 10.1 Where things stand

- Core 2 (`vdp1-draw`, `lib.rs:179-196`) is an idle spin loop: it advances a counter and calls
  `sync_core(2, cycles)` forever, doing no work.
- Core 3 (`vdp2-composite`, `lib.rs:198-240`) calls `execute_vdp1` then `render_backdrop`
  serially, once per 16,666 µs of wall clock, and publishes the frame.
- `CLAUDE.md` lists this under "Known architecture debt, not yet reconciled".
- `docs/mimas-architecture-spec.md` §1.1 lists separate VDP1 and VDP2 threads as the target.
- The (now-deleted) `final_architecture_draft.md` argued the *opposite* — that VDP1+VDP2 should
  stay paired in one thread deliberately, because they share VRAM windows and a frame-timing
  protocol that serialises them relative to each other. `history.md` Chapter 8 records that
  argument and notes it is "a thread-topology decision independent of lock topology".

Both positions are defensible, and the reference settles which one is right *for the right reason*.

### 10.2 The argument from the hardware reference

`hardware-reference/vdp1.md` §9 and §11 describe the real relationship precisely: **VDP2 reads the
front framebuffer while VDP1 draws into the back one**, with a swap at a defined instant. That is
a textbook single-producer/single-consumer handoff across a double buffer — which means the two
chips genuinely can run concurrently, and the *only* reason they cannot in Mimas today is that
`shared_buffers.rs:36-37` models the framebuffer as **one flat 512 KB window**, not two banks.

So the pairing argument is not wrong about the coupling; it is describing a coupling that exists
only because the double buffer is missing. Fix the double buffer and the coupling disappears.

### 10.3 Preconditions, in order

- [ ] **P1 — VDP1's framebuffer becomes two real 256 KB banks with an `FBCR`-driven swap.**
      Owned by `docs/implementation-plans/vdp1.md`. Until this exists, VDP2 reading the
      framebuffer while VDP1 writes it is a data race that a thread split would make continuous
      instead of occasional.
- [ ] **P2 — the SPG exists (Phase 1.5).** A swap needs a defined instant, and "whenever Core 3's
      `Instant::now()` crosses a threshold" is not one. The per-frame ordering in
      `hardware-reference/vdp1.md` §9.2 (VBlank erase → manual erase → swap → EDSR shift →
      conditional draw start) is defined in terms of VBLANK, which today only Core 0 knows about.
- [ ] **P3 — the renderer takes a snapshot rather than reading live memory (Phase 1.1).**
      Once VDP2 runs concurrently with Core 0 *and* Core 2, reading `vdp2_regs`/`vdp2_vram`/
      `vdp2_cram` live means a layer can be rasterised half with one register set and half with
      another. The snapshot is required for correctness, not just for lock hygiene.

### 10.4 The move, once preconditions hold

- [ ] Move `execute_vdp1` from `lib.rs:216` to Core 2's loop (`lib.rs:179-196`), replacing the
      idle spin. Core 2 already exists, is already named `vdp1-draw`, and is already wired into
      `LockStepSync` — this is a relocation, not a new thread.
- [ ] Core 2 parks via `park_while_inactive` when no draw is pending, matching Core 1 and Core 6
      (`lib.rs:166`, `lib.rs:322-338`) rather than the `yield_now` spin that Cores 2, 3, 4, 5, 7
      currently use. `CLAUDE.md` calls out that spin as debt; this phase should not add to it.
- [ ] Core 3 keeps VDP2 only, driven by the SPG rather than by its own `Instant` arithmetic.
- [ ] The handoff is the bank swap, signalled by a `Condvar` (the shape
      `mimas-architecture-spec.md` §1.2 already specifies for draw-end), not a function call.
- [ ] Wire the VDP1 draw-end interrupt (`hardware-reference/vdp1.md` §10.2: vector `0x4D`,
      level 2, SCU mask bit `0x2000`) — today nothing signals it. This is VDP1's plan's item, but
      it becomes *reachable* only once VDP1 has its own thread, so the two must land together.

### 10.5 Until then

- [ ] **Change nothing about the thread layout.** Every phase from 1 through 9 works unmodified
      inside Core 3's existing serial loop. Moving threads earlier buys no correctness and costs a
      class of race conditions that would be blamed on the renderer.

---

## Architectural call-outs

### Lock acquisition — confirmed safe, and how to keep it that way

`shared_buffers.rs:17-24` states the rule: no call site holds more than one `WorkRam` region lock
at a time today, and if one ever must, it acquires in field-declaration order.

**Verified for the current code.** `render_backdrop` acquires `vdp2_regs.read()` at `vdp.rs:128`,
drops it at `vdp.rs:138` (or lets it drop on the early return at `vdp.rs:135`), then acquires
`vdp1_framebuffer.read()` at `vdp.rs:142`. Never two at once. `execute_vdp1` acquires
`vdp1_regs.read()` (`vdp.rs:76`), explicitly drops it (`vdp.rs:81`), then takes
`vdp1_vram.write()` and `vdp1_framebuffer.write()` — **that pair is held simultaneously**
(`vdp.rs:83-84`), in field-declaration order (`vdp1_vram` at `shared_buffers.rs:34` precedes
`vdp1_framebuffer` at `:37`), so it complies with the rule. Worth noting in the VDP1 plan; it is
the one existing two-lock site.

**A full VDP2 frame needs four regions**: `vdp2_regs`, `vdp2_vram`, `vdp2_cram` and
`vdp1_framebuffer`. Holding all four across a whole frame would block Core 0 from writing any VDP2
register for ~16 ms, which is worse than the single global lock the project already removed.

- [ ] **Resolution: snapshot, do not hold.** At frame start, acquire each region once, `memcpy`
      into renderer-owned buffers, release. Total ~520 KB per frame (512 KB VRAM + 4 KB CRAM +
      0x200 registers + the VDP1 framebuffer read) ≈ 31 MB/s at 60 fps — negligible, and it is
      what Yabause's own threaded renderer does (§B.1's `vidsoft_thread_context`).
- [ ] This keeps the "never more than one lock at a time" invariant true, so
      `shared_buffers.rs:22-24`'s ordering rule stays untriggered and the doc comment stays
      accurate. **Update that comment** only if a future phase genuinely needs two — do not
      pre-emptively weaken it.
- [ ] The snapshot is also the correctness mechanism for §A.16's per-line register latch: the
      per-line register array is a *sequence* of snapshots, so the same machinery serves both.
- [ ] Acquire in field-declaration order regardless (`vdp1_framebuffer` → `vdp2_vram` →
      `vdp2_cram` → `vdp2_regs`), so that if a future change does end up nesting two, it nests
      correctly by construction.

### Frame allocation

`render_backdrop` allocates a fresh `Framebuffer` (`vdp.rs:133`) every frame and `Arc`s it into
`vdp2_frame` (`lib.rs:231`). That is correct for the `ArcSwap` publish model and should stay — but
the *layer* buffers (six full-screen `PixelData` arrays, Phase 4) must be renderer-owned and
reused across frames, not reallocated. At 704×512 that is 6 × 360 448 × 8 bytes ≈ 17 MB of
per-frame allocation if done naively.

### Two clocks, one system

The most consequential non-obvious finding in this plan: **VBLANK is generated by Core 0**
(`sh2.rs:1674-1700`) **and frames are generated by Core 3** (`lib.rs:207-208`), from two
independent `Instant`-based ~60 Hz clocks. They are never reconciled. Every per-line feature in
VDP2 (§A.16 latching, line scroll, per-line back screen, per-line coefficients, `VCNT`, `HBLANK`)
requires them to be the same clock. Phase 1.5 is where that gets fixed, and it should not be
deferred into a later phase for convenience — everything after Phase 4 quietly assumes it.

---

## Deliberate simplification register

Per `CLAUDE.md`: where a real simplification is made, say so explicitly and keep behaviour honest.
These are the ones this plan commits to. Each should be mirrored into `CLAUDE.md`'s simplification
note as it lands, replacing the current "VDP2 backdrop-only rendering" entry.

| Simplification | Phase | Why |
|---|---|---|
| VRAM timeslot allocation is not enforced; every enabled layer renders as if its fetches succeed | 9 | §A.3/§B.14 item 1: the reference source does not model it either and states it cannot be derived from that source. A validator logs violations instead. |
| The Castlevania cycle-pattern signature match is not ported | 9 | Game-specific hack, not hardware (§A.3). |
| `Rbg0CheckRam`'s coefficient-bank workaround is not ported | 8 | Compensates for a defect Mimas does not have (§B.7). |
| The simplified compositing fast path is not implemented | 4 | Behaviourally equivalent to the general path by construction (§B.10); a second implementation is a drift risk, not a feature. |
| `ZMCTL`, `RPRCTL`, `OVPNRA`/`OVPNRB`, screen-over mode 1, `perline_alpha` are unimplemented | 5, 8 | Never read by any renderer in the reference (§B.14 items 2-4); no observable behaviour to implement. |
| `HCNT` is externally-latched only, with no free-running counter | 1 | §B.14 item 9 — a documented gap in the reference source, not a Mimas choice; recorded as such. |
| Byte/long VDP2 register access is real read/write storage, not Yabause's no-op | 1 | Yabause's behaviour there is a gap (§B.14 item 10); Mimas's is what makes real BIOS probing work. |

## Knowing divergences from the reference source

Cases where this plan deliberately does something *different from* Yabause because the reference
itself identifies the source as wrong or self-contradictory. Each needs a code comment naming the
reference section, so a future "let's match Yabause exactly" pass does not silently undo it.

| Divergence | Phase | Reference |
|---|---|---|
| 5→8 bit channel expansion replicates high bits (`0x1F → 0xFF`) instead of shifting (`0x1F → 0xF8`) | 1 | §0.1 |
| `RAMCTL` bank partition read from bits 8-9, not bits 4-5 | 1 | §A.2 (flagged contradiction) |
| `cpu_cycle` mapping tests the fresh local, not the stale output | 9 | §A.3 (identified bug) |
| Rotation `Px`/`Py`/`Pz`/`Cx`/`Cy`/`Cz` sign-extend with `0xFFFFC000` | 8 | §A.10 (identified copy-paste bug) |
| Vertical cell scroll applies per cell column, not per line | 5 | §B.6 (source's own comment says it is wrong) |
| Fractional scroll (`SCXDN*`/`SCYDN*`) is used, not discarded | 5 | §B.14 item 5 |
| Rotation Paths 2 and 3 both iterate `rbg0height` | 8 | §B.7 (source is inconsistent between paths) |
| Sprite window mask is indexed in screen coordinates on both sides | 7 | §B.13 (source mixes framebuffer and screen coordinates) |
| Layer rendering reads a snapshot, so `SPCTL` in shadow handling is consistent | 4 | §B.11 (source reads a global mid-render) |

## Open questions the reference cannot settle

Carry these forward as comments at the implementation site, not as decisions. Each needs real
hardware or a second independent emulator to resolve.

- [ ] Bitmap palette number scale: `<< 8` (renderer) vs `<< 4` (debug dump) — a factor of 16
      (§A.5).
- [ ] `CCCTL` bits 8-10: additive/bottom blend modes (renderer) vs gradation calculation
      (debug dump) — mutually exclusive readings (§A.14).
- [ ] `CCRLB`'s non-inverted, no-`+1` alpha derivation: deliberate or a bug (§A.14).
- [ ] `PLSZ` encoding 2: the source calls its own 1×1 mapping guesswork (§A.6).
- [ ] `SFPRMD` mode 3: labelled undocumented; treated as mode 0 (§A.13).
- [ ] The `deltaKAx = 0` forcing for parameter B when `RPMD == 2`: present in the float reader with
      a hardware-documentation citation, absent from the fixed-point reader actually used (§A.10).
- [ ] `colornumber == 4`'s byte order relative to `colornumber == 3` (§B.4).
- [ ] The window "outside" branch's vertical-overflow rule: an unexplained empirical observation
      (§B.8).

---

## Suggested execution order, condensed

1. **Phase 1** — register file, CRAM, real back screen, SPG. *Nothing renders differently except
   the backdrop, which becomes correct for the first time.*
2. **Phase 2** — one NBG3 cell on screen, pixel-exact. *First real VDP2 content in the project.*
3. **Phase 3** — the other three NBGs, five pixel formats, bitmap mode.
4. **Phase 4** — priority and colour calculation. *First point at which a real BIOS screen could
   plausibly composite correctly; run `milestone-tests/` here.*
5. **Phase 5** — scroll/zoom/line-scroll/mosaic/line colour screen.
6. **Phase 6** — windows.
7. **Phase 7** — sprite layer read-out; retires `vdp.rs:142-156`.
8. **Phase 8** — rotation.
9. **Phase 9** — cycle patterns (mostly a decision and a diagnostic).
10. **Phase 10** — thread split, gated on the VDP1 plan's double-buffer work.

Phases 1-4 are the BIOS-splash critical path. Phases 5-8 are what real games need. Phases 9-10 are
correctness-of-the-model rather than correctness-of-the-picture.
