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

---

## Chapter 16 — Memory Bus Milestone 1: Phase 0 — Instrument the decode before changing it

To gather evidence on which address-space holes and mirror regions the real Sega Saturn BIOS actually accesses during boot, we implemented the memory bus instrumentation phase:

* **Bus Miss Log (`BUS_MISS_LOG`)**: Added a diagnostic logger that dedups on access keys `(area, block, is_write, width)` and logs a `[BUSMISS]` entry once per distinct key to prevent stderr noise, while still outputting the `self.pc` at the moment of access to make tracing with `tools/sh2dis.py` straightforward.
* **Instrumentation Hook (`check_bus_miss`)**: Wired a hook into all central memory-access entry points (`read_byte`, `write_byte`, `read_word`, `write_word`, `read_long`, `write_long`). The hook catches accesses where:
  * The address points to Area 2, 3, 5, or 6 (areas mapping outside the normal physical address space).
  * The address points to Area 7 but is below on-chip registers (`address < 0xFFFF_FE00`).
  * Bit 28 of the address is set (bit-28 alias).
  * The address belongs to Area 4 (Area-4 alias).
  * The translation lands on an `Unmapped` region (CS0, CS1, FRT Captures, WRAM mirrors, or general holes).
  * The in-window offset exceeds the documented physical device size (e.g. BIOS, Backup RAM, VDP1/2 registers, SCU registers, or High RAM mirror limits).
* **Env-Gate**: Gated all bus miss logging behind the environment variable `MIMAS_BUS_TRACE=1` to keep normal testing fast and quiet.
* **Fidelity Testing**: Wrote unit test `test_bus_miss_logging` verifying that synthetic miss hits generate exactly one distinct log and that duplicates are correctly suppressed. Verified that the entire workspace is green.

---

## Chapter 17 — Memory Bus Milestone 1: Phase 1 — Stage-1 area decode

To implement correct address-space isolation and resolve aliasing conflicts (such as cache scratchpad aliasing onto BIOS ROM), we completed the Stage-1 area decode:

* **Restructured `Sh2::translate`**: Modified the translation logic to switch on the top three bits (`address >> 29`) first:
  * **Area 0 / 1 / 4 / 5** follow the cache-through / bit-28 folding rules, resolving normal memory regions. Area 5 was specifically kept as cache-through normal memory per real hardware and Yabause consistency.
  * **Area 2** maps to `MemRegion::PurgeArea`. Reads return `0xFF` per byte. Byte and word writes correctly fall through to uncached writes, while longword writes act as a no-op associative cache purge.
  * **Area 3** maps to `MemRegion::AddressArray` supporting longword accesses only (byte and word accesses return `0` / behave as `Unmapped`).
  * **Area 6** maps to `MemRegion::DataArray` supporting all access widths (byte, word, long).
  * **Area 7** maps to `MemRegion::OnChip` if `>= 0xFFFF_FE00`, else it falls to `MemRegion::Unmapped`.
* **CPU-private Cache Arrays**: Added the cache array fields `address_array: [u32; 0x100]` and `data_array: Box<[u8; 0x1000]>` directly onto the `Sh2` struct. Since these are CPU-specific cache arrays, they are not shared inside `WorkRam`, avoiding mutex synchronization overhead.
* **Width-Correct On-Chip Dispatch**: Routed byte and word accesses to `MemRegion::OnChip` to `read_onchip` and `write_onchip` via shifted/masked register operations rather than returning `0`.
* **Testing & Integrity**: Added `test_memory_bus_phase_1` to verify correct Area 2/5 read returns, Area 5 normal cache-through behavior, cache array mirror bounds, CPU isolation, longword-only AddressArray accesses, and Area 7 unmapped bounds. Updated existing e2e and unit tests to compile and pass cleanly.

---

## Chapter 18 — Memory Bus Milestone 1: Phase 2 — Correct device sizes and per-region mirror periods

To resolve mirror-period bugs and enforce correct device sizes matching real hardware behaviors, we completed the device boundaries update:

* **High WRAM Correction**: Resized High WRAM allocation from 2 MB to 1 MB. Re-striped WRAM locks from 32 stripes of 64 KB to 32 stripes of 32 KB, maintaining lock granularity. Updated access/wrapping arithmetic to wrap offsets at 1 MB. Extended the translate window to `0x06000000..0x08000000` (entire 32 MB window) mirroring High WRAM every 1 MB.
* **VDP1/VDP2 Register Scaling**: Resized VDP1 registers buffer to 256 B and VDP2 registers buffer to 512 B. Masked VDP2 register offset checking for TVSTAT (`0x004`/`0x005`) to match mirrored addresses (e.g. `0x05F80204`).
* **SCU Register Scaling & Masking**: Resized SCU registers buffer to 256 B. Masked offset (`off &= 0xFF`) at the top of the longword SCU read and write arms to prevent out-of-bounds dropping of DMA triggers and DSP port accesses.
* **Odd-Byte Backup WRAM**: Resized backup RAM to 64 KB, and forced writes to odd-byte offsets (`off | 1`) to emulate the 8-bit SRAM hardware convention.
* **Sound RAM MEM4MB Mirror**: Implemented the `MEM4MB` mirror switch governed by SCSP register `$400` bit 9. Added a non-locking cached `mem4b` atomic flag in `WorkRam` (updated on SCSP register writes to `0x400`). If `mem4b == 0` (default), Sound RAM mirrors every 256 KB. If `mem4b == 1` and address exceeds 512 KB, reads return all-ones (`0xFF`) and writes are dropped.
* **CS2 20-bit Offset**: Passed 20-bit offsets to `MemRegion::Cs2Regs` and blocked reads/writes above 4 KB (returning `0`/dropping writes) to stop silent FIFO-on-CR1 aliasing without resizing the register stub prematurely.
* **BIOS Hardening**: Added explicit 512 KB bounds validation to `load_bios`, warning and truncating/padding images that mismatch.
* **Test Verification**: Wrote `test_memory_bus_phase_2` checking High WRAM mirrors, VDP1/VDP2/SCU scaling, TVSTAT mirroring, odd-byte Backup WRAM writes, Sound RAM `MEM4MB` modes, and CS2 boundary protection. Adjusted existing integration fixtures copy loops to target 32 KB stripes. Checked all workspace tests are green.

---

## Chapter 19 — Memory Bus Milestone 1: Phase 3 — Width-atomic, single-lock region accessors

To prevent multi-threaded data race bugs (torn reads/writes across cores and DMA) and optimize hot path lock contention:

* **High WRAM Single-Lock Accessors**: Refactored `read_high_ram_long`, `write_high_ram_long`, `write_high_ram_word`, and introduced `read_high_ram_word` on `WorkRam` to acquire the stripe lock exactly once for transactions that fall in a single stripe. Added safe, ordered dual-lock acquisition (lower index first) for transactions that straddle a stripe boundary to prevent deadlocks.
* **Unified CPU Accessors**: Added `raw_read_word`, `raw_read_long`, `raw_write_word`, and `raw_write_long` on `Sh2` and mapped their transaction logic to region-specific handlers (`raw_read_word_region`, `raw_read_long_region`, `raw_write_word_region`, `raw_write_long_region`). These methods translate the address once and acquire the target region lock exactly once.
* **Bus Wait Placement**: Preserved the call to `bus_wait()` at the CPU transaction boundary (in `read_word`, `write_word`, etc.) to prevent DMA from re-entering the arbiter.
* **Purge Area Fixes**: Hardened Purge Area handling to return `0xFFFF` on word reads and `0xFFFFFFFF` on longword reads, while longword writes act as no-op associative cache purges, and word writes fall through to uncached writes.
* **Stress Testing**: Added a high-concurrency torn-read stress test `test_torn_read_stress` executing parallel alternating longword writes and reads on both normal and straddled High WRAM boundaries for 200 ms, verifying no torn values occur. All 185 tests are green.

---

## Chapter 20 — CPU Milestone 1: Phase 4 — On-chip register file: storage, reset values, byte/word/long dispatch

To implement the on-chip register file structure and address-width access dispatch:

* **On-Chip Register Storage (`Sh2OnChip`)**: Created [`saturn-core/src/sh2_onchip.rs`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/saturn-core/src/sh2_onchip.rs) containing a dedicated register file structure populated with all hardware-defined reset defaults.
* **On-Chip Width-Access Routing**: Implemented non-recursive lookup helper methods `get_onchip_16` and `get_onchip_32` inside [`saturn-core/src/sh2.rs`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/saturn-core/src/sh2.rs) to avoid stack overflows from nested, width-varying calls. Mapped `MemRegion::OnChip` accesses through specific byte, word, and longword paths.
* **Write Masking & Quirks**: Implemented hardware masking for Standby Control (`SBYCR`), Cache Control (`CCR`), Standby Mode (`DRCR0`/`DRCR1`), and Interrupt Control (`IPRA`, `IPRB`, `VCRA`, `VCRB`, `VCRC`, `VCRD`, `ICR`, `VCRWDT`). Added the destructive byte-write quirks for `IPRB` and `VCRD`, where writing a byte to the high offset (`0x060` or `0x068`) clears/destroys the register's low byte, while byte-writes to the low offsets (`0x061` and `0x069`) are ignored.
* **Master/Slave Reset Selection**: Configured the Bus Control Register 1 (`BCR1`) to initialize with its physical master (`0x0000`) or slave (`0x8000`) construct flag, preserving this state across CPU resets while setting default bits (`| 0x03F0`).
* **Test Verification**: Wrote unit tests checking reset values (`test_onchip_p4_t1_reset_values`), write masking (`test_onchip_p4_t2_write_masking`), destructive byte-write quirks (`test_onchip_p4_t3_destructive_byte_write_quirks`), and width-access contracts (`test_onchip_p4_t4_access_width_matrix`). Verified that the entire workspace is green (all 194 tests passed successfully).

---

## Chapter 21 — CPU Milestone 1: Phase 5 — Real interrupt controller

To implement the on-chip Interrupt Controller (INTC) queue resolution, priority levels, and NMI handling:

* **Interrupt Pending Queue (`InterruptQueue`)**: Implemented a sorted, deduplicated-by-vector queue structure supporting a maximum capacity of 50 interrupts. It sorts pending interrupts ascending by level (highest priority last, preserving insertion order for ties).
* **Width & Priority Delivery**: Replaced the legacy if/else pending checks with an automated queue peeker that extracts the highest-priority pending interrupt and delivers it only if the level is strictly greater than the current SR interrupt mask (or if the level is 16, representing NMI).
* **Migrated Legacy Sources**: Re-routed VBLANK-IN (`0x40`, 15), VBLANK-OUT (`0x41`, 14), SMPC System Manager (`0x47`, 8), and Sound Request (`0x46`, 9) through `queue_send` and `queue_remove` methods, maintaining the thin wrappers and legacy fields to avoid public API breaks.
* **Non-Maskable Interrupts (`nmi`)**: Implemented `Sh2::nmi` which triggers the physical NMI, setting bit 15 of the on-chip Interrupt Control Register (`ICR`) and appending NMI (vector `0xB`, level 16) to the queue. When delivered, it clamps the resulting SR mask update to 15 (`0xF`).
* **Cross-Thread Source Injection**: Added `pub irq_in: Option<Arc<Mutex<InterruptQueue>>>` to `Sh2` and initialized it for Core 0 and Core 1 in `SaturnSystem::start`, falling back to `local_irq_in` when no external queue is wired.
* **Active State Synchronization**: Configured interrupt injection via `queue_send` to automatically wake parked cores by calling `sync.set_thread_active(core_id, true)`.
* **Verification & Regression Testing**: Added unit tests checking queue sorting/deduplication (`test_onchip_p5_t1_sort_dedupe`), masking thresholds (`test_onchip_p5_t2_strictly_greater_masking`), delivery pacing (`test_onchip_p5_t3_one_per_call`), NMI exception clamping (`test_onchip_p5_t4_nmi_clamp`), and delay-slot interrupt blocking (`test_onchip_p5_t6_delay_slot_no_interrupt`). Verified all 199 tests pass.

---

## Chapter 22 — CPU Milestone 1: Phase 6 — DIVU division hardening and overflow interrupts

To handle boundary overflows and prevent system-killing crashes during arithmetic divisions:

* **Hardened Division Guards**: Added checks for signed division overflows (`i32::MIN / -1` and `i64::MIN / -1`) in the 32÷32 and 64÷32 arithmetic paths in [`saturn-core/src/sh2.rs`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/saturn-core/src/sh2.rs), resolving division to a safe two's-complement wrap instead of letting Rust hard-panic.
* **Quotient Overflow Checks**: Implemented range bounds checking for 64÷32 divisions. When the computed quotient overflows 32-bit limits, Mimas now sets the overflow flag (`DVCR |= 1`), updates the output registers with clamped limits (`0x7FFF_FFFF` or `0x8000_0000`), and populates `dvdntul`/`dvdntuh`.
* **Mirrored/Indirect Registers**: Wired direct writes for `DVDNTUH` and `DVDNTUL` and confirmed that aliases between the `0x100` and `0x120` register ranges map correctly.
* **Interrupt Injection**: Integrated `divu_check_interrupt` which dispatches a queue interrupt dynamically mapped using priority (`IPRA` bits 12-15) and vector (`VCRDIV`) fields whenever `DVCR` overflow triggers with interrupt enable active.
* **Testing**: Added unit test `test_divu_p6_t1_crash_regression_overflow` confirming division guards wrap safely under `i32::MIN / -1` and `i64::MIN / -1`, and `test_divu_p6_t4_overflow_interrupt` asserting correct priority/vector interrupt dispatch during divide-by-zero. All 206 tests pass.

---

## Chapter 23 — CPU Milestone 1: Phase 7 — Free-Running Timer (FRT)

To implement the on-chip Free-Running Timer (FRT) registers, prescaler tick-advancing, compare matches, and interrupts:

* **FRT Registers & Behavior**: Corrected byte, word, and long write/read routing to the on-chip FRT registers.
  * Mapped `TIER` write logic forcing bit 0 set, and triggering ICI interrupts immediately upon re-arming when ICF is already set.
  * FTCSR byte-writes implement write-0-to-clear masks on status flags (bits 7-1) and direct assignment on `CCLRA` (bit 0).
  * TCR byte-writes parse TCR bits 0-1 to configure the prescaler division factor (`frc_shift` as 3, 5, or 7, leaving it unchanged on external clock selects).
  * TOCR write logic handles selection mapping (`TOCR & 0x10`) dynamically directing accesses at offset `0x014`/`0x015` to either the `ocra` or `ocrb` registers.
* **Prescaler Execution (`frt_exec`)**: Added `frc_leftover` fractional clock cycle accumulation and prescaler shifting. Compares OCRA and OCRB crossing boundaries sequentially. Supports the hardware-accurate wrapped Compare Match Miss deviation where a large cycle step jumping past the OCR value in the same tick it wraps does not flag Compare Match.
* **Interrupt Routing**: Mapped dynamic vector/priority lookups resolving OCIA (`VCRC` / `IPRB`), OCIB (`VCRC` / `IPRB`), OVI (`VCRD` / `IPRB`), and ICI (`VCRC` / `IPRB`) interrupts through the sorted INTC queue.
* **Retired Cycle Driving**: Configured the per-instruction execution step to drive FRT ticks dynamically via the retired instruction cycle count, guarded by a cheap `frc_shift <= 7` early-out.
* **Verification**: Wrote unit tests `test_frt_p7_t1_prescaler`, `test_frt_p7_t2_counter_advance`, `test_frt_p7_t3_ftcsr_write_clear`, `test_frt_p7_t4_ocra_ocrb_selector`, `test_frt_p7_t5_compare_match_cclra`, `test_frt_p7_t6_missed_compare_deviation`, and `test_frt_p7_t7_tier_ici_rearm`. All 213 tests pass.

---

## Chapter 24 — CPU Milestone 1: Phase 8 — Per-opcode cycle costs and memory wait states

To implement accurate per-instruction execution base cycle costs, memory access wait states, and sync-loop batch pacing:

* **Instruction Base Costs**: Added `get_base_cycles(opcode)` method that looks up instruction base cycles exactly as specified in the SH-2 Hardware Reference (HR §9). Every basic/shift instruction defaults to 1 cycle, with special instructions charging 2 (branches, RTS, jumps, multiplies), 3 (SLEEP, taken branches), 4 (TAS.B, RTE), or 8 cycles (TRAPA).
* **Memory Wait States**: Implemented `mem_cycles_r(addr)` and `mem_cycles_w(addr)` which map physical memory address regions to their respective Saturn bus wait states (e.g. BIOS/Backup ROM: 16 cycles; Low WRAM: 12 read/7 write cycles; CS0/CS2: 24 cycles; Sound RAM: 50 read/7 write cycles; VDP1 RAM: 50 read/2 write cycles; High WRAM: 0 read/2 write cycles). These wait-state cycles are safely injected immediately following bus arbitration (`bus_wait`), avoiding clock overwrite bugs.
* **Delay Slot Pacing**: Fixed delay-slot execution cost mapping (`delay_slot_and_jump`) to properly charge the base cycles of whichever instruction is executed inside the branch delay slot.
* **Synchronization Batching**: Replaced the previous `batch_mask` logic in `run_loop` with an explicit accumulator `pending_sync += delta` which correctly handles variable instruction cycle steps and triggers Core 0 / Core 1 synchronization exactly on the boundary threshold.
* **Clock Throttle Documentation**: Cleaned up the `ClockThrottle` doc comment in `throttle.rs` to reflect the updated cycle-accurate SH-2 implementation.
* **Testing**: Added unit tests `test_cycles_p8_t1_base_costs`, `test_cycles_p8_t2_wait_states`, `test_cycles_p8_t3_conditional_branch`, and `test_cycles_p8_t5_throttle_end_to_end`. All 217 tests pass.

## Chapter 25 — CPU Milestone 1: Phase 9 — On-chip DMA controller (DMAC), and a test that predated its own boundary condition

The user asked whether the Gemini session had run out of API budget mid-way through Phase 9 (DMAC) of `sh2-cpu.md`. It had: Phases 1-8 were all marked done, Phase 9 was still `[ ]`, and `cargo test --workspace` had exactly one failure, `sh2::opcode_tests::test_dmac_p9_t5_eat_table`, asserting `TCR0` was nonzero after 139 of an expected 140 cycles — both sides read `0`.

The implementation itself (`dma_exec`/`dma_proc`/`get_eat_clock`/`dma_transfer_cycles` in `sh2.rs`, plus the `Sh2OnChip` DMA register fields) turned out to be essentially complete and faithful to the reference — P9-1 through P9-9 were all genuinely there, including the budgeted `copy_clock`-accumulator transfer engine, the full source×dest `eat` latency table, `BusArbiter` locking, and the CHCR write protocol with its `CHCRnM` shadow. Only the one test was wrong, and it was wrong in an instructive way:

* **The real bug hunt.** Arming a DMA channel — the `DMAOR`/`CHCR` write that transitions `DME`/`DE` into the armed state (`sh2.rs:3465`-`:3513`) — synchronously calls `dma_exec()`, a fixed `dma_proc(200)` burst. That looked exactly like the kind of half-finished, conflicting-with-the-new-budgeted-model leftover code Phase 9 being unfinished would produce. It is not: `docs/hardware-reference/sh2-cpu.md:1327` documents this exact call, citing real Yabause `sh2core.c:2140` — arming a channel really does grant it a free 200-cycle `DMAExec()` head start on real hardware (or at least in the reference emulator). Confirmed this isn't a misread by hand-tracing the actual `dma_transfer_cycles` loop against the failing test's register values line by line.
* **Why the test broke.** `test_dmac_p9_t5_eat_table` set `TCR0 = 10` (140 cycles needed for a WRAM→WRAM transfer at 14 cycles/unit). The free 200-cycle arm-time burst alone is more than enough to finish that — so the transfer was already complete by the time the `DMAOR` write returned, before the test's own explicit `dma_proc(139)`/`dma_proc(1)` calls ever ran. The sibling tests `P9-T2`/`P9-T3`/`P9-T4` all use `TCR0 = 1` and so complete either way, which is why only `P9-T5` exposed this.
* **The fix.** Raised `TCR0` to `20` (280 cycles total — more than the 200-cycle arm burst can cover on its own) so the precise one-cycle-short-vs-exactly-enough boundary the test wants to assert actually lands on the explicit `dma_proc()` calls. Hand-verified the exact arithmetic against the real loop (200-cycle burst → 14 units done, 4 cycles banked; `dma_proc(79)` → 5 more units, 1 remaining, 13 banked; `dma_proc(1)` → completes) before editing, then confirmed with `cargo test --workspace` (118 passed in `saturn-core`, 0 failed workspace-wide).
* **Documentation.** Marked all of Phase 9's `P9-1`-`P9-9` and `P9-T1`-`P9-T6` checkboxes done in `docs/implementation-plans/sh2-cpu.md`, with inline notes on the two places (P9-2's arm-source-of-truth inconsistency, P9-5/P9-T5's arm-time free burst) where the reference itself is subtle enough to be worth calling out for the next reader. Marked `sh2-cpu.md` Phase 9 done in `.development/phased_development_plan.md`.

The takeaway for future sessions: a synchronous, seemingly-eager side effect inside a register write handler is not automatically leftover/conflicting code — check the hardware reference's own citation before assuming the "real" per-step budgeted engine and a write-time burst are in tension. Here they were always meant to coexist.

## Chapter 26 — SCU Milestone 2: Phase 1 — finishing the SCU DSP interpreter

With `sh2-cpu.md`'s Milestone 1 fully checked off, the roadmap's critical-path order (not `sh2-cpu.md`'s own remaining phase numbers, which are backlog/game-compat-only) points at Milestone 2: `scu.md` Phase 1, the SCU DSP's known 2-of-8-DMA-variants gap and a confirmed opcode-mask bug. Before writing any code, read `docs/hardware-reference/scu.md` §3.3-3.12 in full and cross-checked every claim directly against `../yabause/src/scu.c` line-by-line (`readgensrc`, `writed1busdest`, `writeloadimdest`, `dsp_dma01`-`dsp_dma08`, `step_dsp_dma`, `ScuExec`'s per-instruction preamble) rather than trusting the HR doc's summary alone — this surfaced two things the HR text didn't spell out on its own:

* **D-DSP-5's "`P`" is not the DSP's arithmetic accumulator.** The plan's own wording ("END/ENDI writes `P = PC + 1`") reads as if it means `self.p` (the 48-bit multiply/accumulate register moved by `MOV [s],P` etc.). Reading `scu.c:1932` directly showed it's actually `ScuDsp->ProgControlPort.part.P` — the Program Control Port's own low-byte PC-snapshot/reload field, a completely different piece of state that happens to share the letter "P" in Yabause's naming. Before this fix, `sh2.rs`'s End-instruction handler wrote the *pre-increment* `self.pc` into the control port instead of `pc+1`, so reading the control port back after a real DSP program's End instruction would show a stale, one-too-low PC.
* **Arming a DMA and reading Data RAM/registers mid-transfer force-completes it, using the same `step_dsp_dma`/`step_dma` function the normal per-instruction driver uses** (`scu.c:500-503` etc.: `dsp_dma_wait = 0; step_dsp_dma(ScuDsp);`) — not a separate "flush" code path. Mirrored this exactly (`ScuDsp::force_complete_dma`) rather than inventing a parallel dispatch function, so there's exactly one place the eight DMA variants are ever selected from.

What changed in `scu_dsp.rs`:

* **D-DSP-1 (phantom ALU ops).** `execute_alu`'s op selector was `(instruction >> 26) & 0xF` — masking to 4 bits discarded the instruction-class bits entirely, so a JMP/LPS/BTM/MVI's own encoding could accidentally land on bits 29:26 that happen to match a real ALU opcode, silently corrupting `Z`/`S`/`C`. The real switch never masks; classes other than Operation Commands naturally produce a 6-bit selector ≥ `0x10`, which just falls to `default`. Fix was a one-line unmask (`instruction >> 26`, no `& 0xF`) — the existing match arms already only define `0x0`-`0xF`, so anything ≥ `0x10` now correctly no-ops.
* **D-DSP-2/3 (deferred increments).** Added `inc_flg: [bool; 4]`, cleared at the top of `step()`, applied once after the whole instruction body (before `PC++`), with the one early-apply exception inside `MOV SImm,[d]`. Before this, `read_gen_src`/`write_d1_bus_dest` incremented `CT[n]` *immediately* at the point of use — wrong whenever one instruction reads the same `MCn` source twice (X-bus and Y-bus in the same cycle), since real hardware defers all increments to the end of the instruction, so both reads see the same word.
* **D-DSP-4 (DMA force-complete).** `read_gen_src`/`write_d1_bus_dest`/`write_load_im_dest`/`start_dma` all now call `force_complete_dma`, which calls the *same* `step_dma` the main loop uses (matching the real source's own call pattern exactly, not a reimplementation).
* **D-DSP-6 (region coverage).** Replaced the old 3-parallel-if-chain `read_long`/`write_long`/`write_word` with one shared `decode(address) -> Option<(DspRegion, usize)>`, extended to SCSP regs, VDP1 VRAM/framebuffer/regs, VDP2 regs, and CS2 — previously DSP DMA into any of those silently went nowhere.
* **D-DSP-7.** `dsp_dma03` (renamed from `dma_read_from_main_ram`) now only writes `RA0` back on the non-A-Bus path, matching the real quirk instead of always writing it back.
* **1b (six missing DMA variants).** Added `dsp_dma01`/`02`/`05`/`06`/`07`/`08`, a shared `dsp_dma_write_d0bus` (extracted from the existing `dsp_dma04`, now used by both `02` and `04`), and separate `dma_read_add`/`dma_write_add` helpers for the two genuinely-different address-increment decodes HR §3.8.4 documents. `step_dma`'s dispatch is now the real 8-way if-else chain in the reference's own order.

Testing: derived every new instruction word and its expected transfer result via a throwaway Python model (`dsp_dma_model.py`, not checked in) rather than hand-typing values that could just mirror the same misunderstanding as the implementation — all 8 tests (one per new variant, with the hold ones asserting *both* the data movement and the register-restore) passed on the first run. Added dedicated D-DSP-1/2/3/5/6 regression tests with hand-derived AC/P values chosen so a reverted fix produces a *different*, not coincidentally-identical, result (e.g. the D-DSP-1 test's `AC=5,P=8` makes the real SUB and the old bug's phantom ADD disagree on both `S` and `C`, not just happen to agree). `cargo test --workspace`: 129 passed (up from 118), 0 failed.

**Left undone, deliberately:** the plan's last testing item asks for the existing real-BIOS-program anchor test to be strengthened with a full independent Python hand-trace asserting final `MD[]`/register/flag state, not just termination. Decoding that 32-word program showed it uses conditional `MVI`, conditional `JMP` with the delayed-branch/loop-back timing, and `dsp_dma03`/`04` — a fully independent re-implementation of all of that is real, separately-scoped effort with its own risk of just being a self-consistent replica of the same code it's meant to check independently. The mechanisms it would exercise (D-DSP-1/2/3) already have dedicated regression tests above; the anchor test itself is unchanged and still passes. Recorded as an open item in `scu.md` rather than silently dropped.

## Chapter 27 — Real-game register capture: a third source of truth alongside the SH-2 manual and Yabause

Everything in `docs/hardware-reference/` up to this point comes from two sources: the SH-2
hardware manual and Yabause's C source (with `file:line` citations). Both describe what the
hardware/reference emulator is *capable* of, not what any specific real game actually exercises. To
get a third, independent data point, we instrumented a Yabause/YabaSanshiro build to hook every
VDP1/VDP2/SCU/SMPC register write during a real play session of a real commercial game (Magic
Knight Rayearth) and stream it out for aggregation — recording every **distinct** value written to
each register offset across the whole session, not just whether it was touched.

**Why this is worth having, concretely, not just in principle:** it's what a real game actually
writes, not what the reference emulator's C code merely accepts. Where a captured value matches
this repo's own documentation, that's independent confirmation. Where a register this repo marks
"not yet implemented" turns out to have real observed traffic, that's a concrete target value to
build against later instead of only a synthetic one — matching this repo's own rule (`CLAUDE.md`):
"Write regression tests from independently-derived values... never assert a value you haven't
independently derived."

**Added `docs/hardware-reference/real-game-capture-appendix.md`** (new file): the real, distinct
register-write values this session produced for every captured SMPC/VDP1/VDP2/SCU offset, with
names attached only where the offset could be independently matched against this repo's own
register tables — SMPC's `0x1F`/COMREG and `0x63`/SF matched exactly, confirming the small-offset
convention used is the same one already used throughout this directory. Everything else stays a
bare offset rather than guessing a name from a value pattern alone, which would violate the same
rule this data exists to serve.

**Cross-referenced into three existing files:**

* **`vdp2.md`** (§A.3, CYCA0L/U etc.): real observed steady-state values (`0x4455`/`0x66FF`/
  `0x0F1F` family) for the VRAM access cycle pattern registers — concrete target values for Phase 9
  ("VRAM access cycle patterns: what to actually build"), not yet implemented as of this writing.
* **`smpc-peripheral.md`** (register table): COMREG's real command distribution (mostly `0x10`/
  INTBACK, but real low-frequency use of `0x19`/`0x07`/`0x06`/`0x03`/`0x02`/`0x1A` too), and
  independent confirmation that real code does write DDR2 (`0x7B`) despite this file's own
  documented `[QUIRK]` that Yabause has no case for it at all.
* **`cs2-cdblock.md`** (§6.7, `Cs2GetIP`): two real-disc data points from this same game's actual
  `IP.BIN` — `firstprogsize` (`0xF4`) reads `0` on this real disc (the true executable length was
  only recoverable from the ISO9660 directory entry, not the IP.BIN header at all — worth checking
  `Cs2GetIP`'s caller doesn't trust a zero size as "load nothing" once a real disc like this is
  tested); and a note on this game's runtime overlay-loading pattern (16 `OL00.BIN`-`OL15.BIN`
  files, CD-read one at a time into a shared fixed high-WRAM range as the game progresses) as a
  concrete real-world CD-access scenario for whenever CD-ROM gets wired to CS2 (currently not
  integrated into the emulated system at all, per `CLAUDE.md`).

**Why selective, not a wholesale data dump:** the full session captured 144 distinct VDP2 offsets
alone, most of which stayed at a constant value the whole session. Only offsets independently
confirmed against this repo's own register tables, or landing on a not-yet-built phase, were called
out by name in the appendix and cross-referenced into the files above.

## Chapter 28 — An independent opcode-by-opcode audit of the SH-2 core against Yabause

`docs/implementation-plans/sh2-cpu.md` §0's opcode-coverage numbers ("133 decode, 9 missing, 4
wrong, silent no-op fallthrough") had sat unrefreshed since Phase 0/1 despite Phase 1 and Phase 2
both landing and being marked `[x]` further down the *same* document — a real instance of exactly
the staleness `CLAUDE.md`'s own documentation-tracking rule exists to prevent. This chapter is
about finally re-verifying what's actually true, prompted by a concrete, external data point: a
sibling project on this machine (`portal_to_another_world`, a separate Sega Saturn
static-recompilation tool, unrelated to mimas except for targeting the same hardware and having its
own from-scratch SH-2 disassembler) spent a session auditing its own opcode table line-by-line
against `sh2int.c` and found 8 real decode-table bugs — missing `LDS.L`/`STS.L`/`STC.L`/`LDC.L`
control-register variants, a missing `TAS.B`, `MAC.W`/`MAC.L` only partially covered, and a group of
0-operand pseudo-instructions requiring an exact 16-bit match when real hardware ignores the n
field. Given that project independently found real bugs with this exact method, the obvious question
was whether the same method, pointed at mimas's own, much more mature SH-2 core, would find
anything.

**Two machine-checked, exhaustive results, run first before touching any code:**

1. **Decode-set equivalence.** All 65536 possible opcodes, decoded both by a transcription of
   `sh2int.c`'s real `decode()` (`retroarch-cores/yabause/src/sh2int.c:2639-2921` — the sibling
   project's *own* vendored copy of this file has `portal_trace.c` instrumentation shifting its line
   numbers, so this checkout is the one to cite, never that one) and by walking `execute()`'s 7
   sequential `match opcode & mask` blocks (`sh2.rs:2536-3513`) in dispatch order. Result: 53,616
   encodings decode identically, 11,920 correctly reach the illegal-instruction path on both sides,
   **0 disagree, 0 arms shadowed**. `execute()`'s decode cascade is exactly as correct as this
   project's own docs already claimed it to be, verified rather than asserted.
2. **Cycle-cost equivalence.** The same treatment applied to `get_base_cycles` (`sh2.rs:654-753`) —
   a function that exists purely to *cost* opcodes and re-decodes them independently of `execute()`.
   This one **was not** clean: **D-21, D-22, D-23**, three real, previously-undocumented defects
   (full detail in `docs/implementation-plans/sh2-cpu.md` §0.9 and `.development/current_bugs.md`).
   In short: `MAC.W` and a common `MOV.L` addressing form had their cycle costs swapped by a wrong
   mask; `STC.L {SR,GBR,VBR},@-Rn` had no cost entry at all; and `RTS`/`RTE`/`SLEEP` used exact
   16-bit equality where real hardware (and `execute()` itself, correctly) treats the n field as a
   don't-care — the identical mistake `execute()` had already been fixed for once (D-5), made again
   independently in the separate cost function and never caught, because the existing
   `test_mac_l_mac_w` test calls `cpu.execute()` directly, bypassing `step()` and therefore
   `get_base_cycles` entirely.

All three fixed in `saturn-core/src/sh2.rs`, each backed by a new `#[test]` whose expected value was
derived from `sh2int.c`'s real `cycles +=` lines, never from running the code being tested — the
same discipline `CLAUDE.md` already requires. D-21's test was deliberately checked for real
regression-catching power, not just written and trusted: reverted the fix locally, reran just that
test, watched it fail with the exact wrong value (`left: 1, right: 3`), then restored the fix and
confirmed green again. `cargo test --package saturn-core`: 79 passed in the `sh2` module (was 76), 0
failed. `cargo test --workspace`: 226 passed, 0 failed. `cargo fmt` applied clean.

**What this chapter does not claim:** decode-set equivalence proves every opcode reaches *a*
handler, not that the handler's *behavior* is correct — that's a separate axis (semantic
correctness per mnemonic) that this pass did not exhaustively re-walk. A handful of handlers were
spot-checked along the way (`ROTL`/`ROTR`, `DIV0S`, `CMP/STR`, `SUBV`, `NEGC`, `MOVA`, the `0xC?00`
immediate group including the already-fixed D-1 `OR`/`XOR` swap, the `MOV.B/W @(disp,Rm),R0` nibble
forms) and all matched HR, but that is not the same as re-deriving all 142 mnemonics from scratch.
Recorded as open, not silently assumed done — see the status update inserted at the top of
`docs/implementation-plans/sh2-cpu.md` §0.2, and `.development/current_bugs.md`'s own "not
individually re-verified" section for D-1 through D-20.

**Documentation touched:** `docs/implementation-plans/sh2-cpu.md` (§0.2 status-update block, §0.9
D-21/22/23), `docs/hardware-reference/sh2-cpu.md` (§9.11, a third independent cross-check alongside
the interpreter and `sh2d.c`), `.development/current_bugs.md` (seeded from zero bytes — this file
existed empty since the project's first commit despite Phase 1's own text asking for it to be
populated).

## Chapter 29 — SCU Milestone 2: Phase 2 — a real SCU register file, and `Sh2::scu` stops being an `Option`

`saturn-core/src/scu.rs` was a 25-line stub (`Scu::start_dma`, `Scu::run_dsp_instruction`)
referenced only by two dead-code e2e tests, never constructed by `SaturnSystem` — `CLAUDE.md`'s
"Known architecture debt" section had named this exact gap for a while. `docs/implementation-plans/scu.md`
Phase 2 repurposes the module into the real thing: a typed `ScuRegisters` struct with every offset
from `docs/hardware-reference/scu.md` §1 named, `Scu::new()`/`reset()` applying §0.1's real reset
table (`DnAD = 0x101`, `DnMD = 0x7`, `IMS = 0xBFFF`, `VER = 0x04`, …), and `read_long`/`write_long`
implementing the R/W column exactly — including the non-obvious asymmetry that `D0AD`/`D0EN`/`D0MD`
and several others have **no CPU-visible read handler at all** (deviation #19: they return 0 on a
long read regardless of what was written, even though the write really did land in storage).

**The one real design decision, not just plumbing:** the plan's own text said
`Sh2::scu: Option<Arc<Scu>>`, mirroring the existing `Option<Arc<Mutex<T>>>` shape used for
`smpc`/the old `scu_dsp` (`None` in bare unit tests, falling back to a raw byte array). But this
phase also retires `WorkRam::scu_regs` — the exact byte array those fallbacks depended on. Keeping
`scu` genuinely optional would have meant either resurrecting a second storage array just for the
`None` case (defeating the entire point of "one real home for SCU registers") or breaking every
existing bare-`Sh2` test that pokes SCU registers directly (`test_scu_dma_direct`,
`peripheral_regions_are_real_readwrite_memory`'s SCU-regs probe, the mirroring test). The fix:
`Sh2::scu` is a plain `Arc<Scu>`, not `Option`. `Sh2::new()`'s struct literal gives every `Sh2` —
including ones built with no `SaturnSystem` at all — its own private, fully-real `Scu` by default;
`SaturnSystem::start` then overwrites the field with a shared clone for Core 0 (register access)
and Core 6 (DSP stepping), the same "construct a working default, then swap in a shared instance
post-construction" shape the `bios: Arc<Vec<u8>>` field / `set_bios_arc` already used, just applied
to a new field instead of an existing one. `Sh2::new()`'s 3-argument signature never changes.
Every pre-existing bare-`Sh2` SCU test kept passing unmodified because of this choice — the
alternative (matching the plan's literal `Option` wording) would have required inventing a second
fallback store this phase was specifically trying to eliminate.

`Scu`'s internal shape follows the plan's §2 sketch: `regs`/`irq`/`dma`/`timers`/`dsp`, each behind
its own `Mutex`, so Core 0 touching registers and Core 6 stepping the DSP never serialize against
each other on one big lock — mirroring `WorkRam`'s per-region-lock rationale. `irq`/`timers` are
empty placeholder structs today (Phases 3/5's future homes); `dma` holds one real field,
`transfer_number` per level, consumed only by `DSTA`'s live busy-bit recompute (`read_long(0x7C)`)
— since the pre-Phase-4 `Sh2::execute_scu_dma` still completes every transfer synchronously inside
one register write, that field never actually has an observable nonzero window yet, but the wiring
is real and Phase 4 has somewhere to put genuine state instead of retrofitting `DSTA` a second
time.

Two smaller, deliberate divergences from the reference, both already flagged by the plan's own §9
table and kept exactly as it recommended: byte access to any SCU offset other than `0xA7` returns
a real (permissive) per-byte view of the true register value instead of the reference's hard
"unhandled, log, return 0" (deviation #2); 16-bit SCU access stays live as two composed byte
accesses rather than copying the reference's "all word access is a no-op" [QUIRK] (deviation #20).
`0xA7` itself keeps its real AND-only-clear semantics (`IST &= 0xFFFF_FF00 | val`), which is not a
divergence — that's the one byte-addressable register the reference genuinely implements.

`execute_scu_dma` (the pre-Phase-4 synchronous stand-in, unchanged in *behavior* — fixing its own
D-DMA-1..9 bugs is explicitly Phase 4's job) had its storage access rerouted from
`work_ram.scu_regs` to a new pair of internal-engine accessors, `Scu::raw_read_long`/
`raw_write_long`, which bypass the CPU-facing read-visibility rules the same way Yabause's own C
reads `ScuRegs->D0AD` as a plain struct field rather than through the register-read dispatch
function. Without that distinction, routing the DMA engine through the public `read_long` would
have made `D0AD`/`D0MD` always read back 0 to the *engine itself*, breaking the direct/indirect
mode decode and the address-increment table it depends on — an easy trap this phase's design
avoids by keeping "what the CPU can observe" and "what the engine actually configured" as two
different accessors from the start, rather than fixing it after the fact.

Verification: `cargo test --workspace` green throughout — 139 passed in `saturn-core`'s own test
binary (7 new in `scu::tests`, all previously-passing SCU-touching tests in `sh2.rs` unmodified
and still green), 70 in `e2e-tests` (one rewritten to assert real reset values, one deleted with
an explanatory comment per the plan's own instruction — "invalid channel" was never a hardware
concept), plus the existing adversarial/sync/fixture suites. `cargo fmt` applied clean.

**Documentation touched:** `docs/implementation-plans/scu.md` (Phase 2 checklist and its testing
section both flipped to `[x]`, with the `Option` deviation recorded inline),
`.development/phased_development_plan.md` (Milestone 2's Phase 2 line), `CLAUDE.md` ("Known
architecture debt" — the `scu.rs`-is-dead-code bullet rewritten to describe what's real now and
what Phase 4 still owes).

## Chapter 30 — SCU Milestone 2: Phase 3 — one interrupt controller instead of five

Phase 2 gave `Scu` a real register file. Phase 3 gives it the thing that file was actually for:
`docs/implementation-plans/scu.md`'s own framing calls this out directly — before this phase, four
unrelated bools on `Sh2` (`vblank_pending`, `vblank_out_pending`, `smpc_irq_pending`,
`sound_req_irq`) were each their own miniature interrupt controller, prioritized by a hardcoded
if-chain in `service_pending_interrupt`. Bolting a fifth, SCU-owned controller on next to them
would have made five mechanisms instead of one; the plan is explicit that this is a *replacement*,
not an addition, and that's what landed.

**The discovery that shaped everything else.** Reading `docs/hardware-reference/scu.md` §4 closely
enough to implement it revealed that real hardware's SCU isn't a pass-through — it's a genuine
masking/staging layer sitting *between* every interrupt source and the SH-2's own interrupt queue.
`IMS` gates whether a source's interrupt reaches the SH-2 immediately (unmasked: deliver now, `IST`
untouched — the reference's own comments call this the single most surprising asymmetry in the
whole design, since an interrupt that never had to wait never latches a status bit) or waits in a
*separate* SCU-side queue (`ScuIrq`, distinct from and upstream of `Sh2`'s existing `InterruptQueue`
from `sh2-cpu.md` Phase 5) with its `IST` bit latched, until a write to `IMS`/`IST`/`AIACK` asks
`test_interrupt_mask` (the real `ScuTestInterruptMask`) to look again and drain at most one
non-external entry, highest level first. Getting this two-queue relationship right — `ScuIrq::queue`
holds only masked, waiting interrupts; `Sh2::irq_in`'s `InterruptQueue` holds only interrupts that
have already cleared the SCU's gate and are now genuinely pending CPU delivery — was the load-bearing
design decision the rest of the phase hangs off.

**`Sh2::irq_in` stops being `Option`, for the same reason `Sh2::scu` did in Phase 2.** `Scu` needs
to push delivered interrupts into the *same* queue object the SH-2 itself polls in
`service_pending_interrupt` — a sibling relationship, not a child one, since `Sh2` owns `Scu` but
`Scu` also needs a handle back into `Sh2`'s queue. Making `irq_in: Arc<Mutex<InterruptQueue>>`
always-present (no `None` case, `local_irq_in` field deleted entirely) and having `Sh2::new()`
construct both `irq_in` and `scu` together, then call `scu.set_master_target(irq_in.clone())` before
either is used, keeps a bare `Sh2::new()` in unit tests fully self-consistent — `Scu` always has
somewhere real to deliver to, even with no `SaturnSystem` involved — while `SaturnSystem::with_slack`
does the identical two-line wiring dance with the shared, cross-thread instances instead. Same
pattern as `bios`/`scu` before it: construct a working default, then let the real system swap in a
shared clone.

**A real, pre-existing bug got fixed as a side effect, not by special-casing it.** Before this
phase, *both* Master (Core 0) and Slave (Core 1) `Sh2::run_loop` ran their own independent
wall-clock VBLANK-IN/OUT timer — two unsynchronized frame clocks, plus a *third*, Core 3's own
16.6ms render tick, all claiming to know when VBLANK happens. Moving VBLANK generation onto Core 3
(the one thread whose frame tick is the actual source of truth) and having it call
`Scu::vblank_in()`/`vblank_out()` directly means there is now exactly one VBLANK clock in the whole
system; the slave's queue only ever sees VBLANK/HBlank through the SCU's real hardwired mirror
(vector `0x42`→slave `0x41`/level 1, `0x40`→slave `0x43`/level 2 — see §4.2's closing paragraph),
not through a second clock generating its own vectors. `Sh2::tvstat_word()` was also rewired off
Core 0's now-deleted timer onto a new `WorkRam::vblank_active: AtomicBool` (Release/Acquire,
mirroring the discipline the rest of `shared_buffers.rs` already uses) that Core 3 alone writes and
either SH-2 core can read — no special logic needed, the fix fell out of centralizing generation in
the first place.

**Sound-request interrupt made event-driven — directly answering the user's performance
instruction mid-phase** ("please be very careful with performance on sound... no polling loops").
The pre-existing sound-request path was a shared `Arc<AtomicBool>` set by `M68k::write_byte`'s MCIPD
handler and polled by `Sh2::service_pending_interrupt` on *every single SH-2 instruction* — a hot-path
poll on the CPU most sensitive to being kept fast. `M68k::scu: Option<Arc<Scu>>` replaces the
`AtomicBool` field; the MCIPD write handler now calls `scu.sound_request()` directly, at the exact
moment the real register write happens, and the corresponding poll inside
`service_pending_interrupt` was deleted outright — a net removal of per-instruction work, not just an
avoidance of adding more. Core 5's SCSP synthesis loop itself was not touched in this phase at all.

**Deviation #17, fixed rather than copied (per the plan's explicit instruction not to
transliterate it):** the reference's own `ScuRemoveInterruptByCPU` is dead code — a C operator-
precedence bug (`0x01 == 0` binds before the intended comparison) that makes its guard always
false, so a queued interrupt whose `IST` bit the CPU clears by hand leaks in the real queue forever.
`test_interrupt_mask` opens with `irq.queue.retain(|q| regs.ist & q.statusbit != 0)` — the *intended*
behavior — before doing anything else, so a stale entry never survives to the next drain.

**One documented simplification, not yet a real cost:** the slave mirror in `Scu::send` fires
whenever a slave `InterruptQueue` is wired at all, rather than checking real hardware's
`yabsys.IsSSH2Running` gate — doing that properly would mean giving `Scu` a `LockStepSync` handle
just for this one narrow case. Harmless today: Core 1 stays parked via
`LockStepSync::park_while_inactive` and never calls `step()` (so never polls its queue) until SSHON
actually wakes it, so a stray pre-SSHON mirror entry just sits unread. Flagged in `scu.rs`'s doc
comment and in `scu.md`'s Phase 3 checklist for revisiting if a real boot trace ever shows otherwise.

**Regression risk taken seriously, then checked, not just asserted away.** `IMS` resets to `0xBFFF`
— everything masked by default — so this phase is the first time interrupt delivery is genuinely
conditional rather than unconditional; if real BIOS code doesn't unmask `IMS` early, Phase 3 could
regress boot progress even with every unit test green. Two pieces of evidence say it doesn't:
`docs/hardware-reference/real-game-capture-appendix.md`'s captured trace shows SCU offset `0xA0`
(`IMS`) written 114,805 times in one session, dominated by near-all-ones values, meaning real BIOS/
game code actively manages this register rather than leaving it masked; and a post-migration
`MIMAS_BOOT_WATCH_SECS=90` run against `Sega Saturn BIOS (USA).bin` (release build) showed Core 0
still executing real BIOS instructions, settling into the same `0x22xx`-`0x4Axx` CD-header polling
loop observed before this phase, with no crash, no panic, and no stall short of that point.

Verification: `cargo test --workspace` green (150 in `saturn-core`'s own binary — 12 new in
`scu::tests`, ~13 rewritten in `sh2.rs` to unmask `IMS` before triggering an interrupt or to read
`cpu.irq_in.lock().unwrap()`/`cpu.queue_peek()` instead of the deleted bool fields — plus 70 in
`e2e-tests` and the existing adversarial/sync/fixture suites, 0 failed), `cargo fmt --all` applied
clean, plus the boot-watch run above. Two test-design mistakes were caught and corrected while
writing this phase's own tests, not left in: a first draft of the source-table test forgot `IMS`
defaults to all-masked and had to unmask before calling each source method; and a first draft
asserted a *masked external* interrupt still latches its `IST` bit, which traced back against
`Scu::send`'s own (correct) branch structure as wrong — the reference's "queue and latch" `else`
branch only exists for non-external sources, so a masked external is simply lost, identical to the
`AIACK == 0` case — and was rewritten into `masked_external_interrupt_is_lost_not_queued` asserting
the right, negative behavior instead.

**Documentation touched:** `docs/implementation-plans/scu.md` (Phase 3's 3a/3b/3c checklists and
its testing section flipped to `[x]`, with the slave-mirror simplification and the deviation #17
fix recorded inline), `.development/phased_development_plan.md` (Milestone 2's Phase 3 line),
`CLAUDE.md` ("Known architecture debt" — the `scu.rs` bullet extended to describe the real
interrupt controller, the centralized VBLANK clock, and the event-driven sound-request path).

## Chapter 31 — SCU Milestone 2: Phase 4 — a real, budgeted DMA engine finally moves off Core 0

Phase 3 gave `Scu` a real interrupt controller. Phase 4 retires the last synchronous stand-in on
the SCU side: `Sh2::execute_scu_dma`, which ran an entire DMA transfer — direct or indirect,
however large — inside one SH-2 register write, holding `BusArbiter`'s single global lock for the
whole copy. A CPU write to `DnEN` now only ever marks a level pending and wakes Core 6
(`SaturnSystem`'s `scu-dma-dsp` thread); a real, budgeted engine (`Scu::step_dma_pass`) does the
actual work there, releasing the bus lock between short bursts instead of holding it for an entire
transfer.

**The handoff pattern, mirrored deliberately from the DSP's own `EX` bit.** `Scu::request_dma_trigger`
(called from `Sh2::write_long`, on the CPU's own thread) does no transfer work at all — it checks
`DnMD[2:0] == 7` (§2.2 path (a), fixing **D-DMA-7**: the old stand-in triggered on any `DnEN` bit-0
write regardless of mode) and, if satisfied, sets `DmaLevel::trigger_pending` and returns `true` so
`Sh2::write_long` can call `sync.set_thread_active(6, true)` — the exact same wake call the DSP's
`EX` control-port write already makes. `Scu::service_trigger`, called only from Core 6's own
`step_dma_pass`, does the real work: snapshot `DnR`/`DnW`/`DnC`/`DnAD`/`DnMD` into a `DmaLevel`
working copy (§2.1), decode `read_add`/`write_add` from `DnAD` (§1.2), apply the count clamp
(§1.2, fixing **D-DMA-4**: level 0 keeps the full 32 bits with `0` meaning `0x100000`; levels 1/2
clamp to 12 bits with `0` meaning `0x1000`), and — for indirect mode (`DnMD` bit 24, fixing
**D-DMA-1**) — load the first descriptor from the table `DnW` actually points at, rather than the
old stand-in's `DnR` (fixing **D-DMA-2**/**D-DMA-3**, the wrong-register bug covered below).

**Fill mode's read-once cache is a real correctness requirement, not an optimization.** §2.4's
"constant source" test (Low WRAM, High WRAM, Sound RAM, VDP1/VDP2 RAM) exists in the reference
because those regions can't change out from under a synchronous, single-threaded transfer — so
Yabause just reads the source long once and reuses it. Mimas's transfer is *not* synchronous: a
large fill can span many Core-6 budget passes, with Core 0/1 free to run (and potentially write
that same source address) in between passes. Re-reading a "constant" source on every iteration —
the naive port — would therefore diverge from real hardware on any fill transfer wide enough to
span more than one pass: real hardware's single early read would miss a later CPU write that
Mimas's naive re-read would pick up. `DmaLevel::fill_cached`/`fill_value` close this gap: the
source is read exactly once, at trigger time (or at each descriptor load, for indirect fill
transfers), and reused for the rest of that level's run regardless of how many passes it takes.
A non-constant source (anything else, treated as a live register) is deliberately left uncached
and re-read every iteration, matching the reference's own asymmetry. Two tests capture the
distinction directly: `fill_mode_constant_source_is_cached_once_not_re_read_across_passes` mutates
the source *after* the cache should have primed and confirms it never shows up; the mirror-image
`fill_mode_non_constant_source_is_re_read_live_every_iteration` mutates a VDP2 register between two
passes and confirms the *next* iteration does observe the change.

**Threading (§2.6/4c), the part the plan itself flagged as needing the most care.** `Scu::step_dma_pass`
services all three levels per Core-6 pass, each with its own private copy of a fixed budget
(`DMA_BUDGET_PER_PASS = 512`, chosen in `lib.rs` since Core 6 has no per-SH-2-instruction cycle bus
to derive the reference's `timing << 4` from), in strict textual order 0 → 1 → 2 with no priority
arbitration between levels — matching the reference exactly rather than inventing a scheme Mimas
has no way to validate. `BusArbiter::lock_for_dma()`/`unlock_from_dma()` wrap one whole pass (bounded
to `3 × budget_per_level` iterations), not the transfer's entire duration — the single change that
actually solves the "Core 0 stalls for the whole copy" problem the old stand-in had. DMA and DSP
share Core 6 by simple sequential serialization inside one loop iteration (`if dsp.is_executing()
{...} if dma_active() {...}`) rather than a counting bus lock — the simpler of the two options the
plan offered, and sufficient since only one real engine is ever likely to be busy at a time in
practice. Core 6's own `sync_core(6, cycles)` call happens *after* `step_dma_pass` has already
released the bus lock for that pass, preserving the ordering the plan calls out by name: Core 6
must never hold the bus lock while also blocking on other cores' cycles in `sync_core`, or a
DMA-stalled Core 0/1 (deactivated inside `acquire_bus_sync`) could never report progress to unblock
it.

**A second real bug found and fixed while implementing 4a, not in the original checklist.** §1.2
states plainly that "the whole written value is stored to `DnEN` after the go-check" — meaning
real hardware does *not* clear `DnEN` after an immediate (path (a)) trigger; only the still-deferred
factor path (b) does, as its one-shot arming mechanism. The old `execute_scu_dma` stand-in cleared
`DnEN` unconditionally at the end of every transfer, immediate or not — a divergence nobody had
reason to notice before, since `DnEN` has no CPU-read handler either way (deviation #19), so the
difference was never observable from the guest's own reads. The new engine simply never clears it
for path (a), with a dedicated regression test
(`immediate_trigger_leaves_den_bit_set_real_hardware_never_auto_clears_it`) reading the *internal*
stored value via `raw_read_long` to make the distinction observable in a test even though the CPU
itself could never tell the difference.

**The D-DMA-2/3 regression guard took a different shape than the plan sketched.** The plan proposed
feeding the engine the *old* buggy layout (end bit in the count word, table at `DnR`) and asserting
"the wrong answer" — but "the wrong answer" from a genuinely malformed table is not a clean, single
assertable value. `indirect_table_pointer_comes_from_dnw_not_dnr_regression_guard_d_dma_2_3` proves
the same thing more directly instead: point `DnR` at a *different*, equally well-formed
descriptor table (writing to a decoy destination) and `DnW` at the real one, then confirm only
`DnW`'s table was ever honored and the decoy destination was never touched. This is a strictly
stronger guarantee than "produces a different value" — it proves the engine never reads from `DnR`
in indirect mode at all, not just that it happens to read something different.

**One Mimas-only addition with no reference counterpart at all**: `run_level`'s indirect-chain walk
caps itself at 4096 chased descriptors (`log_scu_dma_malformed_chain_once`), forcing completion and
logging rather than looping forever. A malformed or non-terminating chain (no descriptor ever
setting the end bit) has the identical failure mode in the reference — an infinite loop — but there
it only ever hangs Yabause's single emulation thread. On Mimas, Core 6 is its own dedicated thread
inside `LockStepSync`'s bounded-slack lockstep, where a stuck-forever "active" core eventually blocks
every other core waiting for it to report progress that will never come — turning a local hang into
a whole-system one. The cap changes nothing for any well-formed chain (real ones terminate in a
handful of descriptors); it only guards against a failure mode Mimas's own architecture makes worse
than the reference's.

**Two real, non-obvious threading bugs were found and fixed in this phase's own cross-thread test**,
not in the engine itself — worth recording since they're exactly the kind of thing this project's
"write regression tests independently, verify they'd actually fail" discipline exists to catch:

1. *A lost-wakeup race in the test's own harness.* Spawning the Core-6 stand-in thread and
   triggering the DMA from the main thread with no synchronization between them let the trigger's
   `set_thread_active(6, true)` run *before* the spawned thread's own startup
   `set_thread_active(6, false)` — the later call silently clobbered the earlier wake, and the
   thread parked forever with nothing left to reactivate it. Diagnosed with temporary `eprintln!`
   checkpoints and `--nocapture`, not guesswork; fixed with an explicit `AtomicBool` readiness
   barrier the main thread waits on before triggering. This is a latent hazard in
   `LockStepSync::set_thread_active`/`park_while_inactive`'s contract itself (a wake can only ever
   be observed by a thread that's already checked its state under the shared mutex) — real
   `SaturnSystem` avoids it in practice only because Core 6 finishes its own startup deactivation
   long before real BIOS code could possibly reach a DMA-triggering register write, not because the
   race is structurally impossible.
2. *A `LockStepSync`-modeling mismatch, not a production bug.* An early version of the test left
   cores 0-5 at the constructor's default `active = true` while only driving cores 0 and 6 for
   real. Core 6's own `sync_core(6, cycles)` heartbeat then computed its slack against cores that
   never reported any cycles at all (frozen at 0 forever), and blocked permanently once its own
   cycle count drifted past the slack limit — a hang entirely of the test's own making, since real
   `SaturnSystem` has genuine threads behind every one of those indices. Fixed by explicitly
   deactivating every index the test doesn't actually drive.

Both bugs were reproduced and root-caused before being fixed — including confirming the fix by
re-running `cargo test --workspace` ten times in a row after the change (it had failed roughly one
run in three before), since flaky cross-thread tests are exactly the kind of thing that looks fixed
after one clean run and isn't.

Verification: `cargo test --workspace` green — 165 in `saturn-core`'s own binary (18 new: 15 in
`scu::tests`, 2 rewritten in `sh2.rs`'s `test_scu_dma_direct` replacement, plus the format pass), 70
in `e2e-tests` (untouched — `test_tier3_combination_f4_f5_scu_dma_cdrom_transfer` tests a separate,
unrelated `Cdrom::dma_triggered` flag, not this engine), 9 in `adversarial_tests` (1 new), plus the
existing sync/fixture suites, 0 failed, confirmed clean across ten consecutive full-workspace runs.
`cargo fmt --all` applied clean. A post-migration `MIMAS_BOOT_WATCH_SECS=90` run against
`Sega Saturn BIOS (USA).bin` (release build) reached the identical CD-header polling checkpoint
(`0x22xx`-`0x4Axx` PC oscillation) observed before this phase, with no crash, panic, or stall short
of that point.

**Documentation touched:** `docs/implementation-plans/scu.md` (Phase 4's 4a/4b/4c checklists and
testing section flipped to `[x]`, with the `mode`-field omission, the busy-retrigger-flush
simplification, and the malformed-chain safety cap all recorded inline),
`.development/phased_development_plan.md` (Milestone 2's Phase 4 line), `CLAUDE.md` ("Known
architecture debt" — the `scu.rs` bullet extended to describe the real DMA engine and note
start-factor triggers/timers as the remaining Phase 5/6 gaps).

## Chapter 32 — Cores 2, 4, 7 finally park; a real trap found trying to do the same for Core 3

Triggered by the user watching a live boot window in htop and noticing 5 of 8 `mimas_window` threads
pegged at 40-98% CPU for a session showing nothing but a black screen — a concern this project's own
`CLAUDE.md` had already named ("Only Core 1 and Core 6 truly park... not the 'park when idle' model
the rest of this doc describes as the target") but never acted on. Confirmed by reading the actual
`SaturnSystem::start` code fresh rather than trusting the doc comment from memory: Cores 2
(`vdp1-draw`), 3 (`vdp2-composite`), 4 (`m68k-sound-cpu`), 5 (`scsp-synth`), and 7 (`smpc-cd-block`)
all looped forever on `thread::yield_now()` plus a `sync_core` heartbeat regardless of whether they
had any real work — `thread::yield_now()` is a scheduler hint, not a block, so on a mostly-idle host
each just gets rescheduled immediately and spins at ~100% of a core forever.

**Cores 2 and 7 have zero real work in the current implementation, full stop** — VDP1 execution
actually runs on Core 3 (a pre-existing, already-documented "Core 2 vs Core 3" mismatch), and no
SMPC/CD-block logic runs on Core 7 at all (SMPC commands execute via `Sh2`/`Smpc` methods on Core
0's own thread; `Cdrom` isn't wired into the address space yet). Both now deactivate themselves once
and park forever via `LockStepSync::park_while_inactive`, exactly like Core 1 (before SSHON) and
Core 6 (before the DSP/DMA engine has anything to do) already did. Nothing wakes them today; a
future phase that gives either real work will need its own wake call, mirroring the DSP's `EX` bit /
DMA's `DnEN` trigger.

**Core 4 got the same park treatment, but with a real event to wake on.** Unlike 2/7, Core 4 has two
genuine states — idle while SNDOFF, real work while SNDON — so it now mirrors Core 6's DSP/DMA shape
exactly: deactivate at startup, `park_while_inactive(4)`, and once reactivated run an inner loop
(reset the M68K, step it while `m68k_control` stays true, re-park the moment it goes false again).
The wake call (`sync.set_thread_active(4, true)`) was added at both places that flip `m68k_control`
true on SNDON -- `Sh2::apply_smpc_effects` (the real, wired-in path) and `smpc_execute_command` (the
bare-`Sh2` fallback) -- right alongside the existing `flag.store(true, Release)`, exactly the same
shape SSHON already uses to wake Core 1. This directly extends the "no polling on the sound path"
fix from `scu.md` Phase 3 (which removed a per-*instruction* poll of a sound-request `AtomicBool`) to
also remove this per-*loop-iteration* poll of `m68k_control` -- the same class of waste, a different
thread. (A stale doc comment claiming Sound RAM writes get published "to Core 3's subsequent Acquire
load" was also fixed in passing -- it's Core 4 that actually reads `m68k_control`, not Core 3; simple
copy-paste drift from whenever that comment was first written.)

**Core 5 got a smaller, different fix**: real hardware's SCSP synthesizes continuously regardless of
the M68K's own run/stop state, so unlike 2/4/7 it can never park -- but it previously ran completely
unthrottled regardless of `self.speed` (the shared multiplier every other core already respects),
spinning flat-out generating audio far faster than anything could play it back. Wired into the same
`ClockThrottle` mechanism the SH-2s and M68K already use, paced against a new `SCSP_SAMPLE_RATE_HZ`
constant (`throttle.rs`) derived from the existing `M68K_CLOCK_HZ` citation (`44_100.0 * 256.0` Hz on
the master clock / `256` cycles per output sample = `44_100.0` Hz) rather than a second independent
literal. A no-op today (`ThrottleSpeed::Unthrottled` is the default, so this doesn't change current
behavior or CPU usage), but makes Core 5 consistent with the rest of the system's speed-slider model
instead of being the one core that silently ignores it.

**Core 3 got the first fix tried, and it was wrong — caught by a real boot-watch run, not a unit
test.** The first attempt replaced Core 3's spin with a sleep until whichever deadline (next frame
tick or the pending VBLANK-OUT edge) came sooner, capped at 1ms so shutdown stayed responsive --
symmetric with Core 2/7's fix, and it compiled, and every existing unit/integration test stayed
green. A `MIMAS_BOOT_WATCH_SECS=90` run immediately after told a different story: Master SH-2 got
"stuck" oscillating between two addresses (`0x2B0`/`0x2B2`) for the entire run, with the boot-watch's
own settle-detector firing. Decoding the actual BIOS opcodes there (`MOV.L R4,@R3` / `DT R6` / `BF/S`
back -- a completely ordinary memory-clear loop, `tools/sh2dis.py`-equivalent hand-decode, not
guesswork) proved this wasn't a new infinite loop: the loop was making genuine progress, just at a
small fraction of its normal speed. The mechanism: `LockStepSync`'s bounded-slack model requires
every *active* core to report its cycle count often enough that a fast, unthrottled core (Master
SH-2, executing millions of instructions/sec) never drifts more than `slack_limit` cycles ahead of
the *slowest active* one -- and Core 3, unlike Core 2/7, cannot fully deactivate (it has genuine
periodic work and never has a reason to stop being "active"). Dropping its reporting frequency from
"as fast as a tight spin loop can go" to "at most once per 1ms" was coarse enough that Core 0 spent
most of its time blocked waiting for Core 3's next report, rather than executing -- slowing the
memory-clear loop (and, by extension, all of early boot) by roughly two orders of magnitude. Reverted
outright back to the original `thread::yield_now()` spin, with a doc comment recording exactly why,
so a future session doesn't rediscover this the slow way. Core 3 belongs in the same "always active,
spins on purpose when unthrottled" category as Core 0/1/5, not the same category as Core 2/4(idle)/7.

**Net effect, verified empirically with `ps -L` on a live `mimas_window` process** (not just read
from code): before, 5 of 8 core threads (`vdp1-draw`, `vdp2-composite`, `m68k-sound-cpu`,
`scsp-synth`, `smpc-cd-block`) spun at 25-99% CPU regardless of real work. After: `vdp1-draw` and
`smpc-cd-block` sit at a genuine 0.0%, always; `m68k-sound-cpu` sits at 0.0% until SNDON fires, then
genuinely runs (confirmed live via `MIMAS_DEBUG_M68K=1`: `[M68K] reset: SP=0x0000A000 PC=0x00001000`,
followed by real execution through the uploaded sound driver); `sh2-master`, `vdp2-composite`, and
`scsp-synth` remain busy by design, matching real hardware's own continuously-running components.
As a side effect (removing an accidental throttle plus freeing host CPU previously wasted on three
permanently-spinning placeholder threads), a real BIOS boot-watch now reliably progresses much
further than any run earlier in this project's testing had reached within the same time budget --
past the CD-block polling loop this session had been treating as the expected settling point, all
the way to a real, previously-undocumented-in-testing checkpoint: SNDON firing and the M68K driver
actually executing, hitting the SCSP/M68K interpreter's own known incompleteness (unimplemented
opcodes `0xFFFC`/`0xFF00`, a separate, pre-existing gap in `m68k.rs` -- not something this fix
touched) rather than the CD-block stub. Verified deterministic across three repeated boot-watch runs
(same settling PC, `0x06001694`, every time), not a lucky one-off.

Verification: `cargo test --workspace` green across three consecutive full runs (165 saturn-core + 70
e2e-tests + 9 adversarial + the other suites, 0 failed), `cargo fmt --all` applied clean, plus the
live `ps -L` per-thread CPU inspection and `MIMAS_DEBUG_M68K` trace above. No unit test caught either
the original CPU-spin issue or the Core 3 regression along the way -- both were only visible by
actually running the system and watching real cross-thread timing behavior, the same category of gap
`sh2-cpu.md`'s own "verify against real hardware, not the manual alone" discipline exists for, just
applied to this project's own threading model instead of SH-2 semantics.

**Documentation touched:** `CLAUDE.md` ("Known architecture debt" -- the "Only Core 1 and Core 6
truly park" bullet rewritten to describe which cores now park, which spin on purpose, and the Core 3
trap in enough detail that it doesn't get rediscovered by trying the same fix again).

## Chapter 33 — SCU Milestone 2: Phase 5 — SCU timers 0 and 1

Phase 4 gave the SCU a real DMA engine. Phase 5 gives it the last piece of §5's register file real
behavior: Timer 0 (a scanline counter compared against `T0C`) and Timer 1 (a down-counter reloaded
from `T1S`), both raising their vectors through Phase 3's controller instead of sitting inert.

**`ScuTimers` grew real fields** (`timer0`, `timer0_set`, `timer1_counter`, `timer1_set`,
`timer1_preset`), matching `scu.h`'s internal state with one deliberate omission: the reference's own
`timer1` field (distinct from `timer1_counter`) is written only once, at reset, to `0`, and never read
or written anywhere else in `scu.c` -- genuinely dead state, so Mimas doesn't carry a matching field,
the same "don't reproduce dead C struct fields" call Phase 4 already made for `DmaLevel`'s `mode`.

**`hblank_in`/`vblank_out` grew real bookkeeping, in the reference's exact order, confirmed against
the literal C** (`ScuSendHBlankIN`/`ScuSendVBlankOUT`, `yabause/src/scu.c:3250-3301`) rather than
inferred from the hardware reference doc's own (slightly compressed) prose: the interrupt dispatch
always runs first, and `timer0`'s increment (H-Blank IN) or reset-to-zero (V-Blank OUT) is
**unconditional** -- only the *compare* against `T0C` (and therefore `Scu::timer0()`/`timer0_set`) is
gated on `T1MD` bit 0. A first draft of `hblank_in`'s new code gated the whole thing (increment
included) on bit 0, which would have silently frozen `timer0` whenever a game temporarily disabled
the global timer enable; a dedicated regression test
(`timer0_increments_and_timer1_reload_are_unconditional_only_the_compare_is_gated`) guards against
reintroducing that.

**Timer 1's countdown is driven by real Master SH-2 cycles, not a synthetic heartbeat -- a deliberate
correction of the plan's own suggestion, not a literal implementation of it.** The plan text proposed
deriving Timer 1's tick from "Core 6's own `sync_core` cycle accounting". Chapter 32's investigation
(days -- well, messages -- earlier in this same session) had already established that every core's own
`cycles` counter fed into `LockStepSync::sync_core` is a **synthetic pacing heartbeat**
(`cycles += step`, `step` clamped from `slack_limit`), with no relationship to real elapsed hardware
time at all. Driving Timer 1 from that would have given it an arbitrary, throttle-independent tick
rate -- wrong, since real hardware's SCU and SH-2 share exactly one physical clock. Instead, Timer 1
is driven by `Sh2::step`'s own real executed-cycle delta, batched (`SCU_TIMER_BATCH_CYCLES = 128`, a
Mimas-specific choice -- not a translation of the reference's per-deciline granularity, just "small
enough not to matter, big enough not to lock `Scu::timers` every instruction") and gated to the
Master only (`!is_slave`) -- real hardware has exactly one SCU shared between both cores, and driving
this from both would double-count. §5.4's own citation confirms the reference does the equivalent
thing: `timing = sh2cycles >> 1` is computed once per main-loop iteration, straight from real executed
CPU cycles, never a host clock.

**Deviation #18, fixed not copied**: the reference's outer gate in `ScuExec`
(`if (T1MD & 0x80 == 0)`) is a C operator-precedence bug -- `0x80 == 0` evaluates first and is always
false, so in practice `ScuTimer1Exec` (and therefore the whole Timer 1 countdown) only ever runs on
the scanline where `LineCount == T0C`, not "every tick when bit 7 is clear" like the code obviously
intended. Mimas implements the intended reading: `timer1_tick` runs its real countdown every call
(subject only to the correctly-written inner gate, T1MD bit 0), and Timer 1 mode (bit 7: fire on
every expiry vs. only when Timer 0 also matched this line) is checked only at the point of firing, not
as an outer "does the whole timer even tick" gate.

**The `Scu::write_long` special case for `T1S` (`0x94`) follows the same shape Phase 3/4 already
established** for registers with real write-side effects (`0xA0`/`0xA4`/`0xA8`, `0x60`): store the raw
value, then run the side effect (`timer1_preset = val as i32; timer1_set = true`) as a second,
explicit step, rather than hiding it inside the generic `raw_set` fallback (which stays a plain store,
reached only by `raw_write_long`/the byte-access fallback -- neither of which should trigger CPU-write
side effects).

Verification: 9 new tests (hand-traced 263-line scanline sequence deriving Timer 0's single expected
firing line independently; `T0C == 0` firing at V-Blank OUT; `T1MD` bit 0 disabling both timers; both
readings of bit 7, including the deviation #18 regression guard; Timer 1's reload re-arming at the
next H-Blank IN; the countdown's exact `cycles >> 2` arithmetic, independently re-derived; the
unconditional-increment regression guard above; a Master-vs-Slave wiring test mirroring the equivalent
DMA/interrupt tests from earlier phases). `cargo test --workspace` green throughout.

**Documentation touched:** left for the next chapter, since Phase 5's own H-Blank IN *source* (the
periodic driver that calls `hblank_in()` in the first place) was wired into Core 3's existing
wall-clock frame loop as an interim step here, then superseded within the same session by Chapter 34's
redesign before this chapter's own tracking-doc pass happened. See that chapter for the final state of
`docs/implementation-plans/scu.md`'s Phase 5 checklist and `.development/phased_development_plan.md`.

## Chapter 34 — Core 3 becomes genuinely event-driven: VBLANK/H-Blank move onto Master SH-2's cycles

Chapter 32 drew a line between two categories of thread: "genuinely idle, should park" (Cores 1, 2,
4-while-SNDOFF, 6, 7) and "always active by hardware design, spins on purpose when unthrottled" (Cores
0, 3, 5). Phase 5 (Chapter 33) then added a *third* piece of real, periodic work to Core 3's existing
wall-clock loop -- 263 evenly-spaced H-Blank IN ticks per frame, alongside the VBLANK-IN/OUT it
already generated. The user, watching this land, pushed back hard: watching a live boot window and
then reading this project's own `docs/mimas-architecture-spec.md` §1.4/§1.5 together, they held the
line that the *only* continuous loop anywhere in the system should be the CPU's, with every other
component activated purely by event -- and that if Core 3 genuinely couldn't be redesigned that way,
the performance story wasn't worth continuing to invest in.

**Fact-checking the pushback against the actual spec text, not memory, changed the outcome.**
§1.4 explicitly sanctions wall-clock batching, but scoped to "CPU core scheduling" -- and §1.5, read
in full, already said "no component thread is allowed to loop... **without parking**", with its own
example list ("the Slave SH-2 is parked, or the SCU DSP is idle") naming exactly the *idle* case, not
"never reference a clock at all". Under that reading, Core 3 seemed compliant -- it has continuous
real work, so §1.5 doesn't obviously forbid it. But re-reading Yabause's actual main loop
(`yabause/src/yabause.c:762-810`, fetched fresh rather than trusted from an earlier skim) settled it:
`yabsys.LineCount++` (H-Blank/V-Blank generation), `ScspExec()` (audio), `SmpcExec()`, and `Cs2Exec()`
are **all** called from inside the same loop that computes `sh2cycles` each iteration and feeds it to
`ScuExec(sh2cycles >> 1)` -- the reference has no independent wall-clock timer anywhere for *any*
peripheral. Its own frame-rate throttle operates one level up, pacing how fast `sh2cycles` itself
accumulates; everything *inside* that is cycle-driven, not clock-driven. Mimas's Core-3-as-a-
wall-clock-timer design was a real, avoidable deviation from the architecture both the spec doc and
the reference emulator actually describe -- not a defensible simplification, once looked at squarely.

**The fix: VBLANK/H-Blank generation moved onto the exact mechanism Phase 5 had just built for Timer
1.** `Sh2::step` already batched Master SH-2's real executed-cycle deltas for `Scu::timer1_tick`
(Chapter 33); a second, parallel accumulator (`pending_line_cycles`, threshold `SH2_CYCLES_PER_LINE`
-- `SH2_CLOCK_HZ / 60.0 / 263.0`, derived from the same 60fps/263-line assumption the rest of the
codebase already uses, not a new independent figure) now drives a new `Scu::advance_video_line`: it
calls `hblank_in()` (reusing `timers.timer0` as the running line counter -- real hardware's own
`LineCount` and the SCU's Timer 0 increment on the identical H-Blank IN edge, so there was only ever
one counter to keep, not two that could drift), then compares the result against NTSC's real
`VBlankLineCount`/`MaxLineCount` (`225`/`263`, cross-checked independently against
`yabause/src/vdp2.cpp:515` and `yabause/src/yabause.c:1027`, not re-derived from Mimas's own constants)
to decide whether V-Blank IN or V-Blank OUT just fired. `Scu` still holds no `LockStepSync` handle of
its own (the same reason `Scu::request_dma_trigger` doesn't wake Core 6 directly) -- it returns
`true` exactly when V-Blank IN fires, and `Sh2::step` is the one that calls
`sync.set_thread_active(3, true)`, mirroring the DMA-trigger and DSP-`EX`-bit wake patterns exactly.

**Core 3 itself shrank to almost nothing.** The entire ~50-line wall-clock frame loop (`next_frame_due`,
`frame_interval`, `vblank_duration`, `next_vblank_out_due`, the H-Blank line-interval tracking Chapter
33 had just added) is gone. Core 3 now deactivates once, calls `park_while_inactive(3)`, and on each
wake does exactly the render work (`execute_vdp1`, `render_backdrop`, publish, one `sync_core` call)
before re-parking. This sidesteps Chapter 32's own regression entirely, not by luck but by
construction: a parked core is excluded from `LockStepSync`'s bounded-slack computation altogether, so
there is no "active but reporting too infrequently" failure mode left for Core 3 to fall into. Chapter
32's fix (revert to a wall-clock spin) and this chapter's fix (stop being wall-clock at all) solve the
same symptom by moving in opposite directions -- the first kept Core 3 spinning to keep the heartbeat
frequent, the second removes the need for a heartbeat by removing Core 3 from the active set entirely
except during the brief real render burst.

**A real, separate cycle-accounting bug was found and fixed while building this, unrelated to
VBLANK/H-Blank semantics themselves.** `Sh2::step`'s new video-line accumulator (and, it turned out,
Chapter 33's *existing* Timer 1 accumulator) was seeded from `base` -- the fetched instruction's own
decode cost -- rather than the *total* real cycle cost of the step. `execute()`'s branch handlers
(`delay_slot_and_jump`) charge a **second**, separate cost to `self.cycles` for the delay-slot
instruction they execute internally, on top of the outer step's own charge for the branch itself; a
`base`-only accumulator silently undercounts every branch by roughly a third. This was invisible to
Chapter 33's own tests (which only checked "did the counter move at all", a weak assertion that passes
either way) and was only caught building this chapter's own cross-thread regression test, which needed
a real branching loop (a single NOP runs off the end of initialized memory within a few hundred steps)
and therefore needed the accounting to actually be right. Fixed by capturing `cycles_before` at the
top of `step()` (before `service_pending_interrupt`, so interrupt-entry overhead is captured too) and
using the real `self.cycles` delta for both the Timer 1 and video-line accumulators -- the same
before/after-delta pattern `run_loop()` already used at the outer level, now applied inside `step()`
itself.

**Two test-design lessons, both baked into `write_self_loop`'s and the two Master-vs-Slave tests' own
doc comments so they aren't rediscovered the slow way:**

1. A single `NOP` with nothing past it is only safe for short-lived tests (a few hundred steps).
   Longer-running tests need a genuine self-contained loop (`NOP; BRA back to the NOP, with its own
   delay-slot NOP`) -- otherwise `pc` walks off into uninitialized memory, decodes garbage, and
   (depending on VBR) can end up jumping through the illegal-instruction vector repeatedly, silently
   invalidating whatever the test was trying to measure rather than failing loudly.
2. Precomputing "N steps == M lines/timer-periods" from an assumed per-instruction cycle cost is
   fragile even when the assumption is *currently* correct, because it silently breaks the moment
   that ratio changes (exactly what happened here, mid-session, once the accounting bug above was
   fixed). Both regression tests now step in a loop and check for the expected state transition
   directly, with a generously-sized but explicitly-labeled safety cap instead of a precisely
   computed step count -- robust to the exact cycle cost of whatever loop body is used.

Verification: `cargo test --workspace` green across three consecutive runs (177 in `saturn-core`'s own
binary -- 3 net new: two `Scu`-level `advance_video_line` tests hand-deriving the 225/263 boundaries
independently, one Master-vs-Slave wiring test -- plus the pre-existing Timer 1 wiring test rewritten
to use the same safe self-loop and observe-don't-precompute pattern), `cargo fmt --all` clean. A real
`MIMAS_BOOT_WATCH_SECS=45` boot-watch reached the identical settling PC (`0x06001694`) across three
runs, both before and after this change -- no regression. Empirically confirmed via `ps -L` on a live
`mimas_window` process (not just inferred from code): `vdp2-composite` sits at a genuine `0.0%` CPU
throughout a real boot run, including while frames are actively being rendered (the render bursts are
too short to register in the sampled average) -- down from the ~46% continuous spin the very first
`ps -L` snapshot in this session had shown for the same thread.

**Documentation touched:** `docs/mimas-architecture-spec.md` (§1.4 scoped explicitly to CPU pacing
only; §1.5 rewritten to state the cycle-driven mechanism as the concrete implementation of "event-driven",
not just the abstract policy, and to name Core 5/SCSP as a tracked, honest exception rather than a
silent gap), `CLAUDE.md` ("Known architecture debt" -- the Core 3 bullet rewritten to describe the
final, cycle-driven design and Core 3's three-attempt history in one place, so a future session
doesn't try the wall-clock-sleep fix a third time), `docs/implementation-plans/scu.md` (Phase 5's
checklist -- including the "H-Blank IN source" item, whose *implementation* moved from Core 3's frame
loop to `Sh2::step` between Chapters 33 and 34 -- flipped to `[x]`), `.development/phased_development_plan.md`
(Milestone 2's Phase 5 line).

## Chapter 35 — SCU Milestone 2: Phase 6 — DMA start factors and DSP End close the loop

Phase 6 (`docs/implementation-plans/scu.md`) is the last of `scu.md`'s six phases and the one that
finally connects two mechanisms Phases 3-5 had each built in isolation: the 7 real interrupt sources
that can *also* arm-and-start a DMA level (§2.3's "start factors"), and the SCU DSP's `ENDI`
instruction, which had been setting its own sticky `E` status flag since the DSP interpreter was
finished but never actually raising the interrupt that flag is supposed to accompany.

**DMA start factors (§2.3).** Real hardware's `ScuChekIntrruptDMA(id)` is called from the tail of
seven of the SCU's interrupt-dispatch functions -- never from the other seven (DSP End, System
Manager, Pad, the three DMA-end senders, DMA Illegal) and never from the 16 externals. This was
confirmed the slow-but-correct way: reading the literal C body of every one of
`ScuSendVBlankIN`/`OUT`, `ScuSendHBlankIN`, `ScuSendTimer0`/`Timer1`, `ScuSendSoundRequest`,
`ScuSendDrawEnd`, `ScuSendDSPEnd`, `ScuSendSystemManager`, `ScuSendPadInterrupt`,
`ScuSendLevel0/1/2DMAEnd`, `ScuSendDMAIllegal` in `yabause/src/scu.c` one at a time, rather than
trusting a summary table. Landed as a single new private method:

```rust
fn check_dma_start_factor(&self, factor_id: u8) {
    for level in 0..3 {
        let base = level * 0x20;
        let den = self.raw_read_long(base + 0x10);
        let dmd = self.raw_read_long(base + 0x14);
        if den & 0x100 != 0 && (dmd & 0x7) as u8 == factor_id {
            let mut dma = self.dma.lock().unwrap();
            dma[level].trigger_pending = true;
            dma[level].clear_den_after_trigger = true;
        }
    }
}
```

called once, unconditionally, at the tail of each of the 7 real dispatchers (`vblank_in`,
`vblank_out`, `hblank_in`, `timer0`, `timer1`, `sound_request`, `draw_end`), each passing its own
factor id (0-6). It deliberately runs regardless of `IMS` masking -- §2.3 states outright that a
masked V-Blank IN still starts a DMA armed on factor 0, which reads as a bug until you remember the
interrupt controller and the DMA start-factor logic are two separate real circuits on the die, and
only one of them consults `IMS`. `masked_vblank_in_still_starts_a_factor_armed_dma` pins this down:
`IMS` is left at its power-on default (V-Blank IN's own bit masked) and the DMA still runs.

Reusing rather than rebuilding: the actual "arm and start" mechanics are Phase 4's existing
`trigger_pending` flag plus `snapshot_and_start`/`service_trigger`, unchanged. The only new piece is
a `DmaLevel::clear_den_after_trigger: bool` field, needed because a start-factor trigger clears
`DnEN` to 0 after servicing (§2.2 path b, one-shot arming) while an *immediate* trigger (§2.2 path a,
`DnEN` bit 0) leaves the CPU's written value untouched -- an asymmetry Phase 4 had already found and
tested (`immediate_trigger_leaves_den_bit_set_real_hardware_never_auto_clears_it`). The first draft of
`service_trigger` read `clear_den_after_trigger` *after* calling `snapshot_and_start`, which
constructs a fresh `DmaLevel` struct literal and silently resets the flag to `false` (`Default`) --
caught before ever running the tests, by re-reading the order of operations, and fixed by moving the
read to before the `snapshot_and_start` call. `immediate_trigger_never_sets_clear_den_after_trigger`
is the end-to-end regression guard for the asymmetry itself.

**A real statement-order bug found mid-implementation.** `ScuSendHBlankIN`'s C body does not call
`ScuChekIntrruptDMA(2)` first and then handle Timer 0 -- it calls `ScuSendTimer0()` (which contains
Timer 0's *own* `ScuChekIntrruptDMA(3)` call) before touching the Timer 1 reload-arm state at all. An
initial Rust draft of `tick_timer0_and_arm_timer1` held the `timers` lock across both the Timer 0
compare and the Timer 1 reload-arm check in one scope, which happened to run the reload-arm logic
*before* `self.timer0()` executed -- the reverse of the real order, and invisible until a test tried
to exercise both a factor-2 and a factor-3 armed DMA level from a single `hblank_in()` call. Caught
by re-reading the real C source line by line rather than trusting the mental model from Phase 5, and
fixed by restructuring the helper to drop the `timers` lock, call `self.timer0()` conditionally
(which itself locks `regs`/`irq` and, now, `dma`), then re-acquire `timers` for the reload-arm check
-- matching the real sequence exactly.
`hblank_in_can_start_two_different_dma_levels_on_factors_2_and_3_in_one_call` is the regression test:
level 0 armed on factor 2, level 1 armed on factor 3 via `T0C = 1`, a single `hblank_in()` call must
start both.

**DSP End / `ENDI` (§3.12).** `ScuDsp::write_control_port` already excluded `PCP_E` (bit 18) from its
writable mask before this phase -- confirmed by bit arithmetic, not by guessing, so `E`'s stickiness
needed no fix. What was actually missing was the interrupt itself: `ENDI` (End-with-interrupt) set
the flag and stopped there, with a `TODO` where vector `0x45`/level 10/mask `0x0020` should have been
raised. `ScuDsp` has no path to `Scu`'s `irq`/`regs` locks and no `LockStepSync` handle -- the same
constraint Phase 5's Timer 1 wiring ran into -- so the fix follows the same "signal while locked, act
after releasing" shape already established for `tick_timer0_and_arm_timer1`: `ScuDsp::step` (and the
`execute_other` it delegates the `0xF`-class End-Commands opcode to) changed from returning `()` to
`bool`, `true` exactly on the instruction where `ENDI` just executed. Core 6's loop (`lib.rs`) now
reads that return value after `step()` returns (so the `dsp` lock is already released) and calls
`Scu::dsp_end()` only then:

```rust
if scu_c6.dsp.lock().unwrap().is_executing() {
    let dsp_end = scu_c6.dsp.lock().unwrap().step(&work_ram_c6);
    if dsp_end {
        scu_c6.dsp_end();
    }
}
```

`endi_raises_dsp_end_and_leaves_e_set_across_a_subsequent_control_port_write` simulates this exact
call pattern from a bare `Scu` (no `Sh2`, via the existing `wire_master` test helper), asserting
vector `0x45`/level 10 is delivered once and that `E` survives a second, unrelated Program Control
Port write afterward.

**Deliberately left unreachable, matching the plan.** Sprite Draw End's SCU-side entry point
(`draw_end()`, vector `0x4D`/level 2/mask `0x2000`/factor 6) is fully implemented and tested --
including its own factor-6 start check -- but nothing calls it yet, because VDP1's `execute_vdp1`
(Core 3) doesn't raise any interrupt on command-list completion today. That trigger belongs to
`docs/implementation-plans/vdp1.md` Phase 3, which this phase unblocks rather than duplicates. Pad
interrupt stays deferred to `smpc-peripheral.md` for the same reason (SMPC-side trigger, not an SCU
concern). External interrupts 00-15 and DMA Illegal remain unreachable, matching divergence #14 and
the reference's own lack of a producer for either.

**Testing.** 12 new tests in `scu.rs`'s existing `mod tests`: a shared
`assert_factor_arms_and_starts_dma(source: fn(&Scu), factor_id: u8)` helper (arms a level with
`DnEN = 0x100`/`DnMD = id`, fires the given source function pointer, asserts the transfer ran and
`DnEN` was cleared to 0) driving 7 individual tests `dma_start_factor_0_vblank_in` through
`dma_start_factor_6_draw_end`; `masked_vblank_in_still_starts_a_factor_armed_dma`;
`hblank_in_can_start_two_different_dma_levels_on_factors_2_and_3_in_one_call`;
`immediate_trigger_never_sets_clear_den_after_trigger`; and
`endi_raises_dsp_end_and_leaves_e_set_across_a_subsequent_control_port_write`. All 12 passed on the
first run after the statement-order fix above was made -- no test needed a second iteration once that
was corrected.

Verification: `cargo build --workspace` clean; `cargo test --workspace` green (70 e2e-tests, 188 in
`saturn-core`'s own binary -- up from 177, exactly the 11 net-new tests above accounted for [the
factor-0..6 helper-driven tests count as 7, plus 4 more], 9 sync-tests, 6 real-fixtures, 9
adversarial); `cargo fmt --all` clean. A real `MIMAS_BOOT_WATCH_SECS=90` run against
`scratch/ra_system/saturn_bios.bin` (release build) reached the identical, previously-documented
settling PC (`0x06001694`, first seen in Chapter 34) -- no regression, as expected: this phase adds a
few extra register reads per interrupt dispatch (three levels' `DnEN`/`DnMD`, checked only on the 7
real dispatchers, never in a hot per-cycle path) and does not change what unblocks BIOS progress past
that point, which needs VDP1/SMPC work this phase deliberately did not attempt.

**Documentation touched:** `docs/implementation-plans/scu.md` (Phase 6's checklist and testing
section flipped to `[x]`, with the two deliberately-deferred items -- Sprite Draw End's VDP1-side
trigger, Pad's SMPC-side trigger -- left `[ ]` and annotated with what they're waiting on and why),
`.development/phased_development_plan.md` (Milestone 2's Phase 6 line, noting the same deferral so a
future session doesn't read `[x]` as "VDP1 already raises Draw End").

With Phase 6 landed, `scu.md`'s own six phases (DSP completion, register file, interrupt controller,
DMA controller, timers, start factors/DSP-End/Draw-End) are all done. The SCU subsystem's remaining
open work is entirely owned by other plans now: VDP1 Phase 3 (Draw End's trigger), SMPC Phase 6
(Pad's trigger, and moving SMPC itself onto Core 7).
