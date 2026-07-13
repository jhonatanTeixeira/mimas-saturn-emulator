# Honest architecture review

This is a candid technical assessment of Mimas's distributed, multi-threaded
architecture — not a pitch for it. It's written from direct experience
implementing against it this session (including finding and crudely
patching a real synchronization race between two of its threads), not from
reading the design doc and taking it at face value. Where I disagree with
the architecture's premises, I say so; where it's genuinely earned its
keep, I say that too.

> **Update, same session, after this review prompted a deeper design
> conversation:** the fixes this document proposes below are no longer
> just proposals — they're a committed, ordered plan in `TECH_DEBT.md`,
> and the target shape they add up to is written out concretely in
> `docs/final_architecture_draft.md`. That conversation also specifically
> tested and settled the "real OS processes instead of threads" question
> this review doesn't get into below (it was a live consideration from the
> project's original planning notes, `docs/initial_architecture_idea.md`):
> processes don't avoid context-switch cost the way that motivated wanting
> them (threads are actually the *cheaper* of the two on Linux, since
> switching between processes also swaps the address space/TLB, which
> threads sharing one address space never pay), and cache-coherency
> contention — the real cost worth avoiding — is a hardware property blind
> to the process/thread distinction, addressed by message-passing
> discipline instead, which works identically well with in-process
> channels. Threads are confirmed as the right call; see `history.md`
> Chapter 6 for the full reasoning and `docs/final_architecture_draft.md`
> for where that leaves the design. The performance-payoff question this
> review raises below is still open and still needs real measurement —
> settling "threads vs. processes" didn't settle "this multi-threaded
> design vs. a single-threaded cooperative one," which remains
> deliberately deferred, not resolved.

## What the architecture actually is

One OS thread per hardware "block" — Master SH-2, Slave SH-2, SCU/SMPC/
CD-ROM, VDP1/VDP2/SCSP (which as of this session also owns the SCSP's
M68000 core) — coordinated by two primitives:

- **`BusArbiter`**: a single global `AtomicBool` + `Condvar` modeling "is a
  DMA transfer holding the bus." Cheap in the common case (an uncontended
  atomic load), blocking (mutex + condvar wait) only while a DMA is
  actually in flight. This part is well-designed for what it does.
- **`LockStepSync`**: bounded-slack lockstep. Every core calls
  `sync_core(core_id, current_cycles)` — measured directly in this
  session's work, this happens **once per emulated CPU instruction**, from
  `Sh2::run_loop`. `sync_core` unconditionally acquires a single
  `Mutex<SyncState>` shared by all threads, does a linear scan over every
  other active thread's cycle counter to find the minimum, and only
  escapes into a `Condvar::wait` if this thread has drifted more than
  `slack_limit` cycles ahead. In the non-blocking (common) case it still
  pays the mutex lock + O(threads) scan on every single instruction.

Memory itself is a single `Arc<RwLock<WorkRam>>` — one monolithic struct
holding every region (Low/High Work RAM, Sound RAM, SCSP/VDP1/VDP2/SCU
registers, backup RAM, everything). Every byte-level read or write from
any core takes a read or write lock on this same `RwLock`, regardless of
which region it targets — a VDP2 CRAM write and an SH-2 Work RAM read
contend on the identical lock even though nothing about them is actually
related.

## The real challenges, from direct experience

**1. This is a genuine distributed-systems synchronization problem, not
just "four CPUs."** Real Saturn hardware doesn't have this problem the way
this architecture does — on real silicon, "synchronization" between the
SH-2s and the SCSP is just... the bus protocol; there's no analogue of "an
OS thread observes a flag a few milliseconds late." This session hit that
gap directly: SMPC's `SNDON` command resets the SCSP's M68000, and on real
hardware that reset genuinely only happens once the SH-2's own driver
upload — which is just the *next few instructions* in the same linear
program — has run. In this architecture, Core 0 (SH-2) and Core 3 (which
owns the M68K) are independent OS threads with no barrier between them
beyond one shared `AtomicBool`. Core 3 could plausibly observe the flag
and reset the M68K before Core 0's very next instructions (writing the
rest of the driver) have actually executed. The fix applied this session
— a 2ms wall-clock debounce before Core 3 acts on the flag — is a real
patch that works empirically, but it's not a *correct* fix: it's a timing
assumption that could break under system load, on slower hardware, or
under a different scheduler, and it adds a fixed 2ms of latency to every
SNDON regardless of whether it was ever needed. This class of bug is
structural to the architecture, not a one-off implementation slip — any
two cores that share a memory region and need ordered visibility of each
other's writes will have this problem, and Saturn's real hardware has
*many* such shared regions (Sound RAM, SCSP registers, VDP1/VDP2 VRAM
accessed by DMA, SCU registers).

**2. Per-instruction global-mutex synchronization is expensive, and it's
paid unconditionally.** `LockStepSync::sync_core` isn't a fast path that
occasionally falls back to a slow path — every one of the 4 threads
acquires the *same* mutex on *every single instruction* it executes, even
when nothing is contended. On a modern desktop x86 core an uncontended
`Mutex::lock()` is maybe 15-25ns, so at, say, 10M SH-2 instructions/second
this is a genuinely measurable tax (hundreds of milliseconds per second of
wall time spent just in synchronization, across 4 threads hammering one
mutex) — and that's the *optimistic* case where the mutex is rarely
contended. Real contention (4 threads genuinely fighting over the same
lock on every instruction boundary) will be markedly worse, especially
because CPU cores don't run at identical instruction throughput, so the
"minimum cycles among active threads" this loop computes will keep some
threads legitimately blocked in `Condvar::wait` — meaning real OS-level
context switches, not just spin — a cost an order of magnitude higher
than the mutex itself.

**3. Fine-grained work sharing one coarse-grained lock is close to a
worst case for multi-threading.** The `RwLock<WorkRam>` is acquired on
every single byte access from every core. Emulated CPU instructions are
memory-access-dominated (every instruction fetch alone is a memory read),
so this isn't an occasional cost — it's paid continuously, by design, on
the hottest path in the whole system. Splitting `WorkRam` into per-region
locks (or better, single-owner regions with message passing, the pattern
already used correctly for `vdp2_frame`'s `ArcSwap`) would remove a lot of
*false* contention — a VDP2 register write and an SH-2 Work RAM access
have nothing to do with each other but currently fight over the same lock.

**4. The four-thread mapping itself hides idle cores burning full CPU for
zero work.** Found directly while working through the design conversation
that produced `TECH_DEBT.md`: Core 1 (the Slave SH-2) starts executing
immediately even though it never receives a real reset/BIOS load (matches
real hardware — it's supposed to stay halted until the master issues
SMPC's SSHON — but the thread runs `run_loop()` regardless, decoding
whatever garbage sits at address 0, forever), and Core 2's SCU/DSP portion
is a bare `cycles += 2; sync_core(); yield_now();` loop that has never had
anything real to do. Both are consequences of the same root cause as
challenges 1-3: components got bundled into a fixed thread count instead
of being spun up (or parked) based on whether they have real work, which
this review's "optimizable" section below now addresses directly rather
than leaving implicit.

## Is the architecture optimizable?

Partially, and the fixes are identifiable, not speculative — as of this
update, they're written up as an ordered, actionable plan in
`TECH_DEBT.md` rather than just proposed here in the abstract:

- **Split the monolithic `RwLock<WorkRam>`** into per-region locks, or
  (better, since most regions really are owned by exactly one core on real
  hardware) single-owner regions exposed to other cores only through an
  explicit, infrequent channel — the same pattern `vdp2_frame`'s
  `ArcSwap<Framebuffer>` already uses successfully: the renderer publishes,
  the frontend reads, and neither ever blocks the other.
- **Only arbitrate memory that's genuinely shared on real hardware.** Most
  of Saturn's memory *isn't* actually contended between chips — Low/High
  Work RAM is SH-2-exclusive, VDP1/VDP2 VRAM is effectively
  renderer-exclusive outside of DMA windows. `BusArbiter`'s single global
  lock is architecturally *correct* to be a bottleneck for the memory that
  really is bus-arbitrated on real hardware (that's what the real bus
  does too), but routing every access through the same contention point
  regardless of region isn't required by the hardware being modeled.
- **Replace the SNDON debounce with a real logical handshake** instead of
  a wall-clock guess — e.g., have the SH-2's own upload routine set an
  explicit "driver ready" flag as its last write, and gate the M68K reset
  on *that* rather than on SNDON plus an arbitrary delay. This is strictly
  better: correct regardless of system speed, and removes a fixed latency
  tax that's currently paid even when it isn't needed.
- **Widen the lockstep sync granularity.** Right now `sync_core` runs every
  instruction; real hardware only actually cares about relative ordering
  at much coarser points — DMA completion, VBLANK, explicit
  interrupt/mailbox handoffs. Syncing per-instruction buys cycle-accurate
  drift bounds that this project doesn't currently seem to depend on
  anywhere (nothing found so far needs sub-instruction timing precision
  between cores) at a cost paid on literally every step. A hybrid — free
  running with periodic barriers at real hardware synchronization events —
  would cut the per-instruction tax dramatically while still bounding
  drift at the points that actually matter.
- **Decompose by real hardware component, not by target core count, and
  park idle ones instead of spinning them.** Directly addresses challenge
  4 above: one thread per distinct component (Master/Slave SH-2, SCU,
  SMPC, CS2, VDP1+VDP2, SCSP, M68K — nine, not four), each parked on a
  `Condvar` with zero CPU cost when it genuinely has nothing to do (Slave
  SH-2 before SSHON, the SCU DSP before anything targets it) instead of
  spinning a `yield_now()` loop. Let the OS scheduler place and migrate
  across whatever cores actually exist — that's a better job than a
  hand-written four-way mapping was ever going to do. Written out in full
  in `docs/final_architecture_draft.md`.

None of this is free to build — it's real engineering work layered on top
of an already very large task (correctly implementing every piece of
Saturn hardware). But it's tractable, incremental, and doesn't require
abandoning the architecture's core idea — and it's no longer just this
review's opinion about what *could* be done; `TECH_DEBT.md` has it as a
concrete, ordered work plan.

## Will it pay off, performance-wise?

Depends heavily on the target, and I think the honest answer differs
sharply between the two audiences this project has already named for
itself (the README explicitly calls out devices "like the R36S").

**On a modern desktop CPU**: almost certainly yes, in the sense that
matters practically — "full speed" emulation. Saturn's real chips ran at
20-28MHz; a modern desktop core is 100-200x faster per core, so even
heavy synchronization overhead eating a large fraction of that headroom
still leaves an enormous margin. The architecture's inefficiency probably
won't be *visible* on desktop hardware regardless of how this review reads.

**On the stated constrained target (R36S: quad-core ARM Cortex-A53 @
1.5GHz)**: this is a real, open risk, not a settled win, and there's
concrete, first-party evidence for that risk sitting in this very repo.
The R36S optimization history for Yabause/YabaSanshiro (a *different*
Saturn emulator in this same monorepo) documents its own investigation
into async SCSP/M68K threading on this exact hardware, and found genuine,
hard-to-resolve synchronization races (register-write races between the
SH-2 and SCSP/M68K threads, and a deeper one in DSP delay-line memory
access) as a direct consequence of putting the sound chip on its own
thread with shared memory. That's not a hypothetical concern raised by
this document — it's a documented outcome of trying almost exactly this
pattern, on exactly this target hardware, in this repo. Cortex-A53 cores
are in-order, have modest cache-coherency throughput compared to desktop
x86, and OS thread wake/context-switch latency doesn't shrink just
because the CPU is weaker — if anything it's relatively larger against a
weaker core's instruction throughput. A per-instruction global mutex plus
a monolithic memory lock is exactly the kind of design that tends to
underperform on this class of hardware relative to a single-threaded
interleaved interpreter with none of that synchronization cost at all.

I'd treat "does this architecture actually run faster than a
single-threaded interleaved interpreter, on the real target hardware" as
an open empirical question worth measuring directly and early — not an
assumption to build years of further work on top of.

## How this compares to modern emulator architectures

- **higan/bsnes** (byuu/near) — widely regarded as the reference for
  accurate multi-chip system emulation (SNES has 5+ heterogeneous chips:
  65816, SPC700, a DSP, the PPU, coprocessors). Deliberately
  **single-threaded**, using cooperative fibers/coroutines to interleave
  chips with explicit, deterministic cycle-budget handoffs. This sidesteps
  OS thread synchronization cost and non-determinism entirely, and is
  specifically *why* it's able to claim cycle-exact accuracy — determinism
  is much easier to reason about and test when there's no real concurrency
  to race against.
- **Mednafen** and most other high-accuracy multi-system emulators follow
  the same single-threaded-interleaved pattern for the same reasons.
- **Yabause itself** (the reference used throughout this project) supports
  an optional async SCSP thread, off by default in many configurations —
  and this repo's own R36S optimization work found real bugs in exactly
  that mode. This is close to a natural experiment already run on the
  Mimas's own stated target platform, with a documented negative result.
- **RPCS3 / Xenia / Dolphin / PCSX2** genuinely do use multi-threading
  aggressively, but the comparison doesn't transfer cleanly to Mimas's
  situation: (a) their target systems have chips that are *actually*
  loosely coupled on real hardware (PS3's SPUs communicate via infrequent
  DMA/mailbox operations, not shared memory on every access; GPU work is
  driven by command buffers, not per-cycle CPU/GPU memory sharing), so
  their threading model matches the real hardware's actual coupling
  granularity; and (b) they're JIT/recompiler-based, so raw CPU emulation
  throughput is high enough that synchronization overhead is a much
  smaller fraction of total time than it is for a pure interpreter like
  Mimas's current SH-2/M68K cores. Mimas is closer, structurally, to
  higan/bsnes's problem (tightly-coupled heterogeneous chips sharing
  memory at fine granularity) while using RPCS3-style threading rather
  than higan-style cooperative interleaving — and the R36S-targeted async
  SCSP experience already in this repo is evidence that combination is
  where the real trouble shows up.

## Bottom line

The architecture has already paid for itself once in a way worth naming
honestly: it *found* a real synchronization bug (the SNDON/driver-upload
race) that a single-threaded interleaved model would never have surfaced,
because single-threaded interleaving is atomic between "chips" by
construction — it can silently paper over exactly the kind of ordering
assumption real hardware doesn't actually guarantee either, if a game or
BIOS's timing assumptions were ever fragile. That's a genuine correctness
argument in the architecture's favor, not just a cost.

But the performance case is unproven, and there's real, specific,
in-repo evidence pointing the other way for the constrained target this
project has named for itself. Before investing further in this direction,
I'd want a direct empirical answer to: does the per-component lockstep
design actually outperform a single-threaded, coroutine-interleaved
interpreter running the *same* CPU/hardware logic, on the *actual* target
hardware? If the answer turns out to be "no, and by a lot," the honest
move is either (a) reduce synchronization granularity substantially (per
the optimizable section above) until the answer flips, or (b) keep the
current multi-threaded model for desktop builds where it doesn't matter,
and offer a single-threaded cooperative-scheduling backend sharing the
same core emulation logic for the constrained target — the CPU/hardware
implementations underneath don't need to change either way, only how
they're driven.

As of this update, that measurement is still the missing piece, not an
afterthought — `docs/final_architecture_draft.md` explicitly defers the
threads-vs-cooperative-scheduling decision until the CPU clock throttle
work (also planned, not yet built) gives a real, comparable instructions-
per-second number to measure against, rather than deciding it on
intuition either way. The parts of this review that *were* resolved this
session (threads over processes, specifically) were resolved by tracing
through the actual OS mechanics, not by assumption — the same standard
the remaining open question should be held to before it's answered.
