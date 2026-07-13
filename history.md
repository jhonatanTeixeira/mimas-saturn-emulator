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
