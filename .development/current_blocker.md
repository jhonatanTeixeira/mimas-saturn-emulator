# Current blocker: M68K sound driver self-corrupts before finishing setup

**Goal this unblocks**: BIOS boot progress toward rendering the Saturn logo
in `mimas_window`. This is the single thing standing between "BIOS reaches
new territory" and "BIOS reaches VDP2 setup and we see something on
screen" as of this writing — though there may be further walls after it,
undiscovered because this one hasn't cleared yet.

## Where things stand

A real M68000 interpreter (`saturn-core/src/m68k.rs`) now runs the actual
sound driver the BIOS uploads into Sound RAM, triggered by real SMPC
SNDON, with the real SCSP→SH-2 "Sound Request" interrupt (MCIPD/MCIEB →
SCU vector 0x46, level 9) implemented and wired up. **This already moved
the boot wall**: Core 0's PC now reaches new territory around `0x06011900`
(re-diagnose by address each session — BIOS revisions/timing shift this),
past the earlier `0x060108xx` loop it used to get stuck in.

The M68K core itself, however, does not run the driver to completion. It
derails partway through the driver's very first real action.

## The specific failure, traced

1. On reset (real M68000 semantics: SSP from Sound RAM address 0, PC from
   address 4), the driver's PC starts at `0x1000`. Sound RAM from `0x1000`
   to `0x322D` is entirely zero bytes at that point — no real code there
   yet, decoded as a long run of harmless `ORI.B #0,D0` no-ops by the
   interpreter until it reaches real code at `0x322E`.
2. Real code at `0x322E`: `MOVE.L D0,(A0)+` (2 bytes) then `DBRA D7,-4`
   (4 bytes) — a textbook "clear N longwords starting at A0" loop.
   Observed register state at first entry: `D0=0`, `A0=0`, `D7=0xFFFF`.
   `D7=0xFFFF` is the standard 68000 idiom for "loop 65536 times" (DBcc
   decrements then exits only on wrapping to `0xFFFF`).
3. Each iteration writes a zeroed 32-bit longword to `(A0)` then
   increments `A0` by 4. Because `MOVE.L` writes 4 *consecutive* bytes,
   and the loop's own 6 bytes of code sit inside the address range being
   swept (starting from 0), iteration ~3211 writes to `A0=0x322C..0x322F`
   — which includes the loop's own `MOVE.L` opcode at `0x322E-0x322F`.
   That opcode is now `0x0000`. The interpreter re-fetches at `0x322E`,
   decodes `0x0000` as another (harmless) `ORI.B #0,D0`, consuming what
   used to be the `DBRA`'s displacement operand as a fresh instruction,
   and PC ends up at `0x3232` — the middle of what used to be a valid
   instruction stream, now garbage. From here it reads `0xFFFC`/`0xFF00`-
   pattern words as unimplemented "Line F" opcodes and makes no further
   real progress.
4. Right after this (still-intact) loop, at `0x3234-0x323B`: a second,
   near-identical setup — `MOVEA.L A5,A0` (A5 holds `0x100000`, the SCSP
   register base), `MOVEQ #0,D0`, `MOVE.W #$FF,D7` — feeding a *second*
   instance of the same 2-instruction clear loop at `0x323C`. This
   confirms the pattern is a **reusable helper**, called with different
   parameters for different regions (Sound RAM vs. SCSP registers, it
   looks like) — not a one-off. The first call's parameters (clear 256KB
   starting at address 0, using code positioned only ~12KB in) are the
   ones that don't fit.

## What's been ruled out

- **Not a decode bug in `DBcc` or `MOVE`.** Both were checked
  instruction-by-instruction against Musashi's real implementation
  (`yabause/src/musashi/m68kopdm.c`'s `m68k_op_dbf_16`, and `MOVE`'s
  field-layout math) and match exactly, including the branch-target
  formula's `PC - 2 + disp` and the post-decrement `!= 0xFFFF` exit
  check.
- **Not a debounce/timing race.** Added a 2ms debounce before Core 3
  resets the M68K on the SNDON edge (theory: the SH-2's own upload
  routine might still be running when Core 3 observes the flag, given
  they're on independent OS threads with no barrier beyond the flag).
  The Sound RAM image captured at reset time was byte-for-byte identical
  with and without the debounce — ruling this out. Whatever's in Sound
  RAM at SNDON time is already final by the time the SH-2 executes the
  `COMREG=0x06` write; it isn't still arriving.
  **Re-verified 2026-07-12** after replacing the debounce entirely with a
  real `Ordering::Release`/`Acquire` signal (`TECH_DEBT.md` item 1,
  `history.md` Chapter 7): identical signature against a real BIOS
  (`Sega Saturn BIOS (USA).bin`) — `D7=0xFFFF, A0=0` at the first
  clear-loop entry, derails to `0x3232` reading `0xFFFC` then a run of
  `0xFF00` "Line F" opcodes, PC reaches the same `~0x06011900` territory.
  This wall is confirmed orthogonal to that architectural fix.
- **Not obviously a Sound RAM address-mapping bug** — the same region the
  SH-2 writes into (0x05A00000/0x25A00000) is what the M68K reads at its
  own address 0, confirmed against Yabause's `M68K->SetFetch(0, 0x80000,
  SoundRam)`.

## Leading hypothesis (untested)

The `D7=0xFFFF, A0=0` combination for the *first* clear-loop call is
wrong — either:

(a) the SH-2's own upload routine writes an *incomplete* driver image
(the real driver's setup code that should load a much smaller D7 value
before this loop runs is missing, and what's left over from a previous
memory state or a partial write happens to look like `0xFFFF`/`0` by
coincidence), or

(b) the SH-2's upload is complete and correct, and `D7=0xFFFF, A0=0`
really is what the real BIOS driver's first pass does — meaning either
real hardware also wouldn't get past this in the state we're feeding it
(unlikely for a real, shipped BIOS), or there's a *third* piece of state
this loop depends on that isn't `D7`/`A0` alone (e.g. a real M68000
subtlety in instruction prefetch timing that makes the corrupted fetch a
non-issue on real silicon, which a simple interpreter without a prefetch
queue wouldn't replicate) — this second branch of (b) is speculative and
the least likely explanation given how deliberately positioned the
"second clear-loop instance" pattern looks.

(a) is more likely given everything traced so far, and is directly
testable.

## Next concrete step

Don't infer the SH-2's intended Sound RAM writes from static post-hoc
dumps anymore — instrument the write path directly and read off the real
sequence:

1. In `Sh2::raw_write_byte`'s `MemRegion::SoundRam` arm (same pattern
   already used successfully for the High RAM counter probe — see
   `CLAUDE.md`'s reusable-diagnostics section), add a temporary probe that
   logs every write into Sound RAM from Core 0, with the writing PC, from
   shortly before the traced `SNDON`-adjacent code
   (`0x06010870`-`0x0601089c`) through to the SNDON `COMREG` write itself.
2. Run against the real BIOS, capture the full write sequence in order.
3. Reconstruct what the *intended* Sound RAM image at address `0x1000`
   onward should look like, and compare against what's actually observed
   at M68K reset time (same technique as the existing
   `sound_ram_dump.bin`/`MIMAS_DEBUG_M68K=1` tooling in `m68k.rs`).
4. If the intended and observed images differ: the SH-2-side write logic
   (or something it depends on, e.g. a source address it reads from BIOS
   ROM) has a bug — decode it the normal way (cross-check Yabause, fix,
   test, reverify).
5. If they're identical: hypothesis (a) is wrong, and this needs a fresh
   look at hypothesis (b) or a new one — at that point, consider whether
   continuing this specific investigation is worth it versus documenting
   it clearly and moving attention to M4 (VDP2 tile rendering) in
   parallel, since the BIOS logo may not strictly require sound to be
   working first (untested assumption — the current boot path happens to
   route through SNDON, but that doesn't prove video setup is gated on
   sound completing).

## Useful commands for whoever picks this up

```bash
# Rebuild with M68K debug instrumentation
cargo build -p saturn-frontend-native --release --bin saturn-frontend-native

# Run against the real BIOS with M68K tracing enabled
MIMAS_DEBUG_M68K=1 MIMAS_BOOT_WATCH_SECS=200 \
  ./target/release/saturn-frontend-native --bios <path-to-real-bios.bin>

# Disassemble a captured RAM dump offline (SH-2 side)
python3 tools/sh2dis.py /tmp/some_dump.bin 0x06000000
```
