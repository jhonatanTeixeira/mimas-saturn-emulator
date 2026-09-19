# Plan 03 — The M68K sound driver erases its own RAM

**Goal:** with a disc, reproduce the real boot past frame 255.

**This is the first blocker on the path to frame 735.** Everything before frame
255 already matches the real boot instruction for instruction.

---

## 1. The chain, measured

### 1.1 What the real boot does (`bios_trace_with_game.txt`)

| frame | event |
|---|---|
| 188 | SMPC SNDOFF |
| 192 | SMPC SNDON, then more driver upload (`0x0600160E` → `0x0600166E`) — the M68K is already running while the upload continues, on real hardware too |
| 222 | mailbox: `0x25A00710 ← 0x87000002`, `0x25A00720 ← 0x83000000` |
| 225 | `0x25A00720 ← 0x82000F00` |
| **255** | `0x25A00710 ← 0x87000000`; **reads `0x25A00700`**; continues at `0x06012F6C`; writes `0x85` to `0x25A00700` |
| 493 | reads `0x25A007A0` |
| 704 | `0x25A00720 ← 0x82000000`; reads `0x25A00700`; writes `0x86` |

`0x25A007xx` is sound RAM `0x700` onward: the mailbox between the SH-2 and the
sound driver. (The trace labels it "CD Block"; the label is wrong, the address
is right.)

### 1.2 Where we split

Identical to the real boot until reference #5634, frame 255. At `0x06012F64`
the SH-2 reads the mailbox byte; the real boot goes on to `0x06012F6C` and we
never do. The driver did not answer.

### 1.3 Why the driver did not answer

With `MIMAS_DEBUG_M68K=1`:

```
[M68K] reset: SP=0x0000A000 PC=0x00001000 first16=[00 00 A0 00 00 00 10 00 …]   ← vectors present at reset
[M68K] first entry to clear-loop at 0x0000322E: d=[0,0,0,0,0,0,0,0000FFFF] a=[00000000, …]
[M68K] unimplemented opcode=0xFFFC at pc=0x00003232 d=[000000CF, …, 0000F373] a=[00003230, …]
```

The loop:

```
0x322E: MOVE.L D0,(A0)+     ; writes zero at A0, A0 += 4
0x3230: DBF D7,-4           ; D7 -= 1, back to 0x322E
```

Entered with `A0 = 0`, `D7 = 0xFFFF`, `D0 = 0`, it clears sound RAM upward from
address 0. After `0xC8C` passes, `A0 = 0x322C` and the `MOVE.L` zeroes
`0x322C–0x322F` — **including its own opcode at `0x322E`**. On the next pass the
M68K fetches `0x0000` = `ORI.B #imm,D0`, takes the next word `0x51CF` (the `DBF`
opcode) as its immediate, sets `D0 |= 0xCF`, and lands on `0x3232` = `0xFFFC`,
the `DBF`'s displacement word.

All three registers at the failure match this exactly: `D0 = 0xCF`,
`A0 = 0xC8C × 4 = 0x3230`, `D7 = 0xFFFF − 0xC8C = 0xF373`.

Sound RAM `0x0000–0x322F` is erased: reset vectors, the driver's own code, and
the mailbox at `0x700`. That is why the SH-2 at frame 255 never gets its answer.

### 1.4 What is ruled out

- **A missing instruction.** `DBcc` is implemented and correct
  (`0x51CF & 0xF0F8 == 0x50C8`, target `pc − 2 + disp = 0x322E`). Every
  "unimplemented opcode" after the erase is a consequence.
- **The upload not having arrived.** The vectors were in sound RAM at reset; the
  zeros were written by the M68K itself.
- **The SCU DMA stride** (DeepSeek session). The boot does not use the
  `src_is_bbus` branch, and the code there was already `write_add >> 1`.

### 1.5 The open question

**Why is `A0 = 0` when the M68K enters `0x322E`?** A clear loop that starts at 0
and runs `0x10000` longs erases itself on any machine, so on real hardware `A0`
must start elsewhere, or the loop must be entered through code that sets it. That
code sits below `0x322E` and is erased by the loop, so the post-mortem dump
cannot show it.

## 2. Prerequisites — the instrument is distorting the measurement

In `saturn-core/src/m68k.rs`, `M68k::step()` currently does, **on every M68K
instruction, with debugging off**:

- `UNIMPL_LOG_COUNT.fetch_add` — a probe committed in `ddf3d56`. It also burns
  the counter of the unimplemented-opcode diagnostic, which then never prints
  (this is how the DeepSeek session concluded the derailment had stopped);
- two calls to `std::env::var("MIMAS_DEBUG_M68K")` — each takes the process
  environment lock and allocates. This is the pattern already removed from
  `sh2.rs` (`bus_trace_enabled`, tri-state `AtomicU8`).

With debugging on, it adds a `Mutex` lock and a `Vec::remove(0)` per
instruction (`TRACE_RING`).

These change how fast Core 4 runs relative to Core 0 — and the driver runs while
the SH-2 is still uploading (§1.1). Fix before measuring anything:

1. Remove the `[M68K START]` probe.
2. Replace the per-instruction `env::var` with a tri-state `AtomicU8`, as in
   `sh2.rs`.
3. Replace `TRACE_RING: Mutex<Vec<…>>` with a fixed lock-free ring (the shape of
   `PC_RING` in `sh2.rs`).
4. Remove the other probes committed in `ddf3d56` (`[DEBUG] MISMATCH` in
   `Sh2::run_loop`, three `[WATCH]` in `shared_buffers.rs`,
   `/tmp/mimas_ram.bin` in `main.rs`).
5. Re-measure the with-disc divergence with plan 01's tool. Timing changed; the
   baseline must be re-taken, not assumed.

Recommendation for review (it changes `tools/`): extend the golden rule
`thin-instruction-path` to cover `M68k::step`. It would have caught all of the
above.

## 3. Steps

### 3.1 Capture the driver before it runs

`MIMAS_SOUND_RAM_AT_SNDON=<file>`: dump sound RAM at the moment Core 4 resets
the M68K. Permanent diagnostic, off by default, one check at SNDON — not in any
per-instruction path.

### 3.2 A 68000 disassembler

None is available: this machine's binutils has no m68k target, and `capstone` is
not in `.venv`. Add `capstone` to `.venv` (it supports M68K) and a thin
`tools/m68kdis.py`. Diagnostic only; the gate stays stdlib-only.

### 3.3 Capture the path from reset to `0x322E`

With the lock-free ring from §2, dump it **at the first entry to `0x322E`** —
not at the first unimplemented opcode, as today, by which time the setup code is
erased. Size it to cover reset → `0x322E`; start at 64K entries and measure.

### 3.4 Find the instruction

Disassemble the dump from 3.1 along the path from 3.3. Identify which
instruction should give `A0` (and `D7`) their values before `0x322E`, and what
our interpreter did instead: executed it wrongly, or never reached it.

Semantics: from the reference's 68000 core, read for understanding, never
ported (`CLAUDE.md`, `no-yabause-code`). Test: hand-derived values for that
instruction, written before the fix.

### 3.5 Or it is pacing, not an instruction

If 3.3 shows the M68K ran code that had not been uploaded yet — the path passes
through bytes that differ between the SNDON dump and the upload's source — the
cause is Core 4 running ahead of Core 0's upload. The fix is then in how Core 4
reports cycles to `LockStepSync`, not in `m68k.rs`, and it must respect the
golden rules (no polling, no wall clock, the slack invariant). The real boot
shows the M68K running during the upload, so this is a question of relative
speed, not of ordering.

### 3.6 Ask for an M68K reference trace

If the instrumented YabaSanshiro can trace its 68000 core the same way it traces
the SH-2 (first execution, operands, memory), that trace becomes this plan's
reference, and plan 02's comparator applies to it unchanged. It would replace
3.4's manual reading with a diff.

## 4. Acceptance

- After 60 s of boot with a disc: the first 8 bytes of sound RAM are still the
  reset vectors; no M68K "unimplemented opcode"; the diagnostic
  "region around pc all zero" never fires.
- Plan 01's `boot_diff.py`, with disc: the reproduced prefix passes frame 255;
  `0x06012F6C` is reached; the next mailbox write is `0x85` to `0x25A00700`.
- A unit test for each corrected instruction, from hand-derived values.
- M68K throughput measured before and after §2, reported.
- Gate green except recorded debt; `no-yabause-code` clean.

## 5. Risks

- **More than one bug.** `m68k.rs` is at 24% coverage with three opcode masks
  known to never match (`.development/current_bugs.md`). Expect the driver to
  hit several.
- **Timing.** Removing the probes changes relative core speed. A divergence that
  moves after §2 is information, not noise.
- **No M68K reference** unless 3.6 happens: until then, correctness of the
  driver's path is judged by the SH-2 side (the mailbox) only.
