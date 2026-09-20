//! Roda o driver de som real da BIOS no núcleo 68000 do crate `m68k`, a partir de um
//! despejo da RAM de som feito por `mimasv2 --dump-sound-ram`.
//!
//! O objetivo é responder uma pergunta antes de comprometer o projeto com esse crate:
//! **o driver que a BIOS carrega roda nele, e o que ele escreve no SCSP?** Nada aqui
//! é emulador; é experimento, e o veredito é o que sai na tela.
//!
//!     cargo run --release --bin m68k_probe -- soundram.bin [ciclos]

use m68k::{AddressBus, CpuCore, CpuType};
use std::collections::BTreeMap;

/// Espaço de endereços visto pelo 68000 do Saturn: RAM de som em 0x000000 e os
/// registradores do SCSP em 0x100000.
struct SoundBus {
    ram: Vec<u8>,
    /// Registradores do SCSP: valor corrente e quantas escritas cada um recebeu.
    scsp: BTreeMap<u32, (u16, u32)>,
    escritas_scsp: u64,
    leituras_scsp: u64,
    fora_do_mapa: u64,
}

const RAM_MASK: u32 = 0x7_FFFF;
const SCSP_BASE: u32 = 0x10_0000;
const SCSP_FIM: u32 = 0x10_1000;

impl SoundBus {
    fn new(ram: Vec<u8>) -> Self {
        Self {
            ram,
            scsp: BTreeMap::new(),
            escritas_scsp: 0,
            leituras_scsp: 0,
            fora_do_mapa: 0,
        }
    }

    fn scsp_write(&mut self, reg: u32, v: u16) {
        self.escritas_scsp += 1;
        let e = self.scsp.entry(reg).or_insert((0, 0));
        e.0 = v;
        e.1 += 1;
    }
}

impl AddressBus for SoundBus {
    fn read_byte(&mut self, address: u32) -> u8 {
        let a = address & 0xFF_FFFF;
        if a < 0x8_0000 {
            self.ram[(a & RAM_MASK) as usize]
        } else if (SCSP_BASE..SCSP_FIM).contains(&a) {
            self.leituras_scsp += 1;
            let reg = a & !1;
            let w = self.scsp.get(&reg).map(|e| e.0).unwrap_or(0);
            if a & 1 == 0 { (w >> 8) as u8 } else { w as u8 }
        } else {
            self.fora_do_mapa += 1;
            0xFF
        }
    }

    fn read_word(&mut self, address: u32) -> u16 {
        ((self.read_byte(address) as u16) << 8) | self.read_byte(address.wrapping_add(1)) as u16
    }

    fn read_long(&mut self, address: u32) -> u32 {
        ((self.read_word(address) as u32) << 16) | self.read_word(address.wrapping_add(2)) as u32
    }

    fn write_byte(&mut self, address: u32, value: u8) {
        let a = address & 0xFF_FFFF;
        if a < 0x8_0000 {
            self.ram[(a & RAM_MASK) as usize] = value;
        } else if (SCSP_BASE..SCSP_FIM).contains(&a) {
            let reg = a & !1;
            let atual = self.scsp.get(&reg).map(|e| e.0).unwrap_or(0);
            let novo = if a & 1 == 0 {
                (atual & 0x00FF) | ((value as u16) << 8)
            } else {
                (atual & 0xFF00) | value as u16
            };
            self.scsp_write(reg, novo);
        } else {
            self.fora_do_mapa += 1;
        }
    }

    fn write_word(&mut self, address: u32, value: u16) {
        let a = address & 0xFF_FFFF;
        if (SCSP_BASE..SCSP_FIM).contains(&a) {
            self.scsp_write(a & !1, value);
        } else {
            self.write_byte(address, (value >> 8) as u8);
            self.write_byte(address.wrapping_add(1), value as u8);
        }
    }

    fn write_long(&mut self, address: u32, value: u32) {
        self.write_word(address, (value >> 16) as u16);
        self.write_word(address.wrapping_add(2), value as u16);
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let caminho = args.next().unwrap_or_else(|| {
        eprintln!("uso: m68k_probe <soundram.bin> [ciclos]");
        std::process::exit(2);
    });
    let orcamento: i32 = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);

    let ram = std::fs::read(&caminho).expect("não consegui ler o despejo");
    assert_eq!(ram.len(), 0x8_0000, "a RAM de som tem 512 KB");
    println!(
        "RAM de som: {} bytes não-zero; vetores SP={:08X} PC={:08X}",
        ram.iter().filter(|b| **b != 0).count(),
        u32::from_be_bytes([ram[0], ram[1], ram[2], ram[3]]),
        u32::from_be_bytes([ram[4], ram[5], ram[6], ram[7]]),
    );

    let mut bus = SoundBus::new(ram);
    let mut cpu = CpuCore::new();
    cpu.set_cpu_type(CpuType::M68000);
    cpu.reset(&mut bus);
    println!("após reset: PC={:08X}", cpu.pc);

    // Quem visitou cada PC, e em que ordem — o mesmo filtro de novidade do trace da SH-2.
    let mut vistos: BTreeMap<u32, u64> = BTreeMap::new();
    let mut instrucoes = 0u64;
    let mut ciclos = 0i64;
    let mut saida = String::new();

    let mut restante = orcamento;
    while restante > 0 {
        let fatia = restante.min(10_000);
        let r = cpu.run_for_cycles(&mut bus, fatia);
        ciclos += r.cycles as i64;
        instrucoes += r.instructions as u64;
        *vistos.entry(cpu.pc).or_insert(0) += 1;
        restante -= r.cycles.max(1);
        let quebrou = !matches!(format!("{:?}", r.exit).as_str(), "BudgetExhausted");
        if quebrou {
            saida = format!("{:?}", r.exit);
            break;
        }
    }

    println!(
        "executou {instrucoes} instruções em {ciclos} ciclos; saída: {}",
        if saida.is_empty() {
            "orçamento esgotado".into()
        } else {
            saida
        }
    );
    println!("PC final: {:08X}", cpu.pc);
    println!(
        "SCSP: {} escritas em {} registradores distintos, {} leituras; {} acessos fora do mapa",
        bus.escritas_scsp,
        bus.scsp.len(),
        bus.leituras_scsp,
        bus.fora_do_mapa
    );

    if !bus.scsp.is_empty() {
        println!("registradores do SCSP tocados (offset: valor, escritas):");
        for (reg, (v, n)) in bus.scsp.iter().take(24) {
            let off = reg - SCSP_BASE;
            let slot = off / 0x20;
            let dentro = off % 0x20;
            let quem = if off < 0x400 {
                format!("slot {slot:2} +{dentro:02X}")
            } else {
                format!("comum  +{off:03X}")
            };
            println!("  {off:04X} [{quem}] = {v:04X}  ({n}x)");
        }
    }

    println!("caixa de correio 0x700: {:02X?}", &bus.ram[0x700..0x710]);
}
