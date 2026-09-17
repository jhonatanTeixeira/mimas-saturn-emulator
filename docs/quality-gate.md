# The quality gate

```bash
bash tools/quality_gate.sh                 # 9 steps, ~6 min
.venv/bin/python tools/antipattern_scan.py scan   # semantic pass, ~1 min
```

Both are mandatory before work is called done. This document explains what each
step checks, why it exists, and the one honest way to turn it green.

## Why this exists

On 2026-09-17 two agents, in one working day, between them:

- broke the BIOS boot completely — a summary word read once per emulated
  instruction whose only `store(true)` lived inside `#[cfg(test)]`, so every SMPC
  IRQ, NMI, system reset, clock change and VDP1 draw end was dropped in
  production
- left `LockStepSync::is_shutdown()` returning `false` forever — a mirror flag
  declared, initialised, read, and never written, which silently disabled
  `PanicGuard`, whose entire job is to stop one core's panic from hanging the
  others
- reintroduced a `sched_yield` syscall per emulated instruction that had been
  measured and removed the same morning (10.01 s → 4.13 s to the same boot PC)
- shipped four `assert!(true)` placeholder tests whose names claim the VDP1
  framebuffer swap is verified
- ran `cargo clippy --fix` and auto-committed it, which turned an m68k opcode
  guard into `if cond {}` and silenced the warnings that were flagging
  half-written VDP1 code

`cargo build`, `cargo test` (396 green), `cargo clippy` and the coverage step
were all happy through every one of those. Each step below exists because
something real got past everything that came before it.

---

## The nine steps

### 1 — Formatting

`cargo fmt --all -- --check`. **Honest fix:** `cargo fmt --all`.

### 2 — Mess detect + complexity (clippy, READ-ONLY)

`cargo clippy --workspace --all-targets -- -D warnings -W clippy::cognitive_complexity`

Thresholds live in `clippy.toml`, calibrated against this codebase rather than
copied: `cognitive-complexity-threshold = 25`, `too-many-arguments-threshold = 7`,
`type-complexity-threshold = 250`.

`clippy::too_many_lines` is deliberately **not** enabled. Function length here is
dominated by opcode decode tables — the largest is 863 lines — and gating on
length would only pressure people into splitting tables for no gain. Cognitive
complexity scores branching and nesting instead, so a flat 800-line `match`
barely registers while genuinely tangled control flow does.

**Never run `cargo clippy --fix`.** It was run once and auto-committed. Most of
what it did was harmless, but `needless_return` rewrote

```rust
if (opcode & 0xFFF0) == 0x4E60 || (opcode & 0xFFF0) == 0x4E68 { return; }
```

into `if ... {}` — identical today only because that `if` happens to sit last in
its function, and a silent trap the moment anything is appended after it, in a
file that is otherwise a chain of `if <opcode matches> { …; return; }` guards. It
also silenced six `unused variable` warnings in the VDP1 FBCR handler by
prefixing them with `_`; those warnings were the only signal that the
framebuffer-swap logic there is half-written.

Reading clippy's output, by contrast, pays for itself: the same run reports three
M68K opcode masks that can never match (`0x4E68 & 0xFFF0 == 0x4E60`, so that
branch is dead) and a `|| true` that makes a VDP1 mask check pointless.

**Honest fix:** fix the finding, or `#[allow(...)]` it *with a comment saying why
clippy is wrong here*.

### 3 — Compilation · 4 — Tests

`cargo build --release --workspace`, `cargo test --workspace`.

### 5 — Golden rules (architecture spec)

`python3 tools/golden_rules.py`, preceded by `--self-test`.

Checks the invariants `docs/mimas-architecture-spec.md` states and no compiler
enforces. Nine rules, each citing its spec section:

| rule | spec | catches |
|---|---|---|
| `no-pool-or-async` | 1.1 | `tokio`, `rayon`, `async fn`, `.await`, `ThreadPool`, `fork(`, `available_parallelism` |
| `no-atomic-poll-loop` | 1.2 | an atomic `.load()` as a loop condition |
| `thin-instruction-path` | 1.2b | `.lock()`/`.swap()`/`.fetch_*` on the per-instruction path |
| `atomic-has-producer` | 1.2b | an atomic consumed in production but never raised there |
| `single global lock` | 1.3 | *(see the semantic pass — no deterministic form)* |
| `throttle-cpu-only` | 1.4 | `ClockThrottle` on a non-CPU thread |
| `no-wall-clock` | 1.5 | `Instant::now()` on a component thread |
| `no-yield-now` | 1.5 | `thread::yield_now()` in production |
| `threads-park` | 1.5 | a spawned thread that never reaches `park_while_inactive` |
| `field-is-written` | — | a field declared and read but never assigned |

**`--self-test` runs first, and a failure there fails the step.** It replays each
rule against the commits where the bug actually existed — `9354fd3` must trip
`atomic-has-producer` and `field-is-written`; `194572f` must stay silent on the
first and trip `thin-instruction-path`. A check nobody has watched fail is not a
check, and this one earned its place immediately: it caught two wrong versions of
`atomic-has-producer` before either shipped. The first counted any `store` as
production, but the consumer clears the flag with `store(false)`, so every flag
looked produced and it found nothing. The second counted stores of truthy
*literals* and missed `store(if is_352 { 2 } else { 1 }, …)`, reporting a healthy
commit as broken.

`tools/rustscan.py` underneath does three things and nothing else: blank out
string/char literals and comments so brace counting is reliable, excise
`#[cfg(test)]` modules so "production code" means production code, and return a
named function's body so a rule can say "inside `step`". It is not a parser and
has no dependencies — the gate refuses to install tools, and every dependency is
one more way for it to be unavailable on the machine that needs it.

A regex version of `atomic-has-producer` was written first and **missed the bug it
was written for**: it treated production code as everything before the first
`#[cfg(test)]`, and `sh2.rs` has one at line 2576, about 4,700 lines before the
end of the file. Half the production code was silently discarded. Every rule here
is about *scope*, and regex does not know where a module begins or ends.

**Honest fix:** fix the violation, or `// golden-rule-ok: <reason>` **on the
offending line**, or in the comment block immediately above it.

### 6 — Tests that assert nothing

`python3 tools/assertionless_tests.py`. Finds `#[test]` functions with no
assertion, and assertions that are constants.

Separate from coverage and from clippy on purpose:

- **Coverage is blind to this.** A test that runs code and asserts nothing
  reports 100% coverage of that code.
- **Clippy's `assertions_on_constants`** catches `assert!(true)` — it found four
  in `vdp.rs` — but a crate-level `#![allow(clippy::assertions_on_constants)]`
  switches it off for the whole crate in one line, which is what happened. So
  this tool detects constant assertions itself: *a check that can be disabled by
  the code it checks is not a check.*
- **Neither catches a semantic tautology.** `assert!(pixel.is_some() ||
  pixel.is_none())` in `vdp2.rs` passes every structural check and was found by
  reading.

**Honest fix:** write a real assertion, or `// no-assert: <reason>` if proving
the absence of a panic genuinely is the point.

### 7 — Coverage (90%)

`cargo tarpaulin --ignore-tests --fail-under 90`, **with no `--exclude-files`**.

Measured 2026-09-17: **65.03%** (5853/9000 lines). This step is red and stays red
until the gap closes. Adding exclusions until the number reaches the target
measures how many exclusions were added, nothing else. What is below the bar is
recorded in `.development/current_bugs.md` instead of hidden behind a flag.

`--ignore-tests` excludes the test functions' own bodies from the denominator.
That is what coverage means, not an exclusion of product code.

Worth knowing: the two files with the worst coverage are the two where
independent review has already found real defects — `m68k.rs` at 24% has three
opcode masks that can never match, `vdp2_regs.rs` at 35% had four accessors
reading the wrong register offset. That is not a coincidence.

**Honest fix:** write tests.

### 8 — Code size

Source lines (excluding `target/`) against `MIMAS_LOC_MAX`, release binary
against `MIMAS_BIN_MAX_MB`. The previous version ran
`find . -name "*.rs" | xargs wc -l`, walked `target/`, and reported 243,067 lines
against a real source tree of 28,275 — an 8.6x inflation that made the metric
useless.

### 9 — Smoke test (real BIOS boot)

Boots a real BIOS and asserts three things:

| assertion | default | catches |
|---|---|---|
| Core 0 settle PC | `0x06001694` | functional regression |
| WRAM accesses | ≥ 2,000,000 | exited cleanly without doing the work |
| emulated speed | ≥ 150% of real SH-2 | performance regression |

The previous version ran the binary and checked only its exit code — so when
`9354fd3` broke the emulator badly enough that Core 0 died at `0x2B0` after **4**
memory accesses, the binary still exited 0 and this gate went green. A smoke test
that cannot fail is not a smoke test.

**When boot progresses past this point, update `MIMAS_GATE_PC` and
`MIMAS_GATE_MIN_WRAM`.** A further settle PC is progress. An earlier one is a
regression. Do not delete the check to make it pass.

---

## Thresholds: tightening is free, loosening needs a reason

Every threshold can be overridden from the environment, and the gate prints what
it is actually enforcing. Tightening (coverage 95, speed floor 200) is always
allowed. **Loosening fails the gate unless `MIMAS_OVERRIDE_REASON` says why:**

```bash
# legitimate — the R36S genuinely cannot hit the desktop speed floor
MIMAS_MIN_SPEED_PCT=15 MIMAS_OVERRIDE_REASON="measuring on R36S hardware" \
    bash tools/quality_gate.sh
```

Printing a warning was not enough. It made the weakening *visible*, and whoever
reports "the gate passed" can leave that line out. This was added the same day
`MIMAS_COVERAGE_MIN=80 ./tools/quality_gate.sh` was observed.

Changing `MIMAS_GATE_PC` counts as loosening whatever its value: swapping the
expected PC for whatever a broken build produces is precisely how a functional
regression turns green.

| variable | default | direction |
|---|---|---|
| `MIMAS_COVERAGE_MIN` | 90 | lower is looser |
| `MIMAS_MIN_SPEED_PCT` | 150 | lower is looser |
| `MIMAS_WARN_SPEED_PCT` | 170 | lower is looser |
| `MIMAS_GATE_PC` | `0x06001694` | any change is looser |
| `MIMAS_GATE_MIN_WRAM` | 2000000 | lower is looser |
| `MIMAS_LOC_MAX` | 34000 | higher is looser |
| `MIMAS_BIN_MAX_MB` | 16 | higher is looser |

---

## The semantic pass — `tools/antipattern_scan.py`

Deliberately **not** a gate step: it needs torch, transformers, torchao and two
models, while the gate is stdlib-only so it always runs. Run it separately,
before calling work done. Its output is a review queue, never a verdict.

```bash
.venv/bin/python tools/antipattern_scan.py list    # resolve the corpus, no ML
.venv/bin/python tools/antipattern_scan.py scan    # find look-alikes
.venv/bin/python tools/antipattern_scan.py gold    # measure recall
```

### What it looks for, and what it does not

Only the architectural rules — the ones with **no fixed spelling**. "This thread
polls instead of parking" can be a `while` on an atomic, a `loop` with a
`yield_now`, a sleep-driven deadline, or a mutex retaken every iteration. Same
violation, different text, and no regex reaches all four.

Everything with a fixed form belongs in `golden_rules.py` instead. A dead opcode
mask is `PAT & MASK != PAT` — arithmetic, so a deterministic check finds it every
time. An early version of the corpus was filled with exactly those things and
wasted the semantic layer on work a grep already did.

Five exemplars, each **real code that shipped in this repository**, pulled from
the commit where it existed:

| pattern | spec | from |
|---|---|---|
| wall-clock paces a component thread | 1.4/1.5 | `a54ab64` Core 3 |
| component thread polls instead of parking | 1.5 | `148cac7` Core 2 |
| mutex retaken in a loop, no Condvar | 1.2/1.5 | `194572f` Core 6 |
| timing generator derived from wall clock | 1.5 | `a54ab64` `tvstat_word` |
| single global lock | 1.3 | `194572f` `is_shutdown` |

### Pipeline

```
corpus (pointers into git history, with provenance)
  → GraphCodeBERT embeds every 12-line window of every function
  → top-K nearest windows per pattern, per-pattern threshold
  → Qwen3.5-0.8B answers one factual question per check
  → review queue
```

Four things about the configuration are load-bearing, and all four were arrived
at by measurement:

**Decorrelation.** GraphCodeBERT is pretrained with masked-LM and data-flow
objectives, not a contrastive one, so its raw embeddings are anisotropic — every
cosine inherits one dominant common direction. Measured on this tree, unrelated
functions sat at p50 **0.882**. After subtracting the mean it is **-0.026**, and
the usable p05–p95 range goes from 0.184 to 1.086. One subtraction, roughly six
times more range. Before this was found, the compressed distribution was read as
evidence that Rust must be outside CodeSearchNet's training data, and the
embedder was swapped out — an "improvement" that only compared a model used
correctly against one used wrongly.

**Windows, not whole functions.** An embedding summarises everything it is given,
so a 174-line decode function embeds as "a large decode function" and the two
lines holding the defect contribute almost nothing.

**Per-pattern `top_k` and `min_sim`.** These patterns have very different
prevalence, so one K for all of them is a guess — and the first version, with a
global `top_k=8` and a 0.80 cutoff, produced 16 false positives out of 16
reports. Each threshold is that pattern's own p99.5 against the windowed corpus.
**The scale moves whenever the query or the chunking changes** — whole-function
exemplars sat near 0.98, minimal patterns near 0.92, decorrelated near 0.55. Never
carry a number over; re-measure.

**One fact per call.** Asking "does this contain the same defect?" got YES on 16
of 62 — the question invites agreement. Replacing it with three chained checks
plus 30 words of reasoning got YES on **all 27**: more room to fill, more
agreement. A 0.8B model has short attention, so the fix is less prompt, not more
structure. What is left is recognition, not judgement — "does this code call
`env::var`?" — one question per call, one-token answer.

### Known limit

A controlled test showed the embedder ranks by **form, not meaning**:

| | cosine vs the pattern |
|---|---|
| same defect, written as a `match` instead of an `if` | 0.563 |
| `(opcode & 0xFFF0) == 0x4E60` — one hex digit different, **not** a defect | **0.994** |

So a violation written in a genuinely novel shape will be missed. This hurts less
for architectural patterns, which have recognisable form, but it is why the
output is a review queue and why `golden_rules.py` is the primary defence.

### Setup

Models and packages live beside the repo, not under `$HOME` — on the development
machine `/` runs at 88% while the repo's volume has 100 GB free. The script sets
`HF_HOME` itself, before importing transformers, rather than trusting the
environment.

```bash
python3 -m venv .venv
.venv/bin/pip install torch transformers torchao accelerate
# CPU-only box: add --index-url https://download.pytorch.org/whl/cpu to torch
```

Both models are sized to run on CPU; a GPU is used automatically when present.
Model revisions, the quantization config (`Int8WeightOnlyConfig`), the similarity
thresholds and the fitted corpus are all part of the determinism contract —
changing any one of them can flip a verdict on the same snippet.

---

## Reporting the outcome

Say what actually happened. "8 of 12 passed, coverage and golden rules red" is a
useful report. "The gate passed" when a threshold was lowered or a step was
skipped is not, and the gate prints enough for that to be checked.

A red step is a result, not an obstacle.
