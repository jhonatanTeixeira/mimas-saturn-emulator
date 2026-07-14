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
      used to need is gone, see `history.md` Chapter 7)
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
      of spinning (see `history.md` Chapter 7). This is *only* the spin
      fix, not the features below —
      both cores still have no real work; they're parked, ready to be
      reactivated via `set_thread_active(id, true)` once that work lands.
- [x] `WorkRam`'s monolithic lock split into one `RwLock` per field (see
      `history.md` Chapter 8) — a VDP2 register write and an SH-2 Work RAM
      read no longer contend on the same lock.
- [x] Real wall-clock CPU throttle (`saturn-core/src/throttle.rs`) pacing
      both SH-2s and the M68K against their real Saturn clock rates,
      live-configurable via `SaturnSystem::set_speed`/a `--speed` CLI flag
      on `saturn-frontend-native` (see `history.md` Chapter 9). Defaults to
      unthrottled, same as always — real speed is opt-in, not a new
      default.
- [x] Fixed a real spurious-wakeup bug found via per-thread CPU
      measurement (`/proc/<pid>/task/*/stat`, no `perf` in this
      environment): the parked Slave SH-2 and SCU DSP threads shared
      `LockStepSync`'s `Condvar` with `sync_core`'s per-instruction
      notifications, so they woke up (and re-contended for the shared
      mutex) millions of times a second instead of actually idling at zero
      CPU. Fixed with a separate `park_condvar`. Measured before/after:
      ~2.5 cores' worth of CPU for 2 cores' worth of real work, down to
      almost exactly 2 (see `history.md` Chapter 10). The 4 core threads
      also now have real OS-visible names
      (`sh2-master`/`sh2-slave`/`scu-dsp`/`vdp-scsp-m68k`) for future
      debugging.
- [x] **VBLANK-OUT interrupt** (SCU vector 0x41, level 0xE) — a real,
      separate interrupt from VBLANK-IN, not a duplicate of it. The
      `0x0601360A`/`0x060108ba` plateau flagged in the previous entry
      turned out to be a boot wait loop polling a RAM counter that only
      this BIOS's own VBLANK-OUT handler (found by resolving the BIOS's
      own two-level interrupt dispatch table, VBR-relative) ever
      increments. Cross-checked against Yabause's `scu.c::ScuSendVBlankOUT`
      / `vdp2.cpp::Vdp2VBlankOUT`. Confirmed against the real BIOS: Core
      0's PC moved from the old plateau to genuinely new territory
      (`0x06013264`), reaching the interrupt dispatcher regularly. See
      `history.md`'s latest chapter and `.development/current_blocker.md`.
- [x] **Real SCU DSP interpreter** (`saturn-core/src/scu_dsp.rs`) — Core 2
      (previously permanently parked, per the entry above) now actually
      executes DSP programs. Full ALU/Operation/Load-Immediate/Jump/Loop/
      End instruction groups, cross-checked instruction-by-instruction
      against Yabause's `scu.c`; 2 of real hardware's 8 DMA addressing-mode
      variants implemented (the 2 the real BIOS DSP program actually
      uses — the other 6 are a known, flagged gap, see the module's doc
      comment). Register ports (`0x80`/`0x84`/`0x88`/`0x8C`) intercepted at
      `Sh2::read_long`/`write_long` (real hardware: 32-bit-only ports) and
      shared with Core 2 via `Arc<Mutex<ScuDsp>>`; setting `EX` wakes Core
      2 via `sync.set_thread_active(2, true)`, mirroring the SSHON/SNDON
      reactivation shape. Verified two ways: a unit test running the exact
      captured real BIOS DSP program (32 words dumped from High RAM) to
      completion, and a real-BIOS boot run showing Core 0's PC moving from
      permanently stuck at `0x06013264` to hundreds of new addresses
      before settling at a new, different wall (see
      `.development/current_blocker.md`).

## In progress

- [ ] **M68K sound driver self-corruption** (see
      `.development/current_blocker.md`'s "Still open, but not confirmed
      to be a gate right now"). The uploaded driver's first memory-clear
      loop (`MOVE.L D0,(A0)+` / `DBRA D7,-4`, D7=$FFFF, A0=0) overwrites
      its own code partway through. Verified this isn't a decode bug in
      either instruction (checked byte-for-byte against Musashi).
      **Update 2026-07-13**: re-verified byte-for-byte unchanged through
      both the VBLANK-OUT fix and the SCU DSP fix — two independent walls
      have now cleared without this bug moving, so it's increasingly
      unlikely to be gating overall boot progress, but it's still a real,
      unfixed bug that will need attention eventually (likely before audio
      works at all). Next step if picked back up: instrument the SH-2's
      Sound RAM writes between the SNDON-adjacent code and the M68K reset
      to see the *actual* intended setup values, rather than continuing to
      infer them from a static dump taken after the fact.
- [ ] **New stall past the SCU DSP fix**, not yet root-caused (2026-07-13,
      see `.development/current_blocker.md`): a real-BIOS run now settles
      at `0x060131A8`, inside a bounded-looking counted loop, after
      visiting hundreds of new addresses. The one subroutine call inside
      that loop (`0x06013344`) turned out to be an unrelated generic
      32-bit software-division routine, not the cause. Next step: the
      usual work loop — fresh High RAM dump at the exact stuck PC, decode
      what it's actually waiting on, cross-check Yabause before touching
      anything.
- [ ] **Vision-based "CD Player screen" milestone test**
      (`mimas/milestone-tests/`, `tests/cd_player.rs`) — implemented but
      deliberately not yet run. Boots the real BIOS headlessly, samples
      `SaturnSystem::vdp2_frame` over a bounded window, and checks CLIP
      (`candle-transformers`, model `openai/clip-vit-base-patch32`)
      cosine similarity against a real reference screenshot
      (`fixtures/cd_player_screen.jpg`) of the Saturn BIOS's CD Player
      screen. Deliberately **not** a workspace member (own `Cargo.toml`,
      own lockfile) so `cargo test --workspace` stays fast/deterministic —
      this test needs a real, user-supplied BIOS (`MIMAS_BIOS_PATH`) and
      downloads a ~600MB CLIP model on first run. Worth actually running
      once M4 (VDP2 tile rendering) and M5 (VDP1 sprite rendering) exist —
      today it would just reconfirm the known-failing state (only the flat
      backdrop renders). The similarity threshold (`0.85`) is an
      uncalibrated placeholder, flagged as such in the test itself;
      recalibrate once a real passing frame exists to compare against.

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
      the opcode's match arm in `sh2.rs`) — real dual-CPU spinlock code
      over shared Work RAM needs both.
- [ ] CD-ROM wired into the CPU address space / SMPC CD command protocol
      — see `ROADMAP.md` M7. Out of scope for the current "BIOS logo,
      no CD" goal.
- [ ] Remaining rare SH-2 addressing-mode combinations not yet decoded
      (see the tail comment in `Sh2::execute()`) — add opportunistically
      when actually hit, not preemptively.
