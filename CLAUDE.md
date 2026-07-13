# Mimas — working methodology

Mimas is a from-scratch Sega Saturn emulator in Rust, with a distributed
multi-threaded architecture (`BusArbiter`, `LockStepSync` — see
`docs/PROJECT.md`/`docs/saturn_architecture_report.md`). This file
records the work loop that has reliably unblocked real BIOS boot progress,
one real wall at a time, and should be followed every time a new wall shows
up rather than reinvented per-session.

## The loop

1. **Boot the real BIOS and watch it hang.** Run against the real files
   (`saturn-frontend-native`'s CLI with `--bios`/`--chd`, or
   `saturn-frontend-native`'s `mimas_window` binary for a live window), and
   observe where `PC` (via `SaturnSystem::cpu0_pc`) stops making forward
   progress. `MIMAS_BOOT_WATCH_SECS=N` on the plain CLI controls how long it
   samples before giving up.
2. **Decode exactly what's stuck, don't guess.**
   - If the stuck address is in BIOS ROM (`< 0x00100000`), read the bytes
     directly from the real BIOS file and decode them by hand against the
     opcode table in `sh2.rs`.
   - If it's in RAM (e.g. `0x06000000+`, code the BIOS copied off the ROM
     and is executing live), add a **temporary** `eprintln!` probe in
     `Sh2::execute()` gated on a PC range, rebuild, rerun, and capture
     real register state at that exact point. Remove the probe once
     diagnosed — these are throwaway, not permanent instrumentation.
3. **Cross-check the exact opcode/register/address against a real, working
   emulator before implementing anything.** Two full reference sources live
   in this repo:
   - `../yabause/` — the devMiyax/YabaSanshiro fork (ARM64 dynarec), used
     elsewhere in this repo for the R36S Saturn core.
   - `../yabauseut/` — a plain upstream Yabause checkout.

   For a new opcode: find its handler in `yabause/src/sh2int.c` (interpreter,
   function-pointer table populated by `decode()`) and read the *exact*
   semantics (branch target formula, flag updates, push/pop order) — don't
   infer from the SH-2 manual alone, the emulator's exact behavior is what
   real BIOS/game code was tested against.

   For a memory-mapped register: find its offset and read/write handling in
   `yabause/src/memory.c` (top-level dispatch table) and the relevant
   peripheral file (`smpc.c`, `scu.c`, `vdp1.cpp`, `vdp2.cpp`). Confirm the
   *physical* address (strip the cache-through `0x20000000` bit) and cross
   two independent sources in the C code before trusting a number (e.g. the
   struct's doc comments *and* the read/write `switch` *and* actual use in
   the rendering/logic code) — this caught real inconsistencies before.

   Prefer `vidsoft.c` over `vidogl.c` for VDP1/VDP2 pixel-level algorithms:
   same register offsets and bit semantics, but far more readable (no GPU
   texture/context management obscuring the actual math).
4. **Translate the semantics into our architecture — never copy the C.**
   Our distinguishing architecture (threaded cores, `BusArbiter`,
   `LockStepSync`, lock-free frame publishing via `ArcSwap`) has no
   equivalent in Yabause (single-threaded, GPU-backed). Port *what the
   hardware does*, not Yabause's specific data structures or control flow.
   Idiomatic Rust (`match` over nested `switch`, `Option`/`Result`, no raw
   pointer arithmetic) is expected, not a transliteration.
5. **Write a regression test using real values before moving on.** Real
   BIOS bytes/addresses when the bug was found in ROM; a hand-traced
   algorithm step (computed independently, e.g. in a throwaway Python
   script) when the bug was in a stateful algorithm like `DIV1`. Don't
   assert a value you haven't independently derived — a wrong test that
   "confirms" a wrong implementation is worse than no test (this happened
   once already with the first `bt_bf_no_delay_slot` test and the first
   `DIV1` multi-step test; both asserted self-consistent-but-wrong values
   until checked against real hardware behavior / hand tracing).
6. **Rebuild, run the full workspace test suite, then rerun against the
   real BIOS to confirm the wall actually moved** (new PC territory, not
   just a different loop at the same conceptual wall). Keep the loop going.
7. **Update the `.development/` tracking docs and `history.md` before
   moving on** — every time real progress happens (a wall clears, a bug
   gets fixed, a new gap gets discovered), not just at the end of a
   session:
   - `.development/current_blocker.md`: rewrite it to describe the *new*
     current wall once the old one clears (or update it in place if the
     investigation continues but understanding deepens). This file should
     always describe the single thing actively blocking forward progress
     right now, not a historical log.
   - `.development/current_bugs.md`: add newly discovered gaps/bugs; move
     entries out (or mark fixed and drop them) once genuinely resolved.
   - `.development/TASKS.md`: move items between Done/In progress/Not
     started as their status actually changes.
   - `.development/ROADMAP.md`: update a milestone's status (⬜/🟡/✅) when
     it changes.
   - `history.md`: add a new chapter (or extend the current one) telling
     *why* a non-obvious decision was made, not just what changed — the
     code diff already says what changed.
   Skipping this step is how the next session ends up re-deriving
   knowledge that was already earned once.

## Non-negotiables carried over from earlier fixes

- Keep the architecture faithful to *real Saturn hardware behavior*, not to
  whatever's simplest to implement. Where a real simplification is made
  (e.g. VDP2 backdrop-only rendering before NBG tiles exist, or SMPC
  commands completing "instantly" instead of real timing), say so
  explicitly in a comment and keep it visually/functionally honest (e.g.
  black screen when nothing is configured, not a placeholder color).
- Never break `Sh2::new()`'s 3-argument signature or existing test call
  sites without a strong reason — many tests across `e2e-tests` and
  `saturn-core` depend on it staying stable. Add new capability via setter
  methods or optional fields instead.
- `cargo test --workspace` must stay green (71 official E2E tests +
  everything under `saturn-core`) after every change in this loop, not just
  the specific test for the current fix.

## Useful throwaway-diagnostic recipes (reuse these patterns, don't reinvent)

- **Dump a RAM window once, disassemble offline.** Add a one-shot probe in
  `Sh2::execute()` (gated by a `static AtomicBool` swap so it fires exactly
  once) that `std::fs::write`s a slice of `work_ram.high_ram` to a scratch
  file when PC first enters a stuck address range. Then decode it in bulk
  with `tools/sh2dis.py` (same opcode table as `sh2.rs`'s `execute()`; run
  as `python3 tools/sh2dis.py <dump.bin> <base_addr_hex>`) rather than
  hand-tracing byte-by-byte — BIOS code is dense with interleaved literal
  pools (`MOV.L @(disp,PC),Rn`) that a linear disassembler will misdecode as
  garbage instructions; that's expected, real code resumes after a
  `BRA`/`RTS` that skips the pool. Keep `sh2dis.py` in sync with `sh2.rs`'s
  opcode table by hand if new opcodes are added there.
- **Find who writes a specific RAM variable.** Don't guess from static
  disassembly alone — add a surgical probe directly in `raw_write_byte`'s
  `MemRegion::HighRam` arm, gated on the exact offset, logging `self.pc`.
  This resolved a real wall in one shot after static tracing produced a
  plausible-but-wrong hypothesis (see below).
- **`REG_ACCESS_LOG`/`log_reg_access_once`** in `sh2.rs` (currently live,
  not removed) dedups and logs every distinct SMPC/VDP1/VDP2/SCU/CS2
  register access by offset+direction+value exactly once per process run.
  Grep its `[REGACCESS]` output after any boot run to see which real
  hardware registers the BIOS actually touches before hypothesizing — this
  is what found the INTBACK/COMREG gap and the TVSTAT-read gap, both faster
  than disassembly would have.

## Current wall

See **`.development/current_blocker.md`** for the live, detailed
description of whatever's actively blocking boot progress right now — it's
kept current instead of duplicated here so there's exactly one place that
can go stale. Update *that* file (not this section) when the wall moves;
see the loop's step 7 above.
