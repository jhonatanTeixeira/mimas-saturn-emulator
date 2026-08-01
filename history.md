# Mimas — development history

Mimas is a from-scratch Sega Saturn emulator in Rust, built with a
distributed multi-threaded architecture that mirrors real Saturn hardware
(separate threads for Master SH-2, Slave SH-2, SCU/SMPC/CD-ROM, and
VDP1/VDP2/SCSP, synchronized via a custom `BusArbiter` + `LockStepSync`
pair). This document reconstructs, in chronological order, how the project
got to its current state, so any agent picking it up next has the real
story instead of just the code.

See `docs/PROJECT.md` and `docs/saturn_architecture_report.md` for the
architecture itself; `.development/ROADMAP.md` for what's done and what's
next. This file is about *how* we got here and *why* specific decisions
were made — the parts that don't survive in the code alone.

---

## Chapter 0 — Origins: a multi-agent swarm scaffold (abandoned)

Mimas began as an experiment in having a swarm of orchestrated agents
(explorer/worker/auditor/challenger/reviewer roles, coordinated by an
`orchestrator`) build the emulator's initial scaffold: `BusArbiter`,
`LockStepSync`, the four-thread `SaturnSystem` shape, and a stub SH-2 core.
That swarm stalled before implementing real CPU behavior — the SH-2 it left
behind decoded almost nothing (a handful of opcodes, no real memory map,
no interrupts). The coordination artifacts from that phase lived in a
repo-root `.agents/` directory (removed once this history was written —
they were pure orchestration scaffolding: `BRIEFING.md`/`handoff.md`/
`progress.md` per agent role, no unique source or documentation).

The architecture the swarm proposed (distributed threads, lockstep sync)
was judged sound and kept; the actual CPU/hardware behavior was rebuilt
from scratch afterward, opcode by opcode, cross-checked against a real
working emulator rather than invented.

## Chapter 1 — Picking it back up: "I want to see the Saturn logo on screen"

Work resumed with an explicit, narrow goal: get a real Saturn BIOS to boot
far enough, in this architecture, that its splash screen would render in
an actual window — no CD-ROM required (BIOS-only boot), no shortcuts on
CPU correctness. The standing instruction that shaped everything since:
at every wall, consult a real working emulator (this repo carries two
reference checkouts — `../yabause/`, the devMiyax/YabaSanshiro fork used
elsewhere in this repo for the R36S Saturn core, and `../yabauseut/`, a
plain upstream Yabause) for the *exact* semantics of whatever's blocking
progress, translate that into Mimas's own architecture (never transliterate
Yabause's C directly), and always regression-test against real, independently
verified values. This loop is written down in `CLAUDE.md` and has reliably
unblocked one real wall after another since.

## Chapter 2 — A real SH-2 interpreter

The stub SH-2 was rewritten into a real interpreter: nibble-based opcode
decode, a real physical memory map (BIOS ROM, Low/High Work RAM, SMPC,
Backup RAM, Sound RAM, SCSP registers, VDP1/VDP2 VRAM and registers, SCU
registers — all cross-checked against Yabause's `memory.c` fill table, not
guessed), delay-slot semantics for every branch/call instruction, real
interrupt exception entry (push SR then PC, VBR-relative vector table, SR's
I3-I0 interrupt mask), the DIV0S/DIV0U/DIV1 bit-serial division algorithm
(ported step-by-step against Yabause's exact case analysis — this is easy
to get subtly wrong from the manual alone), and TRAPA.

Two real bugs were found and fixed this way that are worth remembering as
cautionary examples, because both were *self-consistent but wrong* — a test
that only checks internal consistency doesn't catch this class of bug:

- **BT/BF/BT-S/BF-S branch target formula was missing a `+2`.** Caused a
  genuine infinite loop decoding real BIOS bytes. The first regression test
  written for the fix asserted a value computed with the *same* wrong
  formula, so it passed — only checking against the real BIOS's actual
  byte layout caught it.
- **A `DIV1` multi-step test asserted a wrong result.** Independent
  verification (a throwaway Python re-implementation of the exact ported
  algorithm) showed the single-step mechanics were right but the "chain 32
  DIV1s for a full divide" test's expected value was wrong. Replaced with a
  hand-traced single-step test and an explicit code comment flagging that
  the multi-step chaining convention isn't independently verified.

Also found and fixed: missing `LDS.L`/`STC.L`/`LDC.L`/`STS.L`
memory-indirect opcode forms (hit running the real BIOS — a `LDS.L @R15+,PR`
right before `RTS`, silently no-opped, corrupted the return address);
`bus_wait()` being called once per *byte* instead of once per logical
transaction (multiplied bus-arbitration calls 2-4x, causing a real deadlock
under `adversarial_tests.rs`).

## Chapter 3 — SMPC, TVSTAT, and the first real forward progress

Booting the real BIOS against this interpreter surfaced a sequence of real
hardware-handshake walls, each requiring genuine hardware behavior (not
approximation) to clear:

- **SMPC's SF (status flag) defaulted to a nonzero placeholder**, which
  reads as "busy forever" and hangs any real "wait for SMPC" polling loop.
  Fixed to read idle (0) unconditionally — SMPC commands complete
  "instantly" in this simplification, matching how SF then behaves.
- **VDP2's TVSTAT (offset 0x004, the VBLANK/HBLANK status register) was a
  static stored byte, always 0.** Real BIOS code polls it directly
  (independent of the VBLANK-IN interrupt) while synchronizing video
  register writes. Fixed by computing its VBLANK bit live from wall-clock
  frame timing (`Sh2::tvstat_word`) instead of storing it as an ordinary
  register.
- **SMPC's INTBACK command (0x10) was silently discarded** — COMREG writes
  landed nowhere, so OREG never got real data back and the SCU "System
  Manager" interrupt (vector 0x47, level 8) that real INTBACK handshakes
  wait on never fired. Implemented real INTBACK processing (`OREG0-11`
  populated per `SmpcINTBACKStatus`'s real semantics, region defaults to
  Japan matching Yabause's own no-CD fallback) plus the interrupt.

These two fixes (TVSTAT + INTBACK) moved the boot wall for the first time
in a long while — PC left a small idle loop it had been stuck in and
reached meaningfully new BIOS territory.

## Chapter 4 — Discovering the wall is a whole second CPU

The next wall wasn't another opcode or register gap. Tracing (via one-shot
RAM dumps + a throwaway Python SH-2 disassembler, now kept at
`tools/sh2dis.py`) showed the BIOS spinning on a RAM counter that's *never
written* by anything on the SH-2 side. The code immediately before the wait
issues real SMPC `SNDON` (COMREG 0x06) after writing SCSP command bytes
into Sound RAM. On real hardware, `SmpcSNDON()` calls `M68KStart()` — the
SCSP has its own onboard Motorola 68000 CPU, and the BIOS uploads and runs
a real sound driver on it. The counter the wait loop polls is almost
certainly written *by that 68000 driver*, not by anything the SH-2 executes.

This is a real, from-scratch M68000 interpreter (`m68k.rs`), not a copy of
Yabause's Musashi core — the ISA itself is a long-public, thoroughly
documented standard, so opcode semantics were implemented directly from
that, then cross-checked instruction-by-instruction against Musashi
(`yabause/src/musashi/`) wherever a specific encoding or branch-target
formula was in doubt (this caught nothing wrong in `DBcc` or `MOVE` — both
verified byte-for-byte identical to Musashi's real implementation).
Confirmed via Yabause's `scsp.c`: the M68K's entire fetch/execute space is
Sound RAM starting at M68K address 0 (`M68K->SetFetch(0, 0x80000,
SoundRam)`), and M68K addresses ≥ 0x100000 hit the SCSP's own register
block — the *same* dual-ported registers the SH-2 sees at physical
0x05B00000, confirmed against `c68k_byte_read`/`c68k_byte_write`. This is
the real SH-2↔M68K communication path.

Wired into the architecture as Core 3's responsibility (already the
VDP1/VDP2/SCSP thread): `SaturnSystem::m68k_control` lets Core 0's SMPC
command handling flip an atomic flag on SNDON/SNDOFF; Core 3 owns the
actual `M68k` instance and resets/halts it on that edge. A debounce was
added on the reset edge (real SNDON only fires after the SH-2 finishes its
upload; our "commands complete instantly" simplification could otherwise
let Core 3 reset the M68K on a half-written image, a genuine architectural
race between two independent OS threads with no barrier beyond the flag).

Implemented the other half of the real handshake once found by tracing
(`M68k::write_byte`): a write to SCSP's MCIPD register only actually
raises an interrupt to the SH-2 if the same bit is enabled in MCIEB — this
is the SCU's real "Sound Request" interrupt (vector 0x46, level 9,
confirmed against `ScuSendSoundRequest()` in `scu.c`), now a third real
interrupt source in `Sh2::service_pending_interrupt` alongside VBLANK-IN
and the SMPC System Manager interrupt, each cross-checked against its own
real vector/level and given its own regression test.

The Sound Request interrupt alone was enough to move the wall again — Core
0's BIOS PC left the loop it had settled into and reached new territory
(around `0x06011900`, drifts across sessions/BIOS revisions). Real forward
progress, from a fix that had nothing to do with graphics at all.

**Open as of this writing:** the M68K core itself doesn't run the uploaded
driver to completion. Live-tracing found the driver's first real action is
a 65536-iteration `MOVE.L D0,(A0)+` / `DBRA D7,-4` memory-clear loop
starting at Sound RAM address 0 — which, given the loop's own two
instructions sit well inside the swept range, mathematically
self-overwrites its own code partway through (a 4-byte-aligned longword
write at address N touches N..N+3, and the loop's instructions happen to
fall inside that window on a later iteration). Checked both instructions
byte-for-byte against Musashi and they're not the bug. Whether this is a
genuine artifact of an incomplete/mis-timed driver upload from the SH-2
side, or expected real-driver behavior this project doesn't yet correctly
avoid, is unresolved — see `.development/current_blocker.md` for the full
trace, what's been ruled out, and the concrete next diagnostic step.

## Chapter 5 — Documentation reorganized for continuity

By this point the project had accumulated real, hard-won findings (why
`D7=0xFFFF` at the clear-loop matters, why the debounce didn't fix the
race it was meant to fix, which opcodes were cross-checked against which
reference) that existed only in conversation, not in the repository. Given
how much of the value in a session like this is the *reasoning*, not just
the code diff, the project's documentation was restructured around one
question: what does an agent with zero memory of this conversation need to
read, in what order, to pick up exactly where this left off?

- `history.md` (this file): the narrative — what happened and *why*,
  chapter by chapter. Append here when a decision needs explaining, not
  just recording.
- `.development/current_blocker.md`: the single, live, actively-maintained
  description of whatever's blocking progress *right now*. Rewritten
  (not appended to) each time the wall moves, so it never accumulates
  stale alternatives that already got ruled out.
- `.development/current_bugs.md`: known gaps/bugs that aren't the active
  blocker but matter eventually (DIV1 chaining unverified, a couple of
  undecoded SH-2/M68K opcode forms, VDP2 backdrop-only).
- `.development/ROADMAP.md` and `.development/TASKS.md`: milestone- and
  task-level status, so "what's done" doesn't have to be reconstructed
  from git log.
- `docs/`: reference material that doesn't change often (architecture
  report, the original project ask, test infrastructure notes) — moved
  here from the repo root, where it was mixed in with unrelated projects
  in the same monorepo.
- The old `.agents/` directory (repo root) — multi-agent swarm
  orchestration scaffolding from Chapter 0, no unique content beyond
  per-role coordination files — was removed.

`CLAUDE.md`'s work loop now has an explicit step for keeping all of this
current as part of *finishing* a change, not as cleanup after the fact.

## Chapter 6 — Revisiting the architecture itself, from first principles

Prompted first by an honest architecture review (written on request — see
`docs/honest_architecture_review.md` — covering the real costs found this
session: `LockStepSync::sync_core`'s per-instruction global mutex, the
monolithic `RwLock<WorkRam>`, and the SNDON debounce as a symptom of a
structural synchronization gap), and then by a direct comparison against
`docs/initial_architecture_idea.md` (the project's original planning
notes), a long back-and-forth worked through exactly where the current
implementation diverges from the original vision, why, and what the
*correct* version of that vision actually looks like in systems terms.

**Where the original vision and the implementation diverged.** The
original idea called for isolated processes (not threads) communicating
only through a central Mediator, with synchronization emerging from
Saturn-like shared memory buffers rather than an artificial mutex. What
got built instead is four OS threads directly sharing one monolithic
`Arc<RwLock<WorkRam>>` with no message-passing layer at all — closer to
"N threads fighting over one big lock" than to the buffer-based,
actor-style model that was actually designed. The SNDON debounce hack is
a direct symptom: without a real Mediator/signal layer, the fix for a
genuine cross-thread ordering race became a wall-clock guess instead of an
explicit handshake.

**Correcting a specific, consequential misconception.** The discussion
initially got redirected by a natural but incorrect belief: that switching
from threads to real OS processes would avoid context-switch cost. On
Linux, `fork()` and `pthread_create()` are both `clone()` under the hood;
the *scheduler-level* context switch (register save/restore) costs the
same either way, and threads are actually *cheaper* to switch between than
processes specifically because switching between processes also requires
an address-space (TLB) switch that threads, sharing one address space,
avoid entirely. This mattered because it was the stated reason for wanting
processes — once decoupled from that (incorrect) performance claim, the
conversation found the *actual* valid reasons to want process-level
isolation (hardware-enforced fault containment) versus the reasons that
don't hold up.

**Untangling context-switch cost from cache-coherency cost.** A second,
related conflation: the instinct to avoid shared mutable variables between
concurrent units *is* correct and valuable — but the cost it avoids is
cache-line bouncing between cores (a hardware cache-coherency cost), not
context-switch count. Critically, that cache-coherency cost applies
identically to threads and processes: two processes sharing a `shmop`/
`mmap` segment touched frequently from both sides bounces cache lines
exactly like two threads sharing an `Arc<RwLock<T>>` — the CPU's cache
coherency protocol has no notion of "process" vs. "thread," only of which
physical cache line a given core touched. What actually reduces this cost
is message-passing discipline (each component owns its memory, others
request/receive rather than reach in directly) — achievable identically
well with in-process channels as with cross-process sockets.

**Confirming, then upgrading, a genuinely correct instinct.** The user's
own prior work — a PHP `Future` class combining `pcntl_fork`,
`stream_socket_pair`, and a blocking `fgets()` call — was raised as the
mental model for "buffer that blocks until data's ready." Fetching and
reading that implementation
confirmed it does *not* poll: the blocking property comes from `fgets()`
on a real socket, not from shared memory itself (shared memory, via
`shmop` or `Arc<RwLock<T>>` alike, has no blocking semantics of its own on
any platform — it's just visibility). This is the same category of
mechanism as Rust's `Condvar`/`Mutex` pair (or a blocking channel `recv`):
both genuinely park the waiting thread at zero CPU and wake it at the
exact instant the signal fires, categorically different from a sleep-based
poll loop (which introduces real latency/imprecision — a legitimate
concern the user raised directly, correctly worrying that a naive `sleep`
in a hot path could desynchronize timing-sensitive emulation).

One further correction on the same thread: for communication *between
threads of the same process* specifically, real OS sockets are strictly
worse than an in-process `Condvar`/channel, not equivalent — every socket
`read`/`write` is a syscall (kernel-boundary crossing, buffer copy) even
for `AF_UNIX`, on every message, whereas an in-process channel's fast path
never leaves userspace and only pays a `futex` syscall on the genuinely-
blocking case. Sockets earn their keep across process boundaries, where
there's no alternative; within one process they're a needless detour.

**The one remaining, real distinction.** After clearing up context-switch
cost and cache-coherency cost, the one genuine, unique advantage real OS
processes hold over threads is hardware-enforced fault isolation — a page
table boundary the MMU actually polices, versus threads sharing an address
space where a wild write in one component *can* (however rarely, in
`unsafe` territory) reach another's memory. Whether that's worth the extra
ceremony (explicit `shmem`, no cross-process `Condvar`, `fork`'s Unix-only
nature) for this project is an open, deferred decision — see
`TECH_DEBT.md`.

**CPU clock throttling — a real, separate, currently-open gap.**
A late turn in the conversation surfaced something genuinely missing:
Mimas doesn't throttle emulated CPU instruction execution to real hardware
clock rate at all right now — only VBLANK timing and VDP2 frame
publishing are paced against wall-clock (`Instant::now()`); the
interpreters themselves run as fast as the host allows. This matters for
any BIOS/game code using raw cycle-count delay loops instead of hardware
timers (common enough in real BIOS code to be a real risk, not a
hypothetical one). The correct technique — confirmed against how every
practical cycle-throttled emulator does it, not just asserted — is
batched wall-clock comparison (run N cycles unthrottled, compare elapsed
real time against the ideal target, sleep the difference before the next
batch), the same pattern already used for VBLANK/frame pacing, generalized
to instruction execution. A naive per-instruction `sleep()` is not just
inelegant but *impossible* at the relevant timescale — an SH-2 instruction
takes tens of nanoseconds; OS sleep wake-up precision is microseconds at
best, three-plus orders of magnitude too coarse.

**Where this landed.** The concrete plan that came out of all of this —
`Condvar`-backed signaling instead of debounce guesses, per-component
threads instead of a hardcoded four mapped 1:1 to "R36S has four cores,"
letting the OS scheduler place them, real hardware-driven signals instead
of implementation-timed hacks, batched CPU clock throttling, and
eliminating every poll loop that isn't either the CPU's own instruction
loop or a deterministic wall-clock pacer — is written up as an actionable
plan in `TECH_DEBT.md`, not just as conclusions in this chapter. That
file is the next real piece of architectural work, tracked the same way
everything else in `.development/` is: update it as pieces land, don't
let it go stale.

---

## Chapter 7 — Landing `TECH_DEBT.md`'s first two items: idle-spin fix and a real SNDON signal

With `TECH_DEBT.md` and `docs/final_architecture_draft.md` written up as the
decided plan (Chapter 6), this chapter is the first actual execution pass
against it — specifically the "suggested order of attack"'s first two
items, deliberately scoped as their own session before the larger `WorkRam`
split and CPU throttle: stop Core 1 (Slave SH-2) and Core 2 (SCU DSP
stand-in) from spinning at 100% CPU doing nothing real, and replace the
SNDON→M68K-reset 2ms debounce with a real signal.

**Two design-review passes caught real problems before any code landed.**
A first draft proposed a new `enabled: Vec<bool>` field on `LockStepSync`
plus `enable_core`/a fresh `park_until_enabled` API, and a new
`Condvar`+`Mutex<bool>` wrapper type for `m68k_control`. An independent
design-review pass (re-reading the actual `sync.rs`/`lib.rs`/`sh2.rs`
against the proposal) caught that the new `enabled` field would silently
skip `set_thread_active(id, true)`'s existing cycle-catch-up step (already
covered by `test_large_thread_drift_inactive`) — meaning whoever eventually
wires up real SSHON and reactivates Core 1 via the new API would hit a
fresh, subtle drift-stall the moment they used it, since nothing would seed
Core 1's frozen cycle count back to the active minimum. A second pass —
re-reading `docs/final_architecture_draft.md`, which names `BusArbiter` as
the literal reference shape new signals should match — showed the `Condvar`
half of the proposed `m68k_control` wrapper was unused dead weight:
`BusArbiter::is_locked()` itself is a bare `Ordering::Acquire` load with no
`Condvar` at all, reserving the `Condvar` specifically for
`acquire_bus`'s genuinely-blocking caller. Core 3 never genuinely blocks on
SNDON state — it has real, unconditional work every loop iteration (VDP2
frame pacing, M68K stepping) — so checking a correctly-ordered flag there is
the same legitimate category `TECH_DEBT.md` already grants
`sound_req_irq`/`smpc_irq_pending` elsewhere in the same file.

**What actually landed, instead.** `LockStepSync` gained one method,
`park_while_inactive(core_id)`, reusing the struct's own existing
`Mutex`+`Condvar` and its existing `active` field (the same one
`BusArbiter::acquire_bus_sync` already uses to exclude a DMA-blocked core
from drift tracking) — no new field, no new "enable" API. Core 1 and Core 2
now call `set_thread_active(id, false)` then `park_while_inactive(id)` as
their first action; a future SSHON or real-DSP implementation reactivates
with the *existing* `set_thread_active(id, true)`, getting the cycle
catch-up for free. Core 1's closure also picked up `bios`/`reset()` wiring
it never had before (harmless today since nothing reactivates it yet, but a
real gap the first draft would have reproduced one layer down instead of
fixing). `m68k_control` stayed a bare `Arc<AtomicBool>` — the fix was
`Ordering::Release` on the SNDON/SNDOFF stores (`Sh2::smpc_execute_command`)
paired with `Ordering::Acquire` on Core 3's read, mirroring
`BusArbiter::lock_for_dma`/`is_locked`'s existing pair for `locked_by_dma`
exactly, plus deleting the debounce (`m68k_reset_pending_since`,
`M68K_RESET_DEBOUNCE`) entirely.

**Why `Relaxed` was the actual bug, not timing.** Core 0 executes SH-2
instructions in strict program order on one thread, so every Sound RAM
write the driver-upload routine makes necessarily completes before the
COMREG=0x06 write that flips SNDON — the data was always ready. The gap was
that `Ordering::Relaxed` gives no cross-thread visibility guarantee beyond
the bool's own atomicity, so nothing ever *guaranteed* Core 3 would observe
Core 0's Sound RAM writes once it saw the flag — it happened to work on
x86-64/LLVM, consistent with `.development/current_blocker.md`'s own
earlier finding that the debounce made no observable difference (Sound RAM
was byte-for-byte identical with and without it). That finding had already
falsified the debounce's original justification before this chapter's work
began; it just hadn't been acted on yet. `Release`/`Acquire` closes the gap
completely — Core 3 acquiring evidence of the flag is now guaranteed to
also carry every write that preceded it in Core 0's program order, not just
observe it in practice.

**Testing.** `sync_tests.rs` gained three tests exercising
`park_while_inactive` directly (blocks then wakes on reactivation, wakes
on shutdown, and a genuinely-parked core not forcing other active cores to
block on drift — driven through real spawned threads, not just direct
`set_thread_active` calls), plus a `cpu0_pc`-progress assertion added to
the existing `test_saturn_system_startup_shutdown` as a deadlock canary.
`adversarial_tests.rs` gained a two-thread stress test hammering the real
SNDON store/load path a few hundred iterations, writing a distinct Sound
RAM byte before each flip and asserting the reader always observes it
(best-effort/probabilistic — no Miri/loom harness in this repo — but the
standard way to give a missing ordering edge a chance to surface). Since
none of this had any per-test timeout anywhere in the workspace before now
(no CI config, no `cargo-nextest`), both test files also picked up a small
`assert_completes_within` helper (spawn on a thread, `recv_timeout` on a
channel, propagate a real panic verbatim or fail loudly on a genuine hang)
— worth the small addition specifically because this chapter introduces
the codebase's first new blocking-park primitive since `BusArbiter`, and a
mistake in it is categorically the worst kind: a silent hang, not a failed
assertion. Full workspace suite: 125 → 129 passed, 0 failed.

**What this doesn't do.** Confirmed, not just assumed: this work does not
move the current M68K driver self-corruption wall
(`.development/current_blocker.md`) — that's a separate, already-tracked
bug in the SH-2 upload logic, and removing the debounce was expected (per
its own already-falsified premise) to leave that symptom byte-for-byte
unchanged. `WorkRam`'s region split and the CPU clock throttle
(`TECH_DEBT.md` items 3-4) are deliberately still not started — bigger,
more design-open-ended work the plan itself wants proven-pattern-first
before starting, not folded into this pass.

---

## Chapter 8 — Splitting `WorkRam`'s monolithic lock

`TECH_DEBT.md`'s suggested order of attack's third item: `WorkRam`'s single
`Arc<RwLock<WorkRam>>` — one lock covering all 14 real-hardware memory
regions (Low/High Work RAM, Sound RAM, SCSP registers, VDP1/VDP2 VRAM/
framebuffer/CRAM/registers, SCU registers, CS2 registers, backup RAM, SMPC
registers) — split into one `RwLock` per field. Bigger blast radius than
Chapter 7's two fixes (roughly 50 call sites across 6 files, versus a
handful), so this chapter's process leaned harder on verification before
writing any code: a full access-map pass first, then an independent design
review of the proposed split, both folded into the plan before touching a
single file.

**What the access map found.** Grepped every one of the 14 field names
against every file that could plausibly touch them, cross-referenced
against which thread each access runs on. Only 3 fields turned out to be
touched by more than one component *today*: `sound_ram` and `scsp_regs`
(Core 0's `Sh2` and Core 3's `M68k` — the real dual-ported SH-2⇄M68K sound
path) and `vdp2_regs` (Core 0's `Sh2` and Core 3's render loop, every
frame). The other 11 are Core-0-only right now, but only because Core 1
(Slave SH-2) is parked (Chapter 7) — real dual-SH2 Saturn software shares
Low/High Work RAM via `TAS.B`-guarded spinlocks between Master and Slave,
so "single-owner" here is a snapshot of Core 1 being parked, not a
permanent guarantee. No call site anywhere held one lock guard across two
*different* fields, which meant the split itself was mechanically clean —
but the access map surfaced one real, pre-existing gap along the way:
`TAS.B @Rn` does a separate `read_byte` then `write_byte`, two independent
lock acquisitions with a real gap between them, not an atomic test-and-set.
Dormant only because Core 1 never runs.

**What the design review changed.** The first draft proposed grouping
fields by hardware component (e.g. all of VDP1's three regions under one
lock). The review's counter: no site holds two fields under one guard
today, so grouping saves nothing internally, and `docs/final_architecture_draft.md`'s
own topology roadmaps every one of the "11 single-owner" fields toward
genuinely cross-thread once SCU/SMPC/CS2/VDP1+VDP2 get their own threads —
per-field isn't a hedge for Core 1 specifically, it's the right long-term
shape regardless. (VDP1+VDP2 staying one *thread* forever, per that same
doc, is a thread-topology decision independent of lock topology — a thread
never contends with itself.) The review also caught two call sites that a
blind mechanical substitution would have silently broken: `M68k::write_byte`'s
`scsp_regs` branch holds one guard across both the register store and the
immediately-following MCIEB/MCIPD interrupt check (splitting the lock
without preserving that would let a concurrent SH-2 write to MCIEB land in
the gap, changing whether *this* write should have fired the interrupt);
and `vdp::render_backdrop`, which reads `vdp2_regs` twice (TVMD, then
BKTAL) and was previously safe only because its caller already held one
whole-`WorkRam` lock for the entire call — with no outer lock to ride on
post-split, it needed to acquire `vdp2_regs`'s own lock itself, once, held
across both reads.

**What shipped.** Every field of `WorkRam` became `RwLock<Box<[u8; N]>>`
(kept the existing `Box` inside the new lock, preserving the heap-first
construction technique the code already used to avoid ever materializing a
1MB array on the stack); the outer struct no longer needs its own lock, so
`Arc<RwLock<WorkRam>>` became plain `Arc<WorkRam>` everywhere, keeping
`Sh2::new()`'s 3-argument signature intact per `CLAUDE.md`'s non-negotiable
— only what the argument points to changed. `M68k::write_byte`/`read_byte`
reordered to decode the address before acquiring a lock (previously the
reverse), with the `scsp_regs` branch's single held guard preserved exactly
as the review required. `render_backdrop` now acquires `vdp2_regs.read()`
itself, once. All 14 fields landed in one pass rather than staged region by
region (the original migration notes' suggested order) — by the time this
work started, the prerequisite that note was protecting (real signals for
`sound_ram`/`scsp_regs`'s associated events) was already satisfied by
Chapter 7's SNDON fix, so there was no remaining reason to carry two
different access patterns through a multi-session migration.

**The `TAS.B` gap got a comment, not a fix.** Real dual-CPU Saturn
software's whole reason to use `TAS.B` is a spinlock over shared Work RAM
between Master and Slave SH-2 — exactly the scenario the read-then-write
gap breaks. Fixing it needs a real compare-and-swap-style primitive or one
write-lock spanning both operations, a separate, non-trivial change. Left
deliberately unfixed, but flagged loudly directly on the `TAS.B` match arm
in `sh2.rs` (not just in this file): removing the old monolithic lock's
incidental over-serialization between cores makes two concurrent `TAS.B`s
on the same byte *more* likely to actually race the moment Core 1
activates, not equally dormant — this needs to land before or alongside
SSHON, not as a someday-maybe.

**Verification.** Full workspace suite stayed at 129 passed / 0 failed
(the same count as after Chapter 7), the cross-thread stress tests
(`test_sndon_signal_publishes_sound_ram_writes_across_threads`, the parking
tests) were re-run repeatedly rather than trusted on one green pass, and
the same real-BIOS trace from Chapter 7 (`Sega Saturn BIOS (USA).bin`,
`MIMAS_DEBUG_M68K=1`, 200-second window) was re-run to confirm the M68K
wall's exact signature — `D7=0xFFFF, A0=0` at the clear-loop entry, the
same `0xFFFC`/`0xFF00` derailment, PC reaching the same `~0x06011900`
territory — is still byte-for-byte unchanged. This split, like the SNDON
fix before it, is architecture work orthogonal to that investigation, and
now has the evidence to back that claim twice.

---

## Chapter 9 — A real CPU clock throttle, closing out `TECH_DEBT.md`'s suggested order of attack

The last item: `LockStepSync` bounds *relative* drift between cores in
abstract cycle-count terms, but nothing paced execution against a real
wall-clock target — every interpreter ran as fast as the host CPU allowed.
Only VBLANK timing and VDP2 frame publishing were paced against real time.
This matters because real BIOS/game code sometimes uses raw cycle-count
delay loops instead of hardware timers, and running those unthrottled is a
real, if so-far-unconfirmed, risk.

**Confirmed with the user before writing any code**: the default stays
unthrottled. Every existing verification workflow (`CLAUDE.md`'s loop,
`MIMAS_BOOT_WATCH_SECS`, the real-BIOS traces this project has leaned on
repeatedly) keeps running exactly as fast as it always has. Real speed —
or any multiplier — is an explicit, live-adjustable opt-in, not a new
default. This was a genuine judgment call with real trade-offs (hardware
fidelity by default vs. not risking the active BIOS-boot investigation's
ergonomics), not something to decide unilaterally.

**Real clock rates, cross-checked against Yabause's source, not
remembered.** This project's standing rule (never trust an assumed
hardware number) applies just as much to a clock rate as to an opcode's
semantics. Two numbers, pulled directly from the actual constants Yabause
computes cycle-stepping from, not rounded restatements:

- **SH-2 (Master = Slave)**: NTSC 352-dot/28MHz mode —
  `(39_375_000.0 / 11.0) * 8.0` = 28,636,363.636... Hz — from
  `YabauseChangeTiming()` (`yabause/src/yabause.c:165-167`), gated by
  `CLKTYPE_28MHZ`, the mode real hardware boots into. Mimas doesn't
  implement the SMPC CKCHG352/CKCHG320 commands that would switch modes at
  runtime, so this is a fixed constant for now — documented as such rather
  than quietly pretending it's the only possible rate.
- **M68000 (SCSP sound CPU)**: 11,289,600 Hz (`44100.0 * 256.0`), same for
  NTSC/PAL — from `scsp2.c:128-129`'s `SCSP_CLOCK_FREQ` (explicitly
  commented "11.2896 MHz" in Yabause's own source), corroborated
  independently at three more sites in `yabause.c`.

`m68k.rs` doesn't track per-instruction cycle cost at all — unlike `Sh2`,
which already flatly charges 2 cycles/instruction regardless of real
opcode timing, an existing, long-accepted simplification. Building a real
per-opcode M68K cycle table is a separate, much bigger undertaking than a
throttle. The throttle charges a flat nominal 8 cycles/instruction for
M68K instead — a commonly-cited rough average for 68000 code — which is
the same simplification tier already accepted for the SH-2, just extended
to a second CPU, and said out loud rather than dressed up as precise.

**The mechanism**: `saturn-core/src/throttle.rs`'s `ClockThrottle` —
accumulate real cycles silently, and once a batch's worth has built up
(targeting ~1ms of emulated time, large enough that OS sleep-precision
error is negligible relative to it), pace against a shared, live-mutable
`ThrottleSpeed` (`Unthrottled` or `Multiplier(f64)`). The pacing math
mirrors `docs/final_architecture_draft.md`'s own pseudocode exactly:
`next_batch_due` accumulates a fixed ideal duration each batch rather than
resetting to `now + duration`, so a transient slow batch (a scheduler
hiccup) gets made up by running flat-out for a few subsequent batches
instead of permanently losing that time, while a *persistent* inability to
keep up degrades to a permanent no-op — exactly the documented "running
behind real-time... consider this observable" behavior, not a lie about
elapsed time.

`Sh2` gained one new optional field (`speed`, mirroring the existing
`m68k_control`/`sound_req_irq`/`pc_reporter` wiring pattern exactly) —
`None` by default, so every one of the ~70 existing opcode tests that
builds a bare `Sh2` stays exactly as fast as before this existed, zero risk
to the existing test suite. `M68k` itself needed no change at all — its
throttle lives entirely in Core 3's closure in `lib.rs`, calling
`.advance()` once after each `m68k.step()`, right where the old fixed
"200 steps per iteration" magic constant used to be the only pacing
concept in that loop. `SaturnSystem` gained `speed`/`set_speed`/`get_speed`
— one shared knob for the whole system, not independent per-component
sliders, matching how a real Saturn's own clock derivation works (there's
no separate "sound chip speed" control on real hardware either). A
`--speed` CLI flag on `saturn-frontend-native` exposes it for manual
testing; `saturn-frontend-libretro` was left untouched, still just stub
functions with nothing to wire yet.

**A real bug the test suite caught before it shipped.** The first attempt
at a `SaturnSystem`-level test (set an extremely small multiplier, sample
`cpu0_pc` twice a few milliseconds apart, expect identical values) failed
outright — not because the throttle was wrong, but because the test's own
timing assumptions were: this interpreter's per-instruction overhead
(a real syscall in `thread::yield_now()`, lock contention in `sync_core()`,
on *every single instruction*) turned out to be substantial enough that a
14,318-instruction batch took tens of milliseconds to execute even
unthrottled, invalidating the test's assumption that the initial burst
would complete near-instantly. Chasing a robust, non-flaky version of that
specific assertion led somewhere more useful: reasoning through it surfaced
that an extreme multiplier's *ideal* per-batch sleep duration is
unbounded (here: roughly 1000 seconds), and `thread::sleep` cannot be
interrupted once entered — meaning a pathological speed setting, with no
further fix, could make `SaturnSystem::shutdown()` hang for however long
the in-flight sleep happened to be. `ClockThrottle` now caps any single
sleep at 50ms regardless of the ideal duration (for any realistic
multiplier this never binds; it only matters at the pathological extreme).
The `SaturnSystem`-level test was rewritten around confirming *this*
property end-to-end (shutdown stays prompt even with an extreme
multiplier configured) instead of the original, fragile progress-rate
comparison — a more valuable and more robust test than the one first
attempted, found by taking a failure seriously instead of just loosening
a bound until it passed.

**Verification.** Full workspace suite: 129 → 136 passed (5 new
`ClockThrottle` unit tests using a synthetic, decoupled `clock_hz` for
precise, non-flaky timing assertions; 2 new `SaturnSystem`-level tests),
0 failed, re-run five times clean. Real-BIOS trace with the default
(unthrottled) configuration confirmed, a third time now, the exact same
M68K wall signature as Chapters 7 and 8 — this work is what it claims to
be, orthogonal to that investigation.

This closes out `TECH_DEBT.md`'s suggested order of attack, items 1
through 4. Item 5 (revisit threads-vs-cooperative-scheduling with real
measurements) is now finally unblocked — this throttle is exactly the
prerequisite it was waiting on for a real, comparable performance number.
Item 6 (headless BIOS-boot integration tests), added to the document after
this chapter's work began, is the one item left that hasn't been
started.

---

## Chapter 10 — A real, measured spurious-wakeup bug in the parking mechanism

`TECH_DEBT.md`'s item 5 ("revisit threads-vs-cooperative-scheduling with
actual measurements") got substantially reframed before any measuring
started. The abstract R36S-vs-desktop question in the doc turned out not
to be the live question at all: the user's own prior work on this repo's
*other* Saturn emulator (the Yabause/YabaSanshiro fork, `yabause/`) had
already run the real experiment. That project achieved something no other
Saturn emulator has — sample-accurate audio/video sync, real lip-sync in
Magic Knight Rayearth's dialogue — by eliminating its async SCSP thread in
favor of cycle-driven single-thread execution (see `yabause`'s own commit
`f96ebfbe`, "Eliminate SCSP audio thread: cycle-driven single-thread sync
fixes audio-sync crackle for good"). On PC: 60fps at 35% CPU. On the actual
R36S target: only 40fps, with the Mali GPU at ~10% and 3 of 4 CPU cores
sitting almost idle — perfect correctness, but capped by one core's serial
throughput, wasting the rest of the silicon. That's not a hypothetical
concern this project's own docs were citing secondhand; it's the direct
reason Mimas exists at all. So "threads vs. cooperative" isn't genuinely
open the way `TECH_DEBT.md` framed it — multi-threading is the whole point,
*because* single-threading already proved it caps performance on weak
hardware. The real question is whether Mimas's own multi-threaded
implementation is achieving genuine parallelism or quietly serializing
itself the same way single-threading does, just less honestly.

**A crucial detail from the same conversation, worth recording precisely**:
audio crackle in that cycle-driven design isn't a bug to chase — it's the
direct, expected symptom of insufficient throughput. A tightly-coupled
audio/video pipeline with no independent buffer has no slack to absorb a
frame taking too long; it starves the audio backend instead of quietly
dropping a frame. That means the fix for crackle in this architecture is
always "have enough parallel throughput headroom," never "add audio-side
smoothing" (which would just reintroduce the A/V drift the tight coupling
was built to eliminate). This directly validates how `ClockThrottle`
(Chapter 9) already behaves: falling behind real-time is left observable,
never quietly absorbed.

**The measurement.** No `perf` in this environment, no real R36S, but
`/proc/<pid>/task/*/stat` gives real per-thread `utime`/`stime`, and that's
enough to answer the concrete question: is Mimas's existing 4-thread
architecture actually running components in parallel? First measurement,
over an 8-second window on a real BIOS boot: all four core threads showed
substantial, roughly comparable CPU consumption (`~65-96%` of a core
each) — including the two that are supposed to be parked doing nothing
(Slave SH-2, the SCU DSP slot). That's wrong on its face: a genuinely
parked thread blocked on a `Condvar` should cost essentially zero CPU.

**Finding the cause required identifying which thread was which first** —
gdb attach was blocked by this environment's `ptrace_scope` hardening (a
security setting, correctly left alone rather than worked around), so the
four core-spawning threads in `SaturnSystem::start()` got real OS-visible
names (`sh2-master`, `sh2-slave`, `scu-dsp`, `vdp-scsp-m68k`, via
`thread::Builder::name`) — a small, permanent improvement in its own
right, not just a throwaway diagnostic. That confirmed it precisely:
`sh2-slave` and `scu-dsp`, the two parked cores, were each burning around
65% of a core, while `sh2-master` and `vdp-scsp-m68k` (the two with real
work) sat near 100%.

**Root cause, once named threads made it traceable**: `LockStepSync` used
a single `Condvar` for two entirely different purposes — `sync_core`'s
drift-bound waiters (legitimately notified on *every* call, up to once per
emulated instruction from an active core) and `park_while_inactive`'s
waiters (which should only ever wake on a real reactivation or shutdown).
Sharing one condvar meant every parked core got spuriously woken by every
other active core's routine `sync_core` call — millions of times a
second — each time re-contending for the very same mutex the genuinely
busy cores needed for their own synchronization. Parking was logically
correct the whole time (Chapter 7's tests for "blocks then wakes on
reactivation," "wakes on shutdown," and "doesn't force active cores to
wait on drift" all still pass, because they check logical behavior, not
CPU consumption) — it just never actually achieved the zero-CPU idle state
its own doc comment claimed, in any real multi-thread run. Unit tests
proved the *logic*; only a running-system measurement could have caught
that the *mechanism* wasn't free.

**The fix**: a second, dedicated `park_condvar` on `LockStepSync`, sharing
the same underlying `Mutex<SyncState>` (a well-established, safe pattern —
multiple condvars over one mutex, each covering a different wait
condition) but notified only by `set_thread_active`'s reactivation branch
and `request_shutdown`. `sync_core`'s existing high-frequency notifications
never touch it. Full workspace suite stayed green throughout (136 passed,
0 failed) — this was purely a wakeup-efficiency fix, no logic changed.

**Re-measured after the fix, same 8-second window**: `sh2-slave` and
`scu-dsp` both at genuinely 0 ticks — real zero CPU, for the first time.
`sh2-master` and `vdp-scsp-m68k` each still near 100% of their own core,
now uncontended. The system was previously burning roughly 2.5 cores'
worth of CPU to do the work of 2; after the fix, it spends almost exactly
2 cores' worth on 2 cores' worth of real work — matching what this
project's whole reason for existing is asking for.

**An unexpected, second-order result, flagged rather than chased**: the
same real-BIOS re-verification run that confirmed the M68K wall's
signature is still byte-for-byte unchanged (same `D7=0xFFFF, A0=0`
clear-loop entry, same `0xFFFC`/`0xFF00` derailment — this fix, like the
last three, is orthogonal to that bug) also showed Core 0's PC reaching
`0x0601360A` by the end of the 200-second window — past the `~0x06011900`
plateau every single prior trace this session (Chapters 7, 8, 9, and the
pre-fix measurement earlier in this one) got stuck oscillating in for
thousands of samples. With the mutex contention gone, Core 0 simply
executes more real instructions in the same wall-clock budget, and reached
further as a direct consequence. Whether that plateau was a bounded loop
that just needed more throughput to finish, or something more meaningful
for the active M68K investigation, is unresolved and deliberately not
chased here — it's `.development/current_blocker.md`'s territory, with its
own methodology (`CLAUDE.md`'s loop), not something to wander into
sideways under a performance task. Recorded here precisely so whoever
picks up that investigation next isn't surprised by a trace that goes
further than the document currently describes.

---

## Chapter 11 — The flagged side finding resolves: a missing VBLANK-OUT interrupt, and a new wall behind it

Chapter 10 ended with a loose thread, deliberately not pulled at the time:
after the spurious-wakeup fix, a real-BIOS run reached PC `0x0601360A` —
past the `~0x06011900` plateau every prior trace had gotten stuck
oscillating in — with the M68K sound-driver corruption bug's signature
confirmed unchanged in the same run. Unclear at the time whether that
plateau was a genuine wall or just a bounded loop that needed more
real throughput than earlier, contention-heavy runs could deliver.

Picking that thread back up (`.development/current_blocker.md`'s explicit
instruction to re-run the boot-watch loop first) showed a *third* behavior
across runs: a fresh 300-second trace settled into a tight oscillation at
`0x060108ba`-`0x060108c2`, not `0x0601360A`. Three different "final" PCs
across three sessions (the original `~0x06011900`, Chapter 10's
`0x0601360A`, this session's `0x060108ba`) was itself a signal: black-box
PC sampling at a fixed wall-clock cutoff isn't a reliable way to find "the
wall" once the system is fast enough that timing variance moves the
sampling window's endpoint around inside a long-but-not-permanently-stuck
sequence. The fix was to stop trusting "wherever PC happens to be when the
timer runs out" and instead look for a *genuinely tight, sustained*
oscillation — `0x060108ba`-`0x060108c2` (an 8-byte span, thousands of
consecutive hits) qualified in a way the others hadn't been confirmed to.

**Tracing it, following `CLAUDE.md`'s loop exactly.** A one-shot probe
dumped all 1MB of High RAM the instant Core 0's PC first reached
`0x060108ba`, decoded offline with `tools/sh2dis.py`. The loop itself:

```text
0x060108b6: MOV.L @(0x2e,PC),R4   ; R4 = [0x06010970] (a pointer)
0x060108b8: MOV.W @R4,R4          ; R4 = *(u16*)R4  -- snapshot, taken once
0x060108ba: EXTS.W R4,R2          ; <- loop entry
0x060108bc: MOV.L @(0x2c,PC),R3   ; R3 = [0x06010970] (same pointer, reread)
0x060108be: MOV.W @R3,R3          ; R3 = *(u16*)R3  -- current value
0x060108c0: CMP/EQ R3,R2
0x060108c2: BT 0x060108ba          ; loop while unchanged from the snapshot
```

The pointer at `0x06010970` resolved to `0x060408a4` — High RAM offset
`0x0408a4`, the exact "counter byte" a *previous* session's throwaway probe
had already been watching writes to (see the stale probe in
`raw_write_byte`'s `HighRam` arm, predating this session). So: "wait for
this counter to change from whatever it was on entry." Searching the full
1MB dump for every literal-pool reference to `0x060408a4` found five call
sites; four were read-only, one (`0x06010384`-`0x0601038c`) did a genuine
`ADD #1` + store-back — the real increment. That function *also* wrote
VDP1's FBCR register (offset `0x2`) based on two other RAM flags,
cross-checked against Yabause's `vdp1.cpp:474` (`Vdp1WriteWord`, `case
0x2`, sets `Vdp1Regs->FBCR`).

**The dead end that made the next step necessary**: no `BSR`/`JSR`
anywhere in the entire 1MB dump targeted that function's address. It had
to be reached some other way — an interrupt vector, most likely, given
Mimas's own doc comment on `vblank_pending` already noted a near-identical
pattern from an earlier wall ("a RAM counter that only a real VBLANK
interrupt handler... ever increments"). But which interrupt? A second
probe, added alongside the first, captured `self.sr`'s interrupt mask,
`self.vbr`, all three pending-interrupt flags, and a running count of how
many times VBLANK-IN had actually been *serviced* (not just raised) —
right at the same stuck PC. Result: `imask=0` (nothing masked),
`vblank_serviced_count=13594`. VBLANK-IN had fired and been serviced over
thirteen thousand times with zero effect on the counter — definitively
ruling it out. That measurement, not a guess, is what justified spending
the next step resolving the BIOS's *own* interrupt dispatch table instead
of continuing to assume VBLANK-IN was somehow involved.

**Resolving the real handler.** With `vbr=0x06000000` confirmed, the
vector table (`VBR + vector*4`) for every SCU interrupt (0x40-0x4D) was
read directly out of the same dump. All fourteen pointed at a dense block
of near-identical trampolines (push a raw vector number into R0, branch to
one shared dispatcher) — a classic "save the vector, jump to common
handler" pattern, not one stub per interrupt. That shared dispatcher
(`0x060008f4`) used the pushed vector number to index *two* parallel
tables: one producing an SR interrupt-mask value to apply while the
handler runs, the other the real handler function pointer, then `JSR`'d
into it. Resolving both tables for every vector 0x40-0x4D showed thirteen
of them pointing at one shared no-op stub — except vector `0x41`, which
pointed exactly at the counter-incrementing/FBCR-writing function traced
above. `0x41` is **VBLANK-OUT**, not VBLANK-IN (`0x40`) — a real, separate
interrupt Mimas had never implemented. Cross-checked against Yabause's
`scu.c::ScuSendVBlankOUT` (`SendInterrupt(0x41, 0xE, ...)` — level `0xE`,
one below VBLANK-IN's `0xF`) and `vdp2.cpp::Vdp2VBlankOUT` (clears
TVSTAT's VBLANK bit and fires this interrupt in the same step, once per
frame at the transition from blanking back into active display) before
writing a line of Rust.

**The fix**, following the exact shape of the existing VBLANK-IN
machinery rather than inventing a new pattern: `VBLANK_OUT_LEVEL`/
`VBLANK_OUT_VECTOR` constants; a `vblank_out_pending`/
`next_vblank_out_due` field pair; `request_vblank_out_interrupt()`; a new
priority slot in `service_pending_interrupt()` (VBLANK-IN 15 > VBLANK-OUT
14 > Sound Request 9 > SMPC 8); and, in `run_loop()`, scheduling
`next_vblank_out_due` to `VBLANK_DURATION` after each VBLANK-IN edge —
deliberately derived from the *same* `now` sample that advances
`next_vblank_due`, so it stays in exact lockstep with `tvstat_word()`'s
already-existing period-start-plus-duration edge instead of running an
independently drifting timer that could disagree with what TVSTAT itself
reports. Four new tests mirror the existing VBLANK-IN ones exactly
(masked-stays-pending, enters-and-returns) plus a new priority-ordering
test (VBLANK-IN preempts VBLANK-OUT when both are pending simultaneously).
All three throwaway probes (the pre-existing counter-byte write log, the
new High RAM dump, the interrupt-state dump) were removed once the wall
was diagnosed, per `CLAUDE.md`'s own discipline.

**Confirmed against the real BIOS**: Core 0's PC moved from the
`0x060108ba` oscillation to a genuinely different tight loop at
`0x06013264`-`0x06013268`, now mixed with regular visits to the interrupt
dispatcher trampoline itself (`0x060008f4`) — real interrupt delivery
happening, not just a different address inside the same stall. The M68K
corruption signature was re-verified byte-for-byte unchanged in this same
run: the fourth architectural/bugfix change in a row to leave it
untouched, and the first time it's been possible to say with real evidence
(rather than assumption) that it isn't gating *this* particular wait — the
wall that cleared here was never downstream of the M68K's own progress.

**The new wall, found the identical way.** Dumping High RAM again at the
new stuck PC (`0x06013264`) showed:

```text
0x06013264: MOV.L @R1,R0   ; R0 = *(u32*)0x25FE0080  <- loop entry
0x06013266: TST R0,R2      ; R2 = 0x00010000 (mask)
0x06013268: BF 0x06013264   ; loop while (R0 & R2) != 0
```

`0x25FE0080` strips to SCU register offset `0x80` — the DSP Program
Control Port (`scu.c`, `case 0x80`), and bit 16 of that register (the mask
being tested) is `EX`, "program execute control bit," per `scu.h`'s
bitfield layout. Core 0 starts a SCU DSP program and polls waiting for it
to finish. Core 2, the SCU DSP slot, has been parked with zero DSP
execution implemented since Chapter 7 — `TASKS.md`'s own "not yet known to
be required" note about it, written before any wall traced back to it, no
longer holds. `.development/current_blocker.md` carries the rest (what's
known, what isn't, and the concrete next step: dump the BIOS's actual DSP
program before committing to a minimal-unblock vs. full-DSP approach).

**Why this matters beyond the specific fix**: a wall that looked, from
black-box PC sampling alone, like it could plausibly be downstream of an
already-known bug (the M68K corruption) turned out to be something else
entirely — a missing piece of *interrupt infrastructure* that was assumed
covered ("VBLANK is done, it's in `TASKS.md`'s Done list") but was only
half-covered. The lesson isn't "the M68K bug doesn't matter" — it's that
assuming a known open bug explains a new symptom, instead of tracing the
new symptom on its own terms, would have wasted an entire session chasing
the wrong thing. `CLAUDE.md`'s loop — decode exactly what's stuck, don't
guess — is what caught it.

---

## Chapter 12 — A real SCU DSP interpreter, built from a recovered BIOS program

Chapter 11 ended with a new wall: past the VBLANK-OUT fix, Core 0 sets the
SCU DSP's Program Control Port `EX` bit and polls forever waiting for it
to clear. Real hardware clears `EX` when a running DSP program reaches an
End instruction — Core 2, the SCU DSP's slot in `SaturnSystem`, had been
permanently parked since Chapter 7's idle-spin fix, with zero DSP
execution behind it. No register-level patch could fix this; the actual
component needed to exist.

**Recovering the real program, not guessing at one.** The same High RAM
dump used to find the wall showed the wait loop's setup code writing three
words into the DSP's Data RAM (`[0, 0x09694000, 0x000002AB]`) via the Data
RAM Data Port, immediately before the `EX`-setting write. Searching the
1MB dump for that register base as a literal found the actual upload
routine — 32 `MOV.L @R0+,R3` / `MOV.L R3,@(1,R1)` iterations reading a
literal data block and writing it through the Program RAM Data Port,
32 words starting at `0x06013280`. That block is the *real* BIOS DSP
program, byte-for-byte, not a hypothetical one.

**Decoding it against the reference, not the SH-2 manual's cousin.**
Yabause's `scu.c` DSP block uses a genuinely different instruction format
from anything else in this codebase: 32-bit VLIW words, top 2 bits
selecting Operation/Load-Immediate/"Other" (DMA/Jump/Loop/End) groups, an
ALU op that computes unconditionally every cycle whether or not anything
captures it, and three helper functions (`readgensrc`/`writed1busdest`/
`writeloadimdest`) encoding the actual register/Data-RAM addressing modes.
Decoding all 32 words against this exact bit layout (matched against the
real struct layout in `scu.h`, not assumed) showed the program uses: plain
ALU ops (NOP/ADD/SUB), D1-bus stores, conditional and unconditional MVI,
Z/T0-gated conditional jumps, and two of real hardware's eight DMA
addressing-mode variants (Yabause's own naming: `dsp_dma03`, reading Main
RAM into Data RAM/Program RAM; `dsp_dma04`, writing Data RAM out through
three different bus-width branches depending on target address range) —
no loop instructions, no DSP-side interrupt request. That's the exact
scope `saturn-core/src/scu_dsp.rs` implements: the full ALU/Operation/
Load-Immediate/Jump/Loop/End groups (Program RAM is only 256 words, not a
large surface to cover completely), but only those 2 of 8 DMA variants —
the other 6 are a real, explicitly-flagged gap, the same "add opcodes as
hit" discipline used everywhere else in this project.

**Wiring, not just interpreting.** Real hardware's DSP register ports
(`0x80` Program Control Port, `0x84` Program RAM Data Port, `0x88`/`0x8C`
Data RAM Address/Data Port) are 32-bit-only — byte/word access to them is
undefined on real silicon. Mimas's generic `ScuRegs` storage is a plain
byte array with no such distinction, so these four ports are intercepted
one level up, at `Sh2::read_long`/`write_long`, before they'd otherwise
decompose into four separate `raw_write_byte` calls — the first register
group in this codebase needing that treatment. The DSP itself lives behind
`Arc<Mutex<ScuDsp>>`, shared between Core 0 (which only ever touches the
register ports) and Core 2 (which actually steps it) — a write setting
`EX` calls `sync.set_thread_active(2, true)`, the exact same reactivation
call Core 1's (not-yet-implemented) SSHON handling and the M68K's SNDON
handling already use, not a new pattern invented for this. Core 2's loop
mirrors Core 0/Core 1's own instruction-execution loops (legitimate,
"zero-polling"-exempt work, not a wait) while `EX` is set, and re-parks
via the *same* `park_while_inactive` mechanism the instant the DSP clears
`EX` on its own — matching real hardware's DSP genuinely stopping cycle
consumption when its program ends, not just "looking idle."

**Verification, two independent ways.** A new unit test loads the exact
recovered 32-word program and the exact 3 parameter words into a fresh
`ScuDsp`, steps it, and asserts `EX` clears within a bounded step count —
this is the strongest possible signal the interpreter is *correct for the
program that matters*, independent of anything else in the system. It
passed on the first real run after implementation. Separately, a real-BIOS
boot run confirmed the practical effect: Core 0's PC, which previously
never moved past `0x06013264` no matter how long the boot-watch window
ran, now visits hundreds of distinct addresses (the interrupt dispatcher,
several `0x0600xxxx`/`0x0601xxxx` handler bodies, multiple DSP-invocation
call sites reached from different interrupt paths) before settling at a
new address, `0x060131A8` — genuinely further, genuinely different code,
not a repeat of the same stall. The M68K sound-driver corruption bug was
re-verified byte-for-byte unchanged in this same run: the *third*
consecutive real fix (after the `WorkRam` split and the VBLANK-OUT
interrupt) to leave it untouched, reinforcing Chapter 11's conclusion that
it isn't gating overall boot progress even though it remains a real, open
bug.

**The new wall, briefly checked but deliberately not chased further this
session.** `0x060131A8` sits inside a small, bounded-looking counted loop
(a comparison against `50`, another against two loaded values) that calls
one subroutine partway through. That subroutine turned out to be a plain
32-bit software division routine (`DIV0S`/`DIV1`/`ROTCL`, called from many
unrelated places throughout the BIOS for ordinary arithmetic) — not
DSP- or hardware-specific, and very unlikely to be the actual cause.
Given how much ground this session already covered (recovering an entire
undocumented instruction format, building a new CPU-adjacent component
from scratch, and re-verifying two prior fixes), root-causing this next
wall was deliberately left to a fresh session with a fresh High RAM dump
— `CLAUDE.md`'s "one wall at a time" discipline applies to how much a
single sitting should chase, not just to which bug gets priority.

---

## Chapter 13 — An exhaustive hardware-reference rewrite, and a phased plan for every subsystem

A routine `/init` pass (writing `CLAUDE.md`) surfaced that the four existing architecture docs
had drifted shallow, and in places outright wrong: address notation that silently dropped a
digit (`0x5A000000` where the real address is `0x05A00000`), an "SH-2 DIV1" conceptual code
snippet that had accidentally cross-contaminated an `Sh4Context` type from a *Dreamcast*
(SH-4) reference into a Saturn (SH-2) document, and an opcode table covering maybe twenty of the
SH-2's real instructions dressed up as if it were complete. None of this was caught earlier
because nothing forced a line-by-line check against the actual reference source.

**The fix wasn't editing those docs — it was building a real one first.** Eight parallel agents,
one per hardware subsystem (SH-2 CPU, memory/bus, SCU, SMPC/peripheral, VDP1, VDP2, SCSP, CD
block), each read only Yabause's C/C++ source (never prior knowledge, never the shallow docs)
and wrote an exhaustive register/opcode/DMA/command reference with a `yabause/src/<file>:<line>`
citation on every claim, closing with a "known deviations" section cataloguing genuine Yabause
bugs, dead code, and game-specific hacks found along the way — not invented, found. The session
hit an account-level usage limit mid-run; three of the eight had already written their full
output before being cut off, and relaunching the other five surfaced a real duplicate-work risk
(a VDP2 agent from the *first* batch had actually finished and written its file after the
relaunch was already issued — caught and killed before it could clobber the good copy with a
second, redundant pass). All eight landed real depth: the SCU DSP's opcode-class encoding, VDP1's
full 32-byte command-table layout, VDP2's ~100 registers and rotation-parameter math, SCSP's
discovery that `scsp.c` alone contains *two* complete, independently-selectable synthesis
engines behind a runtime flag (plus a third in `scsp2.c`), and more.

**Then, and only then, the four old docs got trimmed** to stop duplicating (and getting wrong) facts
the new `docs/hardware-reference/` now owns authoritatively: addresses fixed, the `Sh4Context`
mistake removed, aspirational diagrams explicitly labeled as target-design rather than current
behavior, and `saturn-architecture.md` (which had literally attempted the same job this new
effort just did properly) reduced to a short index pointing at the real thing.

**The natural next question — how far is the actual Rust code from any of this? — got the same
treatment.** Eight more parallel agents, same subsystem split, each diffing its
`hardware-reference/` file against the real `saturn-core` source and writing a phased
implementation plan in `docs/implementation-plans/`. This diffing exercise alone, before a
single line of implementation changed, surfaced concrete, confirmed bugs: the SH-2 interpreter's
`OR`/`XOR`-with-immediate opcodes are swapped (confirmed against `sh2int.c`'s real dispatch
table, and the code's own comments assert the wrong mapping too, so nothing local would have
caught it); the SCU DSP masks its opcode field to 4 bits where real hardware's encoding needs the
full value, so every `JMP` executes a phantom ALU op alongside the branch; VDP1's command-table
reader is off by whole fields (color and vertex coordinates shifted), and the one existing test
for it was built on top of that same bug; VDP2's backdrop color has never actually been a color —
`BKTAL` is half of a VRAM address, and it only looked right because address `0` and color `0` are
both black; the SCSP's per-voice register reads (start address, pitch, total level) land on the
wrong bytes, with total level's attenuation sense inverted so maximum volume is silence; the
memory decoder is missing its top-level area-select stage entirely, so the SH-2's own 4KB cache
scratchpad at `0xC0000000` silently aliases to BIOS ROM; and the "CD command" path that appears
to exist in `sh2.rs` is entirely fictional — wrong trigger condition, wrong offsets, and it
flips a flag by writing into High Work RAM by mistake.

**A concurrent session, discovered mid-effort, not fought.** Multiple of these sixteen agents
independently flagged dirty files in `git status` they hadn't touched — a `scratch/yabause/`
clone, a `docs/yabause_test_fixtures_extraction_plan.md`, edits to `scsp.rs`/`sync.rs`/
`vdp.rs`/`main.rs`. This turned out to be the project's own maintainer running a second Claude
Code session in parallel on unrelated feature work (a telemetry module, SCSP voice playback,
fixture-extraction planning) — surfaced, confirmed, and left alone rather than treated as
corruption to investigate or revert.

---

## Chapter 14 — A real fixture-extraction pipeline, and SMPC's first three phases

The maintainer's own `docs/yabause_test_fixtures_extraction_plan.md` (written during the
parallel session Chapter 13 surfaced) proposed patching a real Yabause build to dump genuine
emulator state at chosen execution points, so Mimas's test suite could assert against *captured*
hardware behavior instead of hand-picked values — directly attacking the weakness the sixteen
hardware-reference/implementation-plan agents had just exposed: nearly every existing test was
built on top of Mimas's own (sometimes buggy) behavior, not real hardware's.

**Built the pipeline end to end, on a disposable clone, not the pristine reference.** The
maintainer's earlier `scratch/yabause/` clone (a fresh, unmodified checkout of
`libretro/yabause`) became the patch target — never `../yabause/`, which every
`hardware-reference/*.md` file cites by exact line number and which needs to stay byte-for-byte
what those citations point at. One dump hook went into `smpc.c`'s `SmpcINTBACK`, right after
`SmpcINTBACKStatus()` populates the real response, gated by an unset-by-default environment
variable so it's inert unless explicitly enabled. Rebuilt the libretro core with plain `make`,
then ran it **headless** — `video_driver`/`audio_driver`/`input_driver`/`menu_driver` all forced
to `"null"` via an `--appendconfig` override — against a real Saturn BIOS and a real boot-disc
image, so nothing popped up a window on the maintainer's actual desktop. It captured a genuine
0x80-byte SMPC register snapshot at the exact instant the real BIOS's first INTBACK call
finishes, written to `saturn-core/tests/fixtures/smpc_intback_status.bin` with a README
documenting the exact recipe to regenerate it.

**The fixture found a real bug on first use, before any implementation work started.** A Rust
test loaded the captured bytes through `Sh2`'s genuine memory-mapped read path. Every register
matched except one: SF read back `0x00` against the real captured `0x01`, because `Sh2`'s SF read
was hardcoded regardless of the underlying byte. Rather than leave a failing test in the suite or
quietly weaken the assertion, it became a passing test (for the registers that already worked)
plus a second, `#[ignore]`d test carrying the real expected value — a live regression check
already loaded and waiting, not a TODO comment.

**Then `docs/implementation-plans/smpc-peripheral.md`'s first three phases, executed in order.**
Phase 0 extracted a real `Smpc` type out of `sh2.rs`'s inline command handling into its own
module behind `Arc<Mutex<Smpc>>` — mirroring the `scu_dsp` precedent exactly, zero behavior
change, verified by a test asserting byte-identical output against the old inline path before
anything else changed. Phase 1 was the highest-value one: a real SF/`bustmp` handshake (real
hardware's SF sits on a shared internal data bus that other writes also drive — modeled, not
skipped), RESENAB/RESDISA, and two corrected constants the fixture and the plan's own real-BIOS
trace had already pinned exactly — `OREG0` from a hardcoded `0x80` to `0x80 | (resd << 6)`
(`0xC0` at reset), and `SR` from a hardcoded `0x6F` to the spec-correct `0x4F | (intback << 5)`.
The previously-`#[ignore]`d SF test started passing, unmodified, the moment this phase landed —
same fixture, same assertion, now true. Phase 2 completed the INTBACK status block: real RTC
bytes, SMEM, region reporting, and system-state flags. With no `chrono`/`time` dependency in this
project, the calendar math (days-since-epoch → year/month/day) was implemented from scratch via
Howard Hinnant's public-domain `civil_from_days` algorithm — verified by hand-deriving a second
test timestamp (2001-12-25 13:45:59 UTC) independently, cross-checking its implied weekday
(Tuesday) against the historical record, and confirming every resulting BCD byte matched what the
plan's own hand-worked formulas had already predicted, before the code ever ran.

**RTC is UTC, not host-local time — checked with the maintainer first**, because it briefly
looked like it might be introducing a wall-clock dependency into thread synchronization, which it
explicitly isn't: `ClockSource` only ever supplies the byte content of INTBACK's response,
nothing touches `LockStepSync` or `ClockThrottle`. Yabause itself reads the host's local
timezone; a host-timezone dependency would make every RTC-touching test non-reproducible across
machines, so UTC is a deliberate, stated deviation instead.

**Real-BIOS re-runs after each phase, not just unit tests.** A 240-second boot-watch run
(matching the window the plan's own live SMPC trace was originally captured over) after Phase 1
matched the fixture's real values exactly (`OREG0 = 0xC0`, `SR = 0x4F`, `DDR1/DDR2/IOSEL/EXLE =
0x00`) and reached noticeably further than that original trace — into code copied to High Work
RAM, rather than stalling in BIOS ROM. After Phase 2, the same run showed OREG1 tracking the
real host year, with no regression anywhere else in the sequence.

**Deliberately not chased this session: SSHOFF actually resetting the slave core.** Implementing
it exposed that `Sh2::run_loop` never checks whether its own core is still marked active in
`LockStepSync` at all — only the global shutdown flag — so a "deactivated" Core 1 keeps executing
instructions regardless. That's a real, deeper concurrency gap than the plan assumed going in,
not a same-tier fix to bolt onto a register-fidelity pass; left for a session that can give it
proper attention, exactly the "one wall at a time" discipline Chapter 12 also leaned on.

---

## Working principles that held up across all of the above

- **Never trust a self-consistent test.** Every real bug found this way
  (BT/BF's formula, the DIV1 multi-step test, several memory-map
  boundaries) was caught by checking against real BIOS bytes or an
  independently computed value, not by re-deriving the same logic twice.
- **Cross-check against a real, working reference before implementing
  anything**, but translate the *hardware semantics*, never transliterate
  the reference's C — this project's threaded architecture has no
  equivalent in Yabause (single-threaded) or Musashi (also
  single-threaded), so porting data structures directly would fight the
  design instead of fitting it.
- **State simplifications honestly, in comments, at the exact place they're
  made** ("SMPC commands complete instantly," "VDP2 backdrop-only before
  NBG tiles exist") rather than pretending they're not there. This kept
  the boundary between "faithfully emulated" and "simplified for now"
  legible enough that the M68K wall was findable at all.

`TECH_DEBT.md`'s "suggested order of attack" (Chapters 7-9, plus item 6's
integration-test work) was itself organized around five distilled
principles worth keeping even after that file's own removal (its
actionable plan is now fully executed, so the file itself no longer earns
its keep — see its own opening line: "an actionable plan, not a
retrospective"):

1. **`Condvar`-backed signaling for cross-component events, never a
   polled flag or a timing guess** — a component genuinely waiting on
   another's action should block at zero CPU and wake at the exact
   instant of the real signal (the SNDON debounce this replaced, Chapter
   7, is the canonical example of the anti-pattern).
2. **Event-driven thread/task orchestration, not a hardcoded core-count
   mapping** — one schedulable unit per real hardware component, parked
   until it has real work, not bundled to hit a target core count by
   hand.
3. **The game drives the hardware, not the implementation** — every
   "completes instantly" simplification must be paired with the *real*
   signal the actual hardware would raise on completion (an interrupt, a
   status bit the emulated code itself checks), never a wall-clock guess
   from outside the emulated instruction stream. Both the VBLANK-OUT
   interrupt (Chapter 11) and the SCU DSP interpreter (Chapter 12) are
   this principle applied to gaps that turned out to be real, not
   hypothetical.
4. **Real CPU clock throttling** (Chapter 9), so timing-sensitive code
   behaves correctly, doubling as a user-facing speed control.
5. **Zero polling loops**, except the two categories that are exempt by
   nature: a CPU core's own instruction-execution loop, and a
   deterministic wall-clock pacer comparing against a computed target.
    The subtler violation Chapter 10 found — a `Condvar` shared with an
    unrelated high-frequency notifier — showed that "blocks on a real
    `Condvar`" is necessary but not sufficient; what else notifies that
    same `Condvar`, and how often, matters just as much.

---

## Chapter 15 — CPU Milestone 1: SH-2 Opcodes, Missing Instructions, Exceptions, and Address-Space Holes

With the SMPC fixtures extracted and the testing pipeline verified green, Milestone 1 of the CPU development plan (`sh2-cpu.md`) was executed across three sequential phases:

### Phase 1 — Fix opcodes that are wrong today
* **Corrected immediate decodes**: Swapped immediate decodes for XOR and OR (`0xCA00` and `0xCB00`) which were swapped.
* **Fidelity additions**: Masked SR writes (`& SR_WRITE_MASK`) in `LDC Rn,SR`/`LDC.L @Rn+,SR` to preserve unimplemented/reserved SR bit slots.
* **Atomic `TAS.B`**: Implemented a thread-safe, locked region atomic byte operation (`WorkRam::tas_byte`) to execute the test-and-set byte instruction (`0x4B12`) atomically under stripe write locks.
* **Delay Slot Adjustments**: Adjusted PC reporting logic in jumps/branches (`delay_slot_and_jump`) to temporarily subtract 2 from `self.pc` during execution and restore it afterwards, aligning Mimas PC tracking with Yabause's internal convention.
* **Reset Vector loading**: Extended SH-2 `reset()` to initialize CPU general-purpose registers `R0..=R14`, special registers (`gbr`, `vbr`, `mach`, `macl`, `pr`, `cycles`), clear pending interrupts, and fetch the initial PC and stack pointer (R15) from the vector table at VBR.
* **Access clamping**: Clamped BIOS reads to `offset & 0x7FFFF` with explicit bounds checking.

### Phase 2 — Add the 9 missing opcodes
* **Opcode implementations**: Added missing instructions: `SLEEP` (`0x001B`), `BRAF` (`0x0023`), `BSRF` (`0x0003`).
* **GBR-indirect byte operations**: Implemented GBR-relative logical functions `TST.B`, `AND.B`, `XOR.B`, `OR.B` under `0xCC00` - `0xCF00` decodes.
* **Signed Saturation Math**: Implemented `MAC.L @Rm+,@Rn+` and `MAC.W @Rm+,@Rn+` using signed multiplication and full saturation checks governed by the CPU `S` (Saturation) status flag.
* **Verification**: Wrote comprehensive unit tests (`test_sleep`, `test_braf_bsrf`, `test_mac_l_mac_w`, `test_gbr_byte_ops`) confirming arithmetic edge cases.

### Phase 3 — Exceptions and the address-space holes
* **Illegal-Instruction Exceptions**: Replaced silent no-op fall-throughs for illegal opcodes with a hardware-accurate exception sequence: pushes `SR` and `PC` to stack (decrementing `R15` twice), jumps through vector 4 (`[VBR + 16]`), increments elapsed cycle counts, and flags `illegal_instruction_flag = true`.
* **Diagnostic logging**: Implemented `log_illegal_once` to track and report every distinct illegal opcode-at-PC occurrence exactly once in stderr (`[ILLOP] pc=... opcode=...`) to help easily diagnose gaps during boot.
* **Address Partitioning**: Added memory regions for Cache Purge (`address >> 29` equal to 2), Cache Address Array (`address >> 29 == 3`), and Cache Data Array (`address >> 29 == 6`).
* **Cache execution support**: Supported flat backing arrays `cache_address_array` and `cache_data_array` inside `Sh2` using non-aliased addresses. Enabled `EXEC_FROM_CACHE` checking in `step()`, decoding opcodes directly from `cache_data_array` whenever `(pc & 0xC0000000) == 0xC0000000` is hit.
* **E2E test alignment**: Updated existing test vectors in `e2e-tests` (e.g. pc increment, max pc overflow, lockstep sync) to load valid NOP loop BIOS/Cache blocks rather than executing `0x0000` memory holes, verifying that the entire workspace compiles and tests pass 100% green.
* **Critical Bug Fixes**:
  1. **Area 5 CachePurge Fix**: Removed Area 5 (`0xA000_0000`) from the `CachePurge` translation match, restoring it to its correct hardware behavior as a cache-through mirror of physical memory.
  2. **TAS.B Non-RAM Fallback**: Modified the `TAS.B` execution path to fall back to a non-atomic read-modify-write (`raw_read_byte` followed by `raw_write_byte`) for addresses outside Low RAM and High RAM (e.g. SMPC registers), avoiding silent failures/no-ops. Wrote unit test (`test_tas_b_fallback`) confirming correctness.
  3. **Address 7 Cache Range**: Noted that the cache data array is strictly for area 6, but area 7 (`0xE...`/`0xF...`) fetches match the cache-execution boundary as designed.
