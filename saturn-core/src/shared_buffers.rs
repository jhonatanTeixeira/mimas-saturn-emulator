use std::sync::RwLock;

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
///
/// Each field is its own `RwLock` (see `history.md` Chapter 8 /
/// `docs/final_architecture_draft.md`'s "Memory layout" -- this used to be
/// one `RwLock<WorkRam>` covering everything, which meant e.g. a VDP2 CRAM
/// write and an SH-2 Work RAM read contended on an identical lock despite
/// having nothing to do with each other). No call site needs more than one
/// of these locks at once today (verified against every access site before
/// this split landed) -- but if a future one ever does, acquire them in the
/// order the fields are declared below, to avoid a lock-ordering deadlock.
pub struct WorkRam {
    pub low_ram: RwLock<Box<[u8; 0x100000]>>, // 1MB Low Work RAM (0x00200000-0x002FFFFF)
    pub high_ram: [RwLock<Box<[u8; 0x8000]>>; 32], // 32 stripes of 32KB = 1MB High Work RAM (0x06000000-0x06FFFFFF mirrored)
    /// SCSP Sound RAM, real size 512KB (0x05A00000-0x05AFFFFF).
    pub sound_ram: RwLock<Box<[u8; 0x80000]>>,
    /// SCSP register block, separate from Sound RAM on real hardware
    /// (0x05B00000-0x05BFFFFF).
    pub scsp_regs: RwLock<Box<[u8; 0x1000]>>,
    /// VDP1 VRAM, real size 512KB (0x05C00000-0x05C7FFFF).
    pub vdp1_vram: RwLock<Box<[u8; 0x80000]>>,
    /// VDP1 framebuffer (double-buffered on real hardware; modeled here as
    /// one flat window), 512KB (0x05C80000-0x05CFFFFF).
    pub vdp1_framebuffer: RwLock<Box<[u8; 0x80000]>>,
    /// VDP1 registers (0x05D00000-0x05D7FFFF).
    pub vdp1_regs: RwLock<Box<[u8; 0x100]>>,
    /// VDP2 VRAM, real size 512KB (0x05E00000-0x05E7FFFF).
    pub vdp2_vram: RwLock<Box<[u8; 0x80000]>>,
    /// VDP2 color RAM / palette, real size 4KB (0x05F00000-0x05F00FFF).
    pub vdp2_cram: RwLock<Box<[u8; 0x1000]>>,
    /// VDP2 registers (0x05F80000-0x05FBFFFF).
    pub vdp2_regs: RwLock<Box<[u8; 0x200]>>,
    /// Internal backup RAM, real size 64KB (0x00180000-0x001FFFFF).
    pub backup_ram: RwLock<Box<[u8; 0x10000]>>,
    /// SMPC register file, real 0x80-byte window (0x00100000-0x0017FFFF,
    /// mirrored -- see `Sh2::translate`'s `& 0x7F`). Real registers live
    /// only at odd byte offsets (IREG0-6 at 0x01-0x0D, COMREG at 0x1F,
    /// OREG0-31 at 0x21-0x5F, SR at 0x61, SF at 0x63, PDR1/2 at 0x75/0x77,
    /// DDR1/2 at 0x79/0x7B, IOSEL at 0x7D, EXLE at 0x7F) -- cross-checked
    /// against Yabause's `SmpcReadByte`'s `SmpcRegsT[addr >> 1]` indexing
    /// and its register-offset `switch` in `smpc.c`.
    pub smpc_regs: RwLock<Box<[u8; 0x80]>>,
    pub mem4b: std::sync::atomic::AtomicBool,
    /// Real hardware: TVSTAT's VBLANK status bit, currently true from
    /// VBLANK-IN until VBLANK-OUT. Core 3 (`vdp2-composite`) sets/clears
    /// this on its own frame-render clock (`docs/implementation-plans/scu.md`
    /// Phase 3), the same clock that now also raises the VBLANK-IN/OUT
    /// interrupts via `Scu` -- both SH-2 cores read it (via `Sh2::tvstat_word`)
    /// with `Acquire`, paired with Core 3's `Release` store, mirroring
    /// `mem4b`'s existing cross-thread-flag discipline just above.
    pub vblank_active: std::sync::atomic::AtomicBool,
    /// System Manager IRQ (SMPC level 8 interrupt). Set by Core 7 when an
    /// SMPC command finishes, polled by Master SH-2 in `service_pending_interrupt`.
    pub smpc_irq_pending: std::sync::atomic::AtomicBool,
    pub smpc_nmi_pending: std::sync::atomic::AtomicBool,
    pub smpc_sysres_pending: std::sync::atomic::AtomicBool,
    pub smpc_clock_change: std::sync::atomic::AtomicU8, // 0 = None, 1 = 320, 2 = 352
}

impl WorkRam {
    pub fn new() -> Self {
        let high_ram: [RwLock<Box<[u8; 0x8000]>>; 32] = std::array::from_fn(|_| {
            RwLock::new(vec![0; 0x8000].into_boxed_slice().try_into().unwrap())
        });
        Self {
            low_ram: RwLock::new(vec![0; 0x100000].into_boxed_slice().try_into().unwrap()),
            high_ram,
            sound_ram: RwLock::new(vec![0; 0x80000].into_boxed_slice().try_into().unwrap()),
            scsp_regs: RwLock::new(vec![0; 0x1000].into_boxed_slice().try_into().unwrap()),
            vdp1_vram: RwLock::new(vec![0; 0x80000].into_boxed_slice().try_into().unwrap()),
            vdp1_framebuffer: RwLock::new(vec![0; 0x80000].into_boxed_slice().try_into().unwrap()),
            vdp1_regs: RwLock::new(vec![0; 0x100].into_boxed_slice().try_into().unwrap()),
            vdp2_vram: RwLock::new(vec![0; 0x80000].into_boxed_slice().try_into().unwrap()),
            vdp2_cram: RwLock::new(vec![0; 0x1000].into_boxed_slice().try_into().unwrap()),
            vdp2_regs: RwLock::new(vec![0; 0x200].into_boxed_slice().try_into().unwrap()),
            backup_ram: RwLock::new(vec![0; 0x10000].into_boxed_slice().try_into().unwrap()),
            smpc_regs: RwLock::new(vec![0; 0x80].into_boxed_slice().try_into().unwrap()),
            mem4b: std::sync::atomic::AtomicBool::new(false),
            vblank_active: std::sync::atomic::AtomicBool::new(false),
            smpc_irq_pending: std::sync::atomic::AtomicBool::new(false),
            smpc_nmi_pending: std::sync::atomic::AtomicBool::new(false),
            smpc_sysres_pending: std::sync::atomic::AtomicBool::new(false),
            smpc_clock_change: std::sync::atomic::AtomicU8::new(0),
        }
    }

    /// Explicit clear of low work RAM (must be called via register write commands, never automatically on Drop)
    pub fn clear_low_ram(&mut self) {
        self.low_ram.get_mut().unwrap().fill(0);
    }

    /// Explicit clear of high work RAM
    pub fn clear_high_ram(&mut self) {
        for stripe in self.high_ram.iter_mut() {
            stripe.get_mut().unwrap().fill(0);
        }
    }

    pub fn read_high_ram_byte(&self, off: usize) -> u8 {
        crate::telemetry::record_wram_read();
        let off = off & 0xFFFFF;
        let stripe = (off >> 15) & 31;
        let index = off & 0x7FFF;
        self.high_ram[stripe].read().unwrap()[index]
    }

    pub fn write_high_ram_byte(&self, off: usize, val: u8) {
        crate::telemetry::record_wram_write();
        let off = off & 0xFFFFF;
        let stripe = (off >> 15) & 31;
        let index = off & 0x7FFF;
        self.high_ram[stripe].write().unwrap()[index] = val;
    }

    pub fn read_high_ram_long(&self, off: usize) -> u32 {
        crate::telemetry::record_wram_read();
        let off = off & 0xFFFFF;
        let stripe = (off >> 15) & 31;
        let index = off & 0x7FFF;
        if index + 3 < 0x8000 {
            let ram = self.high_ram[stripe].read().unwrap();
            let b0 = ram[index] as u32;
            let b1 = ram[index + 1] as u32;
            let b2 = ram[index + 2] as u32;
            let b3 = ram[index + 3] as u32;
            (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
        } else {
            // Straddling path: acquire lower-index stripe lock first
            let stripe1 = stripe;
            let stripe2 = (stripe1 + 1) & 31;
            let (first, second) = if stripe1 < stripe2 {
                (stripe1, stripe2)
            } else {
                (stripe2, stripe1)
            };
            let lock_first = self.high_ram[first].read().unwrap();
            let lock_second = self.high_ram[second].read().unwrap();
            let mut val = 0u32;
            for i in 0..4 {
                let curr_off = off + i;
                let curr_stripe = (curr_off >> 15) & 31;
                let curr_index = curr_off & 0x7FFF;
                let byte = if curr_stripe == first {
                    lock_first[curr_index]
                } else {
                    lock_second[curr_index]
                };
                val = (val << 8) | (byte as u32);
            }
            val
        }
    }

    pub fn write_high_ram_long(&self, off: usize, val: u32) {
        crate::telemetry::record_wram_write();
        let off = off & 0xFFFFF;
        let stripe = (off >> 15) & 31;
        let index = off & 0x7FFF;
        if index + 3 < 0x8000 {
            let mut ram = self.high_ram[stripe].write().unwrap();
            ram[index] = (val >> 24) as u8;
            ram[index + 1] = (val >> 16) as u8;
            ram[index + 2] = (val >> 8) as u8;
            ram[index + 3] = val as u8;
        } else {
            let stripe1 = stripe;
            let stripe2 = (stripe1 + 1) & 31;
            let (first, second) = if stripe1 < stripe2 {
                (stripe1, stripe2)
            } else {
                (stripe2, stripe1)
            };
            let mut lock_first = self.high_ram[first].write().unwrap();
            let mut lock_second = self.high_ram[second].write().unwrap();
            for i in 0..4 {
                let curr_off = off + i;
                let curr_stripe = (curr_off >> 15) & 31;
                let curr_index = curr_off & 0x7FFF;
                let byte = (val >> (8 * (3 - i))) as u8;
                if curr_stripe == first {
                    lock_first[curr_index] = byte;
                } else {
                    lock_second[curr_index] = byte;
                }
            }
        }
    }

    pub fn write_high_ram_word(&self, off: usize, val: u16) {
        crate::telemetry::record_wram_write();
        let off = off & 0xFFFFF;
        let stripe = (off >> 15) & 31;
        let index = off & 0x7FFF;
        if index + 1 < 0x8000 {
            let mut ram = self.high_ram[stripe].write().unwrap();
            ram[index] = (val >> 8) as u8;
            ram[index + 1] = val as u8;
        } else {
            let stripe1 = stripe;
            let stripe2 = (stripe1 + 1) & 31;
            let (first, second) = if stripe1 < stripe2 {
                (stripe1, stripe2)
            } else {
                (stripe2, stripe1)
            };
            let mut lock_first = self.high_ram[first].write().unwrap();
            let mut lock_second = self.high_ram[second].write().unwrap();
            if stripe1 == first {
                lock_first[index] = (val >> 8) as u8;
                lock_second[0] = val as u8;
            } else {
                lock_second[index] = (val >> 8) as u8;
                lock_first[0] = val as u8;
            }
        }
    }

    pub fn read_high_ram_word(&self, off: usize) -> u16 {
        crate::telemetry::record_wram_read();
        let off = off & 0xFFFFF;
        let stripe = (off >> 15) & 31;
        let index = off & 0x7FFF;
        if index + 1 < 0x8000 {
            let ram = self.high_ram[stripe].read().unwrap();
            let b0 = ram[index] as u16;
            let b1 = ram[index + 1] as u16;
            (b0 << 8) | b1
        } else {
            let stripe1 = stripe;
            let stripe2 = (stripe1 + 1) & 31;
            let (first, second) = if stripe1 < stripe2 {
                (stripe1, stripe2)
            } else {
                (stripe2, stripe1)
            };
            let lock_first = self.high_ram[first].read().unwrap();
            let lock_second = self.high_ram[second].read().unwrap();
            let val1 = if first == stripe1 {
                lock_first[index]
            } else {
                lock_second[index]
            };
            let val2 = if second == stripe2 {
                lock_second[0]
            } else {
                lock_first[0]
            };
            (val1 as u16) << 8 | (val2 as u16)
        }
    }

    /// Atomic Test-And-Set byte transaction for TAS.B.
    /// Note that real SH-2 locks the bus, but in Mimas we achieve atomicity
    /// by holding the stripe/region lock for the whole read-modify-write.
    pub fn tas_byte(&self, address: u32) -> Option<u8> {
        let a = address & 0x0FFF_FFFF;
        if (0x0020_0000..0x0030_0000).contains(&a) {
            let off = (a - 0x0020_0000) as usize;
            let mut ram = self.low_ram.write().unwrap();
            let index = off & (ram.len() - 1);
            let val = ram[index];
            ram[index] = val | 0x80;
            Some(val)
        } else if (0x0600_0000..0x0800_0000).contains(&a) {
            let off = ((a - 0x0600_0000) & 0xFFFFF) as usize;
            let stripe = (off >> 15) & 31;
            let index = off & 0x7FFF;
            let mut ram = self.high_ram[stripe].write().unwrap();
            let val = ram[index];
            ram[index] = val | 0x80;
            Some(val)
        } else {
            None
        }
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
