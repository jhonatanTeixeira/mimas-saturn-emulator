# GEMINI.md - Instructions and Workspace Guide for Mimas

This file provides system context, build/test commands, and guidelines for AI agents (specifically Antigravity / Gemini) working on the **Mimas** Sega Saturn emulator project.

---

## 🛠️ Build, Test & Run Commands

Always verify code correctness by building and running tests. Use the following commands:

### Build Commands
* **Build all workspace packages:**
  ```bash
  cargo build
  ```
* **Build workspace in release mode:**
  ```bash
  cargo build --release
  ```
* **Build native frontend in release mode:**
  ```bash
  cargo build -p saturn-frontend-native --release --bin saturn-frontend-native
  ```

### Run Commands
* **Run native frontend against real BIOS (watching Core 0's PC):**
  ```bash
  MIMAS_BOOT_WATCH_SECS=280 ./target/release/saturn-frontend-native --bios <path-to-real-bios.bin>
  ```
* **Disassemble a captured RAM dump (SH-2 side):**
  ```bash
  python3 tools/sh2dis.py /tmp/some_dump.bin 0x06000000
  ```

### Test Commands
* **Run all tests:**
  ```bash
  cargo test
  ```
* **Run specific tests (e.g., SCU DSP unit tests):**
  ```bash
  cargo test --package saturn-core scu_dsp
  ```

---

## 📁 Workspace Layout

* [`saturn-core/`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/saturn-core/): Core emulator engine definitions (SH-2, SCU, BusArbiter, buffers, sync).
* [`saturn-frontend-native/`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/saturn-frontend-native/): Standalone emulator application frontend.
* [`saturn-frontend-libretro/`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/saturn-frontend-libretro/): Dynamic library frontend exposing the Libretro API (RetroArch).
* [`.development/`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/.development/): Current blockers, bugs, roadmaps, and detailed task tracking.

---

## ⚙️ Architecture & Code Guidelines

Mimas utilizes a **distributed block model** matching the actual Sega Saturn hardware architecture:
1. **Multi-threading:** Each major component runs in its own system thread context (SH-2 Master, SH-2 Slave, SCU, VDP1/VDP2, SCSP/68000, SMPC/CS2).
2. **Synchronization:** Uses a bounded-slack lockstep method to maintain sync between threads without heavy lock overhead, coupled with atomic primitives and `arc-swap`.
3. **Bus Arbitration:** Replicates physical bus locking of DMA/SCU operations using `BusArbiter` with system condition variables (`Condvar`).

### Code Standards
* **Language:** Rust (Edition 2021).
* **Style:** Code must format correctly via `cargo fmt`.
* **Reliability:** Avoid unchecked assumptions. Always verify hardware behaviors and register configurations against real hardware/reference emulators (like Yabause or YabaSanshiro codebases) instead of guessing.
* **Documentation:** Preserve existing docstrings and comments. Keep inline documentation clean.

---

## 🎯 Current Status

Check the following files before starting any implementation task:
1. [`.development/current_blocker.md`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/.development/current_blocker.md) — The current wall preventing boot progress.
2. [`history.md`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/history.md) — Chronological development history and rationale.
3. [`.development/ROADMAP.md`](file:///mnt/jhonatanteixeira/Novo%20volume/projects/jhon/dreams/retroarch-cores/mimas/.development/ROADMAP.md) — High-level project milestone status.
