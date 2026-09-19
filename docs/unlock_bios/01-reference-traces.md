# Plan 01 — Ordered comparison against the real boot

**Goal:** replace "how many addresses did we execute" with "we reproduce the real
boot, instruction by instruction, up to frame N", and make the first point where
we stop matching come out mechanically.

**Unblocks:** every other plan in this directory. Plans 03 and 04 are accepted
by this tool; plan 02 extends it.

---

## 1. The data

Four files from a YabaSanshiro build instrumented to trace, kept in
`../../yabassanshiro/` (outside this repository — see §6):

| file | what | extent |
|---|---|---|
| `bios_trace_with_game.txt` | first execution of each Master SH-2 PC, in order | frames 1–735, 8,871 instructions, Magic Knight Rayearth CHD |
| `bios_trace_no_game.txt` | same, no disc | frames 1–574, 7,256 instructions; the run ends there because YabaSanshiro's CD block crashes with an empty drive |
| `branch_trace_with_game.txt` | first occurrence of each (source → target) branch | 1,695 edges |
| `branch_trace_no_game.txt` | same, no disc | 1,366 edges |

One line of `bios_trace_*`:

```
Frame: 255 | Core: M | PC: 06012F64 | Opcode: 62E0 | 0x06012F64: mov.b @((r14)0x25A00700), ((r2)0x87000000) | Mem: READ 25A00700 (CD Block)
```

Register values in the disassembly are **before** the instruction executes.
`Mem:` gives the address of the access.

## 2. What is known about the data, measured

- **Each PC appears once.** 8,871 lines, 8,871 distinct PCs. Loops appear once;
  a register block written by a loop appears at its first address only.
- **Master SH-2 only.** No Slave, no M68K.
- **With and without disc are identical for the first 6,127 instructions**, then
  split inside the animation code (`0x0603Axxx`, around frames 436–479). That is
  where the BIOS decides between the game and the CD Player screen.
- **Frame numbers are specific to one capture.** Against the earlier portal
  capture (same disc), 8,770 addresses agree but only 3% debut in the same
  frame. Compare by **order**; use frames only to report.
- **Region labels are wrong in three ranges.** `0x2589xxxx` (CD block) is labelled
  "SCSP/Audio RAM", `0x25A0xxxx` (sound RAM) is labelled "CD Block", `0x25FExxxx`
  (SCU) is labelled "VDP2 Regs". Use the address, never the label.
- **The reference's tracer does not record delay slots.** Of 852 delayed branches
  in the game trace, the delay slot (PC+2) appears in only 81 (10%), and those
  are addresses reached some other way.

## 3. What is known about our side, measured

`MIMAS_PC_TRACE` (set of executed PCs, `sh2.rs`) has **two** blind spots:

1. **Delay slots** — they execute inside `step()` via `delay_slot_and_jump`, not
   as a `run_loop` iteration. Same as the reference, so this one cancels out.
2. **The first instruction after an interrupt is accepted.** `step()` calls
   `service_pending_interrupt()`, which moves the PC to the handler, and then
   fetches and executes the handler's first instruction **in the same call**.
   The hook records the PC *before* `step()`. The reference does record it.

Blind spot 2 is what produced the false lead `0x06000846`: it is the first
instruction of the V-Blank OUT trampoline (vector 0x41), and we execute
`0x06000848` and the common dispatcher `0x060008F4` right after it. We do take
V-Blank IN and V-Blank OUT. Every "first gap" that `tools/bios_progress.py` has
reported at `0x06000846` or `0x06000840` is this artefact.

## 4. Steps

### 4.1 Fix our recorder so the two sides see the same things

In `saturn-core/src/sh2.rs`:

- Record the PC **after** `service_pending_interrupt()` and before the fetch, not
  before `step()`. One move of the existing `pc_trace_enabled()` hook; keep its
  cost at one `Relaxed` load when disabled (spec 1.2b, same shape as
  `bus_trace_enabled`).
- Keep delay slots **out** of the ordered trace, to match the reference. (The
  existing `MIMAS_PC_RING` keeps them; that one is for execution order, not for
  comparison.)

Test: an interrupt taken with the handler's first instruction at a known
address, asserting the address is recorded. Today that assertion fails; that is
the regression it guards.

### 4.2 An ordered first-execution trace from Mimas

`MIMAS_FIRST_EXEC_TRACE=<file>`: on the **first** execution of each PC, append
`frame, pc, opcode`. Bitmap-gated like `MIMAS_PC_TRACE`, so the cost after the
first execution is one bit test.

Frame number: Mimas has no frame counter today. Add one to the SCU's V-Blank IN
path (`Scu::vblank_in`), which is already driven by Master SH-2's cycle count —
not by a host clock (golden rule `no-wall-clock`). An `AtomicU64` incremented
there, read `Relaxed` by the recorder.

Dump on shutdown from `saturn-frontend-native`, as `MIMAS_PC_TRACE` does.

### 4.3 `tools/boot_diff.py`

Input: our ordered trace and one reference trace. Output:

```
reference: bios_trace_with_game.txt  (8871 instructions, frames 1–735)
matched in order: 5634 instructions — real boot reproduced up to frame 255
first divergence: reference #5634, frame 255
  06012F6C  mov #0x00, r3        ← real executes this, we never do
  context (real): 06012F58, 06012F5A, 06012F5C (bra 06012F64), 06012F64 (mov.b @r14 → 0x25A00700)
set coverage (for comparison only): 66.2%
```

Rules:

- **Prefix, not set.** Walk the reference in order; the match ends at the first
  reference PC we never executed. Anything we execute after that is reported as
  set coverage, never as progress.
- **Normalise delay slots on both sides:** drop any reference PC that is the
  delay slot of a delayed branch that precedes it in the reference.
- **Report the frame of the last matched reference instruction**, not our own
  frame — ours runs on a different clock.

### 4.4 Commit only derived data

Generate `tools/bios_reference/boot_order_with_disc.json` and
`boot_order_no_disc.json`: ordered `(frame, pc)` pairs, nothing else — no
opcodes, no disassembly, no register values (§6). `boot_diff.py` reads these by
default and the full `.txt` traces when given a path.

### 4.5 Move the gate's step 10 to the prefix metric

Step 10 today uses `bios_progress.py` against the portal capture: a set
comparison, with the frame-number problem of §2 and our blind spot of §3.

Replace with `boot_diff.py` against `boot_order_no_disc.json` (the gate boots
with no disc):

- floor `MIMAS_BIOS_MIN_FRAME` = the reference frame reached, **181** today;
- tightening free, loosening needs `MIMAS_OVERRIDE_REASON`, as for every other
  threshold;
- keep `bios_progress.py` as a tool, with its blind spot documented in its
  header, until nothing depends on it.

Update `docs/quality-gate.md` and `docs/unlock_bios.md` in the same change. The
agent files (`CLAUDE.md`, `GEMINI.md`, `QWEN.md`) hold no metric or baseline —
they point at `docs/unlock_bios.md` — so they need no edit; if one of them has
started describing the metric, move that text out rather than updating it.

## 5. Acceptance

- `boot_diff.py` on today's build reproduces, without hand-editing:
  - with disc: first divergence at reference **#5634, frame 255, `0x06012F6C`**;
  - without disc: first divergence at reference **#496, frame 181, `0x000033D8`**.
- `0x06000846` and `0x06000840` no longer appear as divergences.
- The recorder test of §4.1 passes, and fails if the hook is moved back.
- Gate: step 10 red when the floor is raised above the measured frame, green at
  the measured frame.

## 6. Keep BIOS content out of the repository

The traces contain every executed BIOS opcode with its disassembly — in practice,
the BIOS program in readable form. The repository is public and the BIOS image
was removed from its history for that reason on 2026-09-18. Committing these
files would undo that.

- Full traces stay outside the repo; `boot_diff.py` takes their path (default
  `../yabassanshiro/`), and the gate fails with a clear message if it needs them
  and they are absent — the same way it already treats a missing BIOS.
- Only `(frame, pc)` order goes in, as `bios_boot_frames.json` already does with
  addresses.

## 7. Open questions

- `0x06012F66`–`0x06012F6A` never appear in the reference, although the real
  boot reaches `0x06012F6C` right after `0x06012F64`. They are not delay slots of
  a branch listed before them. Until explained, acceptance is anchored on
  `0x06012F6C`, which is present.
- `../yabassanshiro/decoded_trace.txt` (1.1 GB) has not been examined. If it is a
  complete trace (every execution, not just the first), it removes the
  first-execution limit of §2 and changes plan 02.
