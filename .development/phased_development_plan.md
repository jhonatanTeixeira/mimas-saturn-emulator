# Phased Development Roadmap — execution order for `docs/implementation-plans/`

This is the **cross-subsystem execution order** for the eight phased plans in
`docs/implementation-plans/` (`sh2-cpu.md`, `memory-bus.md`, `scu.md`, `smpc-peripheral.md`,
`vdp1.md`, `vdp2.md`, `scsp.md`, `cs2-cdblock.md`). Each of those files already has its own
internal phase order and full detail (exact registers/opcodes/tests, `file:line` pointers); this
document doesn't repeat that — it says **which subsystem's phases to run when**, and why, based
on real dependencies between the plans (not an arbitrary list order).

Status legend: ✅ done · 🟡 in progress / partially done · ⬜ not started.

For **why** each completed phase was implemented the way it was, see `history.md` (Chapter 14
covers SMPC Phases 0-2). Keep this file's checkmarks and `docs/implementation-plans/*.md`'s own
`- [x]` checklists in sync — see `CLAUDE.md`'s "Tracking docs" section.

---

## How this order was decided

Three real dependencies drive the ordering, all found while writing the implementation plans
themselves, not guessed:

1. **CPU and memory-bus correctness gate everything else.** A wrong opcode (the confirmed SH-2
   `OR`/`XOR` swap) or a wrong address decode (the confirmed missing stage-1 area-select, which
   aliases the SH-2's own cache scratchpad to BIOS ROM) can silently corrupt *any* other
   subsystem's behavior in a way that looks like that subsystem's own bug. Fix the ground first.
2. **SCU's interrupt controller and DMA controller are load-bearing for VDP1 and CS2.** VDP1's
   Draw End interrupt and CS2's sector-to-RAM transfer both explicitly depend on SCU
   infrastructure that doesn't exist yet (`docs/implementation-plans/vdp1.md`'s Draw End
   call-out, `cs2-cdblock.md`'s Architectural call-out A). Building SCU's controllers before
   those two avoids building throwaway ad hoc interrupt/DMA paths that need to be ripped out
   later.
3. **The real BIOS boot trace says where the *next* wall actually is.** `smpc-peripheral.md`
   §0.6 (a real, live 240-second capture) shows the boot sequence proceeding past SMPC into VDP2
   register writes and then into CS2/CD-block polling, where it currently cycles in a CD header
   string-compare — and `cs2-cdblock.md`'s own audit found the existing "CD command" handling in
   `sh2.rs` is entirely fictional (wrong trigger, wrong offsets). That makes CS2 the most likely
   *next* thing standing between here and further real boot progress, ahead of finishing VDP2's
   full compositing (which the BIOS doesn't appear to need yet just to get past this wall).

One coordination point worth flagging now rather than rediscovering later: **`smpc-peripheral.md`
Phase 6 and `cs2-cdblock.md`'s Core 7 work both target the same thread** (`smpc-cd-block`, Core
7 — it's named for hosting both). Land them together, not as two independent migrations onto the
same core.

---

## Milestone 1 — CPU and memory-bus correctness

Nothing else's correctness claims are trustworthy until this lands — nine phases.

- [x] `sh2-cpu.md` Phase 1 — Fix opcodes that are wrong today (the `OR`/`XOR` swap; three other
      confirmed defects)
- [x] `sh2-cpu.md` Phase 2 — Add the 9 missing opcodes (`SLEEP`, `BRAF`/`BSRF`, `MAC.L`/`MAC.W`,
      four `#imm,@(R0,GBR)` forms)
- [x] `sh2-cpu.md` Phase 3 — Exceptions and the address-space holes
- [x] `memory-bus.md` Phase 0 — Instrument the decode before changing it
- [x] `memory-bus.md` Phase 1 — Stage-1 area decode (`addr >> 29`) — fixes the cache-scratchpad
      aliasing to BIOS ROM
- [x] `memory-bus.md` Phase 2 — Correct device sizes and per-region mirror periods
- [x] `memory-bus.md` Phase 3 — Width-atomic, single-lock region accessors
- [x] `sh2-cpu.md` Phase 4 — On-chip register file: storage, reset values, byte/word/long dispatch
- [x] `sh2-cpu.md` Phase 5 — Real interrupt controller (prerequisite for SCU's own interrupt
      controller in Milestone 2 to signal into something real)
- [x] `sh2-cpu.md` Phase 6 — DIVU hardening (fixes the confirmed `i32::MIN / -1` process panic)

---

## Milestone 2 — SCU infrastructure (interrupts, DMA, DSP completion)

Six phases, in the order `scu.md` itself lays out — its own internal dependency chain (DSP →
register file → interrupt controller → DMA controller → timers → wiring it all together) is
already correct and doesn't need reshuffling against the other subsystems.

- [x] `scu.md` Phase 1 — Finish the SCU DSP (the remaining 6 of 8 DMA addressing modes, and the
      confirmed opcode-mask bug that makes every `JMP` execute a phantom ALU op)
- [x] `scu.md` Phase 2 — Real SCU register file (repurposed `scu.rs` into a typed 256-byte
      register file with real reset values and read/write visibility rules, plus `irq`/`dma`/
      `timers` scaffolding for Phases 3-5; `WorkRam::scu_regs` retired). See `history.md`
      Chapter 29.
- [x] `scu.md` Phase 3 — SCU interrupt controller (single `Scu`-owned controller replacing the
      four ad hoc `Sh2` bools; `Sh2::irq_in` became a plain always-present `Arc`; VBLANK
      generation centralized on Core 3, fixing a real dual-clock bug; M68K sound-request path
      made event-driven, removing a per-instruction poll). See `history.md` Chapter 30.
- [x] `scu.md` Phase 4 — Independent SCU DMA controller on Core 6 (real budgeted engine —
      direct/indirect/fill/copy modes, per-burst `BusArbiter` locking, `LockStepSync`
      integration — replacing the synchronous `Sh2::execute_scu_dma` stand-in; fixes
      D-DMA-1/2/3/4/5/6/7). See `history.md` Chapter 31.
- [x] `scu.md` Phase 5 — SCU timers 0 and 1 (Timer 0 scanline compare, Timer 1 down-counter
      driven by real Master SH-2 cycles, deviation #18 fixed not copied). H-Blank IN/V-Blank
      IN/OUT generation also moved off Core 3's wall-clock loop onto the same cycle-driven
      source, making Core 3 genuinely event-driven for the first time. See `history.md`
      Chapters 33-34.
- [x] `scu.md` Phase 6 — Start factors, DSP End, Draw End: closing the loop (all 7 DMA start
      factors 0-6, DSP End/`ENDI` interrupt wiring). Sprite Draw End's SCU-side entry point
      (`draw_end()`) is done and tested, but nothing calls it yet — VDP1's command-list
      completion trigger is still deliberately deferred to `vdp1.md` Phase 3, which this
      unblocks. See `history.md` Chapter 35.

---

## Milestone 3 — Clear the CD-block wall

This is the empirically-next wall per the live BIOS trace (see "How this order was decided"
above). `cs2-cdblock.md`'s own phase order already sequences correctly; Phase 4 needs Milestone
2's SCU DMA controller.

- [x] `cs2-cdblock.md` Phase 1 — Wire the CD block into the system and decode CS2 for real
      (this is also where Core 7 gets its first real work — replace its permanent busy-spin
      placeholder loop with real `park_while_inactive` use, see
      `../docs/lessons-from-yabasanshiro.md` §1) — **done**, see `history.md` Chapter 36
- [x] `cs2-cdblock.md` Phase 2 — The CR1-4/HIRQ handshake, and the commands BIOS boot probes with
      (this is the specific phase most likely to actually move the wall) — **done**, see `history.md` Chapter 36
- [x] `cs2-cdblock.md` Phase 3 — Drive/disc state machine and the two free-running engines
      (size their dispatch cadence to SMPC/CD-block's own real thresholds, not the generic
      per-core clamp used today — see `../docs/lessons-from-yabasanshiro.md` §6, including the
      `if`-vs-`while` threshold-check pitfall a sibling project already hit here) — **done**, see `history.md` Chapter 36
- [x] `cs2-cdblock.md` Phase 4 — Sector buffers, partitions, filters, and getting data to the CPU
      (depends on `scu.md` Phase 4) — **done**, see `history.md` Chapter 36
- [x] `cs2-cdblock.md` Phase 5 — Playback, seek, scan, subcode, CDDA *(game compatibility —
      lower priority than 1-4)* — before/while landing this, replace `Cdrom`'s single-hunk cache
      (`cdrom.rs`'s `current_hunk_num`) with a small LRU: CDDA interleaved with data reads is
      exactly the access pattern that thrashes a 1-slot cache, per
      `../docs/lessons-from-yabasanshiro.md` §2 — **done**, see `history.md` Chapter 36
- [x] `cs2-cdblock.md` Phase 6 — Filesystem commands and IP.BIN *(game compatibility)* — **done**, see `history.md` Chapter 36
- [x] `cs2-cdblock.md` Phase 7 — Remaining commands: MPEG stubs, FAD search, MPEG ROM *(game
      compatibility, lowest priority in this file)* — **done**, see `history.md` Chapter 36

---

## Milestone 4 — Finish SMPC & peripherals

All eight phases landed; Phase 7 has two explicitly deferred sub-items (multi-tap, the live
gun/VDP2 latch path) rather than being fully closed out.

- [x] `smpc-peripheral.md` Phase 0 — Extract a real `Smpc` type (pure refactor) — **done**,
      see `history.md` Chapter 14
- [x] `smpc-peripheral.md` Phase 1 — Register-file discipline, the SF handshake, and the
      commands the real BIOS issues — **done**, except SSHOFF resetting the slave core
      (deliberately deferred — see the phase's own notes on the `Sh2::run_loop` active-state
      gap it surfaced)
- [x] `smpc-peripheral.md` Phase 2 — Complete the INTBACK status block: RTC, SMEM, region,
      system flags — **done**
- [x] `smpc-peripheral.md` Phase 3 — The remaining commands: NMIREQ, MSHON, CDON/CDOFF, SYSRES,
      CKCHG320/352 (CDON/CDOFF are meaningful only once Milestone 3 has wired the CD block in —
      fine to implement as accepted no-ops before that, per the phase's own notes)
- [x] `smpc-peripheral.md` Phase 4 — Peripherals: the digital pad, the INTBACK peripheral path,
      and frontend input (the single largest functional gap — nothing in the workspace can
      express "a button is pressed" today)
- [x] `smpc-peripheral.md` Phase 5 — The direct-access port: PDR1/PDR2, DDR1/DDR2, IOSEL, EXLE
- [x] `smpc-peripheral.md` Phase 6 — Real command timing, and moving the SMPC onto Core 7
      (**coordinate with `cs2-cdblock.md`'s own Core 7 work** — same physical thread, see above.
      Also the point to size SMPC's own dispatch cadence to its real ~83µs timing rather than
      the generic per-core clamp — see `../docs/lessons-from-yabasanshiro.md` §6)
- [x] `smpc-peripheral.md` Phase 7 — Extended peripheral types: wheel, mission stick, 3D pad,
      twin sticks, mouse, keyboard, and a gun's status-only presence *(game compatibility)* —
      **mostly done**; multi-tap (dynamic 6-slot chaining) and the live gun/VDP2 external-latch
      position path are explicitly deferred as genuinely separate pieces of work, not partial
      versions of what's listed. See `history.md` Chapter 37 and the phase's own checklist for
      exactly what's done vs. deferred, item by item.

---

## Milestone 5 — Real video output (VDP1 + VDP2)

The largest milestone. VDP1's framebuffer work (Phase 2) and VDP2's own Phase 10 are explicitly
coupled in both plans — do the shared thread-topology move once, at the end of this milestone,
not twice.

- [ ] `vdp1.md` Phase 0 — Fix what's already there (the confirmed command-table offset bugs,
      the `PTMR`/`TVMR` confusion, the end-bit-checked-after-drawing bug — the existing test
      was built on top of these)
- [ ] `vdp1.md` Phase 1 — Engine state, live registers, and the state-setting commands
- [ ] `vdp2.md` Phase 1 — Foundations: register file, CRAM, the real back screen, and a line
      counter (fixes the confirmed `BKTAL`-is-half-a-VRAM-address bug — the existing backdrop
      test asserts the bug)
- [ ] `vdp1.md` Phase 2 — Framebuffer geometry, two banks, erase and swap
- [ ] `vdp1.md` Phase 3 — Draw End status and interrupt (needs `scu.md` Phase 6)
- [ ] `vdp1.md` Phase 4 — The rasteriser, Normal Sprite, and colour-mode decode
- [ ] `vdp2.md` Phase 2 — One NBG layer, the simplest format, pixel-exact
- [ ] `vdp1.md` Phase 5 — The remaining textured shape commands
- [ ] `vdp2.md` Phase 3 — Remaining NBG layers and every character/bitmap format
- [ ] `vdp1.md` Phase 6 — Colour calculation, gouraud, mesh, MSB
- [ ] `vdp2.md` Phase 4 — Priority resolution and colour calculation
- [ ] `vdp1.md` Phase 7 — Line, Polyline, end codes, flip
- [ ] `vdp2.md` Phase 5 — Scroll, zoom, line scroll, vertical cell scroll, mosaic, and the line
      colour screen
- [ ] `vdp2.md` Phase 6 — Windows
- [ ] `vdp2.md` Phase 7 — Sprite layer read-out (the VDP1 boundary — needs `vdp1.md` Phase 2's
      two-bank framebuffer)
- [ ] `vdp1.md` Phase 8 — 8-bit framebuffer, interlace, and the long tail *(game compatibility)*
- [ ] `vdp2.md` Phase 8 — RBG0/RBG1 rotation
- [ ] `vdp2.md` Phase 9 — VRAM access cycle patterns: what to actually build (if this ends up
      wanting a frame-to-frame render cache as a Mimas-original optimization — not hardware, see
      this milestone's own "Not ported" call-outs for `vdp1_clock`/its dirty bitmap — see
      `../docs/lessons-from-yabasanshiro.md` §4 for the invalidation gotchas a sibling project
      already learned the hard way)
- [ ] `vdp1.md` Phase 9 — Move VDP1 to Core 2 **+** `vdp2.md` Phase 10 — Thread topology — **do
      these together**, last in this milestone. This is also the point to replace Core 2's
      permanent busy-spin placeholder loop with real `park_while_inactive` use — see
      `../docs/lessons-from-yabasanshiro.md` §1

---

## Milestone 6 — Audio (SCSP)

Doesn't block visual boot progress, so it trails — but Phase 1's register-offset bugs are real,
silent correctness bugs worth an early pass on their own merits.

Cross-cutting note for whoever lands this milestone: Cores 4 (`m68k-sound-cpu`) and 5
(`scsp-synth`) both currently loop via `thread::yield_now()` regardless of whether they have
real work — Core 4 already has the right gate (`should_run`) and just needs to call
`set_thread_active`/`park_while_inactive` on its transitions instead of spinning while stopped;
Core 5 has continuous real work but no `ClockThrottle` of its own, unlike Core 4. See
`../docs/lessons-from-yabasanshiro.md` §1 for the full comparison against a near-identical bug
(measured at ~30% of an entire emulator's CPU) a sibling project already hit and fixed.

- [ ] `scsp.md` Phase 1 — Fix the register decode in the existing playback path (confirmed SA/
      FNS/TL offset bugs; TL's attenuation sense is inverted, so max volume is silence today)
- [ ] `scsp.md` Phase 2 — Slot pipeline structure
- [ ] `scsp.md` Phase 3 — Envelope generator
- [ ] `scsp.md` Phase 4 — LFO
- [ ] `scsp.md` Phase 5 — Common Control Registers: MVOL/RBL/RBP, monitor, timers, interrupts, DMA
- [ ] `scsp.md` Phase 6 — Sound DSP (`scspdsp.c` model)
- [ ] `scsp.md` Phase 7 — M68K-side memory map and interrupt handshake completion
- [ ] `scsp.md` Phase 8 — Remaining (deferrable)

---

## Backlog — game-compatibility-only, not on the critical path to boot

These are real, tracked gaps with full detail in their own plan files, but nothing in the boot
trace or the milestones above depends on them. Pick up any time; order among themselves doesn't
matter much.

- [ ] `memory-bus.md` Phase 4 — A-Bus open bus, CS0/CS1 windows, FRT capture
- [ ] `memory-bus.md` Phase 5 — Internal backup RAM fidelity
- [ ] `memory-bus.md` Phase 6 — One decoder for the whole system (consolidation/cleanup)
- [ ] `memory-bus.md` Phase 7 — Cartridge models
- [ ] `memory-bus.md` Phase 8 — Access cost model and A-Bus timing *(optional — the plan itself
      says last)* — if revisited for performance rather than correctness, see
      `../docs/lessons-from-yabasanshiro.md` §3: `sh2.rs`'s `add_wait_states_r`/`_w` already use
      the right accumulate-in-place design a sibling project had to retrofit; a precomputed
      per-region lookup table is the only further idea there, and a low-priority one
- [x] `sh2-cpu.md` Phase 7 — Free-Running Timer (FRT)
- [x] `sh2-cpu.md` Phase 8 — Per-opcode cycle costs and memory wait states
- [x] `sh2-cpu.md` Phase 9 — On-chip DMA controller (DMAC)
- [ ] `sh2-cpu.md` Phase 10 — Watchdog Timer (WDT)
- [ ] `sh2-cpu.md` Phase 11 — Cache model
- [ ] `sh2-cpu.md` Phase 12 — Storage-only peripherals: SCI, SBYCR, BSC refresh
- [ ] `sh2-cpu.md` Phase 13 — User Break Controller (UBC)
- [ ] `sh2-cpu.md` Phase 14 — Dual-core specifics

---

## Deliberately deferred (not a phase, a standing gap)

- **SSHOFF doesn't actually reset the slave SH-2.** Flagged in `smpc-peripheral.md` Phase 1:
  fixing it properly requires `Sh2::run_loop` to check its own core's active/inactive state at
  all (it currently only checks the global shutdown flag), which is a real concurrency change,
  not a register-level fix. Needs its own session; see `history.md` Chapter 14's closing note.
- **SH-2 idle/spin-loop detection.** A sibling project (yabasanshiro) found that fast-forwarding
  busy-wait polling loops (VBlank/HBlank/DMA-completion flags — extremely common in Saturn game
  code) instead of interpreting them cycle-by-cycle was likely its single biggest interpreter
  win. Not a drop-in here: a large instantaneous cycle jump has to interact correctly with
  `LockStepSync`'s bounded-slack accounting, and interrupts can arrive from other real OS threads
  asynchronously in Mimas's model — not just from the same thread's own local peripherals as in
  yabause's single-threaded one. Worth prototyping once boot progress is far enough along that
  interpreter throughput (rather than correctness) is the bottleneck. See
  `../docs/lessons-from-yabasanshiro.md` (no numbered section — it doesn't map to any existing
  phase there either).
- **GPU compute-shader fallback pattern, if a GPU renderer is ever added.** yabasanshiro's RBG
  compute-shader path used to `abort()` the whole process on a driver shader-compile/link
  failure; fixed to fall back to the non-compute path instead, which is what made it safe to
  default on across varied GPU drivers. Not applicable today — `saturn-frontend-native` is
  `minifb`-backed, CPU framebuffer only, no GPU backend exists — but worth applying the same
  "never let a compile failure take the process down" rule if one is ever built. See
  `../docs/lessons-from-yabasanshiro.md` §5.
