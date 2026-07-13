# Project: Mimas Sega Saturn Emulator

## Architecture
Mimas is a distributed, multi-threaded Sega Saturn emulator written in Rust. It distributes emulation across 4 concurrent threads representing the Saturn's hardware blocks:
- **Core 0 (Thread 0)**: Master SH-2 CPU execution.
- **Core 1 (Thread 1)**: Slave SH-2 CPU execution.
- **Core 2 (Thread 2)**: SCU (System Control Unit) DSP, SMPC (System Manager and Peripheral Control), and CD-ROM CS2 subsystem.
- **Core 3 (Thread 3)**: VDP1 (Video Display Processor 1), VDP2 (Video Display Processor 2), and SCSP (Saturn Custom Sound Processor).

### Data Flow & Communication
- **BusArbiter**: Synchronizes access to system buses and memory. Blocks CPU execution during active DMA transfers.
- **Bounded-slack Lockstep Synchronization**: Cores synchronize periodically to ensure no thread drifts more than a specified cycles/slack limit (e.g., 1000 cycles) ahead of others.
- **Shared Buffers**: Work RAM, VRAM, and Frame/Audio Buffers are shared between threads using thread-safe pointers (e.g., `Arc<RwLock<...>>` or direct atomic synchronization).

---

## Code Layout
- `mimas/Cargo.toml`: Workspace configuration.
- `mimas/saturn-core/`: Contains the emulation logic.
  - `src/lib.rs`: Library entry point.
  - `src/bus_arbiter.rs`: DMA arbitration.
  - `src/sh2.rs`: Master and Slave SH-2 emulation.
  - `src/scu.rs`: SCU DSP and DMA.
  - `src/smpc.rs`: SMPC peripheral control.
  - `src/vdp.rs`: VDP1 and VDP2 sprite and background scroll screens.
  - `src/scsp.rs`: Audio synthesis and SCSP registers.
  - `src/cdrom.rs`: CD-ROM / CS2 controller and CHD streaming.
  - `src/shared_buffers.rs`: Shared RAM, VRAM, and framebuffer structures.
  - `src/sync.rs`: Bounded-slack lockstep sync.
- `mimas/saturn-frontend-native/`: Standalone CLI wrapper that runs Mimas.
  - `src/main.rs`: CLI loader for BIOS, CHD, and execution loops.
- `mimas/saturn-frontend-libretro/`: Libretro core wrapper.
  - `src/lib.rs`: C-linkage APIs (`retro_init`, `retro_run`, etc.).

---

## Milestones

| # | Name | Scope | Dependencies | Status |
|---|------|-------|-------------|--------|
| 1 | Sync & Bus Architecture | Multi-threaded core initialization, lockstep sync, BusArbiter completion | None | DONE |
| 2 | CPU & Memory Emulation | SH-2 CPU decoding, pipeline, FRT, cache coherence, memory map | M1 | DONE |
| 3 | System Controllers & SCU | SCU DSP/DMA, SMPC controller, register mapping | M2 | DONE |
| 4 | Graphics & Sound (VDP/SCSP) | VDP1 sprite engine, VDP2 background screens, SCSP sound registers | M3 | DONE |
| 5 | CD-ROM Subsystem (CS2/CHD) | CS2 controller commands, CHD stream/sector parsing | M3 | DONE |
| 6 | Frontend & Target Loader | Native/libretro frontends, BIOS/CHD file loading, audio/video streaming | M4, M5 | DONE |

---

## Interface Contracts

### 1. `BusArbiter`
```rust
pub struct BusArbiter {
    locked_by_dma: AtomicBool,
    unlock_signal: Condvar,
    unlock_mutex: Mutex<()>,
}
impl BusArbiter {
    pub fn new() -> Self;
    pub fn acquire_bus(&self); // Blocks current thread if DMA is active
    pub fn lock_for_dma(&self);
    pub fn unlock_from_dma(&self);
    pub fn is_locked(&self) -> bool;
}
```

### 2. Lockstep Synchronizer (`LockStepSync`)
```rust
pub struct LockStepSync {
    // Thread coordination via atomic barrier or condition variables
}
impl LockStepSync {
    pub fn new(num_threads: usize, slack_limit: u64) -> Self;
    pub fn sync_core(&self, core_id: usize, current_cycles: u64); // Synchronizes calling thread
}
```

### 3. CPU Core State and Step (`Sh2`)
```rust
pub struct Sh2 {
    pub is_slave: bool,
    pub pc: u32,
    // ... registers and peripherals
}
impl Sh2 {
    pub fn step(&mut self);
    pub fn read_word(&mut self, addr: u32) -> u16;
    pub fn write_word(&mut self, addr: u32, val: u16);
}
```

### 4. CD-ROM CHD Interface
```rust
pub struct Cdrom {
    // CHD-rs reader
}
impl Cdrom {
    pub fn open_chd(path: &str) -> Result<Self, String>;
    pub fn read_sector(&mut self, lba: u32, buffer: &mut [u8]) -> Result<(), String>;
    pub fn send_command(&mut self, cmd: &[u8]) -> Vec<u8>;
}
```
