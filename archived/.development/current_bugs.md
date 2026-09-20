# Current bugs — SH-2 core

Full detail lives in `docs/implementation-plans/sh2-cpu.md` §0.9 ("Tracked defects") —
this file is the running index `CLAUDE.md` asks for, not a duplicate of the prose.

## Resolved (2026-08-08)

- **D-21** — `MAC.W` (`0x400F`) charged 1 cycle instead of 3; `MOV.L @(R0,Rm),Rn`
  (`0x000E`) charged 3 instead of 1 — wrong mask in `get_base_cycles` conflated the two.
  Fixed `saturn-core/src/sh2.rs`. Test: `test_cycles_d21_mac_w_and_movl_r0_indexed`.
- **D-22** — `STC.L {SR,GBR,VBR},@-Rn` (push) had no cost entry in `get_base_cycles` at
  all, silently charged the 1-cycle default instead of the real 2. Fixed. Test:
  `test_cycles_d22_stc_l_control_regs`.
- **D-23** — `RTS`/`RTE`/`SLEEP` cost functions used exact `opcode ==` instead of
  `& 0xF0FF`, so nibble B ≠ 0 dropped the cost to the 1-cycle default even though
  `execute()` already dispatches these correctly for any B (real hardware ignores B for
  these 0-operand forms — same class as the already-fixed D-5, but in the separate cost
  function, missed by Phase 1). Fixed. Test: `test_cycles_d23_nibble_b_is_dont_care`.

All three found via an exhaustive opcode-by-opcode audit of `get_base_cycles`
(`sh2.rs:654-753`) against `retroarch-cores/yabause/src/sh2int.c` (real cycle counts per
handler), prompted by a sibling project (`portal_to_another_world`) finding 8 analogous
decode-table bugs in its own SH-2 disassembler the same day using the same method. Full
citations and the audit methodology: `docs/implementation-plans/sh2-cpu.md` §0.2 status
update and §0.9. Verification: `cargo test --package saturn-core` → 79 passed in the `sh2`
module (was 76), 0 failed; `cargo test --workspace` → 226 passed, 0 failed. D-21's test was
confirmed to actually fail before the fix (reverted locally, saw `left: 1, right: 3`,
restored) — not a test that would have passed regardless.

The same audit also exhaustively proved `execute()`'s decode-set (as opposed to
cycle-cost) matches `sh2int.c` for all 65536 opcodes with zero divergence — see
`docs/hardware-reference/sh2-cpu.md` §9.11. It did **not** re-verify per-handler semantic
correctness beyond a handful of spot-checks — that's open work, not claimed as done.

## Not individually re-verified this pass (D-1 .. D-20)

`docs/implementation-plans/sh2-cpu.md` §0.9 has full detail on each. Whether each is
still open or was closed during Phase 1/2 (both marked `[x]` in the same document) was
**not** re-checked item-by-item in this pass — that cross-check is itself open work.
D-1 (`OR`/`XOR` swap) was independently spot-checked as fixed while investigating D-21/22/23
(`sh2.rs:2861/2865` map correctly today: `0xCA00`→XOR, `0xCB00`→OR); the rest (D-2 through
D-20) are unverified here.

## Known Unknowns

These are VDP1 register fields that are stored but deliberately left undecoded because their effect on real hardware is not determinable from the source (see `docs/hardware-reference/vdp1.md` §12 items 1-3):

- **TVM2** (TVMR bit 2)
- **EOS** (FBCR bit 4)
- **HSS** (CMDPMOD bit 12)
- **PCLP** (CMDPMOD bit 11)

## Synchronization overhead (measured, Chapter 39)

Found while measuring the per-instruction `sched_yield` removal. Both are real,
both are open, neither is a correctness bug — they are throughput defects in the
sync layer, measured on a real BIOS boot rather than reasoned about.

- **`sync_core` notifies unconditionally.** `LockStepSync::sync_core` ends with
  `self.condvar.notify_all()` on *every* call (`saturn-core/src/sync.rs:96`),
  including when nothing that could unblock a waiter has changed. Core 0 calls it
  every 32 guest cycles; Core 5 (SCSP) is Condvar-blocked ~90% of wall clock and
  can only make progress once per ~500 cycles, so roughly 15 of every 16 wakeups
  it receives are spurious. Measured cost: **~228,000 voluntary context switches
  per second**, essentially all of the remaining total. This is Chapter 10's
  pathology in a second form — that fix split the condvars so *parked* cores stop
  being woken for nothing; *active-but-drifted* cores still are. A fix needs
  `sync_core` to notify only when the minimum active cycle count advanced far
  enough that some waiter could actually proceed, which means tracking waiter
  thresholds in `SyncState` — a real change to a primitive with its own
  dedicated tests (`sync_tests.rs`, `adversarial_tests.rs`), not a one-liner.

- **Telemetry counters are two globally-shared atomics on the memory hot path.**
  `WRAM_READS`/`WRAM_WRITES` (`saturn-core/src/telemetry.rs:6-7`) are single
  `AtomicU64`s incremented from `shared_buffers.rs:119-238` on every WRAM access
  by both SH-2 cores, so they are a guaranteed cache-line ping-pong between
  cores. `TELEMETRY_ENABLED` defaults to `true` and nothing ever sets it false.
  This is instrumentation cost, not architecture — unmeasured so far, and it
  should be measured before it is either made opt-in or left as-is.

## Open after Chapter 40

- **The SH-2 `SLEEP` opcode still spins at 100% instead of parking.**
  `saturn-core/src/sh2.rs` (opcode `0x001B`). `docs/mimas-architecture-spec.md`
  §1.5 names `SLEEP` explicitly as a case that must yield. It cannot be fixed at
  the opcode: Master SH-2 is the system's only timing source (V-Blank, H-Blank,
  SCU timers and SMPC dispatch all advance from `Sh2::step`'s cycle accounting),
  so a parked Master stops advancing the cycles that would raise the event that
  wakes it -- a hard deadlock. `9354fd3` shipped exactly that and it was reverted.
  Closing this needs the timing generator moved off Master first.

- **`sync.rs` reads the host wall clock, which §1.5 forbids on component threads.**
  `std::time::Instant::now()` brackets the Condvar wait in `sync_core` to feed
  `telemetry::record_idle_time`. Every non-CPU component thread that blocks on drift
  executes it, and `docs/mimas-architecture-spec.md` §1.5 says those threads may not
  reference the host clock "at all". The instrumentation is genuinely useful -- it is
  what found the `yield_now` and Core 5 problems -- so the fix keeps the signal and
  drops the clock: a per-core *block counter* needs no `Instant`.
- **Core 4 and Core 5 must keep their `sync_core` step at or below
  `slack_limit`.** Both now chunk their cycle reports to satisfy this. It is an
  invariant of `LockStepSync`'s bounded-slack model, not a style choice: a core
  whose quantum exceeds the window can never be inside it. Violating it stopped
  the BIOS booting (see `history.md` Chapter 40). `test_saturn_system_startup_shutdown`
  (`saturn-core/tests/sync_tests.rs`, `with_slack(10)`) is the canary.

- **`std::env::var` is still called on hot-ish paths outside `check_bus_miss`.**
  `MIMAS_DEBUG_VDP2` (`saturn-core/src/lib.rs`, once per rendered frame) and
  `MIMAS_DEBUG_M68K` (once per Core 4 wake). Far less hot than the bus path that
  was fixed, but the same defect; `sh2.rs`'s `bus_trace_enabled`/`set_bus_trace`
  pair is the pattern to copy.

## Technical debt — test coverage

**Measured 2026-09-17: 65.03% (5853/9000 lines), against a 90% gate.**
`tools/quality_gate.sh` step 5 enforces 90% with **no `--exclude-files`**, so this
step is red today and stays red until the gap closes. That is deliberate: adding
exclusions until the number reaches the target measures how many exclusions were
added, nothing else. Everything below the bar is listed here instead of hidden
behind a flag.

Roughly 2,500 lines of coverage are missing, concentrated in:

| file | covered | note |
|---|---|---|
| `m68k.rs` | **24%** (178/741) | worst gap. Clippy already found 3 opcode masks in here that can never match (`0x4E68`, `0x0148`, `0x0188` against their own masks) -- dead decode branches that tests would have caught |
| `telemetry.rs` | 23% (11/47) | instrumentation; low value to test, but it is product code and counts |
| `vdp2_regs.rs` | 35% (88/248) | register accessors. The VDP2 review found 4 of them reading the *wrong offset* (`scyn0`, `scyn1`, `craofb`, `spctl`) -- exactly what per-accessor tests exist to catch |
| `scsp.rs` | 46% (24/52) | synthesis is barely implemented yet |
| `scu_dsp.rs` | 61% (350/577) | |
| `lib.rs` | 61% (189/309) | thread spawn/wiring; hard to unit-test, needs integration coverage |
| `vdp.rs` / `vdp2.rs` | 63% | |
| `sh2.rs` | 66% (1530/2329) | largest file; the absolute gap here (~800 lines) is the biggest single chunk |
| `cs2.rs` | 67% (658/989) | |
| frontends (`main.rs`, `mimas_window.rs`, `window_test.rs`, libretro) | **0%** (0/227) | no tests at all |

Already at or above the bar, for reference: `sync.rs` 96%, `bus_arbiter.rs` 94%,
`throttle.rs` 91%, `cdrom.rs` 90%, `peripheral.rs` 89%, `scu.rs` 87%, `smpc.rs` 87%.

The two files with the worst coverage are also the two where independent review
has already found real defects. That is not a coincidence, and it is the argument
for the 90% bar rather than against it.

## Technical debt — tests that assert nothing

`tools/assertionless_tests.py` (quality gate step 5) finds `#[test]` functions
with no assertion at all. Two remain open:

- `e2e-tests/src/lib.rs:81` `test_tier1_f1_lockstep_initial_sync` — calls
  `sync_core` on four cores and checks nothing. Since `sync_core` now returns
  `bool`, "all four cores were active and none blocked" is one line away.
- `e2e-tests/src/lib.rs:473` `test_tier2_f1_lockstep_negative_or_overflow_drift`
  — the worse of the two. It feeds `u64::MAX` to core 0 and `10` to the others
  specifically to exercise the `wrapping_sub` drift arithmetic, and then asserts
  nothing, so the overflow behaviour its own name promises is unverified. The
  correct expectation is derivable from the primitive: `u64::MAX - 10` as `i64`
  is `-11`, not an enormous positive drift, so core 0 must *not* block.

Two others were reviewed and marked `// no-assert: <reason>` rather than
"fixed", because proving the absence of a panic genuinely is the point there:
`scu.rs`'s `no_slave_target_wired_is_a_silent_no_op` and `sh2.rs`'s
`test_cycles_p8_t5_throttle_end_to_end`.

Separately, clippy flags four `assert!(true)` placeholders in `vdp.rs`
(`vdp1_one_cycle_mode_erases_and_swaps_every_frame`,
`vdp1_manual_erase_runs_just_before_swap`, `vdp1_cpu_port_reads_back_bank`,
`vdp1_system_clip_applies_unconditionally`). They pass, they count toward the
green test total, and their names claim the VDP1 framebuffer swap is verified —
the same feature whose half-written state was hidden when `clippy --fix`
prefixed its unused FBCR variables with `_`. Deleting them is more honest than
keeping them; writing them for real is VDP1 Phase 5 work.

And one semantic tautology no structural tool catches: `vdp2.rs`'s
`colornumber_2_ignores_paladdr` asserts `pixel.is_some() || pixel.is_none()`.

## Open — VDP2

- **`saturn-core/src/vdp.rs`, `render_nbg_layer` — colour-calculation ratio
  mapped to the wrong layer.** The two `match layer` tables in the function
  disagree. The priority table names its registers (`0 => regs.prina_nbg0()`),
  so `layer 0` is NBG0; the ratio table says `0 => regs.ccrnb() >> 8`, which is
  NBG3's ratio — the mapping is reversed for all four layers. Correct mapping,
  from `docs/hardware-reference/vdp2.md` ("Each 5-bit ratio field…" table):
  NBG0 `CCRNA & 0x1F`, NBG1 `(CCRNA >> 8) & 0x1F`, NBG2 `CCRNB & 0x1F`, NBG3
  `(CCRNB >> 8) & 0x1F`. No test pins it, which is why it has come back twice;
  the fix needs one with hand-derived values.

## Golden-rule violations (tools/golden_rules.py)

`golden_rules.py` reports none (2026-09-19). The two entries that used to live
here closed in different ways:

- **Core 5 (SCSP) never parked** — fixed: it now parks and synthesizes in
  batches from Master SH-2's cycle count (`history.md`, the chapter on SCSP
  parking).
- **`saturn-core/src/sync.rs:102`, `Instant::now()` on component threads** —
  *not* fixed the way this file prescribed (replace the timing with a per-core
  block counter, which needs no clock). It is excused with
  `// golden-rule-ok: telemetry measurement, not a deadline pacing timer`. Spec
  §1.5 says component threads "may not reference the host wall clock at all";
  whether telemetry is an exception to that is a decision for the human, not
  for the marker.

## How these are checked

Most entries above now have a tool that reports them on every run, so this file
records *why* they are open rather than serving as the only record that they are:

- `bash tools/quality_gate.sh` — its summary is the current state; this file
  records *why* a step is red, not *whether* it is.
- `.venv/bin/python tools/antipattern_scan.py scan` — architectural patterns with
  no fixed spelling. Review queue, not a verdict.

`docs/quality-gate.md` explains each step and the one honest way to turn it green.
Anything fixed here should stop being reported there; if it does not, the fix did
not land.

- **CD block poll fires before the BIOS's first status read (no disc).** `reset_system()` now resets to PAUSE, but `Cs2::exec`'s 333,333 µs poll moves an empty drive to NODISC before the BIOS reads CR1 (`0x27` instead of the real `0x21`), so `0x000033DC`/`0x000032DC` are still not reached without a disc. Forcing the poll off makes both reachable. See `history.md` Chapter 44.
