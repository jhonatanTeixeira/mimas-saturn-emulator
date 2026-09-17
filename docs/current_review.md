# Code review — commit `9354fd3` "Fix architectural violations (Spec 1.2, 1.4, 1.5)"

> The previous contents of this file (the VDP2 Phase 2/3 review, 15 findings, still
> **open**) are recoverable with `git show 9354fd3:docs/current_review.md`. None of
> those findings were addressed by this commit.

**Verdict: this commit must not ship as-is. It breaks BIOS boot completely.**
Verified empirically, not inferred. `cargo test --workspace` is green (396 tests),
which is itself part of the problem — see F3.

## Measured regression

Same binary flags, same BIOS, same machine, back to back:

| | before (`194572f` + the reverted `yield_now` fix) | after `9354fd3` |
|---|---|---|
| Core 0 settle PC | `0x06001694` | **`0x000002B0`** |
| WRAM accesses (R+W) | 2,554,734 | **4** |
| wall clock to settle | 4.05–4.15 s | 0.72 s (dies immediately) |
| process CPU | 152% | 48% |

`0x2B0`/`0x2B2`/`0x2B4` is the BIOS memory-clear loop (`MOV.L R4,@R3` / `DT R6` /
`BF/S`) — the exact address and the exact signature `history.md` Chapter 32 records
for "Master SH-2 throttled by the bounded-slack model". Boot no longer progresses
past it. `perf record` over a full run: **74.6% of samples are in the kernel**
(futex), with the hottest user symbol `Sh2::execute` at 2.55%.

## F1 — `shutdown_flag` is written nowhere; `is_shutdown()` returns `false` forever

`saturn-core/src/sync.rs:4` declares it, `:37` initialises it to `false`, `:180`
reads it. **There is no `store` anywhere in the repo** (`grep -rn shutdown_flag`
returns exactly those three lines). `request_shutdown()` (`sync.rs:172-177`) still
sets only `state.shutdown`.

Consequence: every `sync.is_shutdown()` check is dead — `Sh2::run_loop`
(`sh2.rs:4470`), Core 4 (`lib.rs:427`), Core 5 (`lib.rs:472`), Core 6
(`lib.rs:521`). `PanicGuard`'s whole purpose — "on panic it force-triggers
`sync.request_shutdown()` so one core's crash doesn't hang the rest of the system
silently" (CLAUDE.md) — is now inoperative. A panic on any core hangs the process.

Fix: `request_shutdown` must `self.shutdown_flag.store(true, Ordering::Release)`.

## F2 — the summary word is never set by any production producer

This is the boot-breaking one. `service_pending_interrupt` (`sh2.rs:2766-2769`) now
gates all five hardware events behind `hardware_events_any`. The real producers do
**not** set it:

- `lib.rs:581-582` sets `smpc_irq_pending` — no summary store
- `lib.rs:586-587` sets `smpc_nmi_pending` — no summary store
- `lib.rs:591-592` sets `smpc_sysres_pending` — no summary store
- `lib.rs:595` sets `smpc_clock_change` — no summary store
- `vdp.rs:406-407` sets `vdp1_draw_end_pending` — no summary store

`grep -rn hardware_events_any` finds `store(true, …)` at exactly six places:
`sh2.rs:4945, 4980, 5004, 5382, 5409, 5862` — **all inside `mod opcode_tests`**.

So in production the gate never opens and SMPC System Manager IRQ, NMI, system
reset, 320/352 clock change and VDP1 Draw End are **silently dropped forever**.

The summary-word technique is right; it just has to be written at every producer,
ideally by funnelling all five through one `WorkRam::raise_hardware_event(…)`
helper so a future sixth flag cannot repeat this.

## F3 — the tests were patched instead of the producers, and that masked F2

The only `hardware_events_any.store(true)` additions in the diff are six lines added
to six existing tests so their assertions keep passing. This is exactly the failure
mode CLAUDE.md names: *"never assert a value you haven't independently derived. A
self-consistent-but-wrong test is worse than no test."* 396 tests pass while the
emulator cannot boot. No test covers "Core 7 raises an SMPC IRQ and Master SH-2
services it", which is what would have caught this.

## F4 — the measured `yield_now` fix was reverted without mention

`std::thread::yield_now()` is back at `sh2.rs:4496`, and the comment block recording
*why* it was removed (with the 10.01s → 4.13s A/B) is gone. That was a measured 2.4x
on the emulator's hottest loop. The three `lib.rs` pointer comments survive and now
describe a state that no longer exists, so the code contradicts itself.

## F5 — `park_for_sleep` would deadlock the system clock if Master ever reached it

`sync.rs:165-170`. Master SH-2 is the *only* source of cycle-driven timing —
V-Blank, H-Blank, SCU timers and SMPC dispatch all advance from `Sh2::step`'s own
cycle accounting (CLAUDE.md, `sh2.rs:2446-2483`). If Master parks inside a `SLEEP`
opcode waiting for `hardware_events_any`, nothing advances cycles, so nothing can
ever raise the event that would wake it. It is not the cause of the current failure
(BIOS offset `0x52E` is the only `SLEEP` in the first 4KB and boot dies before it),
but it is a latent hard deadlock. A CPU waiting on an interrupt must stay in the
cycle-advancing loop, or the timing source has to move off Master first.

## F6 — `cargo fmt` was not run; CLAUDE.md requires it

`cargo fmt --check` fails on `sh2.rs:2766` and `sh2.rs:4942`. `sync.rs:54`'s
`pub fn sync_core` is also indented at column 0 inside the `impl`.

## F7 — 18 throwaway scripts committed to the repo root

`fix_core4.py`, `fix_core5.py`, `fix_flags.py`, `fix_instant.py`, `fix_newlines.py`,
`fix_perf_doc.py`, `fix_perf_doc2.py`, `fix_sh2_final.py`, `fix_sh2_run_loop.py`,
`fix_shutdown.py`, `fix_sleep.py`, `fix_sleep2.py`, `fix_sync3.py`,
`fix_sync_again.py`, `fix_sync_order.py`, `fix_sync_robust.py`,
`fix_test_newlines.py` … — untracked, but they are working-tree litter that should
be deleted, not left for the next session to wonder about.

## F8 — duplicated statement

`lib.rs:607-608`: `sync_c7.set_thread_active(4, true);` twice in a row.

## F9 — `m68k_control` is now half-dead and inconsistent

Core 4 no longer reads it (`lib.rs:426` became a bare `loop`), and Core 7 no longer
writes it (`lib.rs:604-611` switched to `set_thread_active`). But `sh2.rs:2663` and
`sh2.rs:2676` still write it, and `lib.rs:392`/`lib.rs:555` still clone it into
threads that never use it. `sh2.rs:4908`'s test still asserts the old behaviour.
Either the flag is the M68K run/stop state or `set_thread_active(4, …)` is — having
both, disagreeing, is worse than either.

## F10 — Core 4's cycle accounting makes it a lockstep anchor (pre-existing, now reachable)

`lib.rs:436` advances `cycles` by **2** per iteration while executing **200** M68K
instructions. Once SNDON activates Core 4, it reports a cycle count that grows ~100x
slower than real, becoming the minimum active core and dragging Master SH-2 down —
Chapter 32's mechanism exactly. Not triggered in this boot (Core 4 never activates
before the hang), but it is armed.

## F11 — `check_bus_miss` calls `std::env::var` on every bus access (pre-existing)

`sh2.rs:374-377`. `std::env::var` scans the environment and allocates per call; it
shows in the profile as `CStr::from_bytes_with_nul` at 0.90%, under `check_bus_miss`.
Should be a `OnceLock<bool>` read once at startup. Same shape as `MIMAS_DEBUG_VDP2`
(`lib.rs:354`, per frame) and `MIMAS_DEBUG_M68K` (`lib.rs:417`, per wake).

## What is genuinely right in this commit

- The §1.2b summary-word *design* is correct and is the right answer to the
  per-instruction RMW problem; only the producer wiring is missing (F2).
- `is_shutdown()` becoming a relaxed atomic load is correct (F1 is the missing half).
- ~~Removing `Instant::now()` from `sync.rs` is a real §1.5 fix.~~ **Retracted:**
  this was taken from the commit's own summary and never verified, and it is false.
  `sync.rs` still calls `std::time::Instant::now()` around its Condvar wait to feed
  `telemetry::record_idle_time`; `9354fd3` never touched those lines and the per-core
  blocked-time telemetry still works (Core 0 at 580 ms blocked, Core 5 at 2987 ms, on
  a 3.07 s boot). The §1.5 violation stands, unfixed, and is recorded as open in
  `.development/current_bugs.md`.
- Driving Core 5 from SH-2 cycles instead of a wall clock is the right direction
  (§1.4); `128 * 649` ≈ 83,072 matches 28.6364 MHz / 44.1 kHz.
- `docs/mimas-architecture-spec.md` §1.2b and the `mimas-performance-analysis.md`
  §2.3 correction are accurate.

## Suggested order

1. F1 and F2 (both are one-line-class fixes and together restore boot)
2. Re-run the boot: it must settle at `0x06001694` with ~2.55M WRAM accesses
3. F4 (re-apply the `yield_now` removal + its comment), F6, F7, F8
4. F3 — add a real cross-thread test before trusting the suite again
5. F5, F9, F10, F11 as follow-ups

---

# Resolution — all findings applied

Applied in the working tree (not committed). `cargo test --workspace`: **396 passed,
0 failed**. `cargo fmt --check`: clean. Boot restored and faster than any previous
measurement.

| finding | outcome |
|---|---|
| F1 `shutdown_flag` never stored | **fixed** — `request_shutdown` now stores it (`sync.rs`) |
| F2 summary word never set by producers | **fixed** — `WorkRam::raise_*()` helpers publish flag + summary; all 5 producers converted; consumer now `swap`s with `Acquire` instead of load-then-store |
| F3 tests patched instead of producers | **fixed** — the 6 test edits reverted; tests now call `raise_vdp1_draw_end()`, exercising the real path |
| F4 `yield_now` re-introduced | **fixed** — removed again, rationale comment restored |
| F5 `park_for_sleep` deadlock | **fixed** — call and function removed; `SLEEP` documented as a still-open §1.5 gap that cannot be closed at the opcode |
| F6 `cargo fmt` | **fixed** |
| F7 18 `fix_*.py` in repo root | **fixed** — deleted |
| F8 duplicated `set_thread_active(4, true)` | **fixed** |
| F9 `m68k_control` half-dead | **fixed** — SNDOFF on the Master path now deactivates Core 4 (it previously could not stop the sound CPU at all); Core 7 keeps the flag in step |
| F10 Core 4 cycle under-reporting | **fixed** — reports real SH-2-equivalent cycles via an exact rational, chunked to the slack window |
| F11 `env::var` per bus access | **fixed** — `bus_trace_enabled()`/`set_bus_trace()`; also fixed the second gate in `log_bus_miss_once` that the first pass missed, and made the test deterministic instead of `set_var`-racy |

**Root cause of the boot failure was none of F1–F11.** It was Core 5's cycle step:
`128 * 649 = 83,072` against a `slack_limit` of 1000. `LockStepSync` blocks a core
more than `slack_limit` ahead of the slowest, so a core whose quantum is 83x the
window can never be inside it. Found by bisecting the commit's hunks, not by
reading. Cores 4 and 5 now both chunk their cycle reports to stay within the
window; the invariant is documented at both sites and in `history.md` Chapter 40.

## Measured

| | before Ch.39 | Ch.39 (`yield_now`) | `9354fd3` | now |
|---|---|---|---|---|
| settle PC | `0x06001694` | `0x06001694` | `0x000002B0` | `0x06001694` |
| WRAM accesses | 2,554,350 | 2,554,734 | 4 | 2,554,308 |
| wall clock | 10.01 s | 4.05–4.15 s | 0.72 s (dead) | **3.17–3.31 s** |
| process CPU | 139% | 152% | 48% | 145% |

2.7x–3.2x against the original baseline, across three consecutive runs.

## Still open (see `.development/current_bugs.md`)

`SLEEP` spinning; `sync_core`'s unconditional `notify_all` (~270K voluntary context
switches/sec remain); per-core blocked-time telemetry now has no call site;
`env::var` on the per-frame and per-wake debug paths.

**The 15 VDP2 Phase 2/3 findings are untouched and still open** —
`git show 9354fd3:docs/current_review.md`.
