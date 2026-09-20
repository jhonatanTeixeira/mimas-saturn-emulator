# GEMINI.md - Instructions and Workspace Guide for Mimas

This file provides system context, build/test commands, and guidelines for AI agents (specifically Antigravity / Gemini) working on the **Mimas** Sega Saturn emulator project. It is a condensed companion to `CLAUDE.md` (the full reference used by Claude Code) — when the two disagree, or when a section here feels thin, check `CLAUDE.md` first; it is kept current with every landed change and this file is not always updated in lockstep.

---

## 🚨 The one rule that must never be broken

**Only Core 0 (`sh2-master`) and Core 1 (`sh2-slave`) may run a continuous loop.** Every other component thread parks (`LockStepSync::park_while_inactive`) and is woken only by a real hardware-equivalent event — a register write, an interrupt, or (for video timing) Master SH-2's own cycle-driven progress. **Zero polling loops. Zero wall-clock timers (`Instant::now()`, `thread::sleep` on a fixed interval) anywhere outside the CPU cores' own clock-rate pacing (`ClockThrottle`).**

This is not a style preference — it was violated once (a sleep-paced "improvement" to a component thread), and it silently throttled the *entire emulator* by roughly two orders of magnitude via `LockStepSync`'s bounded-slack model, because a slow-reporting *active* thread drags every other active thread down with it. The fix was making that thread genuinely *inactive* (parked) instead of *active-but-slow*. Real hardware timing (VBLANK, H-Blank, SCU timers) must be derived from Master SH-2's own executed-cycle count, exactly like the reference emulator's main loop does (`yabsys.LineCount` tied to `sh2cycles`, never a host clock) — never from a wall-clock deadline.

If you cannot implement a component this way, stop and flag it rather than shipping a polling/sleep-based stand-in — it will be treated as a regression, not a working version, regardless of what tests show.

---

## 🧵 The 8 threads (`SaturnSystem::start`, `saturn-core/src/lib.rs`)

| Core | Thread | Loops continuously? | Hardware |
|---|---|---|---|
| 0 | `sh2-master` | **Yes** (only exception #1) | Master SH-2; drives V-Blank/H-Blank/SCU-timer generation from its own cycle count |
| 1 | `sh2-slave` | **Yes** (only exception #2) | Slave SH-2; starts parked, woken by SMPC `SSHON` |
| 2 | `vdp1-draw` | No — parks | VDP1 |
| 3 | `vdp2-composite` | No — parks, woken at V-Blank IN | VDP2 |
| 4 | `m68k-sound-cpu` | No — parks while `SNDOFF` | Sound CPU |
| 5 | `scsp-synth` | No — parks | SCSP |
| 6 | `scu-dma-dsp` | No — parks, woken on DMA/DSP work | SCU DMA engine + DSP |
| 7 | `smpc-cd-block` | No — parks | SMPC command dispatch + CD block |

`golden_rules.py` enforces the "No" column, and for this rule there is **no
escape hatch** — no allowlist, no `// golden-rule-ok:`. What each thread
implements today, and the known gaps, live in `docs/implementation-plans/` and
`.development/current_bugs.md` — not in this file.

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
* [`saturn-frontend-libretro/`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/saturn-frontend-libretro/): Libretro cdylib for RetroArch.
* [`e2e-tests/`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/e2e-tests/): workspace-level integration tests exercising `saturn-core` directly and spawning the native CLI as a subprocess.
* `milestone-tests/`: standalone crate (own workspace root), CLIP-based BIOS boot-screen verification. Not part of routine test runs — see Test Commands above.
* [`tools/sh2dis.py`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/tools/sh2dis.py): standalone SH-2 disassembler for offline RAM-dump analysis.
* [`.development/`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/.development/): live tracking docs — `current_blocker.md` (the one thing blocking boot progress *right now*, not a log), `current_bugs.md`, `TASKS.md`, `ROADMAP.md`, `phased_development_plan.md`.
* [`docs/hardware-reference/`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/docs/hardware-reference/): exhaustive real-Saturn-hardware reference, one file per subsystem, sourced only from Yabause C/C++ source with a `file:line` citation on every claim. **Check here first** for exact register/opcode/DMA behavior before reading Yabause source directly. It describes behaviour, never implementation — never a reason to port Yabause code.
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

### Take Yabause's knowledge, never its code

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
- `REG_ACCESS_LOG`/`log_reg_access_once` in `sh2.rs`: dedups and logs every distinct SMPC/VDP1/VDP2/SCU/CS2 register access once per run. Grep `[REGACCESS]` output before hypothesizing what's missing.
- One-shot RAM dump + `tools/sh2dis.py` offline disassembly beats hand-tracing for stuck-PC investigation — see `CLAUDE.md`'s Diagnostic recipes section for the exact gating pattern.

---

## 🎯 Where the project's state lives

Check before starting any implementation task. These hold the project's state;
this file holds only how to work, and does not repeat them:
1. [`docs/unlock_bios.md`](docs/unlock_bios.md) and `docs/unlock_bios/` — how BIOS progress is measured, the current baseline, the current divergence, and the plans to close it. Reproduce the recorded baseline before changing anything.
2. [`.development/current_bugs.md`](.development/current_bugs.md) — known bugs and debt.
3. [`docs/current_review.md`](docs/current_review.md) — findings of the latest review.
4. [`.development/phased_development_plan.md`](.development/phased_development_plan.md) and `docs/implementation-plans/` — per-subsystem phase status; check which phases are `[x]` before assuming a subsystem is unimplemented.
5. [`history.md`](history.md) — why non-obvious decisions were made.

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

### Before every commit: coverage of what you are about to commit

**This is mandatory and it has to run *before* `git commit`, not after.**

```bash
MIMAS_COVERAGE_COMMITS=HEAD bash tools/quality_gate.sh
```

`HEAD` as the base means "diff HEAD against the working tree" — which, before
you commit, is exactly the change you are about to make. The same 90% floor
applies, to the lines you added or modified. **After** you commit, `HEAD` has
moved and the same command measures nothing; there is no way to run this check
retroactively, which is why the order matters.

If it is red, the output names the exact uncovered lines. Write tests for them.
Do not lower `MIMAS_COVERAGE_MIN`, do not widen the range until the number
improves, and do not commit red and fix it later — the whole-tree figure moves
by fractions of a percent per commit, so nobody will ever notice it again.

Only lines tarpaulin reports as executable count. A change that touches only
tests or docs passes with "nothing to measure", because `--ignore-tests` keeps
test bodies out of the denominator: writing tests shows up as coverage of the
product lines they exercise, never as coverage of themselves.

Two things the tool refuses rather than answering wrongly, so do not try to work
around either:

- **A range whose tip is not HEAD.** Coverage is measured against the working
  tree, so that is the only version whose line numbers it can be intersected
  with. `A..B` with a historical `B` reports the hit counts of whatever now sits
  at those line numbers.
- **Coverage data older than the sources.** If any tracked `.rs` file changed
  after the LCOV was written, re-run tarpaulin. Do not pass `--allow-stale` to
  make the message go away.

A scoped run does not measure the tree, and the gate prints that in its summary.
Say it when reporting: "diff coverage green on my change" is a true statement,
"coverage green" is not.

### Committing

You are the one who commits in this repository — Claude does not. That makes the
check above yours alone to run. A commit that has not had
`MIMAS_COVERAGE_COMMITS=HEAD` run against it is not ready, however green
everything else looks.
