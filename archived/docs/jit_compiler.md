# ARM64 JIT — plan

**Target: AArch64 only.** No x86-64 backend, ever. The desktop exists to develop
and verify on; the R36S (4× Cortex-A53) is the only machine that has to run this
at speed. A second backend doubles the surface that can be subtly wrong and
halves the attention each gets.

Backend assembler: **VIXL**, the same one Flycast uses. Architecture follows
Flycast's, which is the lightest credible design in this space and already
solves the problems this one will hit.

---

## 1. Why, in measured numbers

Host is a Ryzen 5 3500X (Zen 2, 6 cores, ~3.6–4.1 GHz). Emulated SH-2 throughput
against a real BIOS boot:

| run | emulated | **host cycles per emulated SH-2 cycle** |
|---|---|---|
| 2.2 s, early-stop | 57.6 MHz (203% of real) | **62.5** |
| 60 s, full window | 29.3 MHz (102% of real) | **122.8** |

A competent interpreter sits around 30–60 host cycles per guest cycle; a
block-linking JIT sits around 5–15. So the headline is not "the interpreter is
slow" — it is that **we are paying 62–123 and a JIT's floor is roughly 10**.

The gap that has to close is defined by the target, not by taste
(`docs/mimas-performance-analysis.md` §3.2): the Master SH-2 needs ~100% of real
time on **one** A53 core, and an A53 core is worth roughly 1/9 to 1/14 of a Zen 2
core on this workload. That puts the desktop-equivalent target near **1000%**
against today's 102% on a long run — a **10x gap**.

10x is above what a JIT alone reliably delivers (5–10x is the honest band). This
plan therefore assumes the interpreter work continues in parallel and that both
are needed. A JIT that lands 6x while the interpreter finds 2x elsewhere reaches
the target; a JIT alone probably does not.

### 1.1 Phase 0 exists because one number does not add up

Throughput **halves** between the 2.2 s run and the 60 s run: 62.5 → 122.8 host
cycles per guest cycle, same build, same BIOS. An interpreter does not get
slower as it runs. Something else does — lock contention, `LockStepSync` drift
blocking, cache pressure from the growing working set, or Core 5/Core 4 becoming
the lockstep minimum (`docs/current_review.md` finding 10: Core 5 reports one
cycle per frame).

**A JIT will not fix that, and building on top of it would hide it.** Phase 0
finds it first. If the 2x is recoverable, the gap drops from 10x to 5x and the
whole plan gets easier; if it is not, we at least stop attributing it to the
interpreter.

---

## 2. Scope: which processors get a JIT

The request was a distributed JIT in the PCSX2 sense — separate recompilers per
processor rather than one. That is right, and Mimas is already shaped for it:
every component owns a thread, so every recompiler is naturally single-threaded
within its own context.

Flycast is the proof it extends past the main CPU. It ships **three** AArch64
recompilers, and their sizes say exactly how much each class costs:

| Flycast recompiler | lines | shape |
|---|---|---|
| `rec-ARM64/rec_arm64.cpp` (SH4) | 2,612 | full pipeline: decode → SHIL → SSA → regalloc → codegen |
| `hw/arm7/arm64.cpp` (ARM7) | 527 | direct codegen, no IL |
| `hw/aica/dsp_arm64.cpp` (AICA DSP) | 483 | whole-program compile, no blocks at all |

Mapped onto Saturn:

| processor | ISA | today | JIT | model |
|---|---|---|---|---|
| **Master SH-2** | SH-2 | `sh2.rs` | **yes — phase 2** | Flycast SH4 |
| **Slave SH-2** | SH-2 | same code | **yes — same codegen, second context** | — |
| **SCU DSP** | 32-bit VLIW, 256 words | `scu_dsp.rs` | **yes — phase 5** | AICA DSP |
| M68K | 68000 | `m68k.rs` | later, maybe | Flycast ARM7 |
| SCSP DSP | custom | not implemented | not now | — |

### 2.1 The two SH-2s share a code cache, and that is not the PCSX2 model

PCSX2 keeps EE and IOP recompilers separate because they are **different ISAs**.
Our two SH-2s are the *same* ISA running the *same code* out of the *same*
WRAM — the only thing that differs is register state.

So: **one code generator, one code cache, two contexts.** Generated code never
embeds a state address; it reaches guest state through a context base register
(Flycast's convention, §4.2). Compiling the same block twice because a second
CPU happened to run it would double compile time and halve the effective cache
for nothing.

The two consequences that fall out of sharing, both of which have to be designed
in rather than discovered:

- **Invalidation is global.** A WRAM write from *either* SH-2 — or from SCU DMA,
  which writes WRAM without any CPU involved — must invalidate blocks both
  contexts might be inside.
- **The SH-2s have independent on-chip caches.** Two cores can legitimately see
  different bytes at the same address. Where that matters, it is a correctness
  question for the memory model, not the JIT, but the JIT must not *assume* a
  single coherent view.

---

## 3. What makes this harder than Flycast, and where the plan must not be copied

Flycast runs one SH4 in a conventional emulation loop. Mimas runs eight threads
in a bounded-slack lockstep. That difference invalidates two things a
straight port would inherit.

### 3.1 Block granularity versus the slack window — the real risk

`LockStepSync` blocks a core that has drifted more than `slack_limit` cycles
ahead of the slowest active one. The interpreter reports progress inside
`Sh2::run_loop` in batches sized against that limit.

**A JIT block runs to completion before anything can be reported.** If a block's
guest-cycle cost exceeds `slack_limit`, that core lands permanently outside the
window and the pair ping-pongs on the boundary. This is not hypothetical: it is
exactly the bug that stopped the BIOS at `0x2B0` when Core 5 shipped a flat
128-sample step against a 1000-cycle slack, and again in Chapter 32 when Core 3's
reporting rate throttled the Master by two orders of magnitude.

Three ways out, and the choice has to be made before codegen is written because
it changes the block epilogue:

1. **Cap block length in guest cycles** at compile time, splitting a block that
   would exceed a fraction of `slack_limit`. Simple, costs block-chaining
   opportunities on long straight-line code — of which, per §5, there is little.
2. **Emit a sync check in the epilogue** of every block: add the block's cycle
   cost, compare against a threshold in the context, and call out when exceeded.
   Roughly 3 instructions per block. This is the recommended default.
3. **Raise `slack_limit`** so blocks fit. Tempting and wrong: it weakens the
   synchronisation guarantee for every component to make one convenient.

**Verification for whichever is chosen is not a unit test.** It is a real BIOS
boot compared against `tools/bios_progress.py`, because every previous instance
of this bug class was invisible to the test suite and visible only in a boot.

### 3.2 Interrupts and the memory model

The interpreter checks for a pending interrupt once per instruction. A JIT can
only check at block boundaries. With a median block of 5 instructions (§5) the
added latency is small, but it is a real behavioural change and needs to be
stated rather than discovered: **interrupt recognition moves from
per-instruction to per-block.**

Memory access is the other one. Every guest load/store goes through `WorkRam`'s
per-region `RwLock` and `BusArbiter`. A JIT that calls a Rust helper per access
keeps correctness and caps the achievable gain — memory ops are a large fraction
of SH-2 code. Fast paths (inline load/store for WRAM hits, helper call only on
region miss) are where the real speedup lives, and they are **phase 4**, after
correctness, deliberately.

---

## 4. Architecture

Flycast's pipeline, kept:

```
guest code ──► decoder ──► IL ──► SSA + regalloc ──► VIXL ──► code cache
                                                                  │
                                              block cache ◄───────┘
                                                      │
                                              dispatcher ──► linked blocks
```

### 4.1 Why an IL, when a direct SH-2→ARM64 emitter is smaller

Flycast's ARM7 backend has no IL (527 lines) and its SH4 backend does (2,612).
The IL earns its cost on the SH-2 because SH-2 is flag-heavy: `T` is written by
a large share of instructions and read by few. In IL + SSA, dead `T` computations
are eliminated; in a direct emitter they are all materialised. On SH-2 that is
not a marginal optimisation.

SHIL's ~150 operations are mostly FPU and SH4-specific. The SH-2 integer subset
needs roughly 40, and **SH-2 is close to a subset of SH4's integer core**, so
Flycast's decoder structure and IL semantics map over nearly directly. This is
the single biggest reuse available and the reason Flycast is the right model
rather than a generic one.

### 4.2 Register convention

Flycast allocates 8 callee-saved host registers to guest registers
(`W19`–`W26`, `arm64_regalloc.h:40`). SH-2 has 16 GPRs plus `PC`/`PR`/`SR`/
`GBR`/`VBR`/`MACH`/`MACL`, so allocation is per-block, not fixed.

Proposed, to be confirmed against measurement rather than adopted on faith:

| host | holds |
|---|---|
| `x28` | context base — the `Sh2` state this block is executing against |
| `x27` | guest cycle accumulator for the current block |
| `w19`–`w26` | guest registers, allocated per block |
| `x0`–`x17` | scratch, helper-call arguments |
| `w29` | guest PC at block exit (Flycast's convention, kept for the link stubs) |

`x28` being a parameter rather than a constant is what lets both SH-2s share
compiled code (§2.1).

### 4.3 Block cache, chaining, invalidation

- **Cache size**: Flycast uses 15 MB + 1 MB staging for SH4. Saturn's code
  working set is far smaller — the whole BIOS boot touches 9,882 instruction
  addresses (§5) — so start at **4 MB** and measure.
- **Chaining is mandatory, not an optimisation.** With a median block of 5
  instructions, a dispatcher round-trip per block would dominate everything.
  Flycast's link stubs (`ngen_LinkBlock_*_stub` → `rdv_LinkBlock` → `br x0`)
  patch the branch in place on first execution; port that directly.
- **Invalidation**: page-granular write protection over WRAM, as Flycast does
  for its RAM (`unprotected_pages[RAM_SIZE_MAX/PAGE_SIZE]`).

**Self-modifying code is not an edge case here.** The Saturn BIOS copies its own
runtime from ROM into WRAM at `0x06000000` and executes it there — this session
verified the mapping is 1:1 (ROM `0x0016A0` ↔ WRAM `0x060016A0`) by
disassembling the ROM and matching the executing addresses. Games do the same
with overlays. Invalidation must be correct from phase 2, not retrofitted.

The writers that must trigger it: both SH-2s, **SCU DMA** (Core 6 writes WRAM
with no CPU involved), and the CD block. A design that only hooks CPU stores is
wrong.

### 4.4 The Rust ↔ C++ boundary

VIXL is C++. `saturn-core` is Rust. This is a real cost and pretending otherwise
would poison the estimates.

- Build VIXL through the `cc` crate, as a static archive. It is ~8 MB of source
  in Flycast's tree; we need `aarch64` only.
- The shim is a thin C ABI over `MacroAssembler`: create/destroy, emit-op,
  bind-label, finalize-and-flush-icache. **Not** a general binding — an emitter
  interface narrow enough that the Rust side owns all the decisions and C++ owns
  only instruction encoding.
- `cargo test --workspace` must stay green **on x86-64 hosts that will never
  build the backend**, so the whole JIT lives behind a feature gate and a
  `#[cfg(target_arch = "aarch64")]`, with the interpreter as the unconditional
  fallback.
- Cross-compilation to the R36S needs a C++ toolchain in the loop now, not just
  Rust's.

**This is the plan's largest single unknown.** Nothing else here depends on an
untested integration; this does.

---

## 5. Evidence this design fits Saturn code specifically

Not inherited from Flycast — measured on this project's own data.

**Basic blocks are small.** Contiguous instruction runs across the complete
address set of a real BIOS boot (9,882 addresses, from a Yabause capture):

```
966 contiguous runs
  mean 10.23 instructions, median 5
  p75 11, p90 21, p99 88, max 217
```

A contiguous run is an *upper bound* on a basic block (an untaken conditional
does not break the run), so real blocks are smaller still. Consequences, all
already reflected above: chaining is mandatory (§4.3); the per-block sync check
of §3.1 costs ~3 instructions against a ~5-instruction block, which is not free
and is why capping is listed as an alternative; and per-block interrupt latency
stays low (§3.2).

**Execution is extremely concentrated.** From 53.9 M calls in real gameplay
(Magic Knight Rayearth, captured from Yabause):

```
top  10 functions = 44.2% of all calls
top  50 functions = 83.2%
top 100 functions = 92.8%
```

A JIT is exactly the right tool for that shape: compile cost is paid on a small
set and amortised across enormous reuse. It also means a **compile threshold**
(interpret until a block is seen N times) is cheap insurance against pathological
compile churn, and that hot-block statistics are already available to tune it
rather than guessed.

---

## 6. Phases

Each phase ends with a number, not an opinion. The gate additions are not
optional decoration: the one thing this project has proven repeatedly is that a
check nobody runs is not a check.

### Phase 0 — find the missing 2x before building anything

Explain the 62.5 → 122.8 degradation over a run (§1.1). Candidates in order of
suspicion: `LockStepSync` drift blocking (Core 0 idle measured 2,569 ms of 60 s
— 4.3%, so probably not the whole story); Core 5's one-cycle-per-frame reporting
making it the lockstep minimum; `WorkRam` lock contention as more regions go
live; working-set growth.

**Done when**: the degradation is explained and either fixed or attributed.
**Why first**: if it is recoverable, the gap is 5x rather than 10x, which changes
what the JIT has to achieve.

### Phase 1 — VIXL integration, no emulation

Build VIXL via `cc`, expose the shim, emit a hand-written function from Rust
(`add w0, w0, #1; ret`), call it, assert the result. Feature-gated, AArch64-only.

**Done when**: it runs on the R36S and `cargo test --workspace` is unchanged on
x86-64. This phase is pure risk retirement for §4.4 and buys nothing else.

### Phase 2 — SH-2 blocks, correctness only

Decoder → IL → naive codegen (every guest register in memory, no allocation,
helper call per memory access), block cache, chaining, page invalidation.
Interrupts and sync checks at block boundaries per §3.1/§3.2.

**Done when**: `tools/bios_progress.py` reports the **same** reference coverage
as the interpreter (66.5% today) with the JIT enabled. Not "the BIOS boots" —
the same address set, verified against the hardware capture. A differential
harness running interpreter and JIT on identical state and comparing register
files after each block is the cheapest way to get there.

**Expected speed: none.** A naive JIT with memory-resident registers can be
*slower* than a good interpreter. Reporting a slowdown here is the correct
outcome, not a failure.

### Phase 3 — SSA and register allocation

The first phase that is supposed to be faster. Dead `T` elimination (§4.1) and
guest registers in `w19`–`w26`.

**Done when**: measured host-cycles-per-guest-cycle drops, reported in the same
units as §1 so it is comparable to every earlier measurement.

### Phase 4 — memory fast paths

Inline the WRAM hit; helper call only on region miss or bus contention. This is
where the remaining multiple lives.

**Done when**: same, plus `cargo test --workspace` green and the golden rules
clean — the fast path must not become a way to bypass `BusArbiter`.

### Phase 5 — SCU DSP

Whole-program compile on `EX` set, following `dsp_arm64.cpp`: 256 instructions,
no blocks, no chaining, recompile on program write. Smallest possible JIT, and
independently useful.

### Phase 6 — M68K, only if measurement says so

Flycast's ARM7 backend is 527 lines with no IL. Do this only when profiling
shows Core 4 is a real cost on the R36S. It is listed for completeness, not
scheduled.

---

## 7. Gate integration

- Everything behind `--features jit`, default off, interpreter unconditional.
- `MIMAS_JIT=0|1` at runtime so both can be measured in one build.
- The smoke step gains a JIT run when the feature is on: same
  `bios_progress.py` floor, same speed floor, reported separately.
- **The differential harness from phase 2 stays** as a permanent test, not
  scaffolding. It is the only thing that can catch a codegen bug that produces
  plausible-but-wrong state.

## 8. What would make this the wrong plan

Recorded so it can be checked against later rather than rationalised:

- **If phase 0 finds the 2x and the interpreter reaches ~250–300%**, the
  remaining gap is ~3–4x and a simpler threaded-interpreter or computed-goto
  rewrite might reach it at a fraction of this cost and risk.
- **If VIXL integration in phase 1 proves ugly enough to poison the build**, a
  Rust-native AArch64 emitter becomes worth costing — against the explicit
  decision to use VIXL, so it would need to be re-made deliberately.
- **If §3.1's sync check costs more than it looks** — 3 instructions against a
  5-instruction median block is a 60% overhead on the smallest blocks — the
  block-capping alternative wins and the epilogue design changes.

## 9. Sources

- `../flycast` — `core/rec-ARM64/` (SH4 backend, register allocation, link
  stubs), `core/hw/sh4/dyna/` (decoder, SHIL, SSA, block manager),
  `core/hw/arm7/arm64.cpp`, `core/hw/aica/dsp_arm64.cpp`, `core/deps/vixl`.
- `../paralel_exercise/portal_to_another_world/traces/` — real Yabause capture,
  hot-function distribution (§5), and the BIOS boot reference already imported
  into `tools/bios_reference/bios_boot_frames.json`.
- `../yabause/src/sh2int.c` — the interpreter semantics every IL operation must
  match, per `CLAUDE.md`'s "verify against real hardware behavior" rule.
- `docs/mimas-performance-analysis.md` §3.2 — the A53 target arithmetic.
- `docs/unlock_bios.md` — how progress is measured, and why the settle PC is not
  a progress signal.
