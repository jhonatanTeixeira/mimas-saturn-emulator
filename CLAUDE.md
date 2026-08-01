# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Mimas is a from-scratch Sega Saturn emulator in Rust, built with a distributed multi-threaded architecture where each real hardware component (both SH-2 CPUs, SCU, VDP1/VDP2, M68K, SCSP, SMPC/CD block) runs on its own OS thread. Every line of code is written by AI (Claude Code), with a human engineer driving architecture, correctness decisions, and hardware-accuracy verification. See `README.md` for the full framing.

## Build, run, test

```bash
cargo build                                                    # build workspace
cargo build --release
cargo build -p saturn-frontend-native --release --bin saturn-frontend-native

# Run against a real BIOS, watching Core 0's PC for boot progress:
MIMAS_BOOT_WATCH_SECS=280 ./target/release/saturn-frontend-native --bios <path-to-real-bios.bin> [--chd <disc.chd>] [--speed <mult>] [--framedump out.png]

cargo test --workspace                                         # full suite (saturn-core + e2e-tests)
cargo test --package saturn-core scu_dsp                       # narrow to one module's tests
cargo test --package e2e-tests some_test_name                  # single test by name

python3 tools/sh2dis.py <dump.bin> <base_addr_hex>              # offline SH-2 disassembler, kept in sync with sh2.rs's opcode table by hand
```

`milestone-tests/` is **deliberately excluded** from the root workspace (it has its own empty `[workspace]` table in its `Cargo.toml`) because it requires a real `MIMAS_BIOS_PATH` and downloads a ~600MB CLIP model from Hugging Face on first run — both would break `cargo test --workspace`'s fast/deterministic/no-network property. Run it explicitly: `cd milestone-tests && cargo test`.

Format with `cargo fmt` before considering work done.

## Workspace layout

- `saturn-core/`: the emulator engine itself — CPU cores, peripherals, sync primitives. No I/O, no windowing.
- `saturn-frontend-native/`: standalone CLI (`main.rs`) and a `minifb`-backed live window (`bin/mimas_window.rs`).
- `saturn-frontend-libretro/`: Libretro API cdylib for RetroArch — currently just stub entrypoints (`retro_init`/`retro_deinit`), not yet wired to `saturn-core`.
- `e2e-tests/`: workspace-level integration tests exercising `saturn-core` types directly and spawning the native CLI as a subprocess.
- `milestone-tests/`: standalone crate (own workspace root) that uses a CLIP model to visually verify BIOS boot screens against `fixtures/`. Not part of routine test runs.
- `tools/sh2dis.py`: standalone SH-2 disassembler for offline analysis of RAM dumps.
- `.development/`: live tracking docs — `current_blocker.md` (the one thing blocking boot progress right now), `current_bugs.md`, `TASKS.md`, `ROADMAP.md`, `phased_development_plan.md`.
- `docs/hardware-reference/`: exhaustive real-Saturn-hardware reference (one file per subsystem: `sh2-cpu.md`, `memory-bus.md`, `scu.md`, `smpc-peripheral.md`, `vdp1.md`, `vdp2.md`, `scsp.md`, `cs2-cdblock.md`), sourced exclusively from reading Yabause's C/C++ source — every register/opcode/DMA mode with a `file:line` citation, plus a closing "known deviations" section per file cataloging real Yabause bugs/hacks/dead code found along the way. This is the authoritative source for exact hardware behavior; consult it before implementing or fixing anything register/opcode-level.
- `docs/implementation-plans/`: one phased implementation roadmap per subsystem (same 8-way split), each diffing the current Rust implementation against its `hardware-reference/` counterpart and laying out concrete next steps toward full fidelity.
- `docs/`: architecture reference (`saturn-architecture.md`, `mimas-architecture-spec.md`, `mimas_emu_engineering_draft.md`, `mimas-performance-analysis.md`) — Mimas's own design rationale (threads, sync, memory ownership), not hardware facts; those now live in `hardware-reference/` above.
- `history.md`: chronological log of *why* non-obvious decisions were made — read this before assuming a design choice is accidental.

## Architecture

### Thread-per-component model (`SaturnSystem::start`, `saturn-core/src/lib.rs`)

`SaturnSystem` spawns 8 native OS threads, one per physical hardware block, each with a fixed core ID used by the sync layer:

| Core | Thread name | Role |
|---|---|---|
| 0 | `sh2-master` | Master SH-2 — boots from the real BIOS reset vector |
| 1 | `sh2-slave` | Slave SH-2 — starts parked, woken by SMPC `SSHON` |
| 2 | `vdp1-draw` | Named for VDP1, but currently just an idle placeholder loop — see below |
| 3 | `vdp2-composite` | Actually runs **both** `vdp::execute_vdp1` and `vdp::render_backdrop` every ~16.6ms frame tick, then publishes the frame |
| 4 | `m68k-sound-cpu` | Sound CPU, gated on/off by SMPC `SNDON`/`SNDOFF` via `m68k_control` |
| 5 | `scsp-synth` | Calls `Scsp::synthesize` every loop iteration — live, but only basic per-voice PCM playback (no envelope/LFO/DSP) |
| 6 | `scu-dma-dsp` | SCU DSP interpreter (`scu_dsp.rs`); starts parked, activated when the master writes the DSP's `EX` bit |
| 7 | `smpc-cd-block` | Idle placeholder loop — no SMPC or CD-block logic runs here (see below) |

**Known architecture debt, not yet reconciled:**

- **Core 2 vs Core 3**: VDP1 command-list execution (`vdp::execute_vdp1`) is actually invoked from Core 3's loop, immediately before `render_backdrop` — not from Core 2, despite Core 2 being named/intended as the VDP1 thread. Real hardware's VDP1 and VDP2 are independent chips with their own timing; today they run serially on one thread. See `docs/implementation-plans/vdp1.md` and `vdp2.md`.
- **Only Core 1 and Core 6 truly park.** They call `LockStepSync::park_while_inactive` and cost ~0% host CPU while idle. Cores 2, 3, 4, 5, 7 loop forever on `thread::yield_now()` plus `sync_core` regardless of whether they have real work (e.g. Core 4 still loops continuously even while the M68K is stopped) — not the "park when idle" model the rest of this doc describes as the target. Don't assume every idle core is free.
- **`saturn-core/src/scu.rs` (`Scu`) is dead code** — referenced only from unit tests, never constructed by `SaturnSystem`. The real (partial) SCU DSP logic lives in `scu_dsp.rs` (wired to Core 6). A real SCU DMA controller (levels 0-2, independent of the DSP) does not exist yet. `saturn-core/src/smpc.rs` (`Smpc`) **used to be dead code in the same way but is now real** (see `docs/implementation-plans/smpc-peripheral.md` Phase 0): `SaturnSystem` owns an `Arc<Mutex<Smpc>>`, wired into Core 0's `Sh2` via `Sh2::smpc`; register storage stays in `WorkRam::smpc_regs`.
- **CD-ROM is not integrated into the emulated system at all.** `Cdrom` (`cdrom.rs`) reads real CHD sectors correctly, but nothing wires it to the memory-mapped CS2 register block (`work_ram.cs2_regs` is a plain read/write stub) or to Core 7 — it's only ever called directly from `saturn-frontend-native`'s `main()` as a one-shot demo.

Reject the idea of adding a thread pool, `tokio`/async, or process-per-component (`fork`) — those are considered and rejected in `docs/mimas-architecture-spec.md` §2 (shared address space needed for the memory-region model below; a panic anywhere is meant to crash the whole process for debuggability, not be isolated).

### Synchronization primitives

- **`LockStepSync`** (`sync.rs`): bounded-slack lockstep. Each active core reports its cycle count via `sync_core`; a core blocks only if it has drifted more than `slack_limit` cycles ahead of the slowest *active* core. Two separate `Condvar`s are used deliberately — one for drift-waiters (notified on every `sync_core` call), one for `park_while_inactive` waiters (notified only on real reactivation/shutdown). Merging them was tried and measured as a real bug (see the doc comment on `condvar`): a parked core got woken millions of times a second for nothing.
- **`BusArbiter`** (`bus_arbiter.rs`): models the physical bus lock DMA imposes on the CPUs. `acquire_bus_sync` additionally deactivates the calling core in `LockStepSync` while blocked, so a DMA-stalled CPU doesn't drag down the lockstep slack window for everyone else.
- **`PanicGuard`**: RAII guard held by every spawned thread; on panic it force-triggers `sync.request_shutdown()` and `arbiter.abort()` so one core's crash doesn't hang the rest of the system silently.
- **`ClockThrottle`** (`throttle.rs`): paces CPU threads against real Saturn clock rates (28.6 MHz SH-2, M68K rate) via wall-clock batching. Defaults to `ThrottleSpeed::Unthrottled` (as-fast-as-possible) so existing verification workflows are unaffected; live-adjustable via `SaturnSystem::set_speed`.

### Memory model (`shared_buffers.rs`)

`WorkRam` holds one **independent `RwLock` per physical memory region** (low WRAM, 32×64KB-striped high WRAM, sound RAM, SCSP/VDP1/VDP2/SCU/CS2 register blocks, backup RAM, SMPC regs) rather than one global lock — a VDP2 CRAM write and an SH-2 WRAM read have nothing to do with each other and shouldn't contend. This was a deliberate split from an earlier single-lock design (see `history.md`). High WRAM is additionally striped 32 ways by address bits to reduce contention between concurrent accessors within the region itself. No call site currently needs more than one of these locks at once — if a future one does, acquire them in field-declaration order to avoid lock-ordering deadlocks. Region sizes/offsets are cross-checked against Yabause's `memory.c` fill table, not guessed — see field doc comments for the exact physical ranges each backs.

Frames are published lock-free from the VDP2 thread to any reader (e.g. the window frontend) via `arc_swap::ArcSwap<Framebuffer>` — no blocking handoff between renderer and presenter.

### Working methodology: verify against real hardware behavior, not the SH-2 manual alone

This project's most important non-obvious practice: whenever implementing a new opcode or memory-mapped register, cross-check exact semantics against a real, working emulator's source before writing code — sibling checkouts `../yabause/` (devMiyax/YabaSanshiro fork) and `../yabauseut/` (upstream Yabause) live alongside this repo.

**Check `docs/hardware-reference/` first** — it's an exhaustive, already-cross-checked register/opcode/DMA map with `file:line` citations back into `yabause/src/`, built exactly so this lookup doesn't have to be redone from scratch every time. Fall back to reading the Yabause source directly only for something the reference doesn't cover yet or where its citation needs re-verifying.

- **New opcode**: find its handler in `yabause/src/sh2int.c`'s interpreter/decode table. Branch target formulas, flag updates, and push/pop order are the *exact* behavior real BIOS/game code was tested against — don't infer from the SH-2 manual alone.
- **Memory-mapped register**: find its dispatch in `yabause/src/memory.c` plus the relevant peripheral file (`smpc.c`, `scu.c`, `vdp1.cpp`, `vdp2.cpp`). Confirm the *physical* address (strip the cache-through `0x20000000` bit) and cross two independent sources (struct doc comments, the read/write `switch`, and actual use in rendering/logic) before trusting a number.
- Prefer `vidsoft.c` over `vidogl.c` for VDP1/VDP2 pixel algorithms — same register semantics, far less GPU-context noise.
- Port *what the hardware does*, never transliterate Yabause's C data structures or control flow — this project's threaded-core/`BusArbiter`/`LockStepSync` architecture has no equivalent there.
- Write regression tests from independently-derived values (real BIOS bytes/addresses, or a hand-traced algorithm computed separately e.g. in a throwaway script) — never assert a value you haven't independently derived. A self-consistent-but-wrong test is worse than no test; this has bitten this project before (`bt_bf_no_delay_slot`, the first `DIV1` test).
- Where a real simplification is made (e.g. VDP2 backdrop-only rendering, SMPC commands completing "instantly"), say so explicitly and keep behavior honest (black screen when unconfigured, not a placeholder color) rather than faking output.

### Diagnostic recipes (reuse, don't reinvent)

- **`REG_ACCESS_LOG`/`log_reg_access_once`** in `sh2.rs`: dedups and logs every distinct SMPC/VDP1/VDP2/SCU/CS2 register access (offset + direction + value) exactly once per run. Grep `[REGACCESS]` output after a boot run *before* hypothesizing what's missing.
- **One-shot RAM dump + offline disassembly**: gate a probe in `Sh2::execute()` on a `static AtomicBool` so it fires once when PC enters a stuck range, `std::fs::write` a slice of `work_ram.high_ram`, then decode with `python3 tools/sh2dis.py <dump.bin> <base_addr_hex>` rather than hand-tracing — BIOS code interleaves literal pools (`MOV.L @(disp,PC),Rn`) that a linear disassembler will misdecode as garbage; that's expected, real code resumes after a `BRA`/`RTS` past the pool.
- **Find who writes a specific RAM variable**: add a surgical `eprintln!` probe directly in the relevant `MemRegion` write arm (e.g. `HighRam`), gated on the exact offset, logging `self.pc` — faster than static disassembly when a hypothesis from tracing turns out wrong.
- Remove throwaway probes once a bug is diagnosed; they're not permanent instrumentation (unlike `REG_ACCESS_LOG`, which is deliberately kept).

### Stability constraints

- `Sh2::new()`'s 3-argument signature must not break — many tests across `e2e-tests` and `saturn-core` depend on it. Add new capability via setter methods/optional fields (see `pc_reporter`, `m68k_control`, `speed`, `scu_dsp` in `SaturnSystem::start`) instead of changing the constructor.
- `cargo test --workspace` must stay green after every change — not just a narrowly-targeted test for the current fix.

## Tracking docs — update these as you go, not just at session end

- `.development/current_blocker.md`: the single thing actively blocking boot progress right now. Rewrite when the wall clears; this should never read like a historical log.
- `.development/current_bugs.md`: known gaps/bugs; add on discovery, remove once genuinely fixed.
- `.development/TASKS.md` / `.development/ROADMAP.md`: move items between Done/In-progress/Not-started as status actually changes.
- `history.md`: add a chapter (or extend the current one) explaining *why* a non-obvious decision was made — the diff already shows *what* changed.
- **`docs/implementation-plans/*.md`**: each phase's checklist items must be flipped `- [x]` (or annotated with **Simplification**/**Partial**/**Deliberately deferred** and a reason) the moment that work actually lands — not left showing `- [ ]` for work that's already done, and not checked off for anything not fully true. Add a one-line **Status:** note under each phase's heading pointing at the `history.md` chapter that covers it. A future session (or agent) trusts these checklists at face value; a stale one wastes exactly the re-derivation effort this whole tracking-docs section exists to avoid.

Skipping these updates is how the next session ends up re-deriving knowledge that was already earned once.
