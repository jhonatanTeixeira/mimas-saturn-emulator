# Plan 04 — CD block power-on status

**Goal:** with no disc, reproduce the real boot past frame 181.

**Size:** small. One behaviour, one line of `reset_system()`, plus tests. It is
also the cheapest end-to-end check that plan 01's tooling works.

---

## 1. The divergence

Without a disc, the real boot and ours are identical until reference
instruction #496, frame 181. There the BIOS reads a stored CD status byte and
tests it:

```
real:  0x000033A2  extu.b  r0 = 0x21        ; PERI | PAUSE
       0x000033A4  and #0x0F → 1
       0x000033D0  cmp/eq #6,r0   → no  (OPEN)
       0x000033D4  cmp/eq #7,r0   → no  (NODISC)
       0x000033D8  cmp/eq #10,r0  → no  (FATAL)
       0x000033DC  bsr 0x000032DC           ← drive initialisation path
ours:  status 0x27 = PERI | NODISC  →  bt at 0x000033D6 taken, 0x32DC path skipped
```

Our `CR1` reads back `0x27` (`[REGACCESS] Cs2Regs(0x90018) R val=0x27`).

**This happens with a disc too** — the with-disc and no-disc real traces are
identical for their first 6,127 instructions, both past `0x000033D8`. The drive
answers PAUSE on that first query whether or not there is a disc. With a disc
we already answer PAUSE, which is why the with-disc run gets further.

## 2. The hardware behaviour (knowledge from the reference, not code)

YabaSanshiro's CD block (read to understand, per `CLAUDE.md` — not ported):

- on reset, the drive status is **PAUSE**, unconditionally
  (`yabassanshiro/yabause/src/cs2.c:738`);
- a periodic status check, every `_statustiming` cycles, asks the drive backend
  whether a disc is present and only **then** moves an empty drive to NODISC
  (`cs2.c:970-995`).

So an empty drive does not know it is empty at power-on; it finds out on the
first periodic check. The BIOS's first status query lands before that.

## 3. Ours today

`saturn-core/src/cs2.rs`:

```rust
// reset_system()
self.status = if self.disc.is_some() { CDB_STAT_PAUSE } else { CDB_STAT_NODISC };
```

The periodic check already exists and already has the right shape — 3 Hz,
333,333 µs of emulated time, remainder carried forward (`exec()`,
`status_cycles_us`), turning an empty drive into NODISC. Only the reset
short-circuits it.

## 4. Steps

1. **Tests first, from the real boot's values:**
   - after `reset_system()` with no disc, status is `CDB_STAT_PAUSE`;
   - after less than one status period of `exec()`, still PAUSE;
   - after one full period (333,333 µs), NODISC;
   - with a disc, PAUSE throughout (unchanged).
   The first test fails today; the others pin the existing periodic behaviour
   so the change cannot break it.
2. Change `reset_system()` to reset to PAUSE unconditionally. Leave the periodic
   check to move it.
3. Check `load_disc()` and any other place that sets NODISC directly
   (`step_playback`, `cs2.rs:611`) for the same short-circuit. Change only what
   the reference behaviour says; record anything else as a finding.
4. Confirm the timing source: `exec()` is fed emulated microseconds via
   `exec_vblank()`, i.e. Master SH-2 cycle progress, not a host clock. It must
   stay that way (golden rule `no-wall-clock`).
5. Document: a **Status:** line in `docs/implementation-plans/cs2-cdblock.md`,
   and a `history.md` chapter — "why the drive says PAUSE when empty".

## 5. Acceptance

- The tests of step 1 pass.
- `tools/boot_diff.py` (plan 01), no disc: the reproduced prefix goes **past
  frame 181**; `0x000033D8` and `0x000032DC` are reached.
- With disc: the prefix does not move backwards from frame 255.
- Gate green except recorded debt; `no-yabause-code` clean.

## 6. What to expect next

Past `0x000032DC` the no-disc boot enters a path we have never run. The next
divergence is whatever that path needs. Do not guess it; measure it with
`boot_diff.py` and start the next plan from the address it names.

The later split between with-disc and no-disc (animation code, frames 436–479)
is where the periodic NODISC becomes visible to the BIOS. That is the next
reference point for this subsystem.
