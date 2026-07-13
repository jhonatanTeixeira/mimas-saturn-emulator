# Tech debt: closing the gap between the current architecture and the real design

This is an actionable plan, not a retrospective — see `history.md`
Chapter 6 for how these conclusions were reached, and
`docs/honest_architecture_review.md` for the original cost analysis that
started the conversation. Update this file as pieces land: move an item
from "Planned" to "Done" with a short note on what changed and why, the
same discipline `CLAUDE.md` already asks for with `current_bugs.md` and
`current_blocker.md`.

None of this blocks the current BIOS boot investigation
(`.development/current_blocker.md`). It's real, deferred architectural
work — tackle it deliberately, not as a detour from whatever wall is
currently active.

## Progress

- **2026-07-12**: Suggested-order-of-attack steps 1 and 2 are done: Core 1
  (Slave SH-2) and Core 2 (SCU DSP stand-in) no longer spin (see item 2,
  "Known current offenders"), and the SNDON→M68K-reset debounce is replaced
  with a real `Ordering::Release`/`Acquire` signal on the existing
  `m68k_control` flag (see item 1's "Target"). Confirmed *not* to move the
  M68K driver self-corruption wall (`.development/current_blocker.md`) — a
  separate, already-tracked bug. See `history.md` Chapter 7 for the full
  write-up, including two design-review catches that changed the shipped
  approach from what was first proposed. Steps 3 (`WorkRam` split) and 4
  (CPU clock throttle) remain planned, deliberately deferred to a future
  session.
- **2026-07-13**: Suggested-order-of-attack step 3 is done: `WorkRam`'s one
  monolithic `RwLock` is split into a separate `RwLock` per field, all 14
  in one pass (not staged, unlike this section's original migration notes —
  see below for why). Confirmed *not* to move the M68K wall either (same
  real-BIOS re-verification as the step-2 entry above). See `history.md`
  Chapter 8. Step 4 (CPU clock throttle) remains planned.

## The five principles

1. **`Condvar`-backed signaling for cross-component events, not polled
   flags or timing guesses.** A component that's genuinely waiting for
   another component's action (not doing continuous work of its own)
   should block at zero CPU and wake at the exact instant of the real
   signal — never poll an `AtomicBool` in a loop, and never guess a
   wall-clock delay to paper over an ordering race.
2. **Event-driven thread/task orchestration, not a hardcoded four-thread
   mapping.** One schedulable unit per distinct hardware component, not
   per "however many cores the target device happens to have." Let the
   OS scheduler place and migrate them; don't hand-simulate that decision
   by bundling unrelated components into the same thread to hit a target
   core count.
3. **The game drives the hardware, not the implementation.** Every
   "commands complete instantly" or similarly timed simplification must
   be paired with the *real* signal the actual hardware would raise on
   completion (an interrupt, a status-register bit the emulated code
   itself checks) — never a wall-clock guess bolted on from outside the
   emulated instruction stream.
4. **Real CPU clock throttling**, so timing-sensitive code (delay loops,
   not just hardware-timer-driven code) behaves correctly, with the same
   throttle mechanism doubling as a user-facing speed control.
5. **Zero polling loops**, except the two categories that are exempt by
   nature: a CPU core's own instruction-execution loop (that *is* the
   emulated work, not a wait for anything), and a deterministic
   wall-clock pacer comparing against a computed target (frame timing,
   the CPU throttle above) — which is fundamentally different from
   polling for an uncertain external event.

---

## 1. `Condvar`-backed shared buffers

### Current state

**The monolithic-lock part is done (2026-07-13, see Progress above,
`history.md` Chapter 8)** — `WorkRam` now has a separate `RwLock` per
field instead of one `Arc<RwLock<WorkRam>>` covering everything. What
follows describes the *old* state for context.

`Arc<RwLock<WorkRam>>` used to be one monolithic lock covering every memory
region (Low/High Work RAM, Sound RAM, SCSP/VDP1/VDP2/SCU registers,
backup RAM). Every byte-level access from any core took this same lock,
regardless of region — a VDP2 CRAM write and an SH-2 Work RAM read
contended on an identical lock despite having nothing to do with each
other. Cross-component *events* (as opposed to raw memory visibility) are
modeled as bare `Arc<AtomicBool>` flags (`m68k_control`, `sound_req_irq`,
`smpc_irq_pending`), checked by polling once per loop iteration on the
observing side — functionally correct for the interrupt-flag cases (SH-2
already visits `service_pending_interrupt` every instruction regardless),
but the one place this pattern broke down was the SNDON→M68K-reset
handshake, patched with a 2ms wall-clock debounce instead of a real
signal.

### Target

- Split `WorkRam` into per-region locks (or, better, single-owner regions
  exposed to other components only through explicit, infrequent handoffs
  — the pattern `vdp2_frame`'s `ArcSwap<Framebuffer>` already uses
  correctly: the renderer publishes, the frontend reads, neither ever
  blocks the other). Only regions that are genuinely dual-ported on real
  hardware (Sound RAM, SCSP registers) need cross-component shared access
  at all; most of Saturn's memory is exclusive to one component on real
  hardware and shouldn't share a lock with anything else in this
  implementation either. **Done** (2026-07-13, see Progress above) —
  shipped as one `RwLock` per field (all 14), not grouped by hardware
  component and not the "single-owner + handoff" alternative: no call site
  held two *different* fields under one guard, so grouping would have
  saved nothing, and nothing yet consumes 11 of the 14 fields at all (no
  handoff protocol to design against). Two call sites needed more than a
  mechanical substitution — `M68k::write_byte`'s `scsp_regs` branch (must
  hold one lock across the register store *and* the immediately-following
  MCIEB/MCIPD interrupt check) and `vdp::render_backdrop` (must hold one
  `vdp2_regs` lock across both its TVMD and BKTAL reads, since it no
  longer rides on a caller-held whole-`WorkRam` lock) — both preserved
  exactly. See `history.md` Chapter 8.

  **New follow-up surfaced by this split, not fixed here**: `TAS.B @Rn`
  (`sh2.rs`, the SH-2 opcode real dual-CPU spinlock code over shared Low/
  High Work RAM would use) does a read-then-write with a real gap between
  the two lock acquisitions — no atomic test-and-set. Dormant today only
  because Core 1 never runs; removing the old monolithic lock's incidental
  over-serialization between cores makes this gap *more* likely to
  actually race the moment SSHON activates Core 1, not equally dormant.
  Flagged loudly at the `TAS.B` match arm itself — needs a real CAS-style
  fix (or one write-lock spanning both operations) before or alongside
  SSHON, see item 2's Core 1 entry above.
- Replace the SNDON debounce with a real signal: the SH-2's own driver-
  upload routine sets an explicit "upload complete" indicator as its last
  write (in the shared region itself, or via a dedicated `Condvar`+
  `Mutex<bool>` pair), and Core 3 waits on that — no delay, no guess,
  correct regardless of host speed or system load. **Done** (2026-07-12,
  see Progress above) — shipped as `Ordering::Release`/`Acquire` on the
  existing `m68k_control` `AtomicBool` rather than a new `Condvar`+
  `Mutex<bool>` type: Core 3 never genuinely blocks on this signal (it has
  real work every loop iteration regardless), so a `Condvar` would go
  unused — `docs/final_architecture_draft.md`'s own reference shape
  (`BusArbiter::is_locked()`) is exactly this bare-Acquire-load pattern for
  the same reason. See `history.md` Chapter 7.
- For any future component-to-component event of the same shape
  (something on one thread needs to know precisely when another thread's
  write is complete before acting), use `Condvar`+`Mutex<T>` or a bounded
  channel (`std::sync::mpsc` or `crossbeam::channel`) — not a bare
  `AtomicBool` polled on a timer, and not sockets (see `history.md`
  Chapter 6 for why sockets lose to in-process channels here: every
  socket `read`/`write` is a syscall + kernel buffer copy on *every*
  message, even `AF_UNIX`; an in-process channel's fast path stays in
  userspace and only pays a `futex` syscall when a receiver is genuinely
  parked).

### Migration notes

This can happen incrementally, region by region — it doesn't require a
single big-bang rewrite. Natural order: split off the regions that are
*not* genuinely cross-component first (Low/High Work RAM, VDP1/VDP2 VRAM,
CRAM — SH-2-exclusive or renderer-exclusive on real hardware) since
that's the lowest-risk, highest-contention-reduction change; tackle the
genuinely-shared regions (Sound RAM, SCSP registers) alongside whatever
`Condvar`/channel work is needed for their associated events, since
they're coupled anyway (the MCIPD/MCIEB handshake already touches SCSP
registers *and* needs a real signal).

**Shipped as one pass instead of staged (2026-07-13)**: by the time this
landed, the prerequisite this note was guarding against — real signals for
`sound_ram`/`scsp_regs`'s associated events — was already done (item 1's
SNDON fix above; `sound_req_irq` was already a legitimate bare flag, never
broken). With that dependency already satisfied, splitting all 14 fields
uniformly was simpler to reason about than carrying two different access
patterns (some fields locked, some still behind the old monolithic lock)
through a multi-session migration. No call site anywhere held two
*different* fields under one guard (verified against every access site
before starting), so there was no hidden cost to doing it all at once.

## 2. Event-driven, not fixed-four, thread orchestration

### Current state

`SaturnSystem::start()` hardcodes exactly four `thread::spawn` calls,
each bundling multiple, unrelated hardware components together
specifically to land on "four" (matching the R36S's four cores):
Core 2 bundles SCU + SMPC + CS2; Core 3 bundles VDP1 + VDP2 + SCSP + the
M68K core (as of this session). This conflates "how many hardware
components Saturn has" with "how many OS threads/cores happen to be
available on one specific target device," and it's already visibly
straining — Core 3 now does frame pacing *and* M68K stepping *and*
SNDON-edge detection in one loop body, none of which are naturally the
same responsibility.

Two components used to spin with nothing real to do, burning a full core
each for no useful work — found and named directly during a prior session's
conversation, and **fixed as of 2026-07-12** (see Progress above,
`history.md` Chapter 7): both now call `LockStepSync::park_while_inactive`
instead, at zero CPU, excluded from drift tracking exactly like a
DMA-blocked core:

- **Core 1 (Slave SH-2)**: previously never received a real reset/BIOS
  load (matches real hardware — it stays halted until the master issues
  SMPC's SSHON) but ran `run_loop()` immediately anyway, executing whatever
  garbage sat at address 0 continuously. Now parks; reactivating it (once
  SSHON is implemented) via `set_thread_active(1, true)` runs the
  already-wired `set_bios_arc`/`reset()`/`run_loop()` sequence.
- **Core 2's SCU/DSP portion**: previously
  `while ... { cycles += 2; sync_core(); yield_now(); }` forever, doing
  nothing (the "SCU + SMPC + CS2" bundling this section's "Current state"
  describes above was aspirational even before this fix — SMPC command
  processing has always run inline inside `Sh2`, and CS2/CD-ROM was never
  wired to any thread; only the DSP slot itself was spinning). Now parks;
  a future real DSP implementation reactivates it the same way.

### Target

One thread (or a lighter-weight task, if a decision is made to move off
raw OS threads for the leaf components — see the open question below) per
distinct hardware component: Master SH-2, Slave SH-2, SCU, SMPC, CS2/
CD-ROM, VDP1, VDP2, SCSP, the M68K core, the SCU DSP. Components with
nothing to do yet (Slave SH-2 before SSHON, SCU DSP before anything
targets it) park on a `Condvar` (or simply aren't spawned until the first
real signal that gives them work) instead of spinning. Let the OS
scheduler (Linux CFS, or whatever the target platform uses) decide actual
core placement and migration — that's what it's built to do, and
hardcoding a 1:1 "component bundle" to "core count" mapping was always
just simulating that decision, worse, by hand.

### Open question, deliberately not decided here

Real OS threads per component vs. a lighter-weight cooperative-scheduling
model (green threads/fibers/an explicit coroutine scheduler, closer to
how higan/bsnes interleave SNES's several chips deterministically on one
OS thread) is a real, undecided fork in the road — see
`docs/honest_architecture_review.md`'s bottom line. Real threads give
the process-boundary-adjacent mental model (still shared address space,
though — full fault isolation would need actual `fork()`-based processes,
a separate, bigger decision with its own tradeoffs laid out in
`history.md` Chapter 6) and are simple to reason about with std's
synchronization primitives; a cooperative scheduler removes OS
context-switch cost entirely for components that hand off control
frequently, at the cost of needing a custom scheduler loop instead of
`std::thread`. Don't resolve this speculatively — decide it once there's
a concrete performance measurement showing it matters (see item 4's
throttle work as a prerequisite for getting a real, comparable
frame-rate/instructions-per-second number to measure against).

## 3. The game drives the hardware, not the implementation

### Current state

Several real simplifications are honestly documented as such (SMPC
commands "complete instantly," matching real completion interrupts —
this part is done right). The SNDON debounce is the one place this
principle was violated: a wall-clock guess substituting for the real
signal the emulated instruction stream should provide.

### Target

Audit every place a fixed delay, a magic sleep, or an out-of-band timing
assumption stands in for a signal that the emulated CPU's own code could
and should provide. As new hardware components get implemented (VDP2 tile
rendering, VDP1 sprites, CD-ROM command protocol), hold this same
standard: instant completion is fine as a simplification only when paired
with the real completion signal (status bit, interrupt) the actual
hardware would raise, checked/handled by the emulated code exactly as
real code would — never a timer our own scheduler invented.

## 4. Real CPU clock throttling

### Current state

Not implemented at all. `LockStepSync` bounds *relative* drift between
cores (in abstract cycle-count terms) but has no wall-clock reference —
the interpreters run as fast as the host CPU allows. Only VBLANK timing
and VDP2 frame publishing are paced against real time
(`Instant::now()`-based, in `lib.rs`).

### Target

Batched wall-clock throttle, generalizing the pattern already used for
VBLANK/frame pacing: run a batch of N emulated cycles unthrottled, then
compare elapsed real time against the ideal target (based on real SH-2/
M68K clock rates) and sleep the difference before the next batch. A
naive per-instruction sleep is not viable — an SH-2 instruction takes
tens of nanoseconds; OS sleep wake-up precision is microseconds at best —
so the batch needs to be large enough (some fraction of a millisecond of
emulated time, tuned empirically) that sleep-precision error is
negligible relative to the batch duration.

Make the target rate configurable (real speed / unthrottled-turbo /
custom multiplier) — this is the same mechanism as the "speed slider"
other emulators expose, so it's worth designing as a first-class,
user-facing setting from the start rather than bolting one on later.

### Why this matters beyond "correctness in general"

Real BIOS/game code sometimes uses raw cycle-count delay loops instead of
hardware timers (more likely in BIOS-level code, which is closer to the
metal and sometimes hand-tuned for a known clock rate) — running those
unthrottled doesn't necessarily break them, but it's a real, unverified
risk for the current BIOS boot investigation and for any future game
compatibility work. Not currently known to be causing the active M68K
wall (`.development/current_blocker.md`), but worth checking if that
investigation stalls again in a way this document's next reader can't
otherwise explain.

## 5. Zero polling, no exceptions except the two legitimate ones

### The rule

Every `while <condition> { ...; thread::yield_now(); }` or bare
`AtomicBool::load()`-in-a-loop pattern in the codebase should be
justified as one of exactly two things, or replaced:

- **The CPU's own instruction-execution loop.** This isn't "waiting" for
  anything — executing the next instruction *is* the emulated work, by
  definition, for as long as that core is meant to be running.
  Legitimate, don't touch.
- **A deterministic wall-clock pacer** comparing elapsed real time
  against a *computed* target (frame timing, the CPU throttle from item
  4). Legitimate — this isn't polling for an uncertain external event,
  it's checking a clock against a value you already know.

Everything else — "is some other component's flag set yet," "did the
result show up," "is it my turn" — should block on a `Condvar`, a
channel `recv()`, or (if a decision is later made to move to real OS
processes for some component) a blocking pipe/socket read. Not a sleep
loop, not a bare spin, not a debounce.

### Known current offenders (fix these first — concrete, bounded, no
design decisions required)

- Core 1 (Slave SH-2) spinning through unreset garbage — see item 2.
- Core 2's SCU/DSP portion spinning doing nothing — see item 2.
- The SNDON debounce itself — see item 1.

---

## Suggested order of attack

1. ~~Fix the two known idle-spin offenders (Core 1, Core 2's DSP portion)~~
   — **Done** (2026-07-12, see Progress above) — small, bounded, immediately
   validates the `Condvar`-park pattern this whole plan depends on, without
   touching the higher-risk shared-memory split.
2. ~~Replace the SNDON debounce with a real signal~~ — **Done** (2026-07-12,
   see Progress above) — directly relevant to the active boot investigation,
   and the second concrete validation of the same pattern under real
   contention (two threads, real timing pressure, not a toy case).
3. ~~Split `WorkRam` region by region, starting with the non-cross-
   component regions (see item 1's migration notes).~~ — **Done**
   (2026-07-13, see Progress above) — shipped as one pass covering all 14
   fields rather than staged, see item 1's migration notes for why.
4. Design and implement the CPU clock throttle — do this before any
   serious performance comparison work (item 2's "real threads vs.
   cooperative scheduler" open question), since it's a prerequisite for
   getting real, comparable numbers.
5. Revisit the thread-vs-cooperative-scheduling and thread-vs-process
   open questions with actual measurements in hand, not before.

Keep `cargo test --workspace` green after every step, same as every other
change in this project — none of this is exempt from that.
