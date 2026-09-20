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
- `saturn-frontend-libretro/`: Libretro API cdylib for RetroArch.
- `e2e-tests/`: workspace-level integration tests exercising `saturn-core` types directly and spawning the native CLI as a subprocess.
- `milestone-tests/`: standalone crate (own workspace root) that uses a CLIP model to visually verify BIOS boot screens against `fixtures/`. Not part of routine test runs.
- `tools/sh2dis.py`: standalone SH-2 disassembler for offline analysis of RAM dumps.
- `.development/`: live tracking docs — `current_blocker.md` (the one thing blocking boot progress right now), `current_bugs.md`, `TASKS.md`, `ROADMAP.md`, `phased_development_plan.md`.
- `docs/hardware-reference/`: exhaustive real-Saturn-hardware reference (one file per subsystem: `sh2-cpu.md`, `memory-bus.md`, `scu.md`, `smpc-peripheral.md`, `vdp1.md`, `vdp2.md`, `scsp.md`, `cs2-cdblock.md`), sourced exclusively from reading Yabause's C/C++ source — every register/opcode/DMA mode with a `file:line` citation, plus a closing "known deviations" section per file cataloging real Yabause bugs/hacks/dead code found along the way. This is the authoritative source for exact hardware behavior; consult it before implementing or fixing anything register/opcode-level. It describes behaviour, never implementation — nothing in it is a licence to port Yabause code (see "Working methodology" below).
- `docs/implementation-plans/`: one phased implementation roadmap per subsystem (same 8-way split), each diffing the current Rust implementation against its `hardware-reference/` counterpart and laying out concrete next steps toward full fidelity.
- `docs/`: architecture reference (`saturn-architecture.md`, `mimas-architecture-spec.md`, `mimas_emu_engineering_draft.md`, `mimas-performance-analysis.md`) — Mimas's own design rationale (threads, sync, memory ownership), not hardware facts; those now live in `hardware-reference/` above.
- `history.md`: chronological log of *why* non-obvious decisions were made — read this before assuming a design choice is accidental.

## Architecture

### Thread-per-component model (`SaturnSystem::start`, `saturn-core/src/lib.rs`)

`SaturnSystem` spawns 8 native OS threads, one per physical hardware block, each with a fixed core ID used by the sync layer:

| Core | Thread name | Hardware |
|---|---|---|
| 0 | `sh2-master` | Master SH-2 — boots from the real BIOS reset vector; drives video and SCU timing from its own cycle count |
| 1 | `sh2-slave` | Slave SH-2 — starts parked, woken by SMPC `SSHON` |
| 2 | `vdp1-draw` | VDP1 |
| 3 | `vdp2-composite` | VDP2 — woken at the cycle-driven V-Blank IN moment |
| 4 | `m68k-sound-cpu` | Sound CPU, gated by SMPC `SNDON`/`SNDOFF` |
| 5 | `scsp-synth` | SCSP |
| 6 | `scu-dma-dsp` | SCU DMA engine and DSP |
| 7 | `smpc-cd-block` | SMPC command dispatch and CD block |

What each thread actually implements today, and the known gaps (e.g. which
thread really runs VDP1), live in `docs/implementation-plans/` and
`.development/current_bugs.md`, not here.

**Only Cores 0 and 1 loop continuously — every other core is parked until
woken.** This is a hard project rule (`docs/mimas-architecture-spec.md`
§1.4/§1.5), not a preference: the only continuous loop in the system is the
CPU's own (`Sh2::run_loop`), paced against real Saturn clock rates
(`ClockThrottle`). Every other component thread parks via
`LockStepSync::park_while_inactive` and is woken only by a real
hardware-equivalent event — a register write, an interrupt, or Master SH-2's own
cycle-driven timing. `tools/golden_rules.py` enforces it, and
`rule_spawned_threads_park` deliberately has no escape hatch: no allowlist entry,
no `// golden-rule-ok:` marker. "Real hardware does this continuously" is not an
argument for a spinning thread — it is an argument for advancing *emulated
state* continuously, which a parked thread woken in batches on the Master's
schedule does.

**Hardware timing comes from emulated cycles, never from a host clock.**
H-Blank, V-Blank and the SCU timers are generated from Master SH-2's executed
cycles (`Sh2::step` → `Scu::advance_video_line`). A wall-clock or sleep-paced
timer on a component thread was tried and measurably throttled the whole
emulator by about two orders of magnitude through `LockStepSync`'s bounded slack
(`history.md`, Chapters 32–33). If timing ever needs revisiting, drive it from
cycles, not from `Instant::now()`.

Reject the idea of adding a thread pool, `tokio`/async, or process-per-component (`fork`) — those are considered and rejected in `docs/mimas-architecture-spec.md` §2 (shared address space needed for the memory-region model below; a panic anywhere is meant to crash the whole process for debuggability, not be isolated).

### Synchronization primitives

- **`LockStepSync`** (`sync.rs`): bounded-slack lockstep. Each active core reports its cycle count via `sync_core`; a core blocks only if it has drifted more than `slack_limit` cycles ahead of the slowest *active* core. Two separate `Condvar`s are used deliberately — one for drift-waiters (notified on every `sync_core` call), one for `park_while_inactive` waiters (notified only on real reactivation/shutdown). Merging them was tried and measured as a real bug (see the doc comment on `condvar`): a parked core got woken millions of times a second for nothing.
- **`BusArbiter`** (`bus_arbiter.rs`): models the physical bus lock DMA imposes on the CPUs. `acquire_bus_sync` additionally deactivates the calling core in `LockStepSync` while blocked, so a DMA-stalled CPU doesn't drag down the lockstep slack window for everyone else.
- **`PanicGuard`**: RAII guard held by every spawned thread; on panic it force-triggers `sync.request_shutdown()` and `arbiter.abort()` so one core's crash doesn't hang the rest of the system silently.
- **`ClockThrottle`** (`throttle.rs`): paces CPU threads against real Saturn clock rates (28.6 MHz SH-2, M68K rate) via wall-clock batching. Defaults to `ThrottleSpeed::Unthrottled` (as-fast-as-possible) so existing verification workflows are unaffected; live-adjustable via `SaturnSystem::set_speed`.

### Memory model (`shared_buffers.rs`)

`WorkRam` holds one **independent `RwLock` per physical memory region** (low WRAM, 32×64KB-striped high WRAM, sound RAM, SCSP/VDP1/VDP2/SCU/CS2 register blocks, backup RAM, SMPC regs) rather than one global lock — a VDP2 CRAM write and an SH-2 WRAM read have nothing to do with each other and shouldn't contend. This was a deliberate split from an earlier single-lock design (see `history.md`). High WRAM is additionally striped 32 ways by address bits to reduce contention between concurrent accessors within the region itself. No call site currently needs more than one of these locks at once — if a future one does, acquire them in field-declaration order to avoid lock-ordering deadlocks. Region sizes/offsets are cross-checked against Yabause's `memory.c` fill table, not guessed — see field doc comments for the exact physical ranges each backs.

Frames are published lock-free from the VDP2 thread to any reader (e.g. the window frontend) via `arc_swap::ArcSwap<Framebuffer>` — no blocking handoff between renderer and presenter.

### Working methodology: take Yabause's knowledge, never its code

Two different things live in Yabause, and they get opposite treatment.

**Its knowledge of the hardware is sound — use it.** YabaSanshiro runs the
Saturn library, and it can only do that because it gets the processors right:
opcode semantics, branch-target formulas, flag updates, push/pop order,
register meanings, DMA modes. That knowledge is proven, and it is what
`docs/hardware-reference/` extracts — check there first.

- **New opcode**: find its handler in `yabause/src/sh2int.c`. Branch-target
  formulas, flag updates and push/pop order there are the exact behaviour real
  BIOS and game code runs against — don't infer them from the SH-2 manual alone.
- **Memory-mapped register**: find its dispatch in `yabause/src/memory.c` plus
  the peripheral file (`smpc.c`, `scu.c`, `vdp1.cpp`, `vdp2.cpp`). Confirm the
  *physical* address (strip the cache-through `0x20000000` bit) and cross two
  independent sources before trusting a number.
- Prefer `vidsoft.c` over `vidogl.c` for VDP1/VDP2 pixel algorithms — same
  register semantics, far less GPU-context noise.
- Disassembly, traces and captures made with it are valid evidence of what the
  BIOS and games execute (`tools/bios_progress.py` rests on one).

**Its implementation is not — never port it.** Its architecture is nothing like
Mimas's (no threaded cores, no `BusArbiter`, no `LockStepSync`), it is slow,
parts of it are broken (FMV), and the code is a patchwork accumulated over thirty
years. So: no ported code, no data structures, no control flow, no solution
lifted from it. Understand what the hardware does from it, close it, and write
Mimas's own implementation in Mimas's own structure.

- **Cite, never copy.** A comment `// understood from yabause/src/memory.c:120`
  is encouraged; it says where the understanding came from. Its code is not.
- **Enforced:** `tools/golden_rules.py`'s `no-yabause-code` rule fails on
  Yabause's implementation vocabulary in Mimas code (`T1ReadLong`,
  `MappedMemoryReadLong`, `SH2_struct`, `CurrentSH2`, `yabsys`, `c68k_*`, …),
  with **no escape hatch**. Comments are excluded, so citations are fine.
- Each `hardware-reference/` file's "known deviations" section records where
  Yabause's *implementation* hacks around something. Those are the parts not to
  reproduce.
- Write regression tests from independently-derived values (real BIOS bytes,
  addresses, or a hand-traced algorithm computed separately) — never assert a
  value you have not derived yourself. A self-consistent-but-wrong test is worse
  than no test; this has bitten this project before (`bt_bf_no_delay_slot`, the
  first `DIV1` test).
- Where a real simplification is made, say so explicitly and keep behaviour
  honest (black screen when unconfigured, not a placeholder colour).

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
- **`docs/current_review.md`**: whenever a review of recent/pending changes is requested (code review, self-review, etc.), write the full findings here — overwrite the previous contents each time, this is a snapshot of the *latest* review, not an accumulating log. Still summarize the findings in chat as usual; this file is so the next session (or agent) can pick up outstanding findings without re-running the review from scratch.

Skipping these updates is how the next session ends up re-deriving knowledge that was already earned once.

## The quality gate — run it before calling work done

```bash
bash tools/quality_gate.sh                              # deterministic gate
.venv/bin/python tools/antipattern_scan.py scan         # semantic pass, ~1 min
```

**Both are required.** Full detail, and the reasoning behind every threshold, in
`docs/quality-gate.md`.

The gate is deterministic and stdlib-only, so it always runs. The semantic pass
needs local models and is separate for that reason -- it catches the
architectural violations that have no fixed spelling ("this thread polls instead
of parking" can be a `while` on an atomic, a `loop` with a `yield_now`, a
sleep-driven deadline, or a mutex retaken every iteration), which is exactly what
a grep cannot reach. Its output is a review queue, not a verdict: read the code
before acting on it.

This is not a formality. On 2026-09-17 two agents, in one day, broke the BIOS
boot completely, left `is_shutdown()` returning `false` forever, reintroduced a
`sched_yield` syscall per emulated instruction that had been measured and removed
that same morning, and shipped four `assert!(true)` tests whose names claim the
VDP1 framebuffer swap is verified. `cargo build`, `cargo test` (396 green),
`cargo clippy` and the coverage step were happy through all of it.

### A red step is a result, not an obstacle

| step | the one honest fix |
|---|---|
| Formatting | `cargo fmt --all` |
| Mess detect (clippy) | fix it, or `#[allow(...)]` **with a comment saying why clippy is wrong here**. **Never `cargo clippy --fix`** -- it was run once and turned an m68k opcode guard into a no-op while muting the warnings that flagged half-written VDP1 code |
| Golden rules | fix the violation, or `// golden-rule-ok: <reason>` **on the offending line** |
| Tests that assert nothing | write a real assertion, or `// no-assert: <reason>` if proving the absence of a panic genuinely is the point |
| Coverage | write tests. Not `--exclude-files`, not a lower bar. To hold *this change* to 90% instead of the whole tree, `MIMAS_COVERAGE_COMMITS=<base-ref>` — same floor, narrower scope, and it is reported as scoped |
| Smoke test | if boot got *further*, verify and update the expected PC. If it got *shorter*, that is a regression, not a stale constant |
| Semantic pass | read each hit. Fix it, or record why it is not the pattern |

### Thresholds

Tightening is free. **Loosening fails the gate unless `MIMAS_OVERRIDE_REASON`
says why:**

```bash
MIMAS_MIN_SPEED_PCT=15 MIMAS_OVERRIDE_REASON="measuring on R36S hardware" \
    bash tools/quality_gate.sh
```

A green result under loosened thresholds is not the same result and must not be
reported as one.

### Reporting

Say what actually happened. "8 of 12 passed, coverage and golden rules red" is
useful. "The gate passed" when a threshold was lowered or a step skipped is not.

### Coverage of work that is not committed yet

Before calling any change done, measure the coverage of **the change**, not of
the tree:

```bash
MIMAS_COVERAGE_COMMITS=HEAD bash tools/quality_gate.sh
```

`HEAD` as the base means "diff HEAD against the working tree", which is exactly
the uncommitted work — staged and unstaged. Same 90% floor, applied to the lines
being added or modified. The tree's own figure is still below its floor and is
still debt; a scoped run does not measure it, and the gate says so in its
summary. Repeat that when reporting.

Two traps the tool now refuses rather than answering wrongly:

- **A range whose tip is not HEAD.** Coverage is measured against the working
  tree, so that is the only version whose line numbers the data can be
  intersected with. Asking for `A..B` where `B` is historical reports the hit
  counts of whatever now sits at those numbers.
- **Coverage data older than the sources.** If any tracked `.rs` file was edited
  after the LCOV was written, the numbers describe a different version of the
  code. Re-run tarpaulin.

A range that touches only tests or docs passes with "nothing to measure" —
`--ignore-tests` keeps test bodies out of the denominator, so writing tests
shows up as coverage of the product lines they exercise, never as coverage of
themselves.

### Committing

**Claude does not commit in this repository.** Not with `git commit`, not with
`git commit --amend`, not indirectly. Leave the work in the tree and say what
changed; the human or Gemini commits it. This applies even when a change is
finished, verified and green.
