# Mimas roadmap

Tracks what's done and what's left, organized by milestone. The end goal
driving all of this: **a real Sega Saturn BIOS boots in this emulator far
enough that its splash screen renders in an actual window** (no CD-ROM
required — BIOS-only boot). Update this file whenever a milestone's status
changes; it's the first thing a new agent/session should read after
`CLAUDE.md`.

Status legend: ✅ done · 🟡 in progress · ⬜ not started

---

## M0 — Architecture scaffold ✅

- `BusArbiter` + `LockStepSync`: 4-core lockstep synchronization primitive
  matching the real hardware's distributed-CPU design.
- `SaturnSystem`: spawns Core 0 (Master SH-2), Core 1 (Slave SH-2, held in
  reset until real SSHON support exists — see M6), Core 2 (SCU DSP/SMPC/
  CD-ROM, currently a no-op cycle counter), Core 3 (VDP1/VDP2/SCSP,
  real VDP2 backdrop rendering + the M68K core as of M3).
- Originally built by a multi-agent swarm; see `history.md` Chapter 0.

## M1 — Real SH-2 CPU interpreter ✅

- Full physical memory map (BIOS ROM, Low/High Work RAM, SMPC, Backup RAM,
  Sound RAM, SCSP registers, VDP1/VDP2 VRAM+registers, SCU registers, CS2)
  cross-checked against Yabause's `memory.c`, not guessed.
- Broad SH-2 instruction coverage: arithmetic/logic, all addressing modes
  in common use, branches with correct delay-slot semantics, DIV0S/DIV0U/
  DIV1, TRAPA, memory-indirect LDS.L/STC.L/LDC.L/STS.L forms.
- Real interrupt exception entry (push SR then PC, VBR-relative vector
  table, SR interrupt mask).
- 40+ unit tests in `saturn-core/src/sh2.rs`'s `mod opcode_tests`, several
  regression tests using real BIOS bytes or hand-traced algorithm steps
  (not self-consistent assertions — see `history.md` Chapter 2 for why
  that distinction mattered twice already).
- **Known gap, not yet hit in practice**: a handful of rarer addressing-
  mode combinations are still undecoded (see the comment at the end of
  `Sh2::execute()` for the exact list). Add them the same way as
  everything else — hit the wall, decode, cross-check Yabause, implement,
  test.

## M2 — BIOS boot handshakes ✅

- VBLANK-IN interrupt (vector 0x40, level 15), real wall-clock ~60Hz
  pacing.
- TVSTAT (VDP2 status register) computed live from frame timing instead of
  a static byte — real BIOS code polls its VBLANK bit directly.
- SMPC SF (status flag) always reads idle; real INTBACK (COMREG 0x10)
  command processing (OREG0-11 populated per real semantics, region
  defaults to Japan) plus the SCU System Manager interrupt (vector 0x47,
  level 8) it triggers on completion.
- Sound RAM/SCSP register regions wired up as real read/write memory
  (previously unmapped, breaking the BIOS's write-then-verify pattern).

**Result**: BIOS boot PC reliably reaches well past early hardware-
detection code into real application-level boot logic (video/audio driver
setup).

## M3 — SCSP M68000 sound CPU 🟡

- Real M68000 interpreter (`saturn-core/src/m68k.rs`): registers, common
  addressing modes, MOVE family, arithmetic/logic, Bcc/BRA/BSR/DBcc,
  JSR/JMP/RTS, MOVEM, LINK/UNLK, shifts, bit ops (BTST/BSET/BCLR/BCHG),
  TAS. Cross-checked opcode-by-opcode against Musashi
  (`yabause/src/musashi/`) where encoding was in doubt.
- Real M68K address decode: < 0x080000 is Sound RAM (dual-ported with the
  SH-2's own 0x05A00000 view), ≥ 0x100000 is the SCSP register block
  (dual-ported with the SH-2's 0x05B00000 view) — confirmed against
  Yabause's `c68k_byte_read`/`c68k_byte_write`.
- Real SNDON/SNDOFF (`M68KStart`/`M68KStop` equivalent): resets/halts the
  M68K core, with a debounce on the reset edge for the SH-2-upload race
  (see `history.md` Chapter 4).
- Real SCSP→SH-2 "Sound Request" interrupt (MCIPD/MCIEB → SCU vector
  0x46, level 9) — confirmed this alone moves the BIOS wall further.
- 🟡 **Open**: the M68K core derails partway through the real uploaded
  driver's first memory-clear loop (self-modifying-code interaction, not
  a decode bug — see `CLAUDE.md`'s "Current wall" for the full trace and
  the concrete next diagnostic step). Once this clears, re-run the boot
  loop (`CLAUDE.md`'s methodology) to find the next wall.

## M4 — VDP2 tile rendering ⬜

- Currently only the backdrop (solid color) layer renders
  (`saturn-core/src/vdp.rs::render_backdrop`) — explicitly the smallest
  real rendering step, deliberately built to prove register writes reach
  pixels before the bigger pipeline exists.
- Missing: NBG0-3 tile/bitmap layer decoding (pattern names, character
  data fetch, CRAM color lookup — the real `Vdp2MapCalcXY`/
  `Vdp2PatternAddr`/`Vdp2FetchPixel`/`Vdp2ColorRamGetColorSoft` pipeline in
  Yabause's `vidsoft.c`, preferred over `vidogl.c` for readability).
- **Likely required for the Saturn logo itself** — the BIOS splash is
  probably an NBG bitmap/tile layer, not just a backdrop color. Should be
  tackled once BIOS execution reaches actual VDP2 layer setup code (may
  require M3 to be unblocked first, if the boot sequence is linear).

## M5 — VDP1 sprite/polygon rendering ⬜

- VDP1 VRAM/framebuffer/registers are mapped as real memory
  (`shared_buffers.rs`) but nothing reads them for rendering yet.
- May or may not be needed for the BIOS logo specifically (real Saturn
  BIOS splash may be VDP2-only) — defer investigating until M4 shows
  whether VDP1 is actually touched during the relevant boot phase.

## M6 — Slave SH-2 (Core 1) boot ⬜

- Core 1 is intentionally never reset/given BIOS code — matches real
  hardware (slave stays halted until the master issues SMPC's SSHON,
  COMREG 0x02).
- Not yet observed in a real traced boot run (see `CLAUDE.md`'s note: no
  SSHON was issued in the traces gathered so far). Implement the real
  `M68KStart`-style reset-on-SSHON handling for Core 1's `Sh2` the same
  way M3 did for the M68K core, if/when a real boot trace shows SSHON
  being issued.

## M7 — CD-ROM / game loading ⬜

- `Cdrom` (`saturn-core/src/cdrom.rs`) can open and read sectors from a
  `.chd` file already (real CHD hunk reads, not mocked), but isn't wired
  into the CPU's address space (CS2 region is a plain memory stub) or
  into the SMPC CD-ADDR/status command protocol.
- Out of scope for "see the BIOS logo" (explicitly BIOS-only boot, no CD
  needed) — relevant once the goal shifts to booting an actual game.

---

## How to pick this up

1. Read `CLAUDE.md` for the work loop and the exact current wall.
2. Read `history.md` for how we got here and why specific non-obvious
   decisions were made.
3. Check this file's milestone statuses — anything 🟡 is the most likely
   next place to spend effort; anything ⬜ is likely blocked on a 🟡 above
   it unless noted otherwise.
4. `cargo test --workspace` must stay green (125 tests as of this
   writing: 71 E2E + saturn-core's unit tests + 7 adversarial + 4 sync)
   after every change.
