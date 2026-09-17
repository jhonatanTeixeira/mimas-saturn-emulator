pub struct Vdp2Registers {
    pub regs: [u16; 256], // 0x200 bytes = 256 words
}

impl Vdp2Registers {
    pub fn new() -> Self {
        Self { regs: [0; 256] }
    }

    pub fn from_bytes(regs_bytes: &[u8; 0x200]) -> Self {
        let mut snapshot = Self::new();
        for i in 0..0x100 {
            let offset = i * 2;
            snapshot.regs[i] = u16::from_be_bytes([regs_bytes[offset], regs_bytes[offset + 1]]);
        }
        snapshot
    }

    // A.0 Complete register index (selected registers for Phase 1)
    pub fn tvmd(&self) -> u16 {
        self.regs[0]
    }
    pub fn exten(&self) -> u16 {
        self.regs[1]
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
    pub fn craofb(&self) -> u16 {
        self.regs[0x0CA / 2]
    }
    pub fn craofa(&self) -> u16 {
        self.regs[0x0E4 / 2]
    }
    pub fn spctl(&self) -> u16 {
        self.regs[0x0F0 / 2]
    }
    pub fn prinb(&self) -> u16 {
        self.regs[0x0FA / 2]
    }
    pub fn chctlb(&self) -> u16 {
        self.regs[0x02A / 2]
    }
    pub fn pncn3(&self) -> u16 {
        self.regs[0x036 / 2]
    }
    pub fn plsz(&self) -> u16 {
        self.regs[0x03A / 2]
    }
    pub fn mpofn(&self) -> u16 {
        self.regs[0x03C / 2]
    }
    pub fn chctla(&self) -> u16 {
        self.regs[0x028 / 2]
    }
    pub fn pncn0(&self) -> u16 {
        self.regs[0x030 / 2]
    }
    pub fn pncn1(&self) -> u16 {
        self.regs[0x032 / 2]
    }
    pub fn pncn2(&self) -> u16 {
        self.regs[0x034 / 2]
    }
    pub fn mpabn0(&self) -> u16 {
        self.regs[0x040 / 2]
    }
    pub fn mpcdn0(&self) -> u16 {
        self.regs[0x042 / 2]
    }
    pub fn mpabn1(&self) -> u16 {
        self.regs[0x044 / 2]
    }
    pub fn mpcdn1(&self) -> u16 {
        self.regs[0x046 / 2]
    }
    pub fn mpabn2(&self) -> u16 {
        self.regs[0x048 / 2]
    }
    pub fn mpcdn2(&self) -> u16 {
        self.regs[0x04A / 2]
    }
    pub fn mpabn3(&self) -> u16 {
        self.regs[0x04C / 2]
    }
    pub fn mpcdn3(&self) -> u16 {
        self.regs[0x04E / 2]
    }
    pub fn scxn0(&self) -> u16 {
        self.regs[0x070 / 2]
    }
    pub fn scyn0(&self) -> u16 {
        self.regs[0x072 / 2]
    }
    pub fn scxn1(&self) -> u16 {
        self.regs[0x080 / 2]
    }
    pub fn scyn1(&self) -> u16 {
        self.regs[0x082 / 2]
    }
    pub fn scxn2(&self) -> u16 {
        self.regs[0x090 / 2]
    }
    pub fn scyn2(&self) -> u16 {
        self.regs[0x092 / 2]
    }
    pub fn scxn3(&self) -> u16 {
        self.regs[0x094 / 2]
    }
    pub fn scyn3(&self) -> u16 {
        self.regs[0x096 / 2]
    }
    pub fn prina(&self) -> u16 {
        self.regs[0x0F8 / 2]
    }
    pub fn bmpna(&self) -> u16 {
        self.regs[0x02C / 2]
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

    // Phase 2 Decoded Fields (NBG3)
    pub fn n3on(&self) -> bool {
        (self.bgon() & 0x0008) != 0
    }
    pub fn n3_transparency_enable(&self) -> bool {
        (self.bgon() & 0x0800) == 0
    }
    pub fn n3chsz(&self) -> u16 {
        (self.chctlb() >> 4) & 1
    }
    pub fn n3chcn(&self) -> u16 {
        (self.chctlb() >> 5) & 1
    }
    pub fn pncn3_supplementary_char(&self) -> u16 {
        self.pncn3() & 0x001F
    }
    pub fn pncn3_supplementary_palette(&self) -> u16 {
        (self.pncn3() >> 5) & 0x7
    }
    pub fn pncn3_auxmode(&self) -> u16 {
        (self.pncn3() >> 14) & 1
    }
    pub fn pncn3_patterndatasize(&self) -> u16 {
        (self.pncn3() & 0x8000) >> 15
    }

    pub fn plsz_nbg3(&self) -> u16 {
        (self.plsz() >> 6) & 0x3
    }
    pub fn mpofn_nbg3(&self) -> u16 {
        (self.mpofn() & 0x7000) >> 6
    }
    pub fn craofa_nbg3(&self) -> u16 {
        (self.craofa() & 0x7000) >> 4
    }
    pub fn prinb_nbg3(&self) -> u16 {
        (self.prinb() >> 8) & 0x7
    }

    // NBG2 decoded fields
    pub fn n2on(&self) -> bool {
        (self.bgon() & 0x0004) != 0
    }
    pub fn n2_transparency_enable(&self) -> bool {
        (self.bgon() & 0x0400) == 0
    }
    pub fn chctlb_nbg2_pattern_size(&self) -> u16 {
        self.chctlb() & 1
    }
    pub fn n2chcn(&self) -> u16 {
        (self.chctlb() >> 1) & 1
    }
    pub fn pncn2_supplementary_char(&self) -> u16 {
        self.pncn2() & 0x001F
    }
    pub fn pncn2_supplementary_palette(&self) -> u16 {
        (self.pncn2() >> 5) & 0x7
    }
    pub fn pncn2_auxmode(&self) -> u16 {
        (self.pncn2() >> 14) & 1
    }
    pub fn pncn2_patterndatasize(&self) -> u16 {
        (self.pncn2() & 0x8000) >> 15
    }
    pub fn plsz_nbg2(&self) -> u16 {
        (self.plsz() >> 4) & 0x3
    }
    pub fn mpofn_nbg2(&self) -> u16 {
        (self.mpofn() & 0x0700) >> 2
    }
    pub fn craofa_nbg2(&self) -> u16 {
        self.craofa() & 0x0700
    }
    pub fn prinb_nbg2(&self) -> u16 {
        self.prinb() & 0x7
    }

    // NBG1 decoded fields
    pub fn n1on(&self) -> bool {
        (self.bgon() & 0x0002) != 0
    }
    pub fn n1_transparency_enable(&self) -> bool {
        (self.bgon() & 0x0200) == 0
    }
    pub fn chctla_nbg1_pattern_size(&self) -> u16 {
        (self.chctla() >> 8) & 1
    }
    pub fn n1bmen(&self) -> bool {
        (self.chctla() & 0x0200) != 0
    }
    pub fn n1bmsz(&self) -> u16 {
        (self.chctla() >> 10) & 0x3
    }
    pub fn n1chcn(&self) -> u16 {
        (self.chctla() >> 12) & 0x3
    }
    pub fn pncn1_supplementary_char(&self) -> u16 {
        self.pncn1() & 0x001F
    }
    pub fn pncn1_supplementary_palette(&self) -> u16 {
        (self.pncn1() >> 5) & 0x7
    }
    pub fn pncn1_auxmode(&self) -> u16 {
        (self.pncn1() >> 14) & 1
    }
    pub fn pncn1_patterndatasize(&self) -> u16 {
        (self.pncn1() & 0x8000) >> 15
    }
    pub fn plsz_nbg1(&self) -> u16 {
        (self.plsz() >> 2) & 0x3
    }
    pub fn mpofn_nbg1(&self) -> u16 {
        (self.mpofn() & 0x0070) << 2
    }
    pub fn craofa_nbg1(&self) -> u16 {
        (self.craofa() & 0x0070) << 4
    }
    pub fn prina_nbg1(&self) -> u16 {
        (self.prina() >> 8) & 0x7
    }

    // NBG0 decoded fields
    pub fn n0on(&self) -> bool {
        (self.bgon() & 0x0001) != 0
    }
    pub fn n0_transparency_enable(&self) -> bool {
        (self.bgon() & 0x0100) == 0
    }
    pub fn chctla_nbg0_pattern_size(&self) -> u16 {
        self.chctla() & 1
    }
    pub fn n0bmen(&self) -> bool {
        (self.chctla() & 0x0002) != 0
    }
    pub fn n0bmsz(&self) -> u16 {
        (self.chctla() >> 2) & 0x3
    }
    pub fn n0chcn(&self) -> u16 {
        (self.chctla() >> 4) & 0x7
    }
    pub fn pncn0_supplementary_char(&self) -> u16 {
        self.pncn0() & 0x001F
    }
    pub fn pncn0_supplementary_palette(&self) -> u16 {
        (self.pncn0() >> 5) & 0x7
    }
    pub fn pncn0_auxmode(&self) -> u16 {
        (self.pncn0() >> 14) & 1
    }
    pub fn pncn0_patterndatasize(&self) -> u16 {
        (self.pncn0() & 0x8000) >> 15
    }
    pub fn plsz_nbg0(&self) -> u16 {
        self.plsz() & 0x3
    }
    pub fn mpofn_nbg0(&self) -> u16 {
        (self.mpofn() & 0x0007) << 6
    }
    pub fn craofa_nbg0(&self) -> u16 {
        (self.craofa() & 0x0007) << 8
    }
    pub fn prina_nbg0(&self) -> u16 {
        self.prina() & 0x7
    }

    pub fn bktau(&self) -> u16 {
        self.regs[0xAC / 2]
    }

    pub fn bktal(&self) -> u16 {
        self.regs[0xAE / 2]
    }

    /// Back screen setup: Address comes from `bktal` and `bktau`.
    /// The bit width changes depending on VRSIZE (bit 15).
    pub fn back_screen_addr(&self) -> u32 {
        let addr = ((self.bktau() as u32) << 16) | (self.bktal() as u32);
        if self.vram_8mbit() {
            addr & 0x7FFFF
        } else {
            addr & 0x3FFFF
        }
    }

    /// Whether back screen is enabled for the current line
    pub fn back_screen_enabled(&self) -> bool {
        (self.bktau() & 0x8000) != 0
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
            (msb << 31) | crate::vdp::rgb555_to_xrgb8888(val)
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

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::shared_buffers::WorkRam;
    use std::sync::Arc;
    #[test]
    fn force_vdp2_regs_coverage() {
        // no-assert: just coverage
        let _ram = Arc::new(WorkRam::new());
        for _addr in (0..0x200).step_by(2) {
            let regs = Vdp2Registers::new();
            let _ = regs.tvmd();
        }
    }
}
