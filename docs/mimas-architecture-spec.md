# Mimas Sega Saturn Emulator - Architectural Design Specification

This document details the architectural design for the **Mimas** Sega Saturn emulator, following a strict distributed hardware component-thread model.

**This is a decision record for the target design, not a status report.** Several decisions below (event-driven suspension in particular) are only partially realized in the current code — see `CLAUDE.md`'s "Known architecture debt" section for exactly which threads still busy-loop instead of parking, and `docs/implementation-plans/` for the path to closing each gap. For exact real-hardware register/address/opcode facts (as opposed to Mimas's own design decisions), see `docs/hardware-reference/` — this document sticks to *Mimas's* architecture, not the Saturn's.

---

## 1. Architectural Core Decisions

### 1.1. Native OS Threads (`std::thread`) per Hardware Subsystem
* **Decision**: Every major hardware component on the Sega Saturn motherboard runs in its own dedicated, native OS thread (`std::thread`). 
* **Details**: We reject arbitrary thread pools tied to host core count, `tokio`/async runtimes, or OS-level process isolation (`fork`). Each distinct piece of silicon on the physical Sega Saturn board runs concurrently in Mimas.
  * **Master SH-2 Thread**: Executes the master CPU pipeline, registers, and cache.
  * **Slave SH-2 Thread**: Executes the slave CPU pipeline, registers, and cache.
  * **VDP1 Thread**: Handles sprite and polygon rasterization to the framebuffers.
  * **VDP2 Thread**: Handles background compositing, plane rotation, and display output.
  * **MC68000 Thread**: Simulates the 11.3 MHz sound CPU.
  * **SCSP Sound Synthesizer / Sound DSP Thread**: Simulates synthesis channels and audio DSP.
  * **SCU DMA/DSP Thread**: Executes SCU-level DMA transfers and processes SCU DSP programs.
  * **SMPC & CD Block Thread**: Manages CD-ROM state transitions and peripheral system control.

---

### 1.2. Event Signaling via `Condvar` + `Mutex` and Bounded Channels
* **Decision**: We forbid busy-polling of atomic variables (`AtomicBool`) in tight loops.
* **Details**: Inter-component notification is event-driven:
  * When the Master SH-2 writes to the SCU DSP's program control register (`ProgControlPort` setting `EX`), the write signals a `Condvar` to wake up the SCU DSP execution worker.
  * VDP1 signals the completion of its drawing list by waking threads parked on a `Condvar` associated with the `Draw End` interrupt status.
  * Frame timing and VBlank synchronization are regulated by a dedicated display sync loop (SPG equivalent) waking up other components using a `Condvar` state-change handshake.

---

### 1.3. Region-Granular Shared Memory via `Arc<RwLock<T>>`
* **Decision**: We forbid a single global system lock.
* **Details**: Memory is divided into isolated physical regions. A thread must only lock the specific memory region it accesses.
  * `Arc<RwLock<HighWram>>`: Mapped to `0x06000000` - `0x061FFFFF` (2MB, real size — mirrored across the wider `0x06000000`-`0x07FFFFFF` window, see `docs/hardware-reference/memory-bus.md`). Main memory accessed by SH-2 cores and SCU DMA; implemented today as 32 independent 64KB-striped locks, not one lock over the whole region (see `shared_buffers.rs`).
  * `Arc<RwLock<LowWram>>`: Mapped to `0x00200000` - `0x002FFFFF` (1MB).
  * `Arc<RwLock<Vdp1Vram>>`: Owned by VDP1 thread during drawing; accessed read-only by VDP2/host.
  * `Arc<RwLock<Vdp2Vram>>`: Owned by VDP2 thread for background rendering.
  * `Arc<RwLock<SoundRam>>`: Memory boundary for the M68K and SCSP threads.

---

### 1.4. Clock Throttling via Wall-Clock Batching
* **Decision**: CPU core scheduling is paced by batching virtual instruction cycles against host wall-clock time.
* **Details**: 
  * Running `std::thread::sleep` on a per-instruction scale is impossible. Instead, each CPU thread executes instruction cycles in batches representing a small fraction of virtual time (e.g., 1ms increments: ~28,600 instructions for the 28.6 MHz SH-2).
  * After executing a batch, the thread measures elapsed host time and sleeps/waits for the remaining duration of the slice.
  * Synchronization between Master and Slave SH-2 cores is enforced at the batch boundary, keeping their internal cycle counters in a lockstep margin (bounded-slack) of few cycles.

---

### 1.5. No Polling Loops (Event-Driven Suspension)
* **Decision**: No component thread is allowed to loop on a condition variable or register status without parking.
* **Details**: 
  * If a CPU core executes a `SLEEP` opcode or spins on a peripheral flag (e.g., waiting for CD-ROM read), it yields execution.
  * When a thread has no active work (e.g., the Slave SH-2 is parked, or the SCU DSP is idle), it is parked on a `Condvar` or blocks on a channel. It remains suspended until an external hardware event (an interrupt or write) wakes it up.

---

## 2. Threads vs. Processes: Architectural Justification

We evaluate the decision to use threads rather than independent processes (`fork()` or IPC):

1. **Context Switching Overhead**:
   In modern operating systems (such as Linux), both threads (`pthread_create`) and processes (`fork`) use the same underlying system call (`clone`). The performance difference lies in memory space transitions. Transitioning between independent processes requires changing the page tables (MMU reload, TLB invalidation). Since the emulator components constantly access shared memory pools (High/Low WRAM, VDP VRAM, Sound RAM), process-level separation would introduce heavy memory access penalties. Threads sharing the same address space avoid this overhead entirely.
2. **Coherence & Cache Contention**:
   Cache-line bouncing (caused when two cores write to the same memory segment) occurs at the CPU cache level, regardless of whether execution contexts are threads or processes. We address cache contention not by process separation, but by strict ownership partition. Threads write only to their owned domains and use explicit handoffs (e.g. double-buffering) when passing data.
3. **Fault Isolation**:
   While processes offer the advantage of isolating memory faults (preventing a crash in the rendering module from bringing down the entire emulator), Mimas operates as a single-application emulator. During execution and development, a panic in any component (such as the SCU DSP or VDP1 rasterizer) is a critical error. The correct behavior is to crash the entire application and exit immediately to facilitate debugging.

---

## 3. Component Topography & Thread Mapping

The physical Sega Saturn motherboard is partitioned into the following independent execution threads:

```
+---------------------------------------------------------------------------------+
|                               Mimas System Process                              |
|                                                                                 |
|  +---------------------+  +---------------------+  +-------------------------+  |
|  | Master SH-2 Thread  |  |  Slave SH-2 Thread  |  |    SCU DMA/DSP Thread   |  |
|  | (28.6 MHz CPU Core) |  | (28.6 MHz CPU Core) |  | (Co-Processor Engine)   |  |
|  +---------------------+  +---------------------+  +-------------------------+  |
|             ^                        ^                          ^               |
|             |                        |                          |               |
|             v                        v                          v               |
|  +---------------------------------------------------------------------------+  |
|  |                           Bus Arbiter Interface                           |  |
|  |                    (High WRAM, Low WRAM, Registers)                       |  |
|  +---------------------------------------------------------------------------+  |
|             ^                        ^                          ^               |
|             |                        |                          |               |
|             v                        v                          v               |
|  +---------------------+  +---------------------+  +-------------------------+  |
|  |     VDP1 Thread     |  |     VDP2 Thread     |  |    SMPC & CD-ROM Thread |  |
|  |  (Geometry Engine)  |  | (Compositing/Video) |  | (System / Controller)   |  |
|  +---------------------+  +---------------------+  +-------------------------+  |
|                                                                                 |
|  +---------------------+  +---------------------+                               |
|  |    MC68000 Thread   |  |     SCSP Thread     |                               |
|  |   (Sound CPU Core)  |  | (Audio Synthesizer) |                               |
|  +---------------------+  +---------------------+                               |
+---------------------------------------------------------------------------------+
```

### 3.1. Thread Division Details
* **SH-2 Master Core**: Executes the main game loop, controls system boot, and dispatches SCU interrupts.
* **SH-2 Slave Core**: Wakened by SMPC commands to handle parallel arithmetic tasks (such as 3D vector transformations).
* **SCU DMA/DSP**: Handles system DMA requests and runs vector math programs on its internal DSP.
* **VDP1**: Receives commands via VRAM queues, drawing sprites/polygons to the framebuffer.
* **VDP2**: Reads layers from VRAM, performs color blending, and writes frames to the host window.
* **MC68000 / SCSP**: Operates as a separate system processing audio synthesis and running the sound driver.
* **SMPC / CD Block**: Processes controller hardware polling and interfaces with storage image streams.
