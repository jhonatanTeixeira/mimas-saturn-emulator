# QWEN.md — working on Mimas

Mimas is a Sega Saturn emulator in Rust, one OS thread per hardware chip. This
file is for Qwen Code. `CLAUDE.md` is the full reference; read it when this file
is not enough.

This file says **how to work** here. It deliberately holds no project state — no
baselines, no addresses, no list of open bugs — because state goes stale and a
stale instruction is worse than none. Section 4 says where state lives. Where any
document disagrees with a measurement, **the measurement wins**.

Everything below comes from something that actually went wrong here. None of it
is style advice.

---

## 1. Six rules. Break one and the work is rejected, however green it looks

1. **Do not touch `tools/`.** `quality_gate.sh`, `golden_rules.py`,
   `antipattern_scan.py`, `antipatterns/corpus.json`, `diff_coverage.py`,
   `bios_reference/` and the rest are the measuring instrument. Editing them
   while measuring is measuring yourself. If you think one is wrong, **stop and
   report it**. Do not fix it.
2. **Do not silence a check.** No `let _ =` in place of an assertion, no
   `assert!(true)`, no `// no-assert:` or `// golden-rule-ok:` to turn something
   green, no `#![allow(...)]`, never `cargo clippy --fix`, never reshape code so a
   checker stops matching it.
3. **Do not lower a threshold.** Every `MIMAS_*` floor only goes up.
4. **Leave no debug code behind.** Every `eprintln!` probe, dump file and scratch
   script you add, you remove before reporting. No hard-coded `/tmp/...` paths
   with `.unwrap()`. Scratch files go in `/tmp`, never the repo root.
5. **Do not commit.** Leave the work in the tree and report. A human reviews and
   commits.
6. **Take Yabause's knowledge, never its code.** Its understanding of the
   hardware is sound — it runs the whole Saturn library — so read it for what
   opcodes, flags and registers do (`sh2int.c`, `memory.c`, the peripheral
   files). Its implementation is a slow, 30-year patchwork with an architecture
   nothing like Mimas's, broken in places (FMV): never copy code, structures or
   control flow from it. `golden_rules.py` fails on Yabause's own names in Mimas
   code (`T1ReadLong`, `SH2_struct`, `MappedMemoryReadLong`, `CurrentSH2`,
   `c68k_*`, …) with no escape hatch. Citing it in a comment
   (`// understood from yabause/src/memory.c:120`) is fine.

---

## 2. The architecture rule

**Only Core 0 (`sh2-master`) and Core 1 (`sh2-slave`) loop continuously.** Every
other thread parks in `LockStepSync::park_while_inactive` and is woken by a real
hardware event. No polling loops, no `thread::sleep`, no `Instant::now()` to
decide *when* something happens. Hardware timing (V-Blank, H-Blank, SCU timers)
comes from Master SH-2's own executed-cycle count.

This was broken more than once, and each time it quietly slowed the whole
emulator by up to two orders of magnitude, because one slow *active* thread holds
every other active thread back through the lockstep window. If you cannot do
something without polling, stop and say so.

---

## 3. Commands

```bash
cargo build -p saturn-frontend-native --release --bin saturn-frontend-native
cargo test --workspace                 # must stay green after every change
cargo fmt --all
```

Paths on this machine:

```bash
BIOS=scratch/ra_system/saturn_bios.bin
CHD="/media/jhonatanteixeira/Novo volume/projects/jhon/dreams/mkr.chd"
```

---

## 4. Where the state of the project lives

Read these before starting. They change as work lands; this file does not.

| what | where |
|---|---|
| how BIOS progress is measured, the current baseline, the current divergence, the plans to close it | `docs/unlock_bios.md` and `docs/unlock_bios/` |
| known bugs and debt | `.development/current_bugs.md` |
| findings of the latest review | `docs/current_review.md` |
| exact hardware behaviour | `docs/hardware-reference/` |
| why a design choice was made | `history.md` |

**Reproduce the recorded baseline before changing anything.** If your first
measurement does not match what `docs/unlock_bios.md` records, stop and find out
why. Either the document is stale or your setup differs, and both must be
settled before any number you produce means something.

---

## 5. Traps that already cost whole sessions

**A frozen PC is not progress.** A PC that stops changing usually means the
thread is *blocked*, not finished. An address where the run settled is not an
address the boot reached on purpose.

**Know the instrument's blind spots before trusting a gap.** Every tracer here
misses something: delay slots, the first instruction after an interrupt is
taken, how many times an address ran as opposed to whether it ran. Before
chasing a "missing" address, confirm the tool could have seen it. The blind
spots are documented next to each tool; read them.

**The first error in the log is usually a consequence.** An "unimplemented
opcode" or a crash is where the damage became visible, not where it happened.
Find out what state led there before looking for a missing instruction.

**A debug probe changes the result.** A probe in the hot path changes timing
between threads, and a probe can reuse another diagnostic's counter and silence
it. When a debug-only change makes a bug disappear, suspect the probe first.

**Stale builds and filtered diffs.** When comparing two builds, `touch` the
sources and check that the binary's `md5sum` actually changed; a two-second
"build" can be a cached artefact. Read the whole `git diff`, not a `grep` of
it — a filtered diff once hid a probe for an entire afternoon.

---

## 6. The work loop

1. **Measure**, as `docs/unlock_bios.md` says. Write down the result.
2. **Locate** the first divergence from the real boot.
3. **Explain** it: what should happen there, and why ours does not.
4. **Fix** it in Mimas's own code. Get the exact semantics from
   `docs/hardware-reference/` first, then `../yabause/src/`; write the
   implementation yourself (rule 6). A test with hand-derived values comes
   first.
5. **Measure again. The measured progress must move.** If it did not, that was
   not the fix.

**Stop rule.** One divergence at a time. If two fixes in a row do not move the
measurement, stop. Write in `docs/current_review.md`: the address, what it does,
who should reach it, both hypotheses and how each was ruled out, and what you
would measure next. A ruled-out hypothesis with evidence is worth more than code
that did not move the number.

**When a divergence closes**, update `docs/unlock_bios.md` with the new
measurement and report it. The human raises the gate's floor so the gain cannot
be lost.

---

## 7. Before you report

```bash
cargo build -p saturn-core
MIMAS_COVERAGE_COMMITS=HEAD bash tools/quality_gate.sh
.venv/bin/python tools/antipattern_scan.py scan
```

`MIMAS_COVERAGE_COMMITS=HEAD` holds *your change* to the 90% coverage floor
instead of the whole tree. If it is red, it names the uncovered lines: write
tests for them.

Run the gate once **before** you change anything and keep the summary. A step
that was red before you started is recorded debt; a step that turned red during
your work is yours.

Your report contains:

- the progress measurement **before and after**;
- both gate summaries, pasted, not paraphrased;
- every file you changed and why;
- anything you think is wrong in `tools/`, reported, not fixed.

"I unblocked X" only counts if the measurement moved. If you did not measure it,
it did not happen.
