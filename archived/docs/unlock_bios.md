# Unlocking the BIOS — master plan

**Question this answers:** what stands between Mimas and a real Saturn BIOS boot
all the way to frame 735, in what order to attack it, and how each step is
proven.

Detailed plans, in `docs/unlock_bios/`:

| plan | what | why |
|---|---|---|
| [01 — reference traces](unlock_bios/01-reference-traces.md) | ordered comparison against the real boot; fix our recorder's blind spot; move the gate to it | the measuring instrument every other plan is accepted by |
| [02 — value oracle](unlock_bios/02-value-oracle.md) | compare register values, not just addresses | names the first wrong value, which is the bug, instead of the first missed branch, which is the symptom |
| [03 — M68K sound driver](unlock_bios/03-m68k-sound-driver.md) | the driver erases its own RAM | **the first blocker with a disc**, at frame 255 |
| [04 — CD block power-on status](unlock_bios/04-cd-block-reset.md) | an empty drive must answer PAUSE before it answers NODISC | the first blocker without a disc, at frame 181; one line of code |

---

## The data

Real boots captured on an instrumented YabaSanshiro, kept outside this
repository in `../yabassanshiro/` (they contain the BIOS program in readable
form; see plan 01 §6):

- `bios_trace_with_game.txt` — Magic Knight Rayearth CHD, frames 1–735, 8,871
  instructions: every Master SH-2 PC at its first execution, in order, with
  operand register values and memory addresses;
- `bios_trace_no_game.txt` — no disc, frames 1–574 (YabaSanshiro's CD block
  crashes there with an empty drive), 7,256 instructions;
- `branch_trace_with_game.txt`, `branch_trace_no_game.txt` — first occurrence of
  each branch edge.

YabaSanshiro's *knowledge* of the hardware is trusted — it runs the whole
library. Its code is never ported (`CLAUDE.md`, golden rule `no-yabause-code`).

## How to measure, today

Until plan 01 lands, there are two numbers and only one of them has a tool.

**Run** (drop `--chd` for the no-disc scenario, which is the one the gate uses):

```bash
MIMAS_NO_EARLY_STOP=1 MIMAS_PC_TRACE=/tmp/pcs.txt MIMAS_BOOT_WATCH_SECS=25 \
  ./target/release/saturn-frontend-native --bios "$BIOS" --chd "$CHD"
```

**Set coverage** — has a tool, and is what the gate's step 10 enforces
(`MIMAS_BIOS_MIN_COVERAGE`):

```bash
python3 tools/bios_progress.py /tmp/pcs.txt
```

**Ordered prefix** — the metric that counts, and the one in the table below. No
tool yet (plan 01 §4.3 builds `tools/boot_diff.py`). Until then: walk the
reference trace in order, drop delay slots, and stop at the first PC that is
not in `/tmp/pcs.txt`. Use a scratch script outside the repo, not a file in
`tools/`.

Traps of the instrument, all of which have already produced false leads:

- **Always `MIMAS_NO_EARLY_STOP=1`.** Without it, the watcher stops the run when
  Core 0's PC looks unchanged for 500 ms, typically in the delay loop at
  `0x06001694`. A frozen PC there means *blocked*, not *done*, and the run
  under-reports the boot several times over.
- **`MIMAS_PC_TRACE` does not see delay slots or the first instruction after
  an interrupt is accepted** (plan 01 §3). Before chasing a missing address,
  check whether the next one (`+2`) was executed.
- **It records whether an address ran, not in what order or how often.** For
  order, `MIMAS_PC_RING=/tmp/ring.txt` keeps the last 65,536 Master
  instructions, delay slots included.
- **The two scenarios take different paths.** Without a disc the BIOS heads for
  the CD Player screen; frame 735 of the with-disc trace is only reachable with
  `--chd`.

**Disassembling what the BIOS runs.** Only the low runtime (`0x0600xxxx`) is a
1:1 copy of the ROM (ROM `0x0016A0` ↔ WRAM `0x060016A0`), so it can be read
straight from the ROM:

```bash
python3 -c "d=open('$BIOS','rb').read(); open('/tmp/s.bin','wb').write(d[0x0820:0x0880])"
python3 tools/sh2dis.py /tmp/s.bin 0x06000820
```

For `0x0601xxxx` and above, the ROM bytes do not match what executes; the
reference trace carries the opcode and disassembly of every instruction.

## Where we are, measured 2026-09-18

The metric is how far, **in order**, our boot reproduces the real one — not how
many addresses we happen to execute. These numbers came from a one-off ordered
comparison against the traces above (method in the previous section); re-take
them with `boot_diff.py` once plan 01 exists:

| | reproduced in order up to | first divergence | set coverage, for comparison |
|---|---|---|---|
| with disc | **frame 255** of 735 (instruction #5634 of 8,871) | `0x06012F6C`: the SH-2 reads the sound driver mailbox and gets no answer | 66.2% |
| no disc | **frame 181** of 574 (instruction #496 of 7,256) | `0x000033D8`: our CD drive says NODISC where the real one says PAUSE | 80.6% |

Set coverage looks better than it is: it counts code we run after already
leaving the real boot's path.

## Order of work

0. **Remove the debug probes committed in `ddf3d56`** (`[M68K START]`,
   `[DEBUG] MISMATCH`, `[WATCH]`, the `/tmp/mimas_ram.bin` dump) and the
   per-instruction `env::var` in `M68k::step`. They change timing and one of them
   silences the M68K diagnostic. Detail in plan 03 §2. Re-take the baseline
   afterwards.
1. **Plan 01.** Until the comparison is ordered and blind-spot-free, every
   divergence below is a hand-computed estimate.
2. **Plan 04.** Small, and the first end-to-end use of plan 01's tool. Moves the
   gate's scenario (no disc) past frame 181.
3. **Plan 03.** The real blocker on the way to frame 735.
4. **Plan 02**, alongside 03 once 01 exists. It turns each later divergence from
   an investigation into a diff line.

Then loop: measure, take the first divergence, fix, measure again. The frame
number must go up; if it did not, it was not the fix.

## Three claims in earlier versions of this file were wrong

Recorded so they are not rediscovered:

1. **`0x06000846` was never "an interrupt we do not take."** It is the first
   instruction of the V-Blank OUT trampoline, and our recorder cannot see the
   first instruction after an interrupt is accepted (`step()` accepts it and
   executes the handler's first instruction in the same call; the hook runs
   before `step()`). We execute `0x06000848` and the dispatcher right after it.
   Plan 01 §3.
2. **Frame numbers do not transfer between captures.** Two captures of the same
   disc agree on 8,770 addresses and on the debut frame of only 3% of them.
   Compare by order; report frames from the reference.
3. **The ROM is copied 1:1 into WRAM only for the low BIOS runtime
   (`0x0600xxxx`).** Code at `0x0601xxxx` and above is loaded some other way —
   the ROM bytes at the same offset do not match the executed opcodes. For that
   code, the trace itself is the disassembly.

Also retracted: the theory that the sound-driver upload's write-verify loop hangs
(the ring buffer shows it completes every pass), and the reading of the M68K
failure as a race with the upload (plan 03 §1.3 shows the M68K erases the memory
itself).

## After the BIOS runs: what it takes to *see* it

The BIOS boot animation is real-time VDP1 polygons, not FMV (frames 264–516 of
the portal capture show it; it plays with no disc, from a 512 KB ROM). Once the
boot gets there:

- **Needed:** VDP2 Phase 4 finished (`docs/current_review.md` lists what was
  marked done and is not), Phase 6 (windows — `WPEX0` is written), Phase 7
  (sprite layer read-out — without it nothing VDP1 draws is composited), Phase 5
  (scroll).
- **Not needed for this:** VDP2 Phase 8 (rotation), Phase 9 (VRAM cycle
  patterns — programmed, but they govern bandwidth), the thread-topology move,
  Milestone 6 audio. Note that audio being out of scope does **not** excuse the
  M68K: the BIOS waits on the sound driver's mailbox (plan 03).

## Later: the JIT

`docs/jit_compiler.md` phase 2 needs a differential test. Plan 02's oracle is
that test against the real boot rather than against our own interpreter.
