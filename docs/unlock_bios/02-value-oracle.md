# Plan 02 — Value oracle: find the first wrong value, not the first missed address

**Goal:** when our boot diverges, name the first **register value** that differs
from the real boot — which is usually several instructions *before* control
flow splits, and is the actual bug.

**Depends on:** plan 01 (ordered comparison, frame counter, recorder placement).

---

## 1. Why

Plan 01 answers "where did we stop following the real boot". That point is a
symptom: a branch went the other way because some value was already wrong.

Both divergences measured on 2026-09-18 show the gap:

| case | control flow splits at | the wrong value is visible at |
|---|---|---|
| no disc | `0x000033D8`, frame 181 (`cmp/eq #10` never reached) | `0x000033A2`: `extu.b` shows `r0 = 0x21` (PERI \| PAUSE) in the real boot. Ours reports CD status `0x27` (PERI \| NODISC) in `CR1` per `[REGACCESS]`; our `r0` at this PC has not been captured yet — that is what this plan adds |
| with disc | `0x06012F6C`, frame 255 | `0x06012F64`: the byte read from the sound driver mailbox at `0x25A00700` |

Finding the first case took an afternoon of manual reading. With this plan it is
one diff line.

## 2. What the reference gives

Each line shows the operand registers **before** execution, e.g.

```
0x000033A2: extu.b ((r0)0x00000021), ((r0)0x00000021)
0x06000942: mov.l ((r2)0xFFFFFF7D), @((r1)0x25FE00A0)
```

Parse to `(pc, [(reg, value)], mem_addr)`. The text format does not need to be
reproduced; the tuples do.

Limits that carry over from the data (plan 01 §2): one sample per PC (the first
execution), Master only, delay slots absent.

## 3. Steps

### 3.1 Parser — `tools/boot_trace.py`

A small module both `boot_diff.py` and the oracle import:

- parse a reference line into `frame, pc, opcode, operands: dict[reg → u32],
  mem: Optional[addr]`;
- operand syntax: `((rN)0xVVVVVVVV)`, `@((rN)0x…)`, `@-((rN)0x…)`,
  `@((rN)0x…)+`, `@(disp, ((rN)0x…))`; `pc`, `pr`, `gbr`, `vbr`, `macl`,
  `mach`, `sr` as they appear;
- known-answer tests on lines copied from the traces (the parser is the part
  most likely to be subtly wrong).

### 3.2 Recorder — `MIMAS_FIRST_EXEC_VALUES=<file>`

On the first execution of each PC (same bitmap as plan 01), after
`service_pending_interrupt()` and before `execute()`, write `pc` plus the values
of the registers the instruction reads.

Which registers an opcode reads comes from the decode Mimas already does
(`n`/`m` fields; implicit `R0`, `R15`, `PR`, `GBR`, `MACH/MACL`). Put that in one
table-driven function next to the decoder, with a test per opcode family —
not per call site, where it would drift.

Cost: one bit test per instruction after warm-up. Off by default, one `Relaxed`
load when off (spec 1.2b).

### 3.3 Comparator — `boot_diff.py --values`

Walk the matched prefix from plan 01. For each PC present on both sides, compare
the operand values. Report the first mismatch with the previous 10 reference
lines.

### 3.4 Separate real divergence from expected noise

Some values legitimately differ between two correct emulators: free-running
counters (FRT), anything derived from host-dependent timing, uninitialised
memory. Handling, in this order:

1. Run the comparator on today's build and **list every mismatch before the
   control-flow divergence**. Measure the noise before designing around it.
2. Classify each mismatching PC by what it reads (from `mem`): timer register,
   RAM, peripheral register.
3. Only then add an allowlist — `tools/bios_reference/value_noise.json`, one
   entry per PC with the reason — never a blanket tolerance. Same discipline as
   `// golden-rule-ok:`: excused at the site, with a reason, reviewable.

If the noise before the first real bug turns out large, stop and report it —
that changes whether this plan is worth finishing.

## 4. Acceptance

- The parser's known-answer tests pass on lines taken from both traces.
- On today's build, without disc, the first reported mismatch is at or before
  `0x000033A2`, and names `r0` (`0x21` real).
- On today's build, with disc, the first reported mismatch is at or before
  `0x06012F64`.
- Every allowlisted PC has a reason; the allowlist is empty or near-empty before
  frame 181.

## 5. Out of scope

- Slave SH-2 and M68K — the reference does not trace them. If YabaSanshiro can
  trace the M68K core with the same instrumentation, that becomes the reference
  for plan 03 and this comparator applies unchanged.
- Memory contents. The reference shows addresses of accesses, not the data
  written, except where the stored register appears as an operand.

## 6. Also serves the JIT

`docs/jit_compiler.md` phase 2 needs a differential test: interpreter and JIT
must agree. This oracle is that test against the real boot, not just against our
own interpreter — a JIT that agrees with a wrong interpreter would pass the
other kind.
