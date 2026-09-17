# Mimas Performance & Bottleneck Analysis: Mimas vs. Yabause

This document analyzes the architectural bottlenecks in the traditional **Yabause** Sega Saturn emulator and specifies how the new design of **Mimas** resolves them to deliver a lightweight, high-performance emulation loop.

**Note**: several "how Mimas resolves this" claims below describe the *target* architecture rather than what's running today — in particular §2.2 (see the caveat inline). See `CLAUDE.md`'s "Known architecture debt" section and `docs/implementation-plans/` for exactly what's implemented versus planned.

---

## 1. Mapped Bottlenecks in Yabause (The Legacy Design)

Yabause simulates the Sega Saturn's complex multiprocessor hardware through a synchronous, single-threaded execution loop. An analysis of the Yabause codebase (`yabause.c`, `sh2core.c`, `scu.c`, `vdp1.cpp`, `vdp2.cpp`) reveals key performance bottlenecks:

### 1.1. Sequential Component Interleaving
In Yabause, all processors (Master SH-2, Slave SH-2, SCU DSP, MC68000, and VDPs) are simulated in a single loop by interleaving cycles:
```c
// Conceptual loop in traditional YabauseExec
while (emulating) {
    SH2Exec(MSH2, master_cycles);
    SH2Exec(SSH2, slave_cycles);
    ScuExec(scu_cycles);
    M68KExec(m68k_cycles);
    Vdp2DrawScreens();
}
```
* **Impact**: Frequent switching of CPU emulation contexts within a single host thread leads to severe CPU cache pollution and branch mispredictions. The host CPU constantly swaps register maps and instruction cache lines for different virtual processors.

---

### 1.2. Synchronous software rendering (VDP1 and VDP2 serialization)
In Yabause (`vidsoft.c`), VDP1 rasterizes polygons and VDP2 composites background planes within the main execution loop.
* **Impact**: The main SH-2 thread is blocked whenever VDP1 draws complex geometry. Since the drawing pipeline is synchronous, any heavy load in VDP1 rasterization halts instruction execution, causing frame drops and audio crackling.

---

### 1.3. Heavy Bus Arbitration Overhead
Whenever the SH-2 interpreter reads or writes memory in Yabause, it calls functions like `MappedMemoryReadLong` or `MappedMemoryWriteLong`.
* **Impact**: These functions perform pointer-list lookups (`ReadLongList[addr >> 20]`) and register trigger checks synchronously. When DMA is active, the entire system execution waits for the copy loop to complete, causing bus-arbitration overhead.

---

## 2. How Mimas Resolves Bottlenecks

Mimas addresses these bottlenecks by using native threads, split-memory regions, and event-driven suspension.

```
Yabause (Single Threaded Serialization)
[ SH-2 Master ] -> [ SH-2 Slave ] -> [ SCU DSP ] -> [ VDP1 Draw ] -> [ VDP2 Composite ]

Mimas (Distributed Parallel Pipeline)
+---------------------------------------------------------------------------------+
|  sh2_master Thread       [ Batch Execution ] ====> (Sleep / Compensation)      |
|  sh2_slave Thread        [ Batch Execution ] ====> (Sleep / Bounded Slack)     |
|  vdp1_draw Thread        [ Rasterizes VRAM Cmds ] => Handsoff Framebuffer      |
|  vdp2_composite Thread   [ Reads V2_VRAM, Mixes Planes ] => Video Output       |
|  scu_dma_dsp Thread      (Parked on Condvar) => Wakes on DMA / DSP trigger     |
|  scsp_synth Thread       [ Continuous audio stream via lockless ring buffer ]  |
+---------------------------------------------------------------------------------+
```

### 2.1. True Parallel Execution of CPUs and Co-Processors
By mapping the Master SH-2, Slave SH-2, and SCU DSP to separate OS threads:
* The host CPU can distribute emulation tasks across multiple cores.
* Context-switching overhead is eliminated. The Master SH-2 thread remains loaded in the host core's cache registers, executing instructions without interruption.

---

### 2.2. Pipeline Parallelism in Graphics Rendering (VDP1/VDP2 separation)
Mimas's target design decouples geometry drawing from screen composition:
* **VDP1 Thread (`vdp1_draw`)**: Continuously processes drawing commands in VRAM, rasterizing them into its private Framebuffer bank.
* **VDP2 Thread (`vdp2_composite`)**: Reads from the opposite Framebuffer bank to composite background layers and scan out the frame.
* **Impact**: The SH-2 thread never stalls during graphics rendering. VDP1 and VDP2 run concurrently, utilizing double-buffering handoffs to eliminate frame wait-states.

**Current status: not yet true.** Today VDP1 command execution and VDP2 compositing both run serially, back-to-back, inside the `vdp2_composite` thread's own loop — `vdp1_draw` is currently idle, and the framebuffer is a single flat region rather than two banks being swapped. This section describes what `docs/implementation-plans/vdp1.md` and `vdp2.md` are working toward, not a measured result.

---

### 2.3. Lock Striping and Lockless Channels
* **Lock Striping**: High WRAM is split into 32 independent memory blocks, each protected by its own `RwLock`. If the Master SH-2 accesses variables in block 0 while the SCU DMA writes to block 15, both transactions execute in parallel without lock contention.
* **Lockless SPSC Queues (Aspirational)**: Hardware signals (such as DMA triggers or interrupt flags) currently use `Arc<Mutex<InterruptQueue>>` (e.g., `Sh2::irq_in`), not yet lockless ring buffers. Moving them to lockless SPSC queues is a planned optimization to eliminate the overhead of mutex-protected system buses.

---

### 2.4. Sound Decoupling
The Motorola 68000 and SCSP run in a separate sound thread context, writing synthesized samples into a lockless ring buffer.
* **Impact**: Audio generation is independent of the emulation loop's pacing. Even if the main thread lags during heavy 3D calculations, the sound buffer continues playing from the queue, preventing audio stutters.

---

## 3. Measured performance

**First real measurement in this project's history** (2026-09-17). Everything above
this section is design rationale; this section is data. Method: real BIOS boot,
`saturn-frontend-native --bios`, unthrottled, Ryzen 5 3500X (6C/6T, Zen 2), Master
SH-2's own emulated cycle count divided by wall-clock seconds. Reported by the
binary itself (`telemetry::MASTER_CYCLES`), so it is reproducible, not a one-off.

| | before 2026-09-17 | now |
|---|---|---|
| Master SH-2 emulated speed | 12.5 MHz | **53.2-54.1 MHz** |
| ...as % of real 28.636 MHz | **43.5%** | **186-189%** |
| wall clock to the same settle PC (`0x06001694`) | 10.01 s | 2.31-2.35 s |
| process CPU | 139% | **113%** |
| voluntary context switches / sec | 213K | **83K** |
| Core 0 blocked in `sync_core` | 6.1% of wall | **2.7%** |

**4.3x throughput for 19% less CPU.** The last step is the one worth internalising:
the conditional notify below made the emulator *both* 32% faster and 23% cheaper at
the same time, because the work it removed was pure overhead -- no tradeoff was
involved, the CPU was simply being burned on futex wakeups nobody needed.

**3.2x**, from below real-time to comfortably above it on this host. The three
changes that produced it, in order of contribution: removing a `sched_yield`-per-
instruction (`history.md` Chapter 39), replacing five atomic read-modify-writes per
instruction with one relaxed load (§1.2b, Chapter 40), and moving an `std::env::var`
off the per-bus-access path.

### 3.1. What this number does and does not say

- It is **BIOS boot**, the lightest workload the system has: Slave SH-2 parked, SCSP
  synthesizing into a channel nothing reads, VDP1/VDP2 barely drawing. A real game
  wakes all of it. Treat 141% as a ceiling, not a budget.
- Roughly 0.5 s of each 3.1 s run is the boot-watch settle window, where the BIOS
  spins in a cheap poll loop. That inflates the figure somewhat.
- It is unthrottled: `ThrottleSpeed::Unthrottled` is the default, so this is
  "as fast as it can go", not "what it needs".

### 3.2. The R36S question

The target is an R36S: 4x Cortex-A53 at ~1.4 GHz, in-order, dual-issue
(`README.md`, `docs/lessons-from-yabasanshiro.md`). Against a Zen 2 core the gap on
branchy interpreter code is roughly 9x (Geekbench-anchored floor) to 14x (realistic
for indirect-branch-heavy dispatch on an in-order core).

At 141% here, the same binary would land at roughly **10-16% of real-time on the
R36S** -- **6x to 10x short**, before the rest of the system does any real work.

Worse, the defects this measurement was built to find are *disproportionately*
expensive on A53: every atomic read-modify-write is an LL/SC retry loop plus
barriers where x86 has a single `lock` prefix, and `Acquire`/`Release` emit real
`dmb ish` stalls on an in-order pipeline. **Measuring on the dev box systematically
understates them.** Nothing here should be considered settled until it is measured
on the target.

### 3.3. Where the remaining CPU goes

83,000 voluntary context switches per second remain, down from 278,000 once
`sync_core`'s broadcast became conditional (`history.md` Chapter 41). What is left
is the genuine Core 0 <-> Core 5 handoff: Core 5 is still an *active* participant in
the bounded-slack lockstep rather than a parked, event-driven component, so the two
cores must still hand the window back and forth. Parking Core 5 -- the architecture
debt `docs/mimas-architecture-spec.md` §1.5 already records as a known gap -- would
remove the handoff entirely, not just make it cheaper.

The other open lever is `slack_limit` itself (default 1000, `SaturnSystem::new`).
It has never been swept. A wider window means fewer handoffs and more drift between
cores; that is a correctness/throughput tradeoff and should be measured, not
guessed.

