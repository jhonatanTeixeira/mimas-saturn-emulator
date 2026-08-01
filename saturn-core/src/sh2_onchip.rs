#[derive(Debug, Clone)]
pub struct Sh2OnChip {
    // SCI
    pub smr: u8,
    pub brr: u8,
    pub scr: u8,
    pub tdr: u8,
    pub ssr: u8,
    pub rdr: u8,

    // FRT
    pub tier: u8,
    pub ftcsr: u8,
    pub frc: u16,
    pub ocra: u16,
    pub ocrb: u16,
    pub tcr: u8,
    pub tocr: u8,
    pub ficr: u16,

    // INTC
    pub ipra: u16,
    pub iprb: u16,
    pub vcra: u16,
    pub vcrb: u16,
    pub vcrc: u16,
    pub vcrd: u16,
    pub icr: u16,
    pub vcrwdt: u16,

    // WDT
    pub wtcsr: u8,
    pub wtcnt: u8,
    pub rstcsr: u8,

    // SBYCR
    pub sbycr: u8,

    // CCR
    pub ccr: u8,

    // DIVU
    pub dvsr: u32,
    pub dvdnt: u32,
    pub dvcr: u32,
    pub vcrdiv: u32,
    pub dvdnth: u32,
    pub dvdntl: u32,
    pub dvdntuh: u32,
    pub dvdntul: u32,

    // UBC
    pub bara: u32,
    pub bamra: u32,
    pub bbra: u16,
    pub barb: u32,
    pub bamrb: u32,
    pub bbrb: u16,
    pub bdrb: u32,
    pub bdmrb: u32,
    pub brcr: u32,

    // DMA
    pub sar0: u32,
    pub dar0: u32,
    pub tcr0: u32,
    pub chcr0: u32,
    pub sar1: u32,
    pub dar1: u32,
    pub tcr1: u32,
    pub chcr1: u32,
    pub vcrdma0: u32,
    pub vcrdma1: u32,
    pub dmaor: u32,

    // BSC
    pub bcr1: u16,
    pub bcr2: u16,
    pub wcr: u16,
    pub mcr: u16,
    pub rtcsr: u16,
    pub rtcnt: u16,
    pub rtcor: u16,

    // DRCR
    pub drcr0: u8,
    pub drcr1: u8,
}

impl Sh2OnChip {
    pub fn new(is_slave: bool) -> Self {
        Self {
            smr: 0x00,
            brr: 0xFF,
            scr: 0x00,
            tdr: 0xFF,
            ssr: 0x84,
            rdr: 0x00,

            tier: 0x01,
            ftcsr: 0x00,
            frc: 0x0000,
            ocra: 0xFFFF,
            ocrb: 0xFFFF,
            tcr: 0x00,
            tocr: 0xE0,
            ficr: 0x0000,

            ipra: 0x0000,
            iprb: 0x0000,
            vcra: 0x0000,
            vcrb: 0x0000,
            vcrc: 0x0000,
            vcrd: 0x0000,
            icr: 0x0000,
            vcrwdt: 0x0000,

            wtcsr: 0x18,
            wtcnt: 0x00,
            rstcsr: 0x1F,

            sbycr: 0x60,
            ccr: 0x00,

            dvsr: 0,
            dvdnt: 0,
            dvcr: 0,
            vcrdiv: 0,
            dvdnth: 0,
            dvdntl: 0,
            dvdntuh: 0,
            dvdntul: 0,

            bara: 0,
            bamra: 0,
            bbra: 0,
            barb: 0,
            bamrb: 0,
            bbrb: 0,
            bdrb: 0,
            bdmrb: 0,
            brcr: 0,

            sar0: 0,
            dar0: 0,
            tcr0: 0,
            chcr0: 0,
            sar1: 0,
            dar1: 0,
            tcr1: 0,
            chcr1: 0,
            vcrdma0: 0,
            vcrdma1: 0,
            dmaor: 0,

            bcr1: if is_slave { 0x83F0 } else { 0x03F0 },
            bcr2: 0x00FC,
            wcr: 0xAAFF,
            mcr: 0x0000,
            rtcsr: 0x0000,
            rtcnt: 0x0000,
            rtcor: 0x0000,

            drcr0: 0x00,
            drcr1: 0x00,
        }
    }

    pub fn reset(&mut self, is_slave: bool) {
        let bcr1_bit15 = self.bcr1 & 0x8000;
        *self = Self::new(is_slave);
        self.bcr1 = bcr1_bit15 | 0x03F0;
    }
}
