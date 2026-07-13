/// Backing storage for every Saturn physical memory region the SH-2 memory
/// map (`sh2::translate`) understands, beyond BIOS ROM and the SMPC special
/// case. Region boundaries and sizes are cross-checked against Yabause's
/// real, working implementation (`memory.c`'s `MappedMemoryInit` fill
/// table) rather than guessed -- see the comment on each field for the
/// real physical address range it backs.
///
/// Everything here except `low_ram`/`high_ram` is modeled as plain
/// read/write memory with no real register semantics yet (no actual video
/// scanout, no SCSP synthesis, no SCU DMA/DSP execution). That's enough to
/// satisfy the extremely common "write a value, read it back to verify"
/// pattern real hardware-bringup code uses everywhere, without pretending
/// to emulate hardware behavior this project hasn't implemented yet.
pub struct WorkRam {
    pub low_ram: Box<[u8; 0x100000]>,  // 1MB Low Work RAM (0x00200000-0x002FFFFF)
    pub high_ram: Box<[u8; 0x100000]>, // 1MB High Work RAM (0x06000000-0x060FFFFF)
    /// SCSP Sound RAM, real size 512KB (0x05A00000-0x05AFFFFF).
    pub sound_ram: Box<[u8; 0x80000]>,
    /// SCSP register block, separate from Sound RAM on real hardware
    /// (0x05B00000-0x05BFFFFF).
    pub scsp_regs: Box<[u8; 0x1000]>,
    /// VDP1 VRAM, real size 512KB (0x05C00000-0x05C7FFFF).
    pub vdp1_vram: Box<[u8; 0x80000]>,
    /// VDP1 framebuffer (double-buffered on real hardware; modeled here as
    /// one flat window), 512KB (0x05C80000-0x05CFFFFF).
    pub vdp1_framebuffer: Box<[u8; 0x80000]>,
    /// VDP1 registers (0x05D00000-0x05D7FFFF).
    pub vdp1_regs: Box<[u8; 0x1000]>,
    /// VDP2 VRAM, real size 512KB (0x05E00000-0x05E7FFFF).
    pub vdp2_vram: Box<[u8; 0x80000]>,
    /// VDP2 color RAM / palette, real size 4KB (0x05F00000-0x05F00FFF).
    pub vdp2_cram: Box<[u8; 0x1000]>,
    /// VDP2 registers (0x05F80000-0x05FBFFFF).
    pub vdp2_regs: Box<[u8; 0x1000]>,
    /// SCU registers (0x05FE0000-0x05FEFFFF).
    pub scu_regs: Box<[u8; 0x1000]>,
    /// CS2 / CD-ROM block registers (0x05800000-0x058FFFFF). The real CD
    /// command protocol lives in `Cdrom` (CR1-4/HIRQ/DTR); this is a plain
    /// memory stub until that's wired into the CPU's address space.
    pub cs2_regs: Box<[u8; 0x1000]>,
    /// Internal backup RAM, real size 32KB (0x00180000-0x001FFFFF).
    pub backup_ram: Box<[u8; 0x8000]>,
    /// SMPC register file, real 0x80-byte window (0x00100000-0x0017FFFF,
    /// mirrored -- see `Sh2::translate`'s `& 0x7F`). Real registers live
    /// only at odd byte offsets (IREG0-6 at 0x01-0x0D, COMREG at 0x1F,
    /// OREG0-31 at 0x21-0x5F, SR at 0x61, SF at 0x63, PDR1/2 at 0x75/0x77,
    /// DDR1/2 at 0x79/0x7B, IOSEL at 0x7D, EXLE at 0x7F) -- cross-checked
    /// against Yabause's `SmpcReadByte`'s `SmpcRegsT[addr >> 1]` indexing
    /// and its register-offset `switch` in `smpc.c`.
    pub smpc_regs: Box<[u8; 0x80]>,
}

impl WorkRam {
    pub fn new() -> Self {
        Self {
            low_ram: vec![0; 0x100000].into_boxed_slice().try_into().unwrap(),
            high_ram: vec![0; 0x100000].into_boxed_slice().try_into().unwrap(),
            sound_ram: vec![0; 0x80000].into_boxed_slice().try_into().unwrap(),
            scsp_regs: vec![0; 0x1000].into_boxed_slice().try_into().unwrap(),
            vdp1_vram: vec![0; 0x80000].into_boxed_slice().try_into().unwrap(),
            vdp1_framebuffer: vec![0; 0x80000].into_boxed_slice().try_into().unwrap(),
            vdp1_regs: vec![0; 0x1000].into_boxed_slice().try_into().unwrap(),
            vdp2_vram: vec![0; 0x80000].into_boxed_slice().try_into().unwrap(),
            vdp2_cram: vec![0; 0x1000].into_boxed_slice().try_into().unwrap(),
            vdp2_regs: vec![0; 0x1000].into_boxed_slice().try_into().unwrap(),
            scu_regs: vec![0; 0x1000].into_boxed_slice().try_into().unwrap(),
            cs2_regs: vec![0; 0x1000].into_boxed_slice().try_into().unwrap(),
            backup_ram: vec![0; 0x8000].into_boxed_slice().try_into().unwrap(),
            smpc_regs: vec![0; 0x80].into_boxed_slice().try_into().unwrap(),
        }
    }

    /// Explicit clear of low work RAM (must be called via register write commands, never automatically on Drop)
    pub fn clear_low_ram(&mut self) {
        self.low_ram.fill(0);
    }

    /// Explicit clear of high work RAM
    pub fn clear_high_ram(&mut self) {
        self.high_ram.fill(0);
    }
}

impl Default for WorkRam {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Vram {
    pub vram_a: Box<[u8; 0x40000]>, // 256KB VRAM bank A
    pub vram_b: Box<[u8; 0x40000]>, // 256KB VRAM bank B
}

impl Vram {
    pub fn new() -> Self {
        Self {
            vram_a: vec![0; 0x40000].into_boxed_slice().try_into().unwrap(),
            vram_b: vec![0; 0x40000].into_boxed_slice().try_into().unwrap(),
        }
    }

    /// Clear VRAM triggered by VDP1/VDP2 command
    pub fn clear_on_command(&mut self) {
        self.vram_a.fill(0);
        self.vram_b.fill(0);
    }

    /// Write byte to VRAM with bounds validation
    pub fn write_byte(&mut self, addr: usize, val: u8) -> Result<(), String> {
        if addr >= 0x80000 {
            return Err("VRAM out of bounds".to_string());
        }
        if addr < 0x40000 {
            self.vram_a[addr] = val;
        } else {
            self.vram_b[addr - 0x40000] = val;
        }
        Ok(())
    }

}

impl Default for Vram {
    fn default() -> Self {
        Self::new()
    }
}

use arc_swap::ArcSwap;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub struct Framebuffer {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

pub struct DoubleBufferedFramebuffer {
    pub front: ArcSwap<Framebuffer>,
    pub back: ArcSwap<Framebuffer>,
}

impl DoubleBufferedFramebuffer {
    pub fn new(width: u32, height: u32) -> Self {
        let size = (width * height * 4) as usize;
        Self {
            front: ArcSwap::new(Arc::new(Framebuffer {
                width,
                height,
                pixels: vec![0; size],
            })),
            back: ArcSwap::new(Arc::new(Framebuffer {
                width,
                height,
                pixels: vec![0; size],
            })),
        }
    }

    pub fn swap(&self) {
        let front_arc = Arc::clone(&*self.front.load());
        let back_arc = Arc::clone(&*self.back.load());
        self.front.store(back_arc);
        self.back.store(front_arc);
    }
}
