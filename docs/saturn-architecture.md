# Sega Saturn Architecture — Overview & Index

This document used to attempt a full hardware reference manual (register maps, opcode tables, DMA/command protocols) derived from the Yabause codebase. That attempt was necessarily shallow — it named registers and gave rough bit-layouts without exhaustively covering every field, and some of its detail turned out imprecise (e.g. address-notation inconsistencies, an incomplete opcode table) once checked closely.

**That job has since been done properly, exhaustively, in `docs/hardware-reference/`** — one file per subsystem, every register/opcode/DMA-mode/command sourced *exclusively* from reading Yabause's C/C++ source directly, with a `yabause/src/<file>:<line>` citation on every claim so it's independently verifiable, plus a closing "known deviations" section per file cataloging real bugs/hacks/dead code found in Yabause along the way (not invented — verified against the source). Go there for exact hardware facts:

| Subsystem | Reference |
|---|---|
| Dual SH-2 CPU cores, on-chip peripherals, cache | [`hardware-reference/sh2-cpu.md`](hardware-reference/sh2-cpu.md) |
| System memory map, cache-through addressing, A-Bus CS0/CS1 cartridge | [`hardware-reference/memory-bus.md`](hardware-reference/memory-bus.md) |
| SCU: DMA controller, DSP co-processor, interrupt controller, timers | [`hardware-reference/scu.md`](hardware-reference/scu.md) |
| SMPC command protocol, INTBACK, peripheral/controller protocol | [`hardware-reference/smpc-peripheral.md`](hardware-reference/smpc-peripheral.md) |
| VDP1: registers, command table format, every draw command type | [`hardware-reference/vdp1.md`](hardware-reference/vdp1.md) |
| VDP2: ~100 registers, NBG/RBG layers, cycle patterns, color calc | [`hardware-reference/vdp2.md`](hardware-reference/vdp2.md) |
| SCSP: 32 voice slots, envelope/LFO, sound DSP, sound RAM DMA | [`hardware-reference/scsp.md`](hardware-reference/scsp.md) |
| CD block / CS2: CR1-4/HIRQ protocol, every CD command, disc image format | [`hardware-reference/cs2-cdblock.md`](hardware-reference/cs2-cdblock.md) |

For where Mimas's *current Rust implementation* stands relative to each of these, and the concrete plan to close the gap, see `docs/implementation-plans/<subsystem>.md`. For Mimas's own architecture (thread model, memory ownership, synchronization — as opposed to Saturn hardware facts), see `mimas-architecture-spec.md` and `mimas_emu_engineering_draft.md`.

---

## System Block Diagram & Bus Topology

Kept here as a single-page mental model of how the major components connect — a simplified overview, not authoritative on exact bus widths or addresses (defer to `hardware-reference/memory-bus.md` for those).

```mermaid
graph TD
    subgraph CPUCluster ["Dual SH-2 Core Complex"]
        MSH2["Master SH-2 CPU (28.6 MHz)"]
        SSH2["Slave SH-2 CPU (28.6 MHz)"]
        CacheM["Master 4KB Cache / RAM"]
        CacheS["Slave 4KB Cache / RAM"]
        MSH2 --- CacheM
        SSH2 --- CacheS
    end

    subgraph SystemControl ["SCU (System Control Unit)"]
        SCUDSP["SCU DSP"]
        SCUDMA["SCU DMA Controller (L0, L1, L2)"]
        Arbiter["Bus Arbiter"]
        SCUInterrupts["Interrupt Controller"]
    end

    subgraph GraphicsSubsystem ["Video Subsystem"]
        VDP1["VDP1 (Drawing Engine)"]
        VDP2["VDP2 (Display Engine)"]
        VDP1_RAM["VDP1 VRAM (512KB)"]
        VDP1_FB["Framebuffer (2x 256KB)"]
        VDP2_RAM["VDP2 VRAM (512KB)"]
        VDP2_CRAM["Color RAM (4KB)"]
        VDP1 --- VDP1_RAM
        VDP1 --- VDP1_FB
        VDP2 --- VDP2_RAM
        VDP2 --- VDP2_CRAM
    end

    subgraph SoundSubsystem ["Sound Subsystem (SCSP)"]
        M68K["MC68000 Sound CPU (11.3 MHz)"]
        SNDRAM["Sound RAM (512KB)"]
        SNDDSP["SCSP DSP (32 voices)"]
        M68K --- SNDRAM
        SNDDSP --- SNDRAM
    end

    SMPC["SMPC (System Manager / Controllers)"]
    CDBLOCK["CD-ROM Controller (CS2 Block)"]
    HWRAM["High Work RAM (2MB)"]
    LWRAM["Low Work RAM (1MB)"]

    MSH2 & SSH2 <==>|"System Bus"| Arbiter
    Arbiter <==> HWRAM
    Arbiter <==> LWRAM
    Arbiter <==>|"A-Bus"| CDBLOCK
    Arbiter <==>|"A-Bus"| SoundSubsystem
    Arbiter <==>|"A-Bus"| SMPC
    Arbiter <==>|"B-Bus/V-Bus"| GraphicsSubsystem
    SCUDMA <==> Arbiter
    SCUDSP <==> Arbiter
```
