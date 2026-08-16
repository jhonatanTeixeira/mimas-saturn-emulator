# GEMINI.md - Instructions and Workspace Guide for Mimas

This file provides system context, build/test commands, and guidelines for AI agents (specifically Antigravity / Gemini) working on the **Mimas** Sega Saturn emulator project. It is a condensed companion to `CLAUDE.md` (the full reference used by Claude Code) — when the two disagree, or when a section here feels thin, check `CLAUDE.md` first; it is kept current with every landed change and this file is not always updated in lockstep.

---

## 🚨 The one rule that must never be broken

**Only Core 0 (`sh2-master`) and Core 1 (`sh2-slave`) may run a continuous loop.** Every other component thread parks (`LockStepSync::park_while_inactive`) and is woken only by a real hardware-equivalent event — a register write, an interrupt, or (for video timing) Master SH-2's own cycle-driven progress. **Zero polling loops. Zero wall-clock timers (`Instant::now()`, `thread::sleep` on a fixed interval) anywhere outside the CPU cores' own clock-rate pacing (`ClockThrottle`).**

This is not a style preference — it was violated once (a sleep-paced "improvement" to a component thread), and it silently throttled the *entire emulator* by roughly two orders of magnitude via `LockStepSync`'s bounded-slack model, because a slow-reporting *active* thread drags every other active thread down with it. The fix was making that thread genuinely *inactive* (parked) instead of *active-but-slow*. Real hardware timing (VBLANK, H-Blank, SCU timers) must be derived from Master SH-2's own executed-cycle count, exactly like the reference emulator's main loop does (`yabsys.LineCount` tied to `sh2cycles`, never a host clock) — never from a wall-clock deadline.

If you cannot implement a component this way, stop and flag it rather than shipping a polling/sleep-based stand-in — it will be treated as a regression, not a working version, regardless of what tests show.

---

## 🧵 The 8 threads (`SaturnSystem::start`, `saturn-core/src/lib.rs`)

| Core | Thread | Loops continuously? | Role / known state |
|---|---|---|---|
| 0 | `sh2-master` | **Yes** (only exception #1) | Boots from the real BIOS reset vector; drives VBLANK/H-Blank/SCU-timer generation from its own cycle count |
| 1 | `sh2-slave` | **Yes** (only exception #2) | Starts parked, woken by SMPC `SSHON` |
| 2 | `vdp1-draw` | No — parked forever | Named for VDP1 but currently does no real work: VDP1 command-list execution actually runs inline from Core 3, not here |
| 3 | `vdp2-composite` | No — parked, woken at V-Blank IN | Runs both `vdp::execute_vdp1` and `render_backdrop` once per real frame, only when Master SH-2's cycle-driven timing fires V-Blank IN |
| 4 | `m68k-sound-cpu` | No — parked while `SNDOFF`, woken on `SNDON` | Sound CPU |
| 5 | `scsp-synth` | Yes — **documented exception** | Real hardware synthesizes audio continuously regardless of CPU state, so this is genuine hardware-driven continuous work, not a polling loop; paced via `ClockThrottle` |
| 6 | `scu-dma-dsp` | No — parked, woken on DMA/DSP activity | SCU DMA engine + DSP interpreter |
| 7 | `smpc-cd-block` | No — parked forever | No SMPC or CD-block logic runs here yet |

**Known architecture debt** (see `CLAUDE.md`'s "Known architecture debt" for full detail — don't rediscover these from scratch):
- VDP1 execution is on Core 3, not Core 2, despite Core 2's name — real hardware has two independent chips, this project runs them serially on one thread for now.
- Core 3 went through three designs before landing on the cycle-driven one above; don't reintroduce a wall-clock timer there even "temporarily" — it was tried, measured, and reverted.
- CD-ROM (`cdrom.rs`) reads real CHD sectors correctly but is **not wired into the emulated system at all** — no CS2 register block, no Core 7 logic. Only called from a one-shot demo in `main()`.

---

## 🛠️ Build, Test & Run Commands

Always verify code correctness by building and running tests.

### Build
```bash
cargo build                     # workspace
cargo build --release
cargo build -p saturn-frontend-native --release --bin saturn-frontend-native
```

### Run
```bash
# Watches Core 0's PC for real boot progress against a real BIOS:
MIMAS_BOOT_WATCH_SECS=280 ./target/release/saturn-frontend-native --bios <path-to-real-bios.bin> [--chd <disc.chd>] [--speed <mult>] [--framedump out.png]
```

### Test
```bash
cargo test --workspace                              # full suite — must stay green after every change
cargo test --package saturn-core scu_dsp             # narrow to one module
cargo test --package e2e-tests some_test_name        # single e2e test
```
**`milestone-tests/` is a separate workspace, deliberately excluded from the root `cargo test --workspace`** — it needs a real `MIMAS_BIOS_PATH` and downloads a ~600MB CLIP model from Hugging Face on first run, both of which would break the root suite's fast/deterministic/no-network property. Run it explicitly and only when asked: `cd milestone-tests && cargo test`.

Disassemble a captured RAM dump (SH-2 side, kept in sync with `sh2.rs`'s opcode table by hand):
```bash
python3 tools/sh2dis.py /tmp/some_dump.bin 0x06000000
```

Format with `cargo fmt --all` before considering work done.

---

## 📁 Workspace Layout

* [`saturn-core/`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/saturn-core/): the emulator engine — CPU cores, peripherals, sync primitives. No I/O, no windowing.
* [`saturn-frontend-native/`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/saturn-frontend-native/): standalone CLI + a `minifb`-backed live window (`bin/mimas_window.rs`).
* [`saturn-frontend-libretro/`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/saturn-frontend-libretro/): Libretro cdylib for RetroArch — currently just stub entrypoints, not yet wired to `saturn-core`.
* [`e2e-tests/`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/e2e-tests/): workspace-level integration tests exercising `saturn-core` directly and spawning the native CLI as a subprocess.
* `milestone-tests/`: standalone crate (own workspace root), CLIP-based BIOS boot-screen verification. Not part of routine test runs — see Test Commands above.
* [`tools/sh2dis.py`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/tools/sh2dis.py): standalone SH-2 disassembler for offline RAM-dump analysis.
* [`.development/`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/.development/): live tracking docs — `current_blocker.md` (the one thing blocking boot progress *right now*, not a log), `current_bugs.md`, `TASKS.md`, `ROADMAP.md`, `phased_development_plan.md`.
* [`docs/hardware-reference/`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/docs/hardware-reference/): exhaustive real-Saturn-hardware reference, one file per subsystem, sourced only from Yabause C/C++ source with a `file:line` citation on every claim. **Check here first** for exact register/opcode/DMA behavior before reading Yabause source directly.
* [`docs/implementation-plans/`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/docs/implementation-plans/): phased plan per subsystem closing the gap between that reference and the current Rust code. **Keep these current**: when a phase's work lands, flip its checklist items to `- [x]` (or annotate `- [ ]` with why it's partial/deferred) in the same change.
* [`history.md`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/history.md): chronological log of *why* non-obvious decisions were made — read before assuming a design choice is accidental; add a chapter when you make one.

---

## ⚙️ Architecture & Code Guidelines

Mimas is a **thread-per-hardware-component** model: each real Saturn chip runs on its own OS thread (see the table above), not a generic "distributed block" abstraction.

1. **Synchronization**: `LockStepSync` (bounded-slack lockstep) keeps active cores from drifting more than a slack window apart; inactive (parked) cores don't participate in that computation at all — see the 🚨 rule above for why that distinction is load-bearing.
2. **Bus Arbitration**: `BusArbiter` models the physical bus lock DMA imposes on CPUs — Mimas's own addition, not present in the reference emulator (which has no such thing).
3. **Memory**: `WorkRam` (`shared_buffers.rs`) uses one independent `RwLock` per physical memory region (not one global lock) — a VDP2 CRAM write and an SH-2 WRAM read shouldn't contend. Frames publish lock-free via `arc_swap::ArcSwap<Framebuffer>`.
4. Reject thread pools, `tokio`/async, or process-per-component — shared address space is required for the memory-region model, and a panic anywhere is meant to crash the whole process for debuggability (`docs/mimas-architecture-spec.md` §2 has the full rationale).

### Code Standards
* **Language**: Rust (Edition 2021).
* **Style**: must format cleanly via `cargo fmt --all`.
* **`Sh2::new()`'s 3-argument signature must not break** — many tests across `e2e-tests` and `saturn-core` depend on it. Add new capability via setter methods/optional fields instead of changing the constructor.
* **`cargo test --workspace` must stay green after every change** — not just a narrowly-targeted test for the current fix.

### Verify against real hardware behavior, not intuition or the SH-2 manual alone
Whenever implementing a new opcode or memory-mapped register, cross-check exact semantics against a real, working emulator's source — sibling checkouts `../yabause/` (devMiyax/YabaSanshiro fork) and `../yabauseut/` (upstream Yabause) live alongside this repo.
- **New opcode** → find its handler in `yabause/src/sh2int.c`. Branch-target formulas, flag updates, push/pop order are the *exact* behavior real BIOS/game code was tested against.
- **Memory-mapped register** → find its dispatch in `yabause/src/memory.c` plus the relevant peripheral file (`smpc.c`, `scu.c`, `vdp1.cpp`, `vdp2.cpp`). Confirm the *physical* address (strip the cache-through `0x20000000` bit).
- Prefer `vidsoft.c` over `vidogl.c` for VDP1/VDP2 pixel algorithms — same register semantics, far less GPU-context noise.
- **Port what the hardware does, never transliterate Yabause's C data structures or control flow** — this project's threaded-core/`BusArbiter`/`LockStepSync` architecture has no equivalent there.
- **Write regression tests from independently-derived values** (real BIOS bytes, or a hand-traced algorithm computed separately) — never assert a value you haven't independently derived outside the implementation itself.

### Diagnostic recipes (reuse, don't reinvent)
- `REG_ACCESS_LOG`/`log_reg_access_once` in `sh2.rs`: dedups and logs every distinct SMPC/VDP1/VDP2/SCU/CS2 register access once per run. Grep `[REGACCESS]` output before hypothesizing what's missing.
- One-shot RAM dump + `tools/sh2dis.py` offline disassembly beats hand-tracing for stuck-PC investigation — see `CLAUDE.md`'s Diagnostic recipes section for the exact gating pattern.

---

## 🎯 Current Status

Check before starting any implementation task:
1. [`.development/current_blocker.md`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/.development/current_blocker.md) — the current wall preventing boot progress.
2. [`.development/phased_development_plan.md`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/.development/phased_development_plan.md) — the authoritative, per-subsystem-phase milestone tracker; check which phases are `[x]` before assuming a subsystem is unimplemented.
3. [`history.md`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/history.md) — chronological development history and rationale.
4. [`.development/ROADMAP.md`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/.development/ROADMAP.md) — high-level milestone status.

As of the latest landed work: `docs/implementation-plans/scu.md`'s SCU subsystem is fully done (all 6 phases — DSP, register file, interrupt controller, DMA controller, timers, DMA start factors/DSP End/Draw End's SCU-side entry point). CD-ROM/CS2 integration (Milestone 3) and SMPC's remaining phases are the next open subsystem work.
