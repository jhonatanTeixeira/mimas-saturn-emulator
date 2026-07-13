# Mimas task list

Granular companion to `ROADMAP.md`'s milestone view. Update both when
status changes — this file for the specific items, `ROADMAP.md` for the
milestone they roll up into.

## Done

- [x] `BusArbiter` / `LockStepSync` 4-core synchronization primitive
- [x] `SaturnSystem` thread topology (Core 0-3) matching real hardware
- [x] Real SH-2 physical memory map (cross-checked against Yabause
      `memory.c`)
- [x] Real SH-2 opcode decode: arithmetic/logic, common addressing modes,
      branches with delay slots, DIV0S/DIV0U/DIV1, TRAPA, memory-indirect
      LDS.L/STC.L/LDC.L/STS.L
- [x] Real SH-2 interrupt exception entry (push SR/PC, VBR vector table,
      SR interrupt mask)
- [x] VBLANK-IN interrupt (vector 0x40, level 15), real ~60Hz wall-clock
      pacing
- [x] TVSTAT computed live from frame timing (was a static always-0 byte)
- [x] SMPC SF always reads idle (was a hung-forever nonzero placeholder)
- [x] SMPC real INTBACK command processing (OREG0-11, region default,
      SR) + SCU System Manager interrupt (vector 0x47, level 8)
- [x] Sound RAM / SCSP register regions as real read/write memory
- [x] Real M68000 interpreter (`m68k.rs`): registers, MOVE family,
      arithmetic/logic, branches (Bcc/BRA/BSR/DBcc), JSR/JMP/RTS, MOVEM,
      LINK/UNLK, shifts, bit ops, TAS — cross-checked against Musashi
- [x] Real M68K↔SCSP register address decode (Sound RAM < 0x080000,
      SCSP registers ≥ 0x100000, both dual-ported with the SH-2's own
      view of the same physical registers)
- [x] Real SNDON/SNDOFF (M68K reset/halt) wired to Core 3, via a real
      `Ordering::Release`/`Acquire` signal (the wall-clock debounce this
      used to need is gone, see `TECH_DEBT.md` Progress notes)
- [x] Real SCSP→SH-2 "Sound Request" interrupt (MCIPD/MCIEB → SCU vector
      0x46, level 9)
- [x] VDP2 backdrop-only rendering pipeline (`vdp.rs`), real window
      presentation via `minifb` (`mimas_window` binary)
- [x] `tools/sh2dis.py`: reusable Python SH-2 disassembler for offline
      RAM-dump analysis (opcode table kept in sync with `sh2.rs` by hand)
- [x] Documentation reorganized: `docs/` holds `PROJECT.md`,
      `saturn_architecture_report.md`, `ORIGINAL_REQUEST.md`,
      `TEST_INFRA.md`, `TEST_READY.md`; `.development/` holds this file
      and `ROADMAP.md`; `history.md` at the mimas root tells the story;
      stale multi-agent-swarm orchestration artifacts (`.agents/` at the
      repo root) removed.
- [x] Core 1 (Slave SH-2) and Core 2 (SCU DSP slot) idle-spin fixed — both
      now park at zero CPU via `LockStepSync::park_while_inactive` instead
      of spinning (see `TECH_DEBT.md` Progress notes, `history.md`
      Chapter 7). This is *only* the spin fix, not the features below —
      both cores still have no real work; they're parked, ready to be
      reactivated via `set_thread_active(id, true)` once that work lands.
- [x] `WorkRam`'s monolithic lock split into one `RwLock` per field (see
      `TECH_DEBT.md` Progress notes, `history.md` Chapter 8) — a VDP2
      register write and an SH-2 Work RAM read no longer contend on the
      same lock.

## In progress

- [ ] **M68K sound driver self-corruption** (see `CLAUDE.md`'s "Current
      wall"). The uploaded driver's first memory-clear loop
      (`MOVE.L D0,(A0)+` / `DBRA D7,-4`, D7=$FFFF, A0=0) overwrites its
      own code partway through. Verified this isn't a decode bug in
      either instruction (checked byte-for-byte against Musashi). Next
      step: instrument the SH-2's Sound RAM writes between the
      SNDON-adjacent code and the M68K reset to see the *actual* intended
      setup values, rather than continuing to infer them from a static
      dump taken after the fact.

## Not started

- [ ] VDP2 NBG0-3 tile/bitmap layer decoding (pattern names, character
      data, CRAM lookup) — see `ROADMAP.md` M4. Likely required for the
      actual Saturn logo graphic, not just a background color.
- [ ] VDP1 sprite/polygon rendering — see `ROADMAP.md` M5. Defer until
      M4 shows whether the BIOS boot path touches VDP1 at all.
- [ ] Slave SH-2 (Core 1) real SSHON-triggered boot — see `ROADMAP.md`
      M6. Not yet observed being issued in a real traced boot; implement
      once a trace shows it. Core 1 is already parked
      (`LockStepSync::park_while_inactive`) with `bios`/`reset()` wiring in
      place in `SaturnSystem::start()` — SSHON's handler just needs to call
      `sync.set_thread_active(1, true)` to bring it up. Do this alongside
      (not after) fixing `TAS.B`'s read-then-write race gap (flagged at
      the opcode's match arm in `sh2.rs`, see `TECH_DEBT.md` item 1) —
      real dual-CPU spinlock code over shared Work RAM needs both.
- [ ] CD-ROM wired into the CPU address space / SMPC CD command protocol
      — see `ROADMAP.md` M7. Out of scope for the current "BIOS logo,
      no CD" goal.
- [ ] Remaining rare SH-2 addressing-mode combinations not yet decoded
      (see the tail comment in `Sh2::execute()`) — add opportunistically
      when actually hit, not preemptively.
- [ ] SCU DSP (Core 2 is currently parked, no DSP logic implemented) — not
      yet known to be required for the BIOS boot path; re-evaluate if a
      future wall traces back to it. Reactivate via
      `sync.set_thread_active(2, true)` once real DSP execution exists.
