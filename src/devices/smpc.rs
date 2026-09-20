//! SMPC (stub comportamental). Fora do caminho de vídeo: comandos terminam na hora e o
//! INTBACK devolve status simulado (RTC fixo, região, sem periféricos) — o bastante para a
//! BIOS seguir adiante. Registradores ficam em offsets ímpares (2N+1).

use crate::bus::MemoryDevice;

const IREG0: usize = 0x01;
const COMREG: usize = 0x1F;
const OREG0: usize = 0x21;
const SR: usize = 0x61;
const SF: usize = 0x63;

/// Código de área devolvido no OREG9 (0x04 = América do Norte, NTSC).
pub const AREA_CODE: u8 = 0x04;

pub struct Smpc {
    mem: [u8; 0x80],
    /// A BIOS espera a interrupção "system manager" do SCU ao fim do INTBACK.
    pub irq_pending: bool,
    /// Pending request to power the sound 68000 on (`SNDON`) or off (`SNDOFF`).
    pub sound_on: Option<bool>,
    pub commands: Vec<u8>,
}

impl Default for Smpc {
    fn default() -> Self {
        Self::new()
    }
}

impl Smpc {
    pub fn new() -> Self {
        let mut mem = [0u8; 0x80];
        mem[0x75] = 0x7F; // PDR1 sem periférico
        mem[0x77] = 0x7F;
        Self {
            mem,
            irq_pending: false,
            sound_on: None,
            commands: Vec::new(),
        }
    }

    fn oreg(&mut self, n: usize, v: u8) {
        self.mem[OREG0 + 2 * n] = v;
    }

    fn execute(&mut self, cmd: u8) {
        self.commands.push(cmd);
        match cmd {
            0x06 => self.sound_on = Some(true), // SNDON: release the 68000 from reset
            0x07 => self.sound_on = Some(false), // SNDOFF
            0x10 => self.intback(),
            _ => {}
        }
        self.mem[SF] = 0;
        self.mem[COMREG] = cmd;
    }

    /// INTBACK: status do sistema (IREG0 bit 0) e/ou dados de periféricos (IREG1 bit 3).
    fn intback(&mut self) {
        let want_status = self.mem[IREG0] & 1 != 0;
        let want_pad = self.mem[IREG0 + 2] & 8 != 0;
        if want_status {
            self.oreg(0, 0x80); // STE: status válido
            let rtc = [0x19, 0x99, 0x2A, 0x12, 0x12, 0x00, 0x00]; // séc./min./hora/dia/mês+semana/ano BCD
            for (i, b) in rtc.iter().enumerate() {
                self.oreg(1 + i, *b);
            }
            self.oreg(8, 0x00); // código do cartucho
            self.oreg(9, AREA_CODE);
            self.oreg(10, 0x00); // status do sistema 1
            self.oreg(11, 0x00); // status do sistema 2
            for i in 12..16 {
                self.oreg(i, 0x00); // SMEM
            }
            for i in 16..31 {
                self.oreg(i, 0x00);
            }
            self.mem[SR] = if want_pad { 0xC0 } else { 0x40 };
        } else {
            self.mem[SR] = 0x00;
        }
        if want_pad {
            // Sem periféricos conectados nas duas portas.
            self.oreg(0, 0xF0);
            self.oreg(1, 0xF0);
            self.mem[SR] = 0x20;
        }
        self.irq_pending = true;
    }
}

impl MemoryDevice for Smpc {
    fn read_byte(&mut self, off: u32) -> u8 {
        self.mem[(off & 0x7F) as usize]
    }
    fn write_byte(&mut self, off: u32, v: u8) {
        let o = (off & 0x7F) as usize;
        match o {
            COMREG => self.execute(v),
            SR | 0x21..=0x5F if o >= OREG0 => {} // somente leitura
            _ => self.mem[o] = v,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `SNDON` and `SNDOFF` are how the BIOS releases and halts the sound 68000. Losing
    /// either one leaves the driver either never started or never stopped, and the mailbox
    /// handshake then goes one-sided.
    #[test]
    fn sndon_and_sndoff_raise_the_power_request() {
        let mut smpc = Smpc::new();
        assert_eq!(smpc.sound_on, None, "nada pedido antes de um comando");

        smpc.write_byte(COMREG as u32, 0x06);
        assert_eq!(smpc.sound_on.take(), Some(true), "SNDON liga o 68000");

        smpc.write_byte(COMREG as u32, 0x07);
        assert_eq!(smpc.sound_on.take(), Some(false), "SNDOFF o desliga");

        // An unknown command is recorded and changes nothing else.
        smpc.write_byte(COMREG as u32, 0x99);
        assert_eq!(smpc.sound_on, None);
        assert_eq!(smpc.commands, vec![0x06, 0x07, 0x99]);
    }
}
