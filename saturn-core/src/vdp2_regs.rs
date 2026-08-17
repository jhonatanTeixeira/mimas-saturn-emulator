use crate::shared_buffers::WorkRam;

pub struct Vdp2Registers {
    pub regs: [u16; 256], // 0x200 bytes = 256 words
}

impl Vdp2Registers {
    pub fn new() -> Self {
        Self { regs: [0; 256] }
    }

    pub fn snapshot(ram: &WorkRam) -> Self {
        let mut snapshot = Self::new();
        let regs_bytes = ram.vdp2_regs.read().unwrap();
        for i in 0..0x100 {
            // Copy all up to 0x200 bytes
            let offset = i * 2;
            snapshot.regs[i] = u16::from_be_bytes([regs_bytes[offset], regs_bytes[offset + 1]]);
        }
        snapshot
    }

    // A.0 Complete register index (selected registers for Phase 1)
    pub fn tvmd(&self) -> u16 {
        self.regs[0x000 / 2]
    }
    pub fn exten(&self) -> u16 {
        self.regs[0x002 / 2]
    }
    pub fn tvstat(&self) -> u16 {
        self.regs[0x004 / 2]
    }
    pub fn vrsize(&self) -> u16 {
        self.regs[0x006 / 2]
    }
    pub fn hcnt(&self) -> u16 {
        self.regs[0x008 / 2]
    }
    pub fn vcnt(&self) -> u16 {
        self.regs[0x00A / 2]
    }
    pub fn ramctl(&self) -> u16 {
        self.regs[0x00E / 2]
    }
    pub fn cyca0l(&self) -> u16 {
        self.regs[0x010 / 2]
    }
    pub fn cyca0u(&self) -> u16 {
        self.regs[0x012 / 2]
    }
    pub fn cyca1l(&self) -> u16 {
        self.regs[0x014 / 2]
    }
    pub fn cyca1u(&self) -> u16 {
        self.regs[0x016 / 2]
    }
    pub fn cycb0l(&self) -> u16 {
        self.regs[0x018 / 2]
    }
    pub fn cycb0u(&self) -> u16 {
        self.regs[0x01A / 2]
    }
    pub fn cycb1l(&self) -> u16 {
        self.regs[0x01C / 2]
    }
    pub fn cycb1u(&self) -> u16 {
        self.regs[0x01E / 2]
    }
    pub fn bgon(&self) -> u16 {
        self.regs[0x020 / 2]
    }
    pub fn mzctl(&self) -> u16 {
        self.regs[0x022 / 2]
    }
    pub fn sfsel(&self) -> u16 {
        self.regs[0x024 / 2]
    }

    // Decoded fields
    pub fn hreso(&self) -> u16 {
        self.tvmd() & 0x7
    }

    pub fn vreso(&self) -> u16 {
        (self.tvmd() >> 4) & 0x3
    }

    pub fn lsmd(&self) -> u16 {
        (self.tvmd() >> 6) & 0x3
    }

    pub fn bdclmd(&self) -> u16 {
        (self.tvmd() >> 8) & 0x1
    }

    pub fn disp(&self) -> u16 {
        (self.tvmd() >> 15) & 0x1
    }

    pub fn color_mode(&self) -> u16 {
        (self.ramctl() >> 12) & 0x3
    }

    // Hardware reference contradiction (§A.2): `Vdp2GetBank` reads bits 4-5 for partition,
    // while `VDP2genVRamCyclePattern` and `Rbg0CheckRam` read bits 8-9. We resolve this as
    // bits 8-9 because three call sites agree on 8-9, and bits 0-7 are four 2-bit per-bank
    // usage fields.
    pub fn vram_a_partitioned(&self) -> bool {
        (self.ramctl() & 0x0100) != 0
    }

    pub fn vram_b_partitioned(&self) -> bool {
        (self.ramctl() & 0x0200) != 0
    }

    pub fn vram_8mbit(&self) -> bool {
        (self.vrsize() & 0x8000) != 0
    }
}

impl Default for Vdp2Registers {
    fn default() -> Self {
        Self::new()
    }
}

/// §0.1/§1.2: `Vdp2ColorRamGetColorSoft` (`vidsoft.c:206-235`) expands each
/// 5-bit channel to 8 bits by a plain left-shift (`(t&0x1F)<<3 | (t&0x3E0)<<6
/// | (t&0x7C00)<<9 | (t&0x8000)<<16`). `docs/implementation-plans/vdp2.md`
/// §0.4/§1.2 **deliberately** keeps `rgb555_to_xrgb8888`'s bit-replication
/// instead ("the better analogue model... record it, don't 'fix' it") and
/// this reuses that same function for consistency -- see its doc comment.
pub fn cram_lookup(index: u16, mode: u16, cram: &[u8]) -> u32 {
    match mode {
        0 | 1 => {
            let addr = ((index as usize) << 1) & 0xFFF;
            let val = u16::from_be_bytes([cram[addr], cram[addr + 1]]);
            let msb = (val >> 15) as u32;
            let r5 = (val & 0x1F) as u32;
            let g5 = ((val >> 5) & 0x1F) as u32;
            let b5 = ((val >> 10) & 0x1F) as u32;
            let r8 = (r5 << 3) | (r5 >> 2);
            let g8 = (g5 << 3) | (g5 >> 2);
            let b8 = (b5 << 3) | (b5 >> 2);
            (msb << 31) | (r8 << 16) | (g8 << 8) | b8
        }
        2 => {
            let addr = ((index as usize) << 2) & 0xFFF;
            u32::from_be_bytes([cram[addr], cram[addr + 1], cram[addr + 2], cram[addr + 3]])
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sh2::Sh2;
    use crate::shared_buffers::WorkRam;
    use crate::BusArbiter;
    use std::sync::Arc;

    #[test]
    fn vdp2_regs_mirror_write() {
        let work_ram = Arc::new(WorkRam::new());
        let arbiter = Arc::new(BusArbiter::new());
        let mut sh2 = Sh2::new(false, arbiter, work_ram.clone());

        // Write to physical mirror at 0x05F80200
        sh2.write_word(0x05F80200, 0x1214);

        // Visible at 0x05F80000
        let val = sh2.read_word(0x05F80000);
        assert_eq!(val, 0x1214);
    }

    #[test]
    fn vdp2_cram_mirroring() {
        let work_ram = Arc::new(WorkRam::new());
        let arbiter = Arc::new(BusArbiter::new());
        let mut sh2 = Sh2::new(false, arbiter, work_ram.clone());

        // By default, RAMCTL is 0, so ColorMode is 0.
        // Write to CRAM at 0x05F00000
        sh2.write_word(0x05F00000, 0xABCD);

        // Verify it mirrors to 0x05F00800
        assert_eq!(sh2.read_word(0x05F00800), 0xABCD);

        // Change ColorMode to 1 by writing to RAMCTL
        sh2.write_word(0x05F8000E, 0x1000);

        // Write a new value
        sh2.write_word(0x05F00002, 0x9876);

        // Verify it does NOT mirror to 0x05F00802
        assert_ne!(sh2.read_word(0x05F00802), 0x9876);
        assert_eq!(sh2.read_word(0x05F00802), 0x0000); // Because it was originally 0
    }

    #[test]
    fn cram_mode0_and_mode1_decode_identically() {
        let mut cram = vec![0u8; 0x1000];
        // Blue RGB555: 0x7C00 -> b5 = (0x7C00 >> 10) & 0x1F = 0x1F, r5 = g5 = 0.
        // Mimas deliberately keeps bit replication (`rgb555_to_xrgb8888`'s
        // own doc comment): b8 = (0x1F << 3) | (0x1F >> 2) = 0xFF.
        cram[0] = 0x7C;
        cram[1] = 0x00;

        let color0 = cram_lookup(0, 0, &cram);
        let color1 = cram_lookup(0, 1, &cram);

        assert_eq!(color0, color1);
        assert_eq!(color0, 0x000000FF);
    }

    #[test]
    fn cram_mode2_returns_the_long_verbatim() {
        let mut cram = vec![0u8; 0x1000];
        // Long: 0x12345678
        cram[0] = 0x12;
        cram[1] = 0x34;
        cram[2] = 0x56;
        cram[3] = 0x78;

        let color = cram_lookup(0, 2, &cram);
        assert_eq!(color, 0x12345678);
    }

    #[test]
    fn cram_bit15_lands_at_bit31() {
        let mut cram = vec![0u8; 0x1000];
        // Bit 15 set: 0x8000
        cram[0] = 0x80;
        cram[1] = 0x00;

        let color = cram_lookup(0, 0, &cram);
        assert_eq!(color & 0x80000000, 0x80000000);
    }
}
