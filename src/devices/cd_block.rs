//! CD Block (stub comportamental). Fora do caminho de vídeo: não lê disco. Expõe o relatório
//! periódico de status (drive em PAUSE, faixa de dados 1, FAD 150) que a BIOS observa no
//! reset — `CR1=0x2100 CR2=0x4101` foi lido do trace de referência — e responde a qualquer
//! comando repetindo esse relatório com HIRQ.CMOK, o suficiente para a sequência seguir.

use crate::bus::MemoryDevice;

const HIRQ: u32 = 0x9_0008;
const HMASK: u32 = 0x9_000C;
const CR1: u32 = 0x9_0018;
const CR4: u32 = 0x9_0024;

/// Bit "comando aceito" do HIRQ.
pub const HIRQ_CMOK: u16 = 0x0001;
/// Bit "subcódigo Q atualizado": um drive com disco girando o levanta a cada 1/75 s.
pub const HIRQ_SCDQ: u16 = 0x0400;
/// 1/75 s em ciclos do SH-2 (~28,6 MHz).
const SCDQ_PERIOD: u64 = 381_800;

pub struct CdBlock {
    hirq: u16,
    hmask: u16,
    /// Registradores de comando escritos pela BIOS (CR1..CR4).
    cmd: [u16; 4],
    /// Resposta visível nos CR1..CR4.
    resp: [u16; 4],
    pub commands: Vec<u16>,
    scdq_acc: u64,
}

impl Default for CdBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl CdBlock {
    pub fn new() -> Self {
        Self {
            hirq: HIRQ_CMOK,
            hmask: 0,
            cmd: [0; 4],
            resp: Self::status_report(),
            commands: Vec::new(),
            scdq_acc: 0,
        }
    }

    /// status 0x21 = PAUSE | periódico; 0x41/0x01 = faixa de dados nº 1; índice 1; FAD 150.
    fn status_report() -> [u16; 4] {
        [0x2100, 0x4101, 0x0100, 0x0096]
    }

    fn execute(&mut self) {
        self.commands.push(self.cmd[0]);
        self.resp = Self::status_report();
        self.hirq |= HIRQ_CMOK;
    }

    /// Avança o relógio do drive: levanta o SCDQ periodicamente.
    pub fn tick(&mut self, cycles: u64) {
        self.scdq_acc += cycles;
        while self.scdq_acc >= SCDQ_PERIOD {
            self.scdq_acc -= SCDQ_PERIOD;
            self.hirq |= HIRQ_SCDQ;
        }
    }

    fn cr_index(off: u32) -> Option<usize> {
        (CR1..=CR4)
            .contains(&off)
            .then(|| ((off - CR1) / 4) as usize)
    }
}

impl MemoryDevice for CdBlock {
    fn read_byte(&mut self, off: u32) -> u8 {
        let w = self.read_word(off & !1);
        if off & 1 == 0 {
            (w >> 8) as u8
        } else {
            w as u8
        }
    }
    fn write_byte(&mut self, _off: u32, _v: u8) {}

    fn read_word(&mut self, off: u32) -> u16 {
        let off = off & 0xF_FFFE;
        match off {
            HIRQ => self.hirq,
            HMASK => self.hmask,
            o => Self::cr_index(o & !3)
                .filter(|_| o & 2 == 0)
                .map_or(0, |i| self.resp[i]),
        }
    }

    fn write_word(&mut self, off: u32, v: u16) {
        let off = off & 0xF_FFFE;
        match off {
            HIRQ => self.hirq &= v,
            HMASK => self.hmask = v,
            o => {
                if let Some(i) = Self::cr_index(o) {
                    self.cmd[i] = v;
                    if i == 3 {
                        self.execute();
                    }
                }
            }
        }
    }

    fn read_long(&mut self, off: u32) -> u32 {
        // Porta de dados / registradores acessados como long: metades alta e baixa.
        ((self.read_word(off) as u32) << 16) | self.read_word(off + 2) as u32
    }
    fn write_long(&mut self, off: u32, v: u32) {
        self.write_word(off, (v >> 16) as u16);
        self.write_word(off + 2, v as u16);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_reports_paused_drive_with_periodic_flag() {
        let mut cd = CdBlock::new();
        assert_eq!(cd.read_word(CR1) >> 8 & 0x2F, 0x21);
        assert_eq!(cd.read_word(CR1 + 4), 0x4101);
    }

    #[test]
    fn scdq_is_raised_periodically_and_cleared_by_writing_zero() {
        let mut cd = CdBlock::new();
        assert_eq!(cd.read_word(HIRQ) & HIRQ_SCDQ, 0);
        cd.tick(SCDQ_PERIOD);
        assert_eq!(cd.read_word(HIRQ) & HIRQ_SCDQ, HIRQ_SCDQ);
        cd.write_word(HIRQ, !HIRQ_SCDQ);
        assert_eq!(cd.read_word(HIRQ) & HIRQ_SCDQ, 0);
    }

    #[test]
    fn writing_cr4_executes_the_command_and_sets_cmok() {
        let mut cd = CdBlock::new();
        cd.write_word(HIRQ, 0);
        assert_eq!(cd.read_word(HIRQ), 0);
        for (i, v) in [0x0400u16, 0, 0, 0].iter().enumerate() {
            cd.write_word(CR1 + 4 * i as u32, *v);
        }
        assert_eq!(cd.read_word(HIRQ) & HIRQ_CMOK, HIRQ_CMOK);
        assert_eq!(cd.commands, vec![0x0400]);
    }
}
