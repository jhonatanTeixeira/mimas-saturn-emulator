# Current known bugs / open correctness gaps

These are known, real gaps — not the active boot blocker (that's
`current_blocker.md`). None of these are known to be *causing* the current
blocker, but any of them could turn out to matter once the current wall
clears, so they're tracked here rather than silently forgotten.

## 1. DIV1 multi-step chaining is unverified end-to-end

**Where**: `saturn-core/src/sh2.rs`, the `0x3004` (`DIV1 Rm,Rn`) opcode
handler.

**What's verified**: the single-step mechanics (Q/M/T update case analysis
on `(old_q, M)`) were ported from and cross-checked line-by-line against
Yabause's `SH2div1` (`yabause/src/sh2int.c`) and match exactly. There's a
regression test (`div1_single_step_matches_hand_traced_algorithm`) proving
one step matches an independently hand-traced computation.

**What's NOT verified**: the standard "`DIV0U` then 32×`DIV1`" convention
real compiled SH-2 division routines use to get a full 32-bit unsigned
quotient. An earlier version of this test asserted that convention
produces a normal quotient and was *wrong* (caught by independent Python
re-simulation, not by any test) — it was replaced with the single-step
test and an explicit `CAUTION` comment in the code.

**Fix plan**: don't just trust the single-step correctness transfers to
the chained case. Get (or construct) a real SH-2-compiled division
routine's exact instruction sequence and expected output for a handful of
test vectors (signed/unsigned, positive/negative, edge cases like
divide-by-zero and overflow), run it through `Sh2::execute()` 32 times in
a loop matching that real sequence, and compare against the expected
quotient/remainder. Add as a new regression test alongside the existing
single-step one. Low priority unless something in the actual boot path
(or a future game) is found to depend on DIV1 chaining and produces wrong
results.

## 2. GBR-indexed byte TST/AND/OR/XOR forms are undecoded

**Where**: `saturn-core/src/sh2.rs`, tail of `Sh2::execute()`.

**What's missing**: `0xCC00`/`0xCD00`/`0xCE00`/`0xCF00` —
`TST.B/AND.B/OR.B/XOR.B #imm,@(R0,GBR)`. These are distinct from the
already-implemented `0xC800`-`0xCB00` immediate-only forms (`TST/AND/OR/
XOR #imm,R0`), which operate on R0 directly rather than through a
GBR-relative memory address.

**Status**: not yet hit running the real BIOS (confirmed via the "leave
CPU state unchanged" fallback never silently mis-executing anything so
far — if this gap were hit, it would show up as a stuck/wrong-behaving
loop the same way every other opcode gap has, per `CLAUDE.md`'s loop).

**Fix plan**: implement using the same case-analysis pattern as the
existing immediate forms, but computing the effective address as
`gbr + R0` (per real SH-2 addressing) before the read-modify-write. Cross-
check against `yabause/src/sh2int.c`'s equivalent handlers for exact flag/
byte semantics before trusting it, same as every other opcode in this
file. Add opportunistically when actually hit — don't implement
speculatively ahead of need.

## 3. M68K core has real gaps beyond the current blocker

**Where**: `saturn-core/src/m68k.rs`, tail of `M68k::execute()` (major
opcode groups `0xA` and `0xF`, plus any pattern within a handled group
that falls through unmatched).

**Status**: the M68K interpreter covers a broad, common subset (MOVE
family, arithmetic/logic, branches, JSR/JMP/RTS, MOVEM, LINK/UNLK, shifts,
bit ops, TAS) verified against Musashi (`yabause/src/musashi/`) where
encoding was in doubt, but it is not a complete 68000 implementation.
Group `0xA` (line-A, typically unassigned/trap on plain 68000) and group
`0xF` (line-F, coprocessor/MMU — no FPU on plain 68000, so real code
shouldn't hit this, but an interpreter bug elsewhere could cause an
errant fetch to land here, which is exactly what's observed in
`current_blocker.md`) are unimplemented and silently no-op.

**Fix plan**: same as every other opcode gap in this project — don't
implement speculatively. When `current_blocker.md`'s investigation (or any
future M68K wall) surfaces a genuinely-needed opcode outside the current
coverage, decode it from the 68000 Programmer's Reference Manual /
Musashi's `m68kops.c` table, implement, test, move on. `MIMAS_DEBUG_M68K=1`
already logs the first 20 unimplemented-opcode hits with full register
state for exactly this purpose.

## 4. `test_drift_limit_bypass_after_dma` may not exercise its intended path

**Where**: `saturn-core/tests/adversarial_tests.rs`.

**Status**: passes (part of the 125-test green baseline), but has a
comment noting the test's original design intent (cpu1/cpu2 "running
ahead" of cpu0 without touching memory) doesn't match how it's actually
driven (through real `Sh2::step()` calls, which block on DMA locks the
same way cpu0 does, contradicting the premise that they'd run ahead). This
was flagged mid-session; a prior `#[ignore]` attribute someone/something
added was later removed (not by this agent) and the test has passed since
— possibly coincidentally, not because the design gap was fixed.

**Fix plan**: not urgent (it's green, and the thing it's *actually*
testing — general DMA/drift bookkeeping under `Sh2::step()` — is still a
real, valid test even if it doesn't test the *originally intended*
scenario). If genuinely revisiting `BusArbiter`/`LockStepSync` drift logic,
first decide whether the original scenario (cores legitimately running
ahead without blocking) is even possible in the current design, and
rewrite the test to drive it via `sync.sync_core()` directly (like
`test_shutdown_while_blocked_in_sync_condvar` does) if so.

## 5. VDP2 is backdrop-only

**Where**: `saturn-core/src/vdp.rs`.

**Status**: not really a "bug" — an explicit, documented simplification
(see the module doc comment) — but listed here because it's a correctness
gap that will matter as soon as `current_blocker.md` clears and BIOS
execution reaches real VDP2 layer setup: only the solid backdrop color
renders. NBG0-3 tile/bitmap decoding doesn't exist yet. Tracked properly
as milestone M4 in `ROADMAP.md`; listed here too since "screen shows a
color but not the logo" is exactly the kind of result that could look like
a bug at first glance once the current blocker clears.
