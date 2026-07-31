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
* **Lockless SPSC Queues**: Hardware signals (such as DMA triggers or interrupt flags) are pushed onto lockless ring buffers, eliminating the overhead of mutex-protected system buses.

---

### 2.4. Sound Decoupling
The Motorola 68000 and SCSP run in a separate sound thread context, writing synthesized samples into a lockless ring buffer.
* **Impact**: Audio generation is independent of the emulation loop's pacing. Even if the main thread lags during heavy 3D calculations, the sound buffer continues playing from the queue, preventing audio stutters.

---

## 3. Will Mimas Be More Performant?

**Yes.** By partitioning emulation into independent, parallel modules, Mimas optimizes host CPU resource utilization. On multi-core host platforms, Mimas achieves high performance by scaling across physical threads:

1. **Host Cache Locality**: Each thread focuses on a single task (e.g., the Master SH-2 interpreter or VDP2 blending), preventing instruction cache conflicts on the host CPU.
2. **Elimination of Busy-Waiting**: Idle threads (such as the SCU DSP or parked Slave SH-2) are suspended on condition variables, consuming zero CPU cycles.
3. **Decoupled Graphics and Audio**: Separating the execution of the CPU cores, renderer, and sound engine prevents resource starvation, delivering a fluid 60 FPS emulation loop.
