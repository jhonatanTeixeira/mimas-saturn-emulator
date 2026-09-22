//! Registradores do SH7604 em 0xFFFFFE00-0xFFFFFFFF (FRT, WDT, INTC, DIVU, DMAC, BSC, cache).
//! DIVU, FRT e DMAC têm comportamento (a matemática e a temporização da BIOS dependem dos
//! dois primeiros; o DMAC é o que um jogo usa para copiar memória e espera pelo bit TE); os
//! demais são bancos de registradores que guardam e devolvem o que foi escrito.

use super::dmac::Dmac;

const SIZE: usize = 0x200;

pub struct OnChip {
    regs: [u8; SIZE],
    /// Ciclos da CPU, atualizados pela máquina antes de cada bloco (base do FRC).
    pub now: u64,
    frc_base: u16,
    frc_time: u64,
    pub dmac: Dmac,
}

impl Default for OnChip {
    fn default() -> Self {
        Self::new()
    }
}

impl OnChip {
    pub fn new() -> Self {
        let mut o = Self {
            regs: [0; SIZE],
            now: 0,
            frc_base: 0,
            frc_time: 0,
            dmac: Dmac::new(),
        };
        // Valores de reset relevantes.
        o.regs[0x11] = 0x00; // FTCSR
        o.regs[0x14] = 0xFF; // OCRA/OCRB
        o.regs[0x15] = 0xFF;
        o.regs[0x16] = 0x00; // TCR
        o.regs[0x17] = 0xE0; // TOCR
        o.regs[0x92] = 0x00; // CCR
        o
    }

    fn frc_divider(&self) -> u64 {
        match self.regs[0x16] & 3 {
            0 => 8,
            1 => 32,
            2 => 128,
            _ => 8,
        }
    }

    fn frc(&self) -> u16 {
        let ticks = (self.now.saturating_sub(self.frc_time)) / self.frc_divider();
        self.frc_base.wrapping_add(ticks as u16)
    }

    fn get(&self, off: usize, size: usize) -> u32 {
        let mut v = 0u32;
        for i in 0..size {
            v = (v << 8) | self.regs[(off + i) % SIZE] as u32;
        }
        v
    }

    fn put(&mut self, off: usize, size: usize, val: u32) {
        for i in 0..size {
            self.regs[(off + i) % SIZE] = (val >> (8 * (size - 1 - i))) as u8;
        }
    }

    fn rd(&mut self, off: u32, size: usize) -> u32 {
        let off = (off & 0x1FF) as usize;
        if Dmac::owns(off as u32) {
            return self.dmac.read(off as u32, size);
        }
        match off {
            0x12 | 0x13 => {
                let f = self.frc() as u32;
                match (off, size) {
                    (0x12, 2) => f,
                    (0x12, 1) => f >> 8,
                    (0x13, _) => f & 0xFF,
                    _ => f,
                }
            }
            _ => self.get(off, size),
        }
    }

    fn wr(&mut self, off: u32, size: usize, val: u32) {
        let off = (off & 0x1FF) as usize;
        if Dmac::owns(off as u32) {
            self.dmac.write(off as u32, size, val);
            return;
        }
        match off {
            // FRC: reinicia a contagem a partir do valor escrito.
            0x12 | 0x13 => {
                let cur = self.frc();
                let new = match (off, size) {
                    (0x12, 2) => val as u16,
                    (0x12, 1) => ((val as u16) << 8) | (cur & 0xFF),
                    _ => (cur & 0xFF00) | (val as u16 & 0xFF),
                };
                self.frc_base = new;
                self.frc_time = self.now;
            }
            // WDT: escrita de 16 bits com chave 0xA5 (WTCSR) / 0x5A (WTCNT) no byte alto.
            0x80 if size == 2 => match val >> 8 {
                0xA5 => self.regs[0x80] = val as u8,
                0x5A => self.regs[0x81] = val as u8,
                _ => {}
            },
            0x82 if size == 2 => {
                if val >> 8 == 0xA5 {
                    self.regs[0x83] &= val as u8;
                } else if val >> 8 == 0x5A {
                    self.regs[0x83] = val as u8;
                }
            }
            _ => {
                self.put(off, size, val);
                match off & !3 {
                    0x104 => self.divide_32(),
                    0x114 | 0x11C => self.divide_64(),
                    _ => {}
                }
                // Espelhos dos registradores de dividendo (0x118/0x11C = 0x110/0x114).
                if (0x118..0x120).contains(&off) {
                    let v = self.get(off, size);
                    self.put(off - 8, size, v);
                }
            }
        }
    }

    /// Interrupt level the INTC gives the DMAC (IPRA bits 11-8), and the pending request's
    /// vector, if a transfer-end interrupt is waiting.
    pub fn dmac_irq(&self) -> Option<(u32, u32)> {
        let (_, vector) = self.dmac.pending_irq()?;
        Some(((self.regs[0xE2] & 0xF) as u32, vector))
    }

    fn divisor(&self) -> i64 {
        self.get(0x100, 4) as i32 as i64
    }

    /// Escrita em DVDNT: divisão 32/32 com sinal.
    fn divide_32(&mut self) {
        let n = self.get(0x104, 4) as i32 as i64;
        self.finish_division(n);
    }

    /// Escrita em DVDNTL: divisão 64/32 com sinal.
    fn divide_64(&mut self) {
        let n = ((self.get(0x110, 4) as u64) << 32 | self.get(0x114, 4) as u64) as i64;
        self.finish_division(n);
    }

    fn finish_division(&mut self, n: i64) {
        let d = self.divisor();
        let (q, r, overflow) = if d == 0 {
            (
                (if n < 0 { i32::MIN } else { i32::MAX }) as i64,
                n & 0xFFFF_FFFF,
                true,
            )
        } else {
            let q = n.wrapping_div(d);
            let r = n.wrapping_rem(d);
            (q, r, q > i32::MAX as i64 || q < i32::MIN as i64)
        };
        let q = if overflow && d != 0 {
            (if q < 0 { i32::MIN } else { i32::MAX }) as i64
        } else {
            q
        };
        self.put(0x104, 4, q as u32);
        self.put(0x114, 4, q as u32);
        self.put(0x11C, 4, q as u32);
        self.put(0x110, 4, r as u32);
        self.put(0x118, 4, r as u32);
        if overflow {
            let cvr = self.get(0x108, 4) | 1;
            self.put(0x108, 4, cvr);
        }
    }
}

impl crate::bus::MemoryDevice for OnChip {
    fn read_byte(&mut self, off: u32) -> u8 {
        self.rd(off, 1) as u8
    }
    fn write_byte(&mut self, off: u32, v: u8) {
        self.wr(off, 1, v as u32)
    }
    fn read_word(&mut self, off: u32) -> u16 {
        self.rd(off, 2) as u16
    }
    fn write_word(&mut self, off: u32, v: u16) {
        self.wr(off, 2, v as u32)
    }
    fn read_long(&mut self, off: u32) -> u32 {
        self.rd(off, 4)
    }
    fn write_long(&mut self, off: u32, v: u32) {
        self.wr(off, 4, v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::MemoryDevice;

    #[test]
    fn divu_signed_32_by_32() {
        let mut o = OnChip::new();
        o.write_long(0x100, (-7i32) as u32); // DVSR
        o.write_long(0x104, 100); // DVDNT dispara
        assert_eq!(o.read_long(0x104) as i32, 100 / -7);
        assert_eq!(o.read_long(0x110) as i32, 100 % -7);
    }

    #[test]
    fn divu_64_by_32_and_overflow_flag() {
        let mut o = OnChip::new();
        o.write_long(0x100, 1000);
        o.write_long(0x110, 0x0000_0001); // DVDNTH
        o.write_long(0x114, 0x0000_0000); // DVDNTL dispara: 2^32 / 1000
        assert_eq!(o.read_long(0x114), ((1u64 << 32) / 1000) as u32);
        o.write_long(0x100, 0);
        o.write_long(0x104, 5);
        assert_eq!(o.read_long(0x108) & 1, 1, "OVF em divisão por zero");
    }

    #[test]
    fn frc_counts_with_cpu_cycles() {
        let mut o = OnChip::new();
        o.now = 800;
        assert_eq!(o.read_word(0x12), 100, "clk/8");
    }
}
