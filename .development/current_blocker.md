# Current blocker: new stall past the SCU DSP fix, not yet root-caused

**Goal this unblocks**: BIOS boot progress toward rendering the Saturn logo
in `mimas_window`. This is the single thing standing between "BIOS reaches
new territory" and "BIOS reaches VDP2 setup and we see something on
screen" as of this writing — though there may be further walls after it,
undiscovered because this one hasn't cleared yet.

## Where things stand (2026-07-13)

The previous wall described in this file (Core 0 stuck polling the SCU
DSP's Program Control Port `EX` bit forever, at `0x06013264`-`0x06013268`)
is **fixed** — a real SCU DSP interpreter now exists (`saturn-core/src/
scu_dsp.rs`) and Core 2 (previously permanently parked) actually executes
DSP programs.

**Why this needed a whole new component, not just a register fix**: `EX`
only clears when a *running DSP program* reaches an End instruction — Core
2 had zero DSP execution before this, so nothing could ever clear it
regardless of what the register plumbing looked like. The real, uploaded
32-word BIOS DSP program was recovered by dumping High RAM at the stuck PC
and following the wait loop's literal-pool pointer back through the
program's setup code (three Data RAM parameter words, `[0, 0x09694000,
0x000002AB]`, written via the Data RAM Data Port right before the trigger)
to the actual `0x06013280`-`0x060132FC` program bytes. Decoding it against
Yabause's exact bit layout (`scu.c`'s DSP exec block, `readgensrc`/
`writed1busdest`/`writeloadimdest`, `dsp_dma01`-`dsp_dma08`) showed it uses:
plain ALU ops (NOP/ADD/SUB), D1-bus stores, conditional and unconditional
MVI, conditional JMP (Z/T0-gated), and two DMA addressing-mode variants
(main-RAM-read and main-RAM-write directions) — no loop instructions, no
DSP-side interrupt request.

**What's implemented**: the full ALU/Operation/Load-Immediate/Jump/Loop/End
instruction groups (faithful to Yabause's exact bit math, not just the
opcodes the real program happens to use), plus 2 of real hardware's 8 DMA
addressing-mode variants — the 2 the real program actually exercises. The
other 6 are a known, explicitly-flagged gap (see `scu_dsp.rs`'s module doc
comment) — add them the same way as everything else in this project: hit
them, decode, cross-check Yabause, implement, test. Register ports (offsets
`0x80`/`0x84`/`0x88`/`0x8C`) are intercepted at `Sh2::read_long`/
`write_long` (real hardware: 32-bit-only ports, not per-byte storage like
plain SCU registers) and shared with Core 2 via `Arc<Mutex<ScuDsp>>`; a
write setting `EX` calls `sync.set_thread_active(2, true)` to wake Core 2,
mirroring the existing SSHON/Core-1 and SNDON/M68K reactivation shape. Core
2 re-parks the instant the DSP program clears `EX` on its own (real
hardware: the DSP genuinely stops consuming cycles then).

**Verification**: a new unit test (`scu_dsp::tests::
real_bios_dsp_program_runs_to_completion`) loads the exact captured
program + parameter words and confirms it reaches its End instruction in
bounded steps — this is the strongest signal the interpreter is correct
for the program that matters, independent of the full real-BIOS run.
Confirmed against the real BIOS too: Core 0's PC, which previously never
moved past `0x06013264` no matter how long the boot-watch window, now
races through a large amount of new code (the interrupt dispatcher
trampoline, `0x0601xxxx`/`0x0600xxxx` handler bodies, `0x06013144`-
`0x060131a8`-ish DSP-invocation call sites reached from multiple different
interrupt paths) before settling at a *new* address (see below).

## The new stall

A real-BIOS run (`MIMAS_BOOT_WATCH_SECS=280`) now stops early via the
boot-watch loop's "unchanged for 500ms" early-exit, settled at
`0x060131A8`, having reached that point after visiting hundreds of
distinct addresses (real forward progress, not a repeat of the old wall).
`0x060131A8`'s surrounding code (from the same High RAM dump used for the
DSP investigation, still valid since only execution reached further, not
different code) is a bounded-looking `for`-style loop — a counter compared
against `50` at `0x060131e2`, another comparison against values loaded
from `0x060131fc`/`0x060131fe`, and a call (`JSR`, literal at `0x06013228`
→ `0x06013344`) partway through. **`0x06013344` turned out to be a plain
software 32-bit division routine** (a `DIV0S`/`DIV1`/`ROTCL` sequence,
called from many unrelated places throughout the BIOS for ordinary
arithmetic) — not DSP- or hardware-specific, so it's very unlikely to be
the actual cause; the real blocker is more likely something the *outer*
loop or its own caller depends on.

**Not yet investigated**: this needs the same work loop as every prior
wall (`CLAUDE.md`) — a fresh High RAM dump at the exact stuck PC (the one
used for this write-up predates the DSP fix reaching this far, so re-dump
before trusting exact byte contents), decode what register/counter it's
actually waiting on, and cross-check against Yabause before touching
anything. Given how many different call sites funnel through the
`0x06013144`-`0x060131e2` region (interrupt-driven, per the DSP
investigation), a reasonable first hypothesis is that this loop is
iterating over some per-item BIOS bookkeeping (device list, DMA queue,
etc.) and is blocked on a count/flag from a different unimplemented
register — but this is a guess, not yet traced, and should be treated as
such.

## Still open, but not confirmed to be a gate right now

The M68K sound-driver self-corruption bug (uploaded driver's first
`MOVE.L D0,(A0)+`/`DBRA D7,-4` clear loop overwrites its own code,
`D7=0xFFFF, A0=0`) is **still unfixed**. It was re-verified byte-for-byte
unchanged through both the VBLANK-OUT fix and the SCU DSP fix, across
multiple real-BIOS runs — two independent, unrelated walls have now
cleared without it, so it doesn't look like it's gating overall boot
progress, but it's a real bug and will need fixing eventually (likely
before audio can work at all).

## Useful commands for whoever picks this up

```bash
# Rebuild
cargo build -p saturn-frontend-native --release --bin saturn-frontend-native

# Run against the real BIOS, watching Core 0's PC
MIMAS_BOOT_WATCH_SECS=280 \
  ./target/release/saturn-frontend-native --bios <path-to-real-bios.bin>

# Disassemble a captured RAM dump offline (SH-2 side)
python3 tools/sh2dis.py /tmp/some_dump.bin 0x06000000
```
