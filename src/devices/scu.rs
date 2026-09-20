//! SCU: controlador de interrupções (IST/IMS) e DMA — caminho de vídeo real, porque é ele
//! que leva dados para a VRAM e entrega VBlank/fim de desenho à CPU. O DSP e as demais
//! funções são apenas bancos de registradores.

use crate::bus::{MemoryDevice, SystemBus};
use crate::devices::scu_dsp::ScuDsp;

pub const IRQ_VBLANK_IN: u32 = 0;
pub const IRQ_VBLANK_OUT: u32 = 1;
pub const IRQ_HBLANK_IN: u32 = 2;
pub const IRQ_TIMER0: u32 = 3;
pub const IRQ_TIMER1: u32 = 4;
pub const IRQ_SMPC: u32 = 7;
pub const IRQ_DMA2_END: u32 = 9;
pub const IRQ_DMA1_END: u32 = 10;
pub const IRQ_DMA0_END: u32 = 11;
pub const IRQ_SPRITE_END: u32 = 13;

/// Nível de prioridade entregue ao SH-2 para cada bit interno do IST.
const LEVELS: [u32; 16] = [0xF, 0xE, 0xD, 0xC, 0xB, 0xA, 9, 8, 8, 6, 6, 5, 3, 2, 0, 0];

/// Fatores de início de DMA (DxMD bits 2-0).
pub const DMA_FACTOR_VBLANK_IN: u32 = 0;
pub const DMA_FACTOR_VBLANK_OUT: u32 = 1;
pub const DMA_FACTOR_HBLANK_IN: u32 = 2;
pub const DMA_FACTOR_SPRITE_END: u32 = 6;
pub const DMA_FACTOR_SOFTWARE: u32 = 7;

#[derive(Default, Clone, Copy)]
struct DmaChannel {
    r: u32,
    w: u32,
    c: u32,
    ad: u32,
    en: u32,
    md: u32,
    armed: bool,
}

pub struct Scu {
    ch: [DmaChannel; 3],
    pub ist: u32,
    pub ims: u32,
    regs: [u32; 64],
    ready: Vec<usize>,
    pub dma_log: Vec<String>,
    /// Escritas nas portas do DSP (0x80..0x8C) na ordem: (offset, valor).
    pub dsp_log: Vec<(u32, u32)>,
    pub dsp: ScuDsp,
}

impl Default for Scu {
    fn default() -> Self {
        Self::new()
    }
}

impl Scu {
    pub fn new() -> Self {
        let mut s = Self {
            ch: [DmaChannel::default(); 3],
            ist: 0,
            ims: 0xFFFF_BFFF,
            regs: [0; 64],
            ready: Vec::new(),
            dma_log: Vec::new(),
            dsp_log: Vec::new(),
            dsp: ScuDsp::new(),
        };
        s.regs[0xC8 / 4] = 4; // VER
        s
    }

    pub fn raise(&mut self, bit: u32) {
        self.ist |= 1 << bit;
    }

    /// Interrupção pendente de maior prioridade: (nível, vetor, bit do IST).
    pub fn pending_irq(&self) -> Option<(u32, u32, u32)> {
        let active = self.ist & !self.ims;
        let mut best: Option<(u32, u32, u32)> = None;
        for bit in 0..32u32 {
            if active & (1 << bit) == 0 {
                continue;
            }
            let (level, vector) = if bit < 16 {
                (LEVELS[bit as usize], 0x40 + bit)
            } else {
                (7, 0x50 + (bit - 16))
            };
            if level == 0 {
                continue;
            }
            if best.map_or(true, |b| level > b.0) {
                best = Some((level, vector, bit));
            }
        }
        best
    }

    /// A CPU aceitou a interrupção: os fatores internos se limpam sozinhos.
    pub fn acknowledge(&mut self, bit: u32) {
        if bit < 16 {
            self.ist &= !(1 << bit);
        }
    }

    /// Um evento do sistema ocorreu: dispara os DMAs armados nesse fator.
    pub fn trigger_factor(&mut self, factor: u32) {
        for i in 0..3 {
            let c = self.ch[i];
            if c.armed && c.md & 7 == factor {
                self.ready.push(i);
            }
        }
    }

    pub fn take_ready_dma(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.ready)
    }

    fn write_reg(&mut self, off: u32, v: u32) {
        let off = off & 0xFF;
        match off {
            0x00..=0x5F => {
                let (i, r) = ((off / 0x20) as usize, off % 0x20);
                let c = &mut self.ch[i];
                match r {
                    0x00 => c.r = v,
                    0x04 => c.w = v,
                    0x08 => c.c = v,
                    0x0C => c.ad = v,
                    0x10 => {
                        c.en = v & 0x101;
                        if v & 0x100 != 0 {
                            c.armed = true;
                            if c.md & 7 == DMA_FACTOR_SOFTWARE && v & 1 != 0 {
                                self.ready.push(i);
                            }
                        } else {
                            c.armed = false;
                        }
                    }
                    0x14 => c.md = v,
                    _ => {}
                }
            }
            0xA0 => self.ims = v,
            0xA4 => self.ist &= v,
            0xC8 => {}
            0x80..=0x8C => {
                if self.dsp_log.len() < 4000 {
                    self.dsp_log.push((off, v));
                }
                match off {
                    0x80 => self.dsp.write_ppaf(v),
                    0x84 => self.dsp.write_ppd(v),
                    0x88 => self.dsp.write_pda(v),
                    _ => self.dsp.write_pdd(v),
                }
            }
            _ => self.regs[(off / 4) as usize % 64] = v,
        }
    }

    fn read_reg(&mut self, off: u32) -> u32 {
        let off = off & 0xFF;
        match off {
            0x00..=0x5F => {
                let (i, r) = ((off / 0x20) as usize, off % 0x20);
                let c = &self.ch[i];
                match r {
                    0x00 => c.r,
                    0x04 => c.w,
                    0x08 => c.c,
                    0x0C => c.ad,
                    0x10 => c.en,
                    0x14 => c.md,
                    _ => 0,
                }
            }
            0x7C => 0, // DSTA: DMA nunca fica "em andamento" (é instantâneo)
            0x80 => self.dsp.read_ppaf(),
            0x8C => self.dsp.read_pdd(),
            0xA0 => self.ims,
            0xA4 => self.ist,
            _ => self.regs[(off / 4) as usize % 64],
        }
    }

    /// Executa o DSP se a CPU pediu o início (chamado pela máquina fora do acesso ao barramento).
    pub fn run_dsp_if_requested(&mut self, bus: &mut SystemBus) {
        if self.dsp.start_requested && self.dsp.run(bus) {
            self.raise(5);
        }
    }

    /// Executa a transferência do canal `level` sobre o barramento do sistema.
    pub fn run_dma(&mut self, level: usize, bus: &mut SystemBus) {
        let c = self.ch[level];
        let indirect = c.md & (1 << 24) != 0;
        let read_add: u32 = if c.ad & 0x100 != 0 { 4 } else { 0 };
        let write_add: u32 = match c.ad & 7 {
            0 => 0,
            1 => 2,
            2 => 4,
            3 => 8,
            4 => 16,
            5 => 32,
            6 => 64,
            _ => 128,
        };
        let max = if level == 0 { 0x10_0000 } else { 0x1000 };
        let mut transfers: Vec<(u32, u32, u32)> = Vec::new(); // (src, dst, bytes)
        if indirect {
            let mut table = c.w;
            loop {
                let count = bus.read32(table);
                let dst = bus.read32(table.wrapping_add(4));
                let src = bus.read32(table.wrapping_add(8));
                transfers.push((src & 0x7FFF_FFFF, dst, if count == 0 { max } else { count }));
                table = table.wrapping_add(12);
                if src & 0x8000_0000 != 0 || transfers.len() > 4096 {
                    break;
                }
            }
        } else {
            transfers.push((c.r, c.w, if c.c == 0 { max } else { c.c }));
        }
        for (src0, dst0, bytes) in transfers {
            let (mut src, mut dst) = (src0, dst0);
            let b_bus_dst =
                (dst0 & 0x1FFF_FFFF) >= 0x0580_0000 && (dst0 & 0x1FFF_FFFF) < 0x0600_0000;
            self.dma_log.push(format!("DMA{level}: {src0:08X} -> {dst0:08X} {bytes:#X} bytes (rad={read_add} wad={write_add} md={:08X})", c.md));
            let mut done = 0;
            while done < bytes {
                let v = bus.read32(src);
                if b_bus_dst {
                    bus.write16(dst, (v >> 16) as u16);
                    dst = dst.wrapping_add(write_add);
                    bus.write16(dst, v as u16);
                    dst = dst.wrapping_add(write_add);
                } else {
                    bus.write32(dst, v);
                    dst = dst.wrapping_add(write_add);
                }
                src = src.wrapping_add(read_add);
                done += 4;
            }
            if !indirect {
                if c.md & (1 << 16) != 0 {
                    self.ch[level].r = src;
                }
                if c.md & (1 << 8) != 0 {
                    self.ch[level].w = dst;
                }
            }
        }
        self.ch[level].armed = false;
        self.ch[level].en &= !0x100;
        self.raise(match level {
            0 => IRQ_DMA0_END,
            1 => IRQ_DMA1_END,
            _ => IRQ_DMA2_END,
        });
    }

    fn write_sized(&mut self, off: u32, size: u32, v: u32) {
        let aligned = off & !3;
        if size == 4 {
            self.write_reg(aligned, v);
            return;
        }
        let shift = (4 - size - (off & 3)) * 8;
        let mask = (if size == 1 { 0xFF } else { 0xFFFF }) << shift;
        let old = self.read_reg(aligned);
        self.write_reg(aligned, (old & !mask) | ((v << shift) & mask));
    }
}

impl MemoryDevice for Scu {
    fn read_byte(&mut self, off: u32) -> u8 {
        (self.read_reg(off & !3) >> ((3 - (off & 3)) * 8)) as u8
    }
    fn write_byte(&mut self, off: u32, v: u8) {
        self.write_sized(off, 1, v as u32)
    }
    fn read_word(&mut self, off: u32) -> u16 {
        (self.read_reg(off & !3) >> ((2 - (off & 2)) * 8)) as u16
    }
    fn write_word(&mut self, off: u32, v: u16) {
        self.write_sized(off, 2, v as u32)
    }
    fn read_long(&mut self, off: u32) -> u32 {
        self.read_reg(off & !3)
    }
    fn write_long(&mut self, off: u32, v: u32) {
        self.write_sized(off, 4, v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ist_bits_clear_by_writing_zero_and_masking_blocks_delivery() {
        let mut s = Scu::new();
        s.write_long(0xA0, !1); // só VBlank-in desmascarado
        s.raise(IRQ_VBLANK_IN);
        s.raise(IRQ_VBLANK_OUT);
        assert_eq!(s.pending_irq().map(|p| p.1), Some(0x40));
        s.acknowledge(IRQ_VBLANK_IN);
        assert_eq!(s.pending_irq(), None, "VBlank-out continua mascarado");
        s.write_long(0xA0, 0);
        assert_eq!(s.pending_irq().map(|p| (p.0, p.1)), Some((0xE, 0x41)));
        s.write_long(0xA4, !(1 << IRQ_VBLANK_OUT));
        assert_eq!(s.ist, 0);
    }

    #[test]
    fn higher_level_wins() {
        let mut s = Scu::new();
        s.write_long(0xA0, 0);
        s.raise(IRQ_SPRITE_END);
        s.raise(IRQ_HBLANK_IN);
        assert_eq!(s.pending_irq().map(|p| p.2), Some(IRQ_HBLANK_IN));
    }
}
