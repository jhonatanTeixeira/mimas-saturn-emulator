# Mimas: Sega Saturn Distributed Emulator in Rust

Mimas is a distributed Sega Saturn emulator designed to leverage multi-threaded architectures (specifically targeted at devices like the R36S).

## About this project

Every line of code in this repository was written by AI (Claude, via
Claude Code). The research, the architecture, and the engineering
direction — what to build and in what order, which trade-offs to accept,
how to cross-check a claimed fix against real hardware behavior instead of
trusting a self-consistent test — came from a human engineer driving the
process, not the AI on its own.

The motivation is direct: Saturn emulation remains one of the weaker
corners of the emulation scene, precisely because the hardware is
genuinely hard (two SH-2 CPUs running in lockstep, a custom DSP, VDP1/VDP2,
an onboard 68000 just for sound). This project is a deliberate test of a
specific claim — that AI can carry out a task this complex correctly, as
long as a human engineer is actually steering: setting the architecture,
deciding what "correct" means, and catching the difference between a bug
that looks fixed and a bug that is fixed. `CLAUDE.md` and `history.md`
document that process as it actually happened, including the mistakes and
the corrections, not just the result.

## Architecture
- **Distributed block model**: Each major hardware component (SH-2 Master, SH-2 Slave, SCU, VDP1/VDP2, SCSP/68000, SMPC/CS2) runs in its own system thread context.
- **Lock-free shared buffers**: Utilizes `arc-swap` and atomic primitives for efficient inter-thread communication.
- **Physical bus locking**: Replicates the physical bus lock overhead of DMA/SCU operations using `BusArbiter` with system condition variables (`Condvar`).
- **Bounded-slack lockstep**: Maintains synchrony between threads through bounded clock counters.

## Workspace Layout
- `saturn-core/`: Pure emulator engine with core component definitions (`SH-2`, `SCU`, `BusArbiter`, buffers, etc.).
- `saturn-frontend-native/`: Standalone application frontend.
- `saturn-frontend-libretro/`: Dynamic library core exposing the Libretro API interfaces.

## Building
```bash
cargo build --release
```

## Continuing this work

If you're picking this project up (human or agent), read in this order:

1. **`CLAUDE.md`** — the work loop: how to find and clear the next real
   boot wall, and the exact current wall as of the last session.
2. **`history.md`** — how the project got here and why specific
   non-obvious decisions were made.
3. **`.development/ROADMAP.md`** — milestone status (done/in-progress/not
   started).
4. **`.development/TASKS.md`** — the same, at finer granularity.
5. **`docs/`** — background reference: `PROJECT.md` and
   `saturn_architecture_report.md` (architecture), `ORIGINAL_REQUEST.md`
   (the original ask), `TEST_INFRA.md`/`TEST_READY.md` (test setup).
