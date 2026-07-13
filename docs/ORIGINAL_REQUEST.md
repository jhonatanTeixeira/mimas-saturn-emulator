# Original User Request

## Initial Request — 2026-07-09T23:44:48Z

Implement Mimas: a distributed, multi-threaded Sega Saturn emulator written in Rust that follows the specified architectural plan.

Working directory: /mnt/jhonatanteixeira/Novo volume/projects/jhon/dreams/retroarch-cores/mimas
Integrity mode: development

## Requirements

### R1. Distributed Core Architecture & BusArbiter
- Emulate the Sega Saturn hardware blocks running on concurrent threads (Master SH-2 on Core 0, Slave SH-2 on Core 1, SCU/SMPC/CS2 on Core 2, VDP1/2 and SCSP on Core 3).
- Implement the `BusArbiter` to block CPU execution during active DMA.
- Implement the bounded-slack lockstep synchronization between threads.

### R2. CPU & Chip Emulation
- Emulate Hitachi SH-2 (Master and Slave) instruction decoding, pipeline execution, cache coherence, and FRT.
- Emulate the SCU (System Control Unit) VLIW DSP instruction set and DMA registers.
- Emulate the SMPC controller, VDP1 sprite engine, VDP2 background scroll screens, and SCSP DSP sound registers.

### R3. CD-ROM (CS2) with CHD Support
- Implement the CD-ROM controller block (CS2) parsing command sets.
- Add support for loading/streaming sector data from CHD formatted disk images (using crates for CHD support).

### R4. Native Frontend & Target Loading
- Load Saturn BIOS from `/mnt/jhonatanteixeira/Novo volume/games/r36s/bios/dc/`.
- Load the game from `/mnt/jhonatanteixeira/Novo volume/games/r36s/saturn/Magic Knight Rayearth (USA).chd`.
- Render output frames and stream output audio.

## Acceptance Criteria

### Booting and Execution
- [ ] The workspace compiles under Rust stable.
- [ ] The emulator loads the BIOS from the specified path.
- [ ] The CD-ROM subsystem parses and loads Magic Knight Rayearth CHD sector data.
- [ ] Core execution steps through SH-2 code and shows boot progress in standard log output or window frames.

## Follow-up — 2026-07-09T23:47:53Z

The user has updated `saturn-core/src/lib.rs` and `saturn-frontend-native/src/main.rs` directly. They have:
1. Declared several new modules in `lib.rs`: `sync`, `cdrom`, `scu`, `smpc`, `vdp`, and `scsp`.
2. Added command-line argument parsing (`--bios`/`-b` and `--chd`/`-c`) in the native frontend `main.rs`.

Please instruct the orchestrator and implementors to:
- Follow the implementation plan strictly.
- Implement these newly declared modules inside the `saturn-core` crate.
- Stick to the distributed architecture guidelines, threads mapping, `BusArbiter` locks, and bounded-slack lockstep sync (`LockStepSync` in the newly declared `sync` module).
- Ensure the bios loader and CHD loader function correctly with the specified paths.

## Follow-up — 2026-07-09T23:50:10Z

The user has updated several files in the workspace:
1. `saturn-frontend-native/src/main.rs`: changed file path checks to `.is_file()`.
2. `saturn-core/src/shared_buffers.rs`: initialized `WorkRam` and `Vram` buffers using `vec![...].into_boxed_slice().try_into().unwrap()` instead of boxed array literals, and implemented `Default` for them.
3. `saturn-core/src/bus_arbiter.rs`: integrated the `sync::LockStepSync` type, implemented `Default`, and added `acquire_bus_sync(core_id, sync)` to set thread active status around lock checks.
4. `saturn-core/src/sh2.rs`: integrated `core_id`, `cycles`, and `sync` (`Option<Arc<LockStepSync>>`). Added `write_word`, wrapping logic in PC increments, and step-by-step synchronization via `sync.sync_core(self.core_id, self.cycles)` inside `run_loop`.

Please pass this update to the orchestrator (`5c36a42e...`) and implementors to ensure they build upon these exact definitions, write the matching `sync::LockStepSync` implementation, and ensure all emulator components integrate cleanly with this synchronization approach.

## Follow-up — 2026-07-09T23:50:17Z

The user has specified a core architectural requirement: Mimas must follow the double-buffer model as the real Sega Saturn does. Specifically:
- Mappings of shared VRAM and framebuffer toggle between VDP1/VDP2 should be modeled using `ArcSwap` (from the `arc-swap` crate) to handle atomic pointer swaps.
- This replicates the hardware V-Blank swap mechanism of the Saturn, allowing lock-free reads while the write operations complete on a separate buffer.
- The ring buffer for sound (SH-2 to SCSP) should use a bounded channel/queue as a circular queue between the producer and consumer threads.

Please instruct the orchestrator and developers to strictly implement this pattern for shared memory (VRAM, framebuffers, Sound RAM buffers) using `ArcSwap` and bounded synchronization buffers as outlined.
