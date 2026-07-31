# Plan: Yabause Fixture Extraction & Real-BIOS Test Integration

This document outlines the strategy for running the reference **Yabause** emulator, capturing execution fixtures at key BIOS boot milestones, and feeding these real-hardware states into the **Mimas** unit test suite.


/mnt/jhonatanteixeira/Novo volume/projects/jhon/dreams/retroarch-cores/yabause/
/mnt/jhonatanteixeira/Novo volume/projects/jhon/dreams/retroarch-cores/yabauseut/

---

## 🎯 Objectives
1. **Trace BIOS Execution**: Identify precisely where the BIOS initializes each subsystem (SMPC, VDP2, CD-ROM, etc.).
2. **Extract Real Fixtures**: Export snapshots of memory regions (Work RAM, VRAM, CRAM) and hardware registers at these execution milestones.
3. **Enhance Test Fidelity**: Replace mocked unit test data in Mimas with real state fixtures from Yabause to verify correct register translation and hardware behavior.

---

## 🛠️ Extraction Strategies

### Option A: Standalone Yabause with GDB/CLI Debugger (Recommended)
Standalone Yabause features a built-in SH-2 debugger and command-line support. We can automate memory dumps using a custom script.
* **How it works**:
  1. Compile/Run Yabause with debugging symbols or use the command-line interface.
  2. Set breakpoints on key BIOS entry points (e.g., SMPC command completions, CD-ROM status checks).
  3. When a breakpoint hits, dump the relevant memory maps using the debugger console:
     ```text
     # Example Yabause CLI / GDB dump commands
     dump binary memory wram_low.bin 0x00200000 0x00300000
     dump binary memory wram_high.bin 0x06000000 0x06100000
     dump binary memory vdp2_regs.bin 0x05F80000 0x05F80080
     ```

### Option B: RetroArch + Libretro Yabause Core with Custom Logging Patches
Since `retroarch` is already installed, we can download the Yabause Libretro core (`yabause_libretro.so`) and inspect its memory space.
* **How it works**:
  1. Clone the Yabause libretro repository.
  2. Insert a dumping hook into the main loop (`YabauseFrame` or `SmpcCmdIntback`):
     ```c
     // Inject dump code directly in C/C++ source
     if (SH2->PC == 0x0000338C) {
         FILE *f = fopen("yabause_smpc_state.bin", "wb");
         fwrite(SmpcRegs, 1, 0x80, f);
         fclose(f);
     }
     ```
  3. Compile the core and run it via RetroArch:
     ```bash
     retroarch -L ./yabause_libretro.so --bios <path-to-bios>
     ```

---

## 📋 Integration into Mimas Unit Tests

Once the binary fixtures (`.bin` files) are extracted, we can load them into Mimas unit tests to simulate real boot states.

### 1. Fixture Directory Structure
We will store the extracted files in:
```text
saturn-core/tests/fixtures/
  ├── bios_post_smpc_wram_high.bin
  ├── bios_post_smpc_regs.bin
  ├── bios_post_vdp2_vram.bin
  └── bios_post_vdp2_regs.bin
```

### 2. Loading Fixtures in Unit Tests
We can write a Rust helper function in Mimas to populate the emulator's `WorkRam` structure using these real-world files:

```rust
fn load_fixture(path: &str) -> Vec<u8> {
    std::fs::read(path).expect("Failed to load test fixture")
}

#[test]
fn test_vdp2_compositor_with_real_bios_state() {
    let system = SaturnSystem::new();

    // Load real BIOS register states from Yabause
    let real_vdp2_regs = load_fixture("tests/fixtures/bios_post_vdp2_regs.bin");
    {
        let mut regs = system.work_ram.vdp2_regs.write().unwrap();
        regs.copy_from_slice(&real_vdp2_regs);
    }

    // Perform composition and assert that Mimas generates the expected pixels
    let frame = render_backdrop(&system.work_ram);
    // Assert against real pixel color values
}
```

---

## 🚀 Recommended Action Plan
1. **Define Breakpoints**: Identify target SH-2 PC addresses (e.g., `0x338C` for SMPC init, `0x3BAE` for CD-ROM boot phase, etc.) using `tools/sh2dis.py` and logs.
2. **Build standalone Yabause or libretro core** with custom debug dump hooks.
3. **Execute and Dump**: Run Yabause with the real Sega Saturn BIOS and output the state files.
4. **Create Rust Fixture Loader**: Implement fixture loading in Mimas to drive regression tests.


## RetroArch + Core Yabause customizado (inserindo ganchos de dump em C++ diretamente no código do core do libretro).

  A abordagem que nos dará dados mais úteis, precisos e profundos é a Opção B: RetroArch + Core Yabause (Libretro) com patches de dump em C++.

  ### Por que esta abordagem é superior?

  1. Acesso a Estados Internos Ocultos:
  Muitas informações críticas do Sega Saturn não estão mapeadas no barramento principal de memória da CPU (SH-2). Por exemplo:
      • O estado interno da FSM (Máquina de Estados) do bloco de CD-ROM.
      • A lista de comandos pendentes no buffer interno do VDP1.
      • Estruturas internas de controle de sincronismo e áudio do SCSP.
      Com a depuração externa simples (GDB), só conseguiríamos ler a RAM física. Com um patch em C++ no código-fonte do core, podemos acessar a memória interna do emulador e exportar essas estruturas complexas exatamente
      como o Yabause as interpreta.
  1. Dumps Automatizados e Precisos por Evento:
  Em vez de adivinhar o momento exato de pausar o emulador no GDB, podemos programar o código C++ para realizar o dump no instante exato em que um evento de hardware ocorre (por exemplo: "no momento em que o registro de
  comando do CD-ROM é escrito" ou "no final do primeiro frame em que o VDP2 ativa a tela").
  1. Garantia de Sincronismo:
  Garantimos que todas as threads internas do Yabause (SH-2 Master, Slave, VDP2) sejam congeladas de forma atômica no mesmo ciclo de clock para que o dump de memória represente um estado de sistema perfeitamente
  consistente.

  ──────
  ### O que faremos a seguir:

  Se concordar com esta abordagem, vou clonar o repositório do core do Yabause para Libretro, encontrar os arquivos responsáveis pelo loop principal do SH-2/SMPC e preparar as rotinas de dump em C++ nos pontos que
  desejamos investigar



  Retroarch is installed in: /usr/bin/retroarch

  Use the already present yabause on the machine to build and run

  Bios can be found in: /mnt/jhonatanteixeira/Novo volume/games/r36s/bios/