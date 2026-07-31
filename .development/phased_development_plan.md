# Mimas Sega Saturn Emulator - Phased Greenfield Development Plan

This document outlines the step-by-step development plan for building the **Mimas** Sega Saturn emulator from scratch (from 0), following a clean parallel thread architecture.

---

## Phase 1: Project Scaffolding and Core Lockstep Sync
* **Focus**: Establish the workspace environment, the 8-thread component topography, the lockstep synchronizer, and region-partitioned memory maps.

### Goals of Phase 1
1. **Thread Topography Scaffold**:
   * Spawn 8 dedicated OS threads representing distinct hardware blocks:
     * `sh2-master` (Master SH-2 CPU)
     * `sh2-slave` (Slave SH-2 CPU)
     * `vdp1-draw` (VDP1 Drawing Engine)
     * `vdp2-composite` (VDP2 Display Compositor)
     * `m68k-sound-cpu` (MC68000 Sound CPU)
     * `scsp-synth` (SCSP Sound Synthesizer / Audio DSP)
     * `scu-dma-dsp` (SCU DMA & Math Co-Processor DSP)
     * `smpc-cd-block` (SMPC & CD-ROM block controller)
2. **Lockstep Synchronizer (`LockStepSync`)**:
   * Build a bounded-slack synchronization manager running across the 8 threads.
   * Support thread parking (`park_while_inactive`) and dynamic reactivation (`set_thread_active`), allowing inactive threads (e.g. parked Slave CPU or SCU DSP) to consume 0% host CPU.
3. **Region-Partitioned Memory maps**:
   * Segregate the Saturn address space into isolated, lock-striped components:
     * High WRAM: Mapped as 32 independent `RwLock<Box<[u8; 64KB]>>` stripes (totaling 2MB).
     * Low WRAM: 1MB `RwLock`.
     * Sound RAM: 512KB `RwLock` shared between sound components.
     * VRAM / Framebuffers: Mapped exclusively to graphic modules.

### Verification of Phase 1
* Spawning test compiles successfully, executing 8 concurrent loops synced via `LockStepSync` without deadlocking.

---

## Phase 2: Master SH-2 Core and Instruction Interpreter
* **Status**: ✅ Completed
* **Focus**: Implement the core 32-bit CPU register state, Status Register (`SR`) flags, memory address translator, and opcode decoder.

### Goals of Phase 2
1. **CPU State and SR Flags**:
   * Implement general purpose registers (`R0`-`R15`) and control structures (`PC`, `GBR`, `VBR`, `PR`, `MACH`, `MACL`).
   * Map Status Register (`SR`) flags: condition bit `T`, saturation bit `S`, division flags `M` and `Q`, and interrupt mask levels `I3`-`I0`.
2. **Instruction Decoder**:
   * Build a 16-bit nibble-based opcode decoder mapping the SuperH SH-2 instruction set.
   * Implement address modes (registers, memory-indirect, displacement-indexed) and delay slot branch semantics.
3. **On-Chip Peripherals**:
   * Implement the serial division unit (`DIV1` step bit arithmetic).
   * Mapped interrupt priorities (handling IRL levels 1 to 15).

### Verification of Phase 2
* Unit tests running basic opcode sequences (arithmetic, delay-slot jumps, 32-step division) return correct registers and status flags.

---

## Phase 3: System Controller (SMPC & SCU)
* **Status**: ✅ Completed
* **Focus**: Build the System Manager command protocol, SCU DMA channels, and the SCU DSP execution engine.

### Goals of Phase 3
1. **SMPC Command Processor**:
   * Implement the `COMREG` and status flag `SF` write handshakes.
   * Implement commands: `MSHON`, `SSHON`, `SSHOFF`, `SNDON`, `SNDOFF`, and the controller peripheral scanning loop (`INTBACK` populating `OREG0`-`OREG31`).
2. **SCU DMA Engine**:
   * Implement Level 0, 1, and 2 DMA channels.
   * Support **Direct Mode** (copying contiguous memory blocks) and **Indirect Mode** (parsing memory-based transfer descriptors containing source, destination, and length arrays).
3. **SCU DSP Interpreter**:
   * Implement the 256-word program RAM and 4-page data RAM (MD) registers.
   * Decode SCU DSP instructions: ALU arithmetic, MVI immediate loading, conditional JMP, and DMA transfers.

### Verification of Phase 3
* Verify that SCU DMA indirect lists copy sectors accurately, and running a test program on the SCU DSP interpreter completes with correct data RAM results.

---

## Phase 4: Sound Subsystem (MC68000 & SCSP)
* **Status**: ✅ Completed
* **Focus**: Implement the Motorola 68000 CPU emulator interface, SCSP voice channels, envelope generator, and output buffers.

### Goals of Phase 4
1. **MC68000 CPU Integration**:
   * Map the Sound CPU registers and execute M68K instructions in a timed execution loop.
   * Mapped address space: Sound RAM (`< 512KB`) and SCSP registers (`>= 0x100000`).
2. **SCSP Synthesizer & Envelope Generator**:
   * Implement the 32 independent synthesis voices (frequency, panning, LFO modulation).
   * Implement envelope shaping (Attack Rate, Decay Rate, Sustain Rate, Release Rate).
3. **Audio Streaming Buffer**:
   * Output mixed audio samples to a lockless ring buffer streaming to the host DAC.

### Verification of Phase 4
* Sound tests verify that M68K memory writes to SCSP slots change voice envelopes and push valid PCM waveforms to the audio stream.

---

## Phase 5: Display Composition & Video Subsystems (VDP1 and VDP2)
* **Status**: ✅ Completed
* **Focus**: Implement VDP2 background planes, cycle pattern memory arbitration, and VDP1 drawing primitives.

### Goals of Phase 5
1. **VDP2 Screen Layer Compositor**:
   * Implement scroll planes (`NBG0`-`NBG3`) and rotational background projection (`RBG0`).
   * Parse VRAM Cycle Pattern registers (`CYCA`/`CYCB`) to schedule memory access slots.
   * Implement Color RAM (CRAM) palette indexing and transparent priority overlays.
2. **VDP1 Geometry Engine**:
   * Implement command table parser reading 32-byte CMD table sequences from VDP1 VRAM.
   * Support drawing primitives: normal sprites, scaled sprites, distorted sprites, polygons, polylines, and lines.
3. **Double-Buffered Framebuffer Presentation**:
   * Implement bank swapping (`FBCR`) to transfer completed frames from `vdp1-draw` to `vdp2-composite` for display presentation.

### Verification of Phase 5
* The window presenter displays composite screen frames containing background layers and polygon sprites.

---

## Phase 6: Multi-Core Execution & CD-ROM Storage
* **Status**: ✅ Completed
* **Focus**: Boot the Slave SH-2 core under SMPC control and implement CD-ROM media streaming.

### Goals of Phase 6
1. **Slave SH-2 Core Boot**:
   * Enable the SMPC SSHON command to boot the `sh2-slave` thread.
   * Configure the dual-core sync margin under `LockStepSync` to prevent race conditions during parallel processing.
2. **CD Block / CS2 Subsystem**:
   * Implement command registers `CR1` - `CR4` and status flags in `HIRQ`.
   * Stream sector sectors from CHD images into the CS2 address range via DMA.

### Verification of Phase 6
* Emulation compiles and runs games from CD media, with both SH-2 cores running parallel game code.

---

## Phase 7: Debugging, Telemetry & Frame Dumping
* **Status**: ✅ Completed
* **Focus**: Implement high-performance telemetry metrics (WRAM accesses, thread idle times) and a zero-dependency frame dumper for debugging.

### Goals of Phase 7
1. **High-Performance Telemetry**:
   * Measure memory read/write frequencies in High WRAM stripes using atomic counters.
   * Trace idle waiting times (`std::time::Instant`) inside the lockstep barrier `sync_core` for all 8 emulated component threads.
2. **Zero-Dependency Frame Dumper**:
   * Add a top-down, standard 32-bit BMP image generator to dump any video frame directly to disk without runtime dependencies.

