//! VDP2: VRAM de fundos, CRAM de paletas e registradores. A composição fica em `video`.

use std::cell::RefCell;
use std::rc::Rc;

use crate::bus::MemoryDevice;

pub const VRAM_SIZE: usize = 0x8_0000;
pub const CRAM_SIZE: usize = 0x1000;
pub const REG_SIZE: usize = 0x120;

pub struct Vdp2 {
    pub vram: Vec<u8>,
    pub cram: Vec<u8>,
    pub regs: [u8; REG_SIZE],
    pub vblank: bool,
    pub hblank: bool,
    pub odd: bool,
    pub line: u32,
    /// (offset, valor, quadro) das escritas em registradores, para diagnóstico.
    pub reg_writes: Vec<(u32, u16, u32)>,
    pub frame: u32,
}

impl Default for Vdp2 {
    fn default() -> Self {
        Self::new()
    }
}

impl Vdp2 {
    pub fn new() -> Self {
        Self {
            vram: vec![0; VRAM_SIZE],
            cram: vec![0; CRAM_SIZE],
            regs: [0; REG_SIZE],
            vblank: false,
            hblank: false,
            odd: false,
            line: 0,
            reg_writes: Vec::new(),
            frame: 0,
        }
    }

    pub fn reg16(&self, off: usize) -> u16 {
        u16::from_be_bytes([self.regs[off], self.regs[off + 1]])
    }

    fn reg_read(&self, off: u32) -> u16 {
        match off & 0x1FE {
            0x04 => {
                ((self.vblank as u16) << 3) | ((self.hblank as u16) << 2) | ((self.odd as u16) << 1)
            }
            0x08 => 0,
            0x0A => self.line as u16,
            o if (o as usize) < REG_SIZE => self.reg16(o as usize),
            _ => 0,
        }
    }

    fn reg_write(&mut self, off: u32, v: u16) {
        let o = (off & 0x1FE) as usize;
        if o >= REG_SIZE {
            return;
        }
        if self.reg_writes.len() < 8192 {
            self.reg_writes.push((o as u32, v, self.frame));
        }
        if matches!(o, 0x04 | 0x08 | 0x0A) {
            return; // somente leitura
        }
        self.regs[o..o + 2].copy_from_slice(&v.to_be_bytes());
    }
}

#[derive(Clone, Copy)]
pub enum Vdp2Area {
    Vram,
    Cram,
    Regs,
}

pub struct Vdp2Port {
    pub vdp: Rc<RefCell<Vdp2>>,
    pub area: Vdp2Area,
}

impl MemoryDevice for Vdp2Port {
    fn read_byte(&mut self, off: u32) -> u8 {
        let v = self.vdp.borrow();
        match self.area {
            Vdp2Area::Vram => v.vram[off as usize % VRAM_SIZE],
            Vdp2Area::Cram => v.cram[off as usize % CRAM_SIZE],
            Vdp2Area::Regs => (v.reg_read(off & !1) >> (8 * (1 - (off & 1)))) as u8,
        }
    }
    fn write_byte(&mut self, off: u32, b: u8) {
        let mut v = self.vdp.borrow_mut();
        match self.area {
            Vdp2Area::Vram => v.vram[off as usize % VRAM_SIZE] = b,
            Vdp2Area::Cram => v.cram[off as usize % CRAM_SIZE] = b,
            Vdp2Area::Regs => {
                // Escrita de byte em registrador: mescla com o valor atual.
                let cur = v.reg_read(off & !1);
                let w = if off & 1 == 0 {
                    (cur & 0x00FF) | ((b as u16) << 8)
                } else {
                    (cur & 0xFF00) | b as u16
                };
                v.reg_write(off & !1, w);
            }
        }
    }
    fn read_word(&mut self, off: u32) -> u16 {
        let v = self.vdp.borrow();
        match self.area {
            Vdp2Area::Vram => u16::from_be_bytes([
                v.vram[off as usize % VRAM_SIZE],
                v.vram[(off as usize + 1) % VRAM_SIZE],
            ]),
            Vdp2Area::Cram => u16::from_be_bytes([
                v.cram[off as usize % CRAM_SIZE],
                v.cram[(off as usize + 1) % CRAM_SIZE],
            ]),
            Vdp2Area::Regs => v.reg_read(off),
        }
    }
    fn write_word(&mut self, off: u32, w: u16) {
        let mut v = self.vdp.borrow_mut();
        match self.area {
            Vdp2Area::Vram => {
                let o = off as usize % VRAM_SIZE;
                v.vram[o..o + 2].copy_from_slice(&w.to_be_bytes());
            }
            Vdp2Area::Cram => {
                let o = off as usize % CRAM_SIZE;
                v.cram[o..o + 2].copy_from_slice(&w.to_be_bytes());
            }
            Vdp2Area::Regs => v.reg_write(off, w),
        }
    }
    fn read_long(&mut self, off: u32) -> u32 {
        ((self.read_word(off) as u32) << 16) | self.read_word(off + 2) as u32
    }
    fn write_long(&mut self, off: u32, l: u32) {
        self.write_word(off, (l >> 16) as u16);
        self.write_word(off + 2, l as u16);
    }
}
