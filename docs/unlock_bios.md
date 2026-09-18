# Unlocking the BIOS — what actually has to land

**Question this file answers:** what is left before a real Saturn BIOS runs all
the way to its interactive screen, and what is *not* in the way despite looking
like it is.

Everything below is measured on the current tree, not inferred from the plans.
The commands are in each section so the numbers can be re-derived rather than
trusted.

---

## Where it stops today

**It does not stop at `0x06001694`. That was an artifact of the boot watcher.**

`saturn-frontend-native` samples Core 0's PC every 20 ms and calls the boot
"settled" after 500 ms on one address, then ends the run. Twenty-five
consecutive samples landing on the same PC is not something a *running*
two-instruction delay loop can produce at 57 MHz — when it fires, the Master
thread is usually **blocked** (LockStepSync drift), and a frozen PC is not a
finished boot. `MIMAS_NO_EARLY_STOP=1` runs the whole window instead.

The difference is not marginal:

| | early stop (default) | `MIMAS_NO_EARLY_STOP=1`, 60 s |
|---|---|---|
| distinct PCs executed | 1,057 | **6,582** |
| of the reference's 9,882 | 10.7% | **66.6%** |
| reference frames complete | 9 of 64 | **12 of 64** |
| furthest PC seen | `0x06001694` | `0x06012F48` |

So every earlier conclusion in this file that treated `0x06001694` as a wall was
wrong, including a long theory about the sound-driver upload's write-verify loop
retrying forever. The ring buffer settles that one too: `0x06001676` (the write)
executes 2,248 times against `0x06001684` (advance to the next long) 2,249 —
**equal**, so the verify succeeds every pass and the copy progresses normally. A
failing verify would show the write count far exceeding the advance count.

## What the BIOS is actually doing there

The caller, also from ROM:

```
060015EA: MOV.L @(0x2d,PC),R7   ; R7 = 0x2010001F  (SMPC COMREG)
060015EC: MOV  #7,R1            ; 7 = SNDOFF
060015EE: BSR  0x06001692       ; 5000-count delay
060015F0: MOV.B R1,@R7          ; delay slot: write SNDOFF
...
06001604: MOV  #6,R1            ; 6 = SNDON
06001606: BSR  0x06001692
06001608: MOV.B R1,@R7          ; delay slot: write SNDON
0600160E: BSR  0x0600166E       ; then eight more calls to this
```

Write command, burn a fixed count, move on. **There is no SF polling here at
all** — an earlier theory that the SMPC completion handshake was hanging the
boot is wrong, and the register trace agrees: our own `[REGACCESS]` shows
`Smpc(99)` (SF) read as `0x11` then `0x10`, i.e. the busy bit set and then
cleared, and the M68K demonstrably starts (`[M68K] reset: SP=0x0000A000`), which
only happens if SNDON executed.

## The strongest candidate: the sound-driver upload

`0x0600166E`, called ten times around that sequence:

```
0600166E: MOV.L @R0+,R6     ; count
06001670: MOV.L @R0+,R3     ; destination
06001672: MOV.L @R0+,R1     ; source
06001674: MOV.L @R1+,R4     ; R4 = *src++
06001676: MOV.L R4,@R3      ; write
06001678: MOV  #10,R2
0600167A: DT R2 / BF        ; settle delay
0600167E: MOV.L @R3,R2      ; read back
06001680: CMP/EQ R2,R4
06001682: BF   0x06001676   ; mismatch -> rewrite. FOREVER. No retry counter.
06001684: DT R6
06001686: BF/S 0x06001674
0600168A: RTS
```

Its tables, read straight out of the ROM:

| table | count | destination | source |
|---|---|---|---|
| ROM `0x0016A8` | 11264 longs | `0x25A00000` | `0x25A00700` |
| ROM `0x0016C4` | 5216 longs | `0x25A00000` | `0x06010000` (WRAM) |

`0x25A00000` is **SCSP sound RAM**. This is the M68K sound-driver upload, done
with write-and-verify, and **eight of the ten calls happen after SNDON** — i.e.
while the M68K is already running.

That matters because our M68K derails immediately (below) and marches through
memory at +0x1000 per step. A derailed M68K writing into sound RAM corrupts the
bytes the BIOS is verifying, the compare never matches, and `BF 0x06001676`
retries without bound.

**This is a hypothesis with one piece that does not fit, and it is recorded
rather than smoothed over:** if the boot were stuck in that verify loop, the
hottest instruction would be its own inner delay at `0x0600167A`, not
`0x06001694`. Either an outer loop re-runs the whole SNDOFF/upload/SNDON
sequence, or something else is going on. Do not treat this section as settled.

## The M68K derails

```
[M68K] unimplemented opcode=0xFFFC at pc=0x00003232
[M68K] unimplemented opcode=0xFF00 at pc=0x00100002
[M68K] unimplemented opcode=0xFF00 at pc=0x00101002     <- +0x1000 per step
```

Sound RAM is 512 KB, so `0x00100000` is already past it: the PC is walking
mirrored memory that returns a constant, and `0xFF00` is a line-F pattern, which
on a real 68000 is the coprocessor trap. The two missing opcodes corrupt control
flow; everything after is consequence. **Implementing them may not be enough —
the symptom to fix is the jump.** `m68k.rs` is at 24% coverage with three opcode
masks that can never match.

## Ground truth is available, and it is very good

`/media/jhonatanteixeira/Novo volume/projects/jhon/dreams/paralel_exercise/portal_to_another_world/`
holds 31 GB of **real Yabause/YabaSanshiro capture** of Magic Knight Rayearth,
from BIOS reset through several minutes of gameplay: 141 M trace events
(`traces/portal_sessions/session_0000.jsonl` — `call`/`return`/`mem_write` with
full register state), 184 captured frames as PNG, and per-frame studies.

Frame 0 executes `0x200003BA`, the same reset PC Mimas reports, so the capture
starts where we start.

**The BIOS boot animation is real-time VDP1 polygons, not FMV.** Settled from
this data:

| frame | content |
|---|---|
| 264 | fragments appearing at a point (`0x060131xx` executing) |
| 370 | shards dispersed, moving in depth (`0x0603A0xx`) |
| 478/480 | converging into a pile (`0x06039Fxx`, `0x0603A0xx`) |
| 515/516 | white flash into the logo (`0x060110xx`) |

Corroborating: the BIOS drives `PTMR` (`0x25D00004`) `= 2` — plot trigger on
every frame change — plus `EWRR = 0x50DF` and `FBCR = 3`; ~100 distinct code
addresses execute per frame inside a tight region, which is a render loop, not a
video decoder; and the sampled trace holds 7,404 VDP1 and 713,062 VDP2 writes.
The clinching argument is simpler: the animation plays with **no disc**, out of a
512 KB ROM. Several seconds of 320×224 video does not fit.

### How to read this capture — it is novelty-filtered

**A frame's address list is the instructions never executed before that point,
not what ran during that frame.** The capture was built to record only frames
that introduce new code; everything already seen is skipped. Two consequences,
both of which have already produced a wrong inference here:

- A gap in the frame numbers means "no new code", not "nothing happened".
- Consecutive captured frames are not adjacent in time, and their contents are
  not a control-flow sequence.

Worked example of the trap: frames 713 and 720 look like a BIOS→game handoff
(713 is the last BIOS-era frame, 720 the first game frame). Their addresses
disassemble to

```
0600252C..06002536:  SHAR R0  (x6)      <- frame 713
06002538:            RTS
06002542:            SHAR R0            <- frame 720
06002544:            SHLR16 R0
06002546:            SHLR8 R0
06002548:            RTS
```

— a shift-helper table, entered at an offset to perform N shifts. Those frames
record the first time each *entry point* was used. They mark nothing about a
handoff.

### What the three trace stores actually contain

| store | size | usable for |
|---|---|---|
| `traces/frames/frameN.txt` + `.png` + `_study.md` | 6 MB | the only place with real frame numbering; novelty lists plus captured images |
| `traces/portal_sessions/session_0000.jsonl` | 18 GB | 141 M `call`/`return`/`mem_write` events with full register state — **every event is `"frame":0`**; the project's own summary records "0 frame markers" |
| `traces/portal_spool/spool_398074.jsonl` | 12 GB | the newer live capture; same shape **plus `"core":"M"`**, so Master and Slave are distinguishable. Also all `frame:0` |

So sequencing has to come from event order in the JSONL, never from frame
numbers, and cross-referencing an image to an event range is not currently
possible.

**So video is not optional for this milestone.** An earlier draft of this file
said video was not blocking; that was true only of the *current* wall and was
written in a way that read as general. The BIOS's own boot sequence is VDP1
polygon work, and it is upstream of everything the CD Player screen needs.

Two corrections this data forces on existing docs:

- `cs2-cdblock.md` labels `0x060131A8` as a "CD-header polling loop" (from
  `history.md` Chapter 12). In the real capture, `0x060131xx` is executing at
  frame 264 — the moment the first polygons appear. That label is probably wrong.
- The portal project's `tools/function_catalog.json` describes `0x06001692` as
  game code. It is BIOS code: its own study marks the whole range
  "fora do alcance de 0.BIN", i.e. not in the game executable.

## Measuring progress properly — `tools/bios_progress.py`

The settle PC cannot say how far a boot got (it names the hottest sampled
instruction, which here is a delay loop). This can, and it replaces the single
target PC that this file previously asked someone to go and observe.

Because the reference capture is novelty-filtered, **the union of frames 0..713
is the complete instruction-address set of a successful BIOS boot** — 9,882
addresses across 64 novelty frames, baked into
`tools/bios_reference/bios_boot_frames.json`. Progress is then a set
intersection, and the first missing address names one instruction to go and look
at:

```bash
MIMAS_PC_TRACE=/tmp/pcs.txt ./target/release/saturn-frontend-native --bios <bios>
python3 tools/bios_progress.py /tmp/pcs.txt
```

`MIMAS_PC_TRACE` follows the `MIMAS_BUS_TRACE` precedent: one `Relaxed` load per
instruction when off, a `Relaxed` `fetch_or` into a 128 KB bitmap when on. The
hook sits **before** `step()`, so it records the instruction about to run — put
it after (next to `pc_reporter`, which correctly wants the *resulting* PC) and
it silently drops the first instruction of the run.

### Result, no disc

```
Mimas executed 1052 distinct PCs, 1051 of them in the reference (10.6%)
Fully reached: 5 of 64 frames (last complete: frame 42)
First gap: frame 180 — 6 of 287 addresses never executed
     0x000033D8 0x000033DA 0x000033DC 0x000033E0 0x000033E2 0x000033E4
```

**1051 of 1052 of our PCs are in the reference** — we are on the real path, not
executing garbage. And the divergence disassembles to an explanation rather than
a bug:

```
0x33D0: CMP/EQ #6,R0  / BT -> exit     ; 6  = CDB_STAT_OPEN
0x33D4: CMP/EQ #7,R0  / BT -> exit     ; 7  = CDB_STAT_NODISC
0x33D8: CMP/EQ #10,R0 / BT -> 0x33B8   ; 10 = CDB_STAT_FATAL   <- never reached
0x33DC: BSR 0x32DC                     ; the "disc present" path
```

Those three constants are exactly `cs2.rs`'s `CDB_STAT_OPEN` / `CDB_STAT_NODISC`
/ `CDB_STAT_FATAL`. This is the BIOS's CD-status dispatch, we answer NODISC, and
we take the early exit — **correctly**, because the smoke run passes `--bios`
with no `--chd` while the reference capture has Magic Knight Rayearth inserted.

So the reference is only comparable past frame 180 when booting **the same
disc**. `chdman` is installed and the images are in the portal project
(`extraction/mkr.cue` + `mkr.bin`), so this is a conversion away, not a blocker.

### Current divergence

With the disc inserted and no early stop, the first reference frame not fully
reached is 214, missing **2 of its 422** addresses:

```
0x06000846
0x06000864
```

Both read as zero in the BIOS ROM, so this region is **not** part of the 1:1
ROM→WRAM copy that holds elsewhere: they are slots the BIOS fills at runtime.
Just below them sits a pointer table (`0x06000646`, `0x06000678`, `0x0600083C`
×2, `0x06000D00`), which is the shape of an installed handler vector. The
reference reaches them and Mimas does not, so the lead is an interrupt or system
call that fires on real hardware and not here — not a missing opcode.

### What replaces `MIMAS_GATE_TARGET_PC`

Reaching "the end of the BIOS" is now expressible without observing a magic
address: `bios_progress.py --require-frame 713`. The gate variable stays for the
no-disc CD-Player milestone, which this capture cannot supply.

## Roadmap items

### Tier 1 — blocks the BIOS from *running*

1. **Determine what `0x06001694` is.** Halt or wait. Everything else on this list
   is ordered on the assumption that it is a wait on the sound driver; if it is a
   halt for another reason, this list is wrong. Cheapest item here by a wide
   margin.
2. **`m68k.rs`: the derailed jump**, and the `0xFFFC`/`0xFF00` opcodes behind it.
   Not on any plan's phase list — `sh2-cpu.md` covers the SH-2, not the 68000.
   This is the one confirmed wall.
3. **Re-verify `cs2-cdblock.md`'s 230/230.** The handshake demonstrably works;
   what is unverified is whether the commands the CD Player screen issues are
   really complete, given the project's track record with `[x]`.

### Tier 2 — needed to *see* the result

Nothing here blocks execution; they decide whether the screen is correct.

4. **`vdp2.md` Phase 4 — finish it.** Five items are marked `[x]` and are not
   implemented (`SFPRMD`, the `SFSEL`/`SFCODE` helper, `SFCCMD` modes 1 and 2,
   shadows). See `docs/current_review.md`.
5. **`vdp2.md` Phase 6 — Windows.** `WPEX0` is written non-zero, so this is load
   bearing, not optional.
6. **`vdp2.md` Phase 7 — sprite layer read-out.** The VDP1↔VDP2 boundary. Without
   it nothing VDP1 draws ever composites, and the CD Player screen has sprites.
7. **`vdp2.md` Phase 5 — scroll, zoom, line scroll, mosaic.** Absent from the
   trace only because the boot stops early; assume it is needed.

### Tier 3 — not needed for this

- **`vdp2.md` Phase 8 (RBG0/RBG1 rotation)** — the BIOS writes no rotation
  registers.
- **`vdp2.md` Phase 9 (VRAM access cycle patterns)** — the BIOS *programs* them
  (`CYCA0L`…`CYCB1U` above), but they govern bandwidth, not what is drawn. The
  plan itself frames this phase as conditional on wanting a render cache.
- **`vdp1.md` Phase 9 + `vdp2.md` Phase 10 (thread topology)** — architecture, not
  output correctness. They belong last in Milestone 5 regardless.
- **All of Milestone 6 (SCSP)** — audio. Note this does *not* excuse item 2: the
  M68K has to survive, whether or not the SCSP makes a sound.

---

## The gate step

Step 9 of `tools/quality_gate.sh` now has a second PC check:

| variable | meaning | today |
|---|---|---|
| `MIMAS_GATE_PC` | the current wall — a *regression* guard | `0x06001694` |
| `MIMAS_GATE_TARGET_PC` | the unlock target — a *milestone* guard | **undetermined** |

The target check is red and will stay red until the BIOS actually reaches an
interactive screen. That is deliberate, and it is the same discipline as
`rule_spawned_threads_park`: a known gap that has been silenced is just an
unknown gap.

It is red in two distinguishable ways, and the message says which:

- **`MIMAS_GATE_TARGET_PC` unset** — nobody has determined the address yet. This
  is Tier 1 item 1. Setting it to a number nobody has observed would be the exact
  failure mode `MIMAS_GATE_PC`'s own "any change is looser" rule exists to
  prevent, so the gate asks for it rather than inventing one.
- **set, but the boot settles somewhere else** — the emulator is not there yet,
  and the message prints both PCs.

### First task, and how to do it

Determine `0x06001694`. `CLAUDE.md`'s own recipe, no new tooling:

1. Gate a probe in `Sh2::execute()` on a `static AtomicBool` so it fires once
   when `self.pc` enters `0x06001690..=0x060016A0`.
2. `std::fs::write` a slice of `work_ram.high_ram` around it.
3. `python3 tools/sh2dis.py <dump.bin> 0x06001600`.
4. Remove the probe — it is a diagnostic, not instrumentation.

Expect literal pools to decode as garbage; real code resumes after the `BRA`/
`RTS` past them. If the loop reads a fixed address, find who writes it with a
surgical `eprintln!` in the matching `MemRegion` write arm.

Record the answer here, replacing this section.
