# Appendix: real-game register values

## Source and methodology

This data was NOT derived from reading Yabause's source (the rest of `hardware-reference/`'s
methodology) — it's a live capture of a real play session of a real commercial game (Magic
Knight Rayearth, Sega Saturn), taken with an instrumented Yabause/YabaSanshiro build that hooks
every VDP1/VDP2/SCU/SMPC register write during real gameplay and streams it out for aggregation,
recording every **distinct** value observed per `(region, offset)` and how many times.

**Why this is a third source of truth, not a duplicate of the Yabause-source citations already in
this file:** it's what a real game actually writes, not what the reference emulator's C code is
capable of accepting. Where a value observed here matches this file's existing documentation, it's
independent confirmation. Where it doesn't, or where a register this file marks "not yet
implemented" has real observed traffic, that's new information worth having before writing the
corresponding Mimas code — matching this project's own stated rule (`CLAUDE.md`): "Write regression
tests from independently-derived values... never assert a value you haven't independently derived."

**Register naming below**: only registers independently confirmed by matching the offset against
this repo's own `vdp1.md`/`vdp2.md`/`scu.md`/`smpc-peripheral.md` tables are named. Everything else
is left as a bare offset — guessing a name from a value pattern alone would violate the same rule
this data is meant to serve. Verify against the relevant register table in this directory before
trusting an unconfirmed offset's identity.

Offsets are the small (`0x00`-`0x1FF`) in-region byte offset convention already used throughout
this directory's own tables, NOT the physical `0x25xxxxxx`/`0x05xxxxxx` bus address — confirmed
compatible by direct match: this capture's SMPC offset `0x1F` and `0x63` line up exactly with
`smpc-peripheral.md`'s own `0x1F` = COMREG and `0x63` = SF entries.

All values below are hex, `value×count` format, most-frequent-first is NOT guaranteed (source order
preserved), capped at 8 distinct values per register by the original aggregation tool.

## SMPC (confirmed against `smpc-peripheral.md`)

| Offset | Register | Writes seen | Real values observed |
|---|---|---|---|
| `0x01` | IREG0 | 26776 | `01×8755, 00×265, 40×277, 80×8748, C0×8731` |
| `0x03` | IREG1 | 9020 | `02×1, 08×9019` |
| `0x05` | IREG2 | 9020 | `F0×9020` (constant — real game always sends the same IREG2 argument) |
| `0x1F` | **COMREG** | 9054 | `10×9020, 19×6, 07×3, 06×3, 03×11, 02×6, 1A×5` — `0x10` (INTBACK) dominates by far; the low-frequency values are the actual OTHER commands this real game issues across a play session, a candidate checklist for "which COMREG commands does real code actually exercise" |
| `0x63` | SF (status flag) | 9050 | `01×9050` (constant — matches this file's own note that real code polls SF as a busy flag) |
| `0x79` | DDR1 | 2 | `00×2` |
| `0x7B` | DDR2 | 2 | `00×2` — consistent with this file's own `[QUIRK]` note that real Yabause has no `case 0x7B` at all; the real game writes here anyway (harmlessly, per the reference) |
| `0x7D` | IOSEL | 2 | `00×2` |
| `0x7F` | EXLE | 2 | `00×2` |

## VDP1 (offsets unconfirmed — cross-check against `vdp1.md`'s own register table before naming)

| Offset | Writes seen | Real values observed |
|---|---|---|
| `0x00` | 19 | `00×19` |
| `0x02` | 7309 | `00×19, 03×7163, 02×127` — heavy, sustained traffic on this offset once past setup; only 3 distinct values the whole session |
| `0x04` | 17 | `02×16, 00×1` |
| `0x06` | 27 | `00×15, 8000×12` |
| `0x08` | 27 | `00×27` |
| `0x0A` | 27 | `50DF×22, 00×1, FFFF×4` |

## VDP2 (144 distinct offsets captured in the full session; the entries below are the ones
independently cross-referenced against this file's own register table and worth calling out)

- **`0x10`-`0x1E`** (CYCA0L/U, CYCA1L/U, CYCB0L/U, CYCB1L/U — VRAM access cycle pattern registers,
  `vdp2.md` §A.3, **Phase 9 "what to actually build" — not yet implemented as of this writing**):
  real steady-state values, after a handful of setup writes: `0x10`→`4455` (×6512), `0x12`→`66FF`
  (×5086), `0x14`→`4455` (×6512), `0x16`→`66FF` (×5086), `0x18`→`4455` (×6512), `0x1A`→`66FF`
  (×5086), `0x1C`→ splits `2FF1`(×5086)/`62FF`(×1426), `0x1E`→`0F1F`(×6512). These are concrete,
  real target values for whenever Phase 9 lands — no synthetic test needed to know what a real
  game actually configures here.
- **`0x00`**: `00×30, 8000×6537` — TVMD-shaped (display on/off), matches expected boot-then-steady
  pattern.
- **`0x0E`**: `0300×6513, 00×32` — small transient-then-steady pattern, same shape as `0x10` family.
- **`0x20`**: `0007×1, 0203×4, 0103×1, 080C×1, 0001×29, 0000×3, 0007×6512` — settles to `0x0007`.
- Every other captured VDP2 offset either stays at `0x0000` for the whole session (dozens of
  registers — see the full table for the complete list, useful as a "this real game never touches
  this register at all" negative-evidence set) or has a similarly small, real, bounded value set.

## SCU (offsets unconfirmed — cross-check against `scu.md` before naming)

| Offset | Writes seen | Real values observed (width) |
|---|---|---|
| `0x00` | 15001 (32-bit) | Wide spread — DMA source addresses, real values include `0608F0A8×1500`, `060C3C66×11049` |
| `0x04` | 15001 (32-bit) | DMA destination addresses — `25C02140×3429`, `25C000C0×3428`, `25F00000×1500` |
| `0x08` | 15001 (32-bit) | Transfer counts/config — small values, `10000×1` up to `5A004×1` |
| `0x0C` | 15001 (32-bit) | `0101×15001` (constant) |
| `0x10` | 15001 (32-bit) | `0101×15001` (constant) |
| `0x14` | 15001 (32-bit) | `0007×15001` (constant) |
| `0xA0` | 114805 (32-bit) | Dominated by `FFFFFFFF`(×9393)/`FFFFFFFE`(×9351) — looks like an interrupt mask/ack register given the near-all-ones traffic |
| `0xA4` | 68 (32-bit) | `FFFFFFFF×37` plus scattered other near-all-ones values |

## SH-2 real workload profile (different capture tool, same game/session family)

The sections above come from a register-*write* hook. This section comes from a different
instrumentation point in the same sibling project (`portal_to_another_world`'s `portal_trace.c`,
built into the same instrumented Yabause/YabaSanshiro lineage): a call/return hook on every
`JSR`/`BSR`/`JMP`/`BRAF`/`BSRF`, aggregated into `tools/portal_trace_report.json`. It answers a
different question than the register-write data above — not "what values does real code write to
hardware" but "which SH-2 *routines* does real code actually spend its time in, and how often." Real
counts from one real Magic Knight Rayearth play session (~59M total calls across 1468 distinct call
targets):

| Address | Real calls | What it is |
|---|---|---|
| `0x06055958` | **4,755,141** | Compiler-generated signed 64/32-bit division: `DIV0S`×2, then a 32-iteration `DIV1`+`ROTCL` bit-serial loop, `ADDC` at the end. The single most-called routine in the whole session — real, ordinary game code apparently divides constantly. At ~65 real SH-2 instructions per call, that's over 300 million real `DIV1`/`ROTCL` executions in this one session alone. |
| `0x060B1BC4` | 4,483,066 | Q16.16 fixed-point integer-part extraction: `MOV R4,R0` / `SHLR16 R0` / `RTS` (delay slot `EXTS.W R0,R0`) — arithmetically `(x >> 16)` sign-extended. |
| `0x060B1B94` | 1,769,550 | Q16.16 fixed-point multiply: `DMULS.L R4,R5` (signed 32×32→64) then `XTRCT` to pull out the middle 32 bits — the standard `(a*b) >> 16` idiom compiled without a real fixed-point multiply instruction. |

**Why this earns a place here rather than being a bulk data dump** (unlike the full 1468-entry call
table and per-site register-argument samples the source tool also has, deliberately *not* included
— no concrete Mimas use today): it converts `DIV1`/`ROTCL`/`DMULS.L`'s cycle costs and edge-case
behavior from "one line in a hardware-reference table" into "the hottest path in a real commercial
game, executed hundreds of millions of times per session." `docs/implementation-plans/sh2-cpu.md`'s
own testing checklist (item **P1-T7**) already flags `DIV1`'s real multi-step hardware behavior as a
still-open question (`div1_single_step_matches_hand_traced_algorithm` explicitly disclaims full
coverage) — this data is the concrete argument for why that item, and `DMULS.L`'s own cycle
accounting, matter in practice rather than only in principle.

## How to use this

1. Before implementing or fixing a register this file lists, check whether it's here too.
2. A match against this file's own documented behavior is a free extra confirmation.
3. A real observed value NOT anticipated by this file's documentation (or a register marked
   not-yet-implemented that has real traffic here) is worth a second look — could be a gap in the
   Yabause-source reading, a genuine hardware quirk only visible from real code, or just a register
   this game happens to exercise that a synthetic/BIOS-only test never would.
4. Only registers worth calling out (matched against this repo's own tables, or landing on a
   not-yet-implemented phase) are listed above; the full session captured 144 distinct VDP2
   offsets alone, most of which stayed at a constant value the whole session and aren't repeated
   here.
