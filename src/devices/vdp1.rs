//! VDP1: VRAM de comandos/texturas, dois framebuffers (a CPU enxerga o de desenho) e
//! registradores. O desenho em si fica em `video::vdp1_draw`.

use std::cell::RefCell;
use std::rc::Rc;

use crate::bus::MemoryDevice;

pub const VRAM_SIZE: usize = 0x8_0000;
pub const FB_SIZE: usize = 0x4_0000;

pub struct Vdp1 {
    pub vram: Vec<u8>,
    /// Dois framebuffers de 256 KiB: `draw` é o que o VDP1 desenha (e a CPU enxerga).
    pub fb: [Vec<u8>; 2],
    pub draw: usize,
    pub tvmr: u16,
    pub fbcr: u16,
    pub ptmr: u16,
    pub ewdr: u16,
    pub ewlr: u16,
    pub ewrr: u16,
    /// Bit 1 = CEF (fim de desenho), bit 0 = BEF.
    pub edsr: u16,
    pub draw_requested: bool,
    pub reg_writes: Vec<(u32, u16)>,
}

impl Default for Vdp1 {
    fn default() -> Self {
        Self::new()
    }
}

impl Vdp1 {
    pub fn new() -> Self {
        Self {
            vram: vec![0; VRAM_SIZE],
            fb: [vec![0; FB_SIZE], vec![0; FB_SIZE]],
            draw: 0,
            tvmr: 0,
            fbcr: 0,
            ptmr: 0,
            ewdr: 0,
            ewlr: 0,
            ewrr: 0,
            edsr: 0,
            draw_requested: false,
            reg_writes: Vec::new(),
        }
    }

    pub fn display_fb(&self) -> &[u8] {
        &self.fb[1 - self.draw]
    }

    pub fn swap_buffers(&mut self) {
        self.draw = 1 - self.draw;
    }

    fn reg_read(&self, off: u32) -> u16 {
        match off & 0x1E {
            0x10 => self.edsr,
            0x12 => 0,      // LOPR
            0x14 => 0,      // COPR
            0x16 => 0x0001, // MODR: versão
            _ => 0,
        }
    }

    fn reg_write(&mut self, off: u32, v: u16) {
        let off = off & 0x1E;
        if self.reg_writes.len() < 4096 {
            self.reg_writes.push((off, v));
        }
        match off {
            0x00 => self.tvmr = v,
            0x02 => self.fbcr = v,
            0x04 => {
                self.ptmr = v;
                if v & 3 == 1 {
                    self.draw_requested = true;
                    self.edsr &= !2;
                }
            }
            0x06 => self.ewdr = v,
            0x08 => self.ewlr = v,
            0x0A => self.ewrr = v,
            _ => {}
        }
    }
}

#[derive(Clone, Copy)]
pub enum Vdp1Area {
    Vram,
    Framebuffer,
    Regs,
}

/// Porta de barramento para uma das áreas do VDP1.
pub struct Vdp1Port {
    pub vdp: Rc<RefCell<Vdp1>>,
    pub area: Vdp1Area,
}

fn be16(m: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([m[o % m.len()], m[(o + 1) % m.len()]])
}

impl MemoryDevice for Vdp1Port {
    fn read_byte(&mut self, off: u32) -> u8 {
        let v = self.vdp.borrow();
        match self.area {
            Vdp1Area::Vram => v.vram[off as usize % VRAM_SIZE],
            Vdp1Area::Framebuffer => v.fb[v.draw][off as usize % FB_SIZE],
            Vdp1Area::Regs => (v.reg_read(off & !1) >> (8 * (1 - (off & 1)))) as u8,
        }
    }
    fn write_byte(&mut self, off: u32, b: u8) {
        let mut v = self.vdp.borrow_mut();
        match self.area {
            Vdp1Area::Vram => v.vram[off as usize % VRAM_SIZE] = b,
            Vdp1Area::Framebuffer => {
                let d = v.draw;
                v.fb[d][off as usize % FB_SIZE] = b
            }
            Vdp1Area::Regs => {}
        }
    }
    fn read_word(&mut self, off: u32) -> u16 {
        let v = self.vdp.borrow();
        match self.area {
            Vdp1Area::Vram => be16(&v.vram, off as usize),
            Vdp1Area::Framebuffer => be16(&v.fb[v.draw], off as usize),
            Vdp1Area::Regs => v.reg_read(off),
        }
    }
    fn write_word(&mut self, off: u32, w: u16) {
        let mut v = self.vdp.borrow_mut();
        match self.area {
            Vdp1Area::Vram => {
                let o = off as usize % VRAM_SIZE;
                v.vram[o..o + 2].copy_from_slice(&w.to_be_bytes());
            }
            Vdp1Area::Framebuffer => {
                let d = v.draw;
                let o = off as usize % FB_SIZE;
                v.fb[d][o..o + 2].copy_from_slice(&w.to_be_bytes());
            }
            Vdp1Area::Regs => v.reg_write(off, w),
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
