# Final architecture draft

This is the decided design, not a menu of options — see `history.md`
Chapter 6 for how the session's conversation got here, and `TECH_DEBT.md`
for the punch-list of concrete work items that implement it. This
document describes the *shape* of the target architecture and the
specific engineering (crates, primitives, techniques) that realizes each
piece, with each choice tied back to what it represents on real Saturn
hardware — this project's standing rule, unchanged.

## The decision, stated plainly

- **OS threads (`std::thread`), one per distinct hardware component** —
  not a hardcoded four bundled to match a specific target device's core
  count, not `tokio`/async tasks, not real OS processes (`fork`).
- **`Condvar`+`Mutex` or bounded channels for cross-component events** —
  never a polled `AtomicBool` in a dedicated wait loop, never a wall-clock
  debounce standing in for a real signal.
- **Per-region `Arc<RwLock<T>>` (or single-owner + explicit handoff) for
  genuinely shared memory** — not one lock covering everything Saturn has.
- **Batched wall-clock throttling for CPU clock rate** — not
  per-instruction sleeping (impossible at the relevant timescale) and not
  the current unthrottled-by-default behavior.
- **No polling loops** outside a CPU core's own instruction-execution loop
  and deterministic wall-clock pacers (frame timing, the clock throttle
  itself).

## Why threads, not processes (the decision, with the reasoning that earned it)

Real OS processes were seriously considered — they're what the project's
original planning notes actually specified (`docs/initial_architecture_idea.md`:
"processos isolados e não threads"). Two claimed advantages didn't survive
scrutiny; one real advantage did, and wasn't enough to outweigh the cost:

- **Claimed: processes avoid context-switch cost.** False on Linux —
  `fork()` and `pthread_create()` both go through `clone()`; threads are
  *cheaper* to switch between than processes specifically because
  switching between processes also swaps the address space (TLB), which
  threads sharing one address space never pay. See `history.md` Chapter 6
  for the full back-and-forth that reached this.
- **Claimed: avoiding shared variables between processes avoids
  contention.** Partially true, but not because of the process boundary —
  cache-coherency cost (cache-line bouncing between cores) is a hardware
  property blind to whether the two things touching the same memory are
  processes or threads. A `shmop`/`mmap` segment shared between two
  processes bounces cache lines exactly like an `Arc<RwLock<T>>` shared
  between two threads, touched with the same frequency. What actually
  avoids this cost is message-passing discipline (own your memory,
  hand off explicitly) — achievable identically well with in-process
  channels as with cross-process sockets, and cheaper with the former
  (every socket `read`/`write` is a syscall + kernel buffer copy per
  message, even `AF_UNIX`; an in-process channel's fast path never leaves
  userspace).
- **Real: processes give hardware-enforced fault isolation** (the MMU
  polices the page-table boundary; a wild write in one component
  literally cannot reach another's memory). This is genuinely something
  threads don't give you for free. It wasn't chosen for this project
  because nothing in Mimas's current or near-term scope needs
  crash-survivability across components the way, say, a job queue serving
  independent client requests does — a crash in the M68K core is a bug to
  fix, not a fault to contain and keep serving through. If that
  calculus changes (e.g. running untrusted/unverified content where
  containment genuinely matters), this decision is revisitable — see
  "What this deliberately does not include" below for what that would
  look like.

## Component topology

One thread per distinct hardware component, decomposed by what's
*actually* a separate piece of silicon on real hardware, not by what's
convenient to bundle for a target core count:

| Thread | Owns | Existing module |
|---|---|---|
| Master SH-2 | Main CPU execution, BIOS/game code | `sh2.rs` (instance) |
| Slave SH-2 | Secondary CPU, halted until real SSHON | `sh2.rs` (instance) |
| SCU | Bus arbitration, DMA channels, the SCU DSP | `scu.rs`, `bus_arbiter.rs` |
| SMPC | Peripheral/system management commands | `smpc.rs` |
| CS2 / CD-ROM | Disc command protocol, sector reads | `cdrom.rs` |
| VDP1 + VDP2 | Sprite/polygon + tile/background rendering | `vdp.rs` |
| SCSP | Sound register/mixing logic | `scsp.rs` |
| M68K | The SCSP's onboard sound CPU | `m68k.rs` |

VDP1 and VDP2 stay paired in one thread deliberately, not as a
core-count concession: on real hardware they're tightly coupled through
shared VRAM windows and a frame-timing protocol (VDP1 finishes a draw
into one framebuffer while VDP2 scans out the other, swapping at
VBLANK — see `docs/saturn_architecture_report.md`) that inherently
serializes them relative to each other far more than, say, VDP2 and the
SCSP are coupled. Everything else gets its own thread because nothing
about real hardware forces them to serialize with each other — the SCU
DSP, for instance, does real parallel work independent of the SH-2s'
instruction stream, and bundling it into "whatever thread has room" (as
the current four-thread model does) hides that.

Components with nothing to do yet don't spin: the Slave SH-2 doesn't run
`run_loop()` at all until a real SSHON reset arrives (park on a
`Condvar`, or don't spawn the thread until the first signal gives it
work — implementation detail, either is fine); the SCU DSP similarly
parks until something actually targets it, instead of the current
`cycles += 2; sync_core(); yield_now();` forever-loop doing nothing.

The OS scheduler places and migrates these across however many real
cores exist on the host — no code decides "component X runs on core Y";
that decision was always the OS's to make better than a hardcoded mapping
could.

## Synchronization primitives, mapped to what they represent

| Rust primitive | Used for | Real hardware analogue |
|---|---|---|
| `Condvar` + `Mutex<T>` | Cross-component event signaling: "wake up, something happened" | An interrupt line (VBLANK-IN, SMPC System Manager, SCU Sound Request — all three already real in `sh2.rs`) |
| `std::sync::mpsc` / `crossbeam::channel` | Discrete command/event handoff between components that don't need shared low-latency memory | A mailbox/command register protocol (e.g. SMPC's IREG/COMREG/OREG handshake, already real) |
| `Arc<RwLock<T>>`, split per region | Genuinely dual-ported memory needing frequent, low-latency access from multiple components | Sound RAM and SCSP registers, which real hardware makes visible to both the SH-2 and the M68K at once |
| `ArcSwap<T>` | Single-producer, multi-consumer published state where the producer must never block on readers | The VDP1/VDP2 double-buffered framebuffer swap at VBLANK (`vdp2_frame` already uses this correctly) |
| `AtomicBool`/`AtomicU32` (bare, no dedicated wait loop) | A flag checked as part of an *already-continuous* loop's existing work (e.g. `Sh2::step()` checking interrupt-pending flags every instruction, which it does anyway) | A status register bit a real CPU polls opportunistically, not a dedicated hardware wait state |
| `std::time::Instant`-based batched comparison | Wall-clock pacing (frame timing, CPU clock throttle) | The video/audio clock crystal's real tick rate |

`crossbeam::channel` over `std::sync::mpsc` where channels are used: it
adds multi-consumer support and a `select!` macro over multiple channels,
both of which a component listening for signals from more than one other
component will plausibly need (e.g. the SCU arbitrating requests from
both SH-2s). `std::sync::mpsc` remains fine for simple single-producer/
single-consumer cases where that's all that's needed — not a hard
requirement to add the dependency everywhere.

`BusArbiter` (`bus_arbiter.rs`) already implements the `Condvar`+
`AtomicBool` pattern correctly for the DMA bus lock — it's the reference
implementation for how every other `Condvar`-based wait in this
architecture should look. New signals (the SNDON→M68K-reset handshake,
Slave SH-2's SSHON wake) should match its shape, not reinvent it.

## Memory layout

`shared_buffers.rs`'s `WorkRam` stops being one struct behind one lock.
Split along real hardware ownership boundaries:

- **Exclusive regions** (Low/High Work RAM — SH-2-only; VDP1/VDP2 VRAM
  and CRAM — renderer-only on real hardware outside of DMA windows):
  each gets its own lock, or becomes genuinely single-owner with other
  components accessing it only through an explicit request/response or
  published-snapshot mechanism (per the `ArcSwap` pattern) if they need
  it at all.
- **Genuinely dual-ported regions** (Sound RAM, SCSP registers — visible
  to both the SH-2 and the M68K on real hardware, confirmed against
  Yabause's `c68k_byte_read`/`c68k_byte_write` this session): keep as
  real shared memory, each in its own lock, separate from everything
  else. This is where most of the actual cross-component contention
  legitimately lives, and it's exactly the region pair the SNDON/MCIPD
  handshake already touches — the `Condvar` work for that handshake and
  this memory split are naturally done together.
- **SCU/SMPC/CS2 registers**: currently backed by plain arrays inside the
  same monolithic struct; once SCU/SMPC/CS2 are real per-component
  threads (not logic embedded in `Sh2`'s own read/write path, as SMPC
  command processing currently is), these regions become that
  component's own locally-owned state, exposed to the SH-2 only through
  the memory-mapped read/write protocol real hardware defines — matching
  real hardware's actual topology instead of centralizing SMPC's command
  execution inside the CPU interpreter that happens to be reading it.

## CPU clock throttling

Batched wall-clock comparison, generalizing the pattern `lib.rs` already
uses for VBLANK/frame pacing (`next_vblank_due`/`frame_interval` against
`Instant::now()`):

```rust
let batch_cycles = /* tuned: enough emulated time that sleep imprecision
                       is negligible relative to it, e.g. a few hundred
                       microseconds to low milliseconds of emulated time */;
let target_hz = /* real SH-2/M68K clock rate, or a configurable multiplier */;
let mut next_batch_due = Instant::now();
loop {
    run_n_cycles(batch_cycles); // unthrottled — no sleeping mid-batch
    let batch_duration = Duration::from_secs_f64(batch_cycles as f64 / target_hz as f64);
    next_batch_due += batch_duration;
    let now = Instant::now();
    if next_batch_due > now {
        thread::sleep(next_batch_due - now);
    } else {
        // running behind real-time; don't sleep, and consider this
        // observable (a frontend "running slow" indicator) rather than
        // silently falling further behind
    }
}
```

Per-instruction sleeping is not a smaller version of this — it's a
different, non-viable approach: an SH-2 instruction takes tens of
nanoseconds; OS sleep wake-up precision is microseconds at best, three-
plus orders of magnitude too coarse to throttle at that granularity.
Batching is the only technique that actually works, and it's the same
one every practical cycle-throttled emulator uses.

`target_hz` being a runtime-configurable parameter (not a compile-time
constant) is the natural way to expose a user-facing speed control
(real-speed / unthrottled-turbo / custom multiplier) — design it as a
first-class setting from the start rather than hardcoding real speed and
bolting a multiplier on later.

## What this deliberately does not include (yet)

**Real OS processes.** Not chosen — see "Why threads, not processes"
above. If ever revisited (concrete trigger: a need for hardware-enforced
fault isolation between components that threads genuinely can't provide),
the engineering would look like:

- `fork` crate (Unix-only — Windows has no `fork()`; a real constraint if
  cross-platform support beyond Linux/the R36S ever matters) for process
  creation instead of `thread::spawn`.
- `shared_memory`/`shmem` crate (or raw `mmap` via `memmap2`) for the
  dual-ported regions (Sound RAM, SCSP registers) that need to stay real
  shared memory — `std`'s `Arc<RwLock<T>>` doesn't cross process
  boundaries at all.
- A blocking pipe or `UnixStream` (`os_pipe`, or `std::os::unix::net::UnixStream`)
  for the signal side of any handshake — `std::sync::Condvar` is
  thread-only and cannot be waited on across a process boundary; a
  `pthread_cond_t` with `PTHREAD_PROCESS_SHARED` placed inside the shared
  memory segment is the closest cross-process equivalent, and is
  meaningfully more `unsafe`-heavy, lower-level territory than anything
  this architecture currently needs.

This is a strictly bigger engineering lift than the threads-based design
above for the same functional result, justified only by the fault-
isolation property specifically — not by performance (see "Why threads,
not processes").

**A cooperative/fiber-based single-thread scheduler** (the higan/bsnes
style — see `docs/honest_architecture_review.md`'s comparison section).
Not ruled out long-term, but deliberately not decided now — it's the
right comparison to make once the CPU clock throttle above exists and
gives a real, comparable instructions-per-second number to measure the
current (soon to be per-component-threaded) design against. Deciding
this without that measurement would be exactly the kind of unproven
performance assumption `docs/honest_architecture_review.md` already
warned against building further work on top of.

## Relationship to existing code

None of this requires a single rewrite. `TECH_DEBT.md`'s suggested order
of attack applies this design incrementally: fix the two components
currently spinning uselessly (Slave SH-2, SCU DSP) first, since that
validates the `Condvar`-park pattern in isolation; replace the SNDON
debounce with a real signal next, since it's directly relevant to the
active boot investigation and exercises the same pattern under real
cross-thread timing pressure; split `WorkRam` region by region after
that; design the CPU throttle before any performance comparison work;
revisit the deferred cooperative-scheduling and process-isolation
questions only once real measurements exist. `cargo test --workspace`
stays green after every step, same as every other change in this
project.
