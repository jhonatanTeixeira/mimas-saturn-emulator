//! DSP do SCU: portas de carga (PPAF/PPD/PDA/PDD) reais, execução por reconhecimento.
//!
//! O único programa que a BIOS carrega no boot é um laço de cópia em blocos: `RA0` <- data[0],
//! `WA0` <- data[1], contador data[2] (palavras), com dois DMA por iteração passando por um
//! banco de 64 palavras (leitura do barramento e depois escrita). O destino é a RAM do SCSP,
//! fora do caminho de vídeo; por isso executamos o efeito (a cópia) em vez de um interpretador
//! completo do conjunto de instruções. Um programa desconhecido é registrado em `unknown`.

use crate::bus::SystemBus;

/// Programa de cópia em blocos (32 palavras) conforme carregado pela BIOS.
const COPY_PROGRAM: [u32; 32] = [
    0x00001C00, 0x00003604, 0x00003704, 0x00001C02, 0x00001D00, 0x00861540, 0x14003109, 0x00003100,
    0x00001D00, 0x00003005, 0x00001C03, 0x00003005, 0x00001C02, 0x83100000, 0x00001C03, 0x82100040,
    0x00001C03, 0x00823500, 0x10000000, 0xD308001F, 0x00000000, 0xD3400015, 0x00001F00, 0xC0012300,
    0xD3400018, 0x00001F00, 0xC000B300, 0xD340001B, 0x00000000, 0xD0000003, 0x00000000, 0xF0000000,
];

pub const PPAF_LE: u32 = 1 << 15;
pub const PPAF_EX: u32 = 1 << 16;

pub struct ScuDsp {
    prog: [u32; 256],
    data: [[u32; 64]; 4],
    load_pc: u8,
    data_addr: u8,
    pub running: bool,
    pub start_requested: bool,
    pub runs: u32,
    /// Palavras (iniciais) de programas não reconhecidos que tentaram executar.
    pub unknown: Vec<Vec<u32>>,
    pub log: Vec<String>,
}

impl Default for ScuDsp {
    fn default() -> Self {
        Self::new()
    }
}

impl ScuDsp {
    pub fn new() -> Self {
        Self {
            prog: [0; 256],
            data: [[0; 64]; 4],
            load_pc: 0,
            data_addr: 0,
            running: false,
            start_requested: false,
            runs: 0,
            unknown: Vec::new(),
            log: Vec::new(),
        }
    }

    pub fn write_ppaf(&mut self, v: u32) {
        if v & PPAF_LE != 0 {
            self.load_pc = v as u8;
        }
        if v & PPAF_EX != 0 {
            self.running = true;
            self.start_requested = true;
        }
    }

    pub fn read_ppaf(&self) -> u32 {
        ((self.running as u32) << 16) | self.load_pc as u32
    }

    pub fn write_ppd(&mut self, v: u32) {
        self.prog[self.load_pc as usize] = v;
        self.load_pc = self.load_pc.wrapping_add(1);
    }

    pub fn write_pda(&mut self, v: u32) {
        self.data_addr = v as u8;
    }

    pub fn write_pdd(&mut self, v: u32) {
        self.data[(self.data_addr >> 6) as usize & 3][(self.data_addr & 0x3F) as usize] = v;
        self.data_addr = self.data_addr.wrapping_add(1);
    }

    pub fn read_pdd(&mut self) -> u32 {
        let v = self.data[(self.data_addr >> 6) as usize & 3][(self.data_addr & 0x3F) as usize];
        self.data_addr = self.data_addr.wrapping_add(1);
        v
    }

    /// Executa o programa carregado (síncrono). Devolve `true` se terminou com interrupção de fim.
    pub fn run(&mut self, bus: &mut SystemBus) -> bool {
        self.start_requested = false;
        self.runs += 1;
        if self.prog[..32] == COPY_PROGRAM {
            let (src, dst, words) = (self.data[0][0] << 2, self.data[0][1] << 2, self.data[0][2]);
            self.log.push(format!(
                "DSP cópia em blocos: {src:08X} -> {dst:08X}, {words:#X} palavras"
            ));
            for i in 0..words {
                let v = bus.read32(src.wrapping_add(i * 4));
                bus.write16(dst.wrapping_add(i * 4), (v >> 16) as u16);
                bus.write16(dst.wrapping_add(i * 4 + 2), v as u16);
            }
        } else {
            self.log
                .push("DSP: PROGRAMA DESCONHECIDO — não executado".to_string());
            self.unknown.push(self.prog[..32].to_vec());
        }
        self.running = false;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::ram::Ram;

    #[test]
    fn known_copy_program_copies_words_and_clears_executing_flag() {
        let mut bus = SystemBus::new();
        bus.map(
            "a",
            0x0600_0000,
            0x10_0000,
            0x10_0000,
            Box::new(Ram::new(0x10_0000)),
            false,
        );
        for i in 0..8u32 {
            bus.write32(0x0600_0100 + i * 4, 0x1000_0000 + i);
        }
        let mut dsp = ScuDsp::new();
        dsp.write_ppaf(PPAF_LE);
        for w in COPY_PROGRAM {
            dsp.write_ppd(w);
        }
        dsp.write_pda(0);
        dsp.write_pdd(0x0600_0100 >> 2);
        dsp.write_pdd(0x0600_0200 >> 2);
        dsp.write_pdd(8);
        dsp.write_ppaf(PPAF_LE | PPAF_EX);
        assert_eq!(dsp.read_ppaf() >> 16 & 1, 1);
        dsp.run(&mut bus);
        assert_eq!(dsp.read_ppaf() >> 16 & 1, 0);
        for i in 0..8u32 {
            assert_eq!(bus.read32(0x0600_0200 + i * 4), 0x1000_0000 + i);
        }
    }

    #[test]
    fn unknown_programs_are_reported_not_silently_accepted() {
        let mut bus = SystemBus::new();
        let mut dsp = ScuDsp::new();
        dsp.write_ppd(0xDEAD_BEEF);
        dsp.write_ppaf(PPAF_EX);
        dsp.run(&mut bus);
        assert_eq!(dsp.unknown.len(), 1);
    }
}
