//! SH7604 on-chip DMA controller: two channels moving data between any two bus addresses.
//!
//! Registers (offsets inside the 0xFFFFFE00 on-chip window):
//! `SAR0/DAR0/TCR0/CHCR0` at 0x180..0x18C, the same four for channel 1 at 0x190..0x19C,
//! `VCRDMA0/1` at 0x1A0/0x1A8 and `DMAOR` at 0x1B0.
//!
//! What is modelled: auto-request transfers (`CHCR.AR = 1`) in byte, word, long and 16-byte
//! units, with fixed/incrementing/decrementing source and destination, run to completion the
//! moment the channel is enabled; the TE flag, the address-error/NMI flags in DMAOR, and the
//! transfer-end interrupt request. What is not: module requests (`DRCR`, from the DIVU or
//! serial ports) and external DREQ — a channel armed for one of those never starts — and cycle
//! stealing; the transfer takes no bus time, so its cost never reaches the CPU's cycle count.

use crate::bus::SystemBus;

pub const REG_FIRST: u32 = 0x180;
pub const REG_LAST: u32 = 0x1B3;

const SAR: u32 = 0x0;
const DAR: u32 = 0x4;
const TCR: u32 = 0x8;
const CHCR: u32 = 0xC;
const VCR0: u32 = 0x1A0;
const VCR1: u32 = 0x1A8;
const DMAOR: u32 = 0x1B0;

const CHCR_DE: u32 = 1 << 0;
const CHCR_TE: u32 = 1 << 1;
const CHCR_IE: u32 = 1 << 2;
const CHCR_AR: u32 = 1 << 9;
const DMAOR_DME: u32 = 1 << 0;
/// NMI flag and address-error flag: while either is set the controller is halted.
const DMAOR_FLAGS: u32 = 0b110;

#[derive(Default)]
pub struct Dmac {
    sar: [u32; 2],
    dar: [u32; 2],
    tcr: [u32; 2],
    chcr: [u32; 2],
    vcr: [u32; 2],
    dmaor: u32,
    /// Set by any write that could have armed a transfer; cleared by `run`.
    kick: bool,
}

/// Merges a partial-width write into the 32-bit register it lands in.
fn merge(old: u32, off: u32, size: usize, val: u32) -> u32 {
    let shift = 8 * (4 - (off & 3) as usize - size);
    let mask = if size == 4 {
        u32::MAX
    } else {
        ((1u32 << (8 * size)) - 1) << shift
    };
    (old & !mask) | ((val << shift) & mask)
}

impl Dmac {
    pub fn new() -> Self {
        Self::default()
    }

    /// True if `off` (window offset) is a DMAC register.
    pub fn owns(off: u32) -> bool {
        (REG_FIRST..=REG_LAST).contains(&off)
    }

    pub fn read(&self, off: u32, size: usize) -> u32 {
        let word = match off & !3 {
            DMAOR => self.dmaor,
            VCR0 => self.vcr[0],
            VCR1 => self.vcr[1],
            o => {
                let ch = ((o - REG_FIRST) / 0x10) as usize;
                match (o - REG_FIRST) % 0x10 {
                    SAR => self.sar[ch],
                    DAR => self.dar[ch],
                    TCR => self.tcr[ch],
                    CHCR => self.chcr[ch],
                    _ => 0,
                }
            }
        };
        let shift = 8 * (4 - (off & 3) as usize - size);
        if size == 4 {
            word
        } else {
            (word >> shift) & ((1 << (8 * size)) - 1)
        }
    }

    pub fn write(&mut self, off: u32, size: usize, val: u32) {
        let word_off = off & !3;
        let merged = |old: u32| merge(old, off, size, val);
        match word_off {
            DMAOR => {
                let v = merged(self.dmaor);
                // NMIF and AE clear by writing 0 over a 1; they never set by software.
                self.dmaor = (v & 0b1001) | (self.dmaor & v & DMAOR_FLAGS);
            }
            VCR0 => self.vcr[0] = merged(self.vcr[0]) & 0xFF,
            VCR1 => self.vcr[1] = merged(self.vcr[1]) & 0xFF,
            o => {
                let ch = ((o - REG_FIRST) / 0x10) as usize;
                match (o - REG_FIRST) % 0x10 {
                    SAR => self.sar[ch] = merged(self.sar[ch]),
                    DAR => self.dar[ch] = merged(self.dar[ch]),
                    TCR => self.tcr[ch] = merged(self.tcr[ch]) & 0xFF_FFFF,
                    CHCR => {
                        let v = merged(self.chcr[ch]);
                        // TE clears by writing 0 over a 1; software cannot set it.
                        self.chcr[ch] = (v & !CHCR_TE) | (self.chcr[ch] & v & CHCR_TE);
                    }
                    _ => {}
                }
            }
        }
        self.kick = true;
    }

    /// The pending transfer-end interrupt with the highest priority: channel 0 before 1.
    /// Returns `(channel, vector)`; the caller supplies the level from IPRA.
    pub fn pending_irq(&self) -> Option<(usize, u32)> {
        (0..2)
            .find(|&ch| self.chcr[ch] & (CHCR_TE | CHCR_IE) == CHCR_TE | CHCR_IE)
            .map(|ch| (ch, self.vcr[ch]))
    }

    /// Runs every armed channel to completion. Cheap when nothing was written since the last
    /// call (one flag test), which is the common case: it sits on the on-chip write path.
    pub fn run(&mut self, sys: &mut SystemBus) {
        if !std::mem::take(&mut self.kick) {
            return;
        }
        if self.dmaor & DMAOR_DME == 0 || self.dmaor & DMAOR_FLAGS != 0 {
            return;
        }
        for ch in 0..2 {
            let c = self.chcr[ch];
            if c & (CHCR_DE | CHCR_AR) != (CHCR_DE | CHCR_AR) || c & CHCR_TE != 0 {
                continue;
            }
            self.transfer(ch, sys);
        }
    }

    fn transfer(&mut self, ch: usize, sys: &mut SystemBus) {
        let c = self.chcr[ch];
        let unit: u32 = match (c >> 10) & 3 {
            0 => 1,
            1 => 2,
            2 => 4,
            _ => 16,
        };
        let step = |mode: u32| -> i64 {
            match mode {
                1 => unit as i64,
                2 => -(unit as i64),
                _ => 0,
            }
        };
        let (src_step, dst_step) = (step((c >> 12) & 3), step((c >> 14) & 3));
        // A count of zero means the maximum, 2^24 transfers.
        let count = if self.tcr[ch] == 0 {
            1 << 24
        } else {
            self.tcr[ch]
        };
        let (mut src, mut dst) = (self.sar[ch], self.dar[ch]);
        for _ in 0..count {
            let (s, d) = (src & 0x1FFF_FFFF, dst & 0x1FFF_FFFF);
            match unit {
                1 => {
                    let v = sys.read8(s);
                    sys.write8(d, v);
                }
                2 => {
                    let v = sys.read16(s);
                    sys.write16(d, v);
                }
                4 => {
                    let v = sys.read32(s);
                    sys.write32(d, v);
                }
                _ => {
                    for i in 0..4 {
                        let v = sys.read32(s + 4 * i);
                        sys.write32(d + 4 * i, v);
                    }
                }
            }
            src = src.wrapping_add(src_step as u32);
            dst = dst.wrapping_add(dst_step as u32);
        }
        self.sar[ch] = src;
        self.dar[ch] = dst;
        self.tcr[ch] = 0;
        self.chcr[ch] |= CHCR_TE;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::ram::Ram;

    fn bus_with_ram() -> SystemBus {
        let mut bus = SystemBus::new();
        bus.map(
            "ram",
            0x0600_0000,
            0x0010_0000,
            0x10_0000,
            Box::new(Ram::new(0x10_0000)),
            true,
        );
        bus
    }

    fn arm(d: &mut Dmac, ch: u32, src: u32, dst: u32, count: u32, chcr: u32) {
        let base = REG_FIRST + ch * 0x10;
        d.write(base + SAR, 4, src);
        d.write(base + DAR, 4, dst);
        d.write(base + TCR, 4, count);
        d.write(DMAOR, 4, DMAOR_DME);
        d.write(base + CHCR, 4, chcr);
    }

    #[test]
    fn auto_request_long_copy_moves_data_and_sets_te() {
        let mut bus = bus_with_ram();
        for i in 0..4u32 {
            bus.write32(0x0600_0000 + 4 * i, 0x1111_1111 * (i + 1));
        }
        let mut d = Dmac::new();
        // long units, both addresses incrementing, auto request, enabled
        let chcr = (1 << 14) | (1 << 12) | (2 << 10) | CHCR_AR | CHCR_DE;
        arm(&mut d, 0, 0x0600_0000, 0x0600_1000, 4, chcr);
        d.run(&mut bus);
        for i in 0..4u32 {
            assert_eq!(bus.read32(0x0600_1000 + 4 * i), 0x1111_1111 * (i + 1));
        }
        assert_eq!(d.read(REG_FIRST + CHCR, 4) & CHCR_TE, CHCR_TE);
        assert_eq!(d.read(REG_FIRST + TCR, 4), 0);
        assert_eq!(d.read(REG_FIRST + SAR, 4), 0x0600_0010);
    }

    #[test]
    fn a_disabled_controller_does_not_transfer() {
        let mut bus = bus_with_ram();
        bus.write32(0x0600_0000, 0xDEAD_BEEF);
        let mut d = Dmac::new();
        let chcr = (1 << 14) | (1 << 12) | (2 << 10) | CHCR_AR | CHCR_DE;
        let base = REG_FIRST;
        d.write(base + SAR, 4, 0x0600_0000);
        d.write(base + DAR, 4, 0x0600_1000);
        d.write(base + TCR, 4, 1);
        d.write(base + CHCR, 4, chcr); // DMAOR.DME is still 0
        d.run(&mut bus);
        assert_eq!(bus.read32(0x0600_1000), 0);
        assert_eq!(d.read(base + CHCR, 4) & CHCR_TE, 0);
    }

    #[test]
    fn te_clears_only_by_writing_zero_and_never_sets_by_software() {
        let mut d = Dmac::new();
        d.write(REG_FIRST + CHCR, 4, CHCR_TE);
        assert_eq!(d.read(REG_FIRST + CHCR, 4) & CHCR_TE, 0);
        let mut bus = bus_with_ram();
        let chcr = (2 << 10) | CHCR_AR | CHCR_DE;
        arm(&mut d, 0, 0x0600_0000, 0x0600_1000, 1, chcr);
        d.run(&mut bus);
        assert_eq!(d.read(REG_FIRST + CHCR, 4) & CHCR_TE, CHCR_TE);
        d.write(REG_FIRST + CHCR, 4, chcr & !CHCR_TE);
        assert_eq!(d.read(REG_FIRST + CHCR, 4) & CHCR_TE, 0);
    }

    #[test]
    fn byte_units_decrement_and_the_transfer_end_interrupt_is_reported() {
        let mut bus = bus_with_ram();
        for i in 0..3u32 {
            bus.write8(0x0600_0000 + i, 0xA0 + i as u8);
        }
        let mut d = Dmac::new();
        d.write(VCR0, 4, 0x48);
        // byte units, source incrementing, destination decrementing, interrupt enabled
        let chcr = (2 << 14) | (1 << 12) | CHCR_AR | CHCR_IE | CHCR_DE;
        arm(&mut d, 0, 0x0600_0000, 0x0600_1002, 3, chcr);
        d.run(&mut bus);
        assert_eq!(bus.read8(0x0600_1002), 0xA0);
        assert_eq!(bus.read8(0x0600_1000), 0xA2);
        assert_eq!(d.pending_irq(), Some((0, 0x48)));
    }

    #[test]
    fn a_transfer_armed_for_a_module_request_never_starts() {
        let mut bus = bus_with_ram();
        bus.write32(0x0600_0000, 1);
        let mut d = Dmac::new();
        let chcr = (2 << 10) | CHCR_DE; // AR = 0: waits for a request that never comes
        arm(&mut d, 0, 0x0600_0000, 0x0600_1000, 1, chcr);
        d.run(&mut bus);
        assert_eq!(bus.read32(0x0600_1000), 0);
    }

    #[test]
    fn sixteen_byte_units_move_four_longs_per_transfer() {
        let mut bus = bus_with_ram();
        for i in 0..8u32 {
            bus.write32(0x0600_0000 + 4 * i, i + 1);
        }
        let mut d = Dmac::new();
        let chcr = (1 << 14) | (1 << 12) | (3 << 10) | CHCR_AR | CHCR_DE;
        arm(&mut d, 1, 0x0600_0000, 0x0600_2000, 2, chcr);
        d.run(&mut bus);
        for i in 0..8u32 {
            assert_eq!(bus.read32(0x0600_2000 + 4 * i), i + 1);
        }
    }

    #[test]
    fn partial_width_writes_merge_into_the_register() {
        let mut d = Dmac::new();
        d.write(REG_FIRST + SAR, 4, 0x1122_3344);
        d.write(REG_FIRST + SAR + 2, 2, 0xAABB);
        assert_eq!(d.read(REG_FIRST + SAR, 4), 0x1122_AABB);
        assert_eq!(d.read(REG_FIRST + SAR + 3, 1), 0xBB);
    }
}
