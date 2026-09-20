# Mimas v2 — a Sega Saturn emulator in Rust, written entirely by AI

Every line of code in this repository was written by AI. The research, the
architecture and the engineering direction — what to build, in what order, which
trade-off to accept, and how to check a fix against real hardware behaviour
instead of trusting a self-consistent test — come from a human engineer running
the process.

## Why this exists

Two goals, and the second one is the unusual one.

**A Saturn emulator that runs well on weak hardware.** The Saturn is
under-served: the emulators that exist are accurate on a desktop and struggle
where the cheap handhelds live. That gap is the target. The architecture here is
chosen for throughput — a single thread, an x86-64 JIT, headless GL — rather than
for resembling the hardware's block diagram.

**An extreme case study of how much engineering competence AI-written code still
needs.** A Saturn emulator is a good stress test: two CPUs, two video processors,
a sound chip with its own CPU and its own DSP, and no room for "close enough" —
a frame is either byte-identical to the real machine or it isn't. If a model can
be steered through this, the interesting question is not whether the model can
write the code. It can. The question is how much the human has to know for that
code to converge instead of merely accumulate.

This repository has a control group for that question. See
[The failure that made this project](#the-failure-that-made-this-project).

## Current stage

**The BIOS intro runs, from power-on through the Sega logo, with sound.**

| what | measured |
|---|---|
| Video | mean error **1.66/255** against 68 real console captures |
| Execution | **92.1%** of the reference trace's program counters, 0 divergent opcodes |
| Effect DSP | bit-exact: 99.7% of 60,000 samples identical, correlation 1.000000, **0 divergent steps out of 108** |
| Speed | **59.5 fps locked to real time** using ~80% of one core; **1.48× real time** unlimited |
| GPU | ~20% of an RTX 3060 |

Measured on an AMD Ryzen 5 3500X. We use 11.3 ms of the 16.68 ms frame budget,
so roughly a third of the budget is spare. That headroom is evidence toward the
weak-hardware goal, not proof of it: nothing here has been measured on a
handheld yet.

Past the logo the BIOS stalls — the fade-out, the black frames and the licence
screen all need a detected disc, which is not implemented. The boot sound plays
at the right pitch and tempo but drones instead of decaying, because the SCSP's
envelope generator does not exist yet.

Numbers, gaps and the current suspicions live in
[`docs/status.md`](docs/status.md) and
[`docs/current_status.md`](docs/current_status.md). Where a document disagrees
with a measurement, the measurement wins.

## The method

- **Truth comes from real execution, never from another emulator.** Execution
  traces of the BIOS (the first run of each address, with the values it read) and
  screen captures from the console are the reference. No neighbouring emulator is
  consulted for its source. A trace is behaviour without implementation, so there
  is nothing to copy an architecture from.
- **Every advance is measured**, never asserted. `tools/quality_gate.sh` runs
  eight deterministic steps — no model, no network — including pixel error
  against the captures and the percentage of the real trace reproduced. A red
  step is a result, not an obstacle. Thresholds may be tightened freely;
  loosening one requires a declared reason and is reported in the summary.
- **Only video, and what video depends on, is real.** Everything else is a stub
  that returns exactly what the real machine answered at that point in the trace,
  each with its provenance written down. A guess labelled as a guess is debt; a
  guess disguised as a fact is a trap.

## The failure that made this project

This repository began as **mimas**: one thread per chip, event-driven, no
lockstep. It ran for three months. It never rendered a single BIOS frame.

The replacement — single-threaded, JIT, the tree you are reading — booted the
BIOS from start to finish **in three hours**.

The difference was not the architecture. It was what the model was allowed to
look at.

The first attempt had a neighbouring open-source Saturn emulator on disk, and the
model kept reading it. Not copying it wholesale — reproducing its shape, its
call structure, its assumptions, and with them its slowness. The result looked
like progress from every angle that does not require the program to work: the
code compiled, the tests passed, the architecture was defensible on paper, the
commit history was steady. It did not converge, and it could not, because its
reference was another program's *implementation* rather than the hardware's
*behaviour*.

What unblocked it was a rule that removed the shortcut: no neighbouring emulator
source, ever. Work from the traces, the captures, the BIOS binary, and general
SH-2 and Saturn knowledge. The only sanctioned contact with another emulator is
instrumenting one to capture data — its **output**, never its code
([`tools/trace-capture/`](tools/trace-capture/)).

The old tree is preserved in `archived/`, with its git history, and is off limits
to agents. It is kept as evidence, not as a library.

### What this says about the engineer, not the model

The rule above is not something a model proposes for itself. A model has no way
to notice that its reference class is wrong — from the inside, reproducing a
working emulator's structure is indistinguishable from good engineering. Somebody
has to know that an emulator converges by being checked against hardware, and
that a plausible architecture is worth nothing until a frame matches.

That knowledge does not come for free either. On this project it came partly from
digging into how **Flycast** extracts throughput on weak hardware, which is what
produced the questions that actually mattered here: does a JIT distribute across
threads on its own, or is that the emulator's own design? Why does one emulator
saturate every core on a handheld while another leaves them idle? Is emulating
the Saturn's master and slave SH-2 as two separate scheduled entities worth
anything, when a single modern core absorbs both several times over?

Those are engineering questions, and the model answers them differently depending
on which one you ask. Without them, direction degrades into "make it work" — and
a capable model fills that vacuum with plausible structure. Three months of
plausible structure is exactly what `archived/` contains.

There is an apparent contradiction here worth stating plainly: the project
forbids the **model** from reading other emulators, while depending on the
**engineer** having read them. They do not conflict. Understanding another
system is what lets you decide what to build. Copying it is what lets you skip
deciding — and it produces code that only appears to work, for as long as nobody
checks it against the real machine.

## Running it

The reference captures are in the repository (`stubs/captures/`). The BIOS and
the traces are not: they *are* the BIOS program. To produce your own traces from
a BIOS you already own, there is an instrumentation patch and instructions in
[`tools/trace-capture/`](tools/trace-capture/). With both on disk:

```bash
cargo build --release
./target/release/mimasv2 --frames 620 --dump out          # headless, writes PNGs
./target/release/compare stubs/captures out --max-frame 728
cargo run --release --bin live                            # window, video and sound
bash tools/quality_gate.sh                                # before calling anything done
```

## Where the state lives

| what | where |
|---|---|
| how to work in this repository | [`AGENTS.md`](AGENTS.md) |
| current video and trace results, known gaps | [`docs/status.md`](docs/status.md) |
| performance, and the current suspicions | [`docs/current_status.md`](docs/current_status.md) |
| what each stub models, and where its value came from | [`docs/stubs.md`](docs/stubs.md) |
| sound: what is measured and what is missing | [`docs/sound.md`](docs/sound.md) |
| the gate's steps and thresholds | [`docs/quality-gate.md`](docs/quality-gate.md) |
