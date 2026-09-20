//! SCSP: real register file, 32 PCM slots and 44.1 kHz mixing.
//!
//! Real here: the register file (the 68000 reads back what it wrote), the slot fields that
//! actually move sound (start address, loop, pitch, level and pan), the three timers, and
//! the interrupts they raise to the 68000.
//!
//! Declared simplification: the envelope. Real hardware has four phases driven by rate
//! tables; here a slot plays while its key is on and its level comes from TL, DISDL and
//! IMXL read live. That is enough for the BIOS sound to appear and wrong for real attack
//! and decay — replacing it with a real envelope generator is known work, not discovery.
//!
//! Nothing here came from another emulator: the fields were derived from the hardware map
//! and checked against what the real BIOS driver writes (see `docs/sound.md`).

use std::collections::BTreeMap;

use crate::bus::MemoryDevice;
use crate::devices::scsp_dsp::ScspDsp;

pub const SLOTS: usize = 32;
pub const REG_SIZE: usize = 0x1000;
pub const SAMPLE_RATE: u32 = 44100;
/// The Saturn's 68000 runs at 11.2896 MHz = 44100 × 256, i.e. 256 cycles per sample.
pub const M68K_CYCLES_PER_SAMPLE: u64 = 256;

const SCIEB: usize = 0x41E;
const SCIPD: usize = 0x420;
const SCIRE: usize = 0x422;
const MCIPD: usize = 0x42C;
const MCIRE: usize = 0x42E;
/// The 68000 interrupt level of each source, one bit per source per register.
const SCILV0: usize = 0x424;
const SCILV1: usize = 0x426;
const SCILV2: usize = 0x428;

/// SCIPD/SCIEB bits. Timer A is bit 6, confirmed by the real driver: it reloads TIMA and
/// acknowledges with SCIRE = 0x0040 on every tick (see `docs/sound.md`).
const BIT_TIMER_A: u16 = 1 << 6;
const BIT_TIMER_B: u16 = 1 << 7;
const BIT_TIMER_C: u16 = 1 << 8;
/// Fires once per sample.
const BIT_SAMPLE: u16 = 1 << 10;

/// Playback state of one slot. Only what changes per sample lives here; level, pan and
/// pitch are read from the registers every sample, because a sound driver keeps writing
/// them while a note plays.
#[derive(Clone, Copy, Default)]
struct Slot {
    active: bool,
    /// Read position in sound RAM, in samples, 16.16 fixed point.
    pos: u64,
    sa: u32,
    lsa: u32,
    lea: u32,
    loop_mode: u8,
    pcm8: bool,
}

pub struct Scsp {
    pub regs: [u8; REG_SIZE],
    slots: [Slot; SLOTS],
    /// Timer A/B/C counters in 8.8 fixed point, like the hardware's: a sample adds
    /// `256 >> prescaler` and the interrupt fires when the count crosses `0xFF00`, so the
    /// period is `(255 - written value) << prescaler` samples.
    timer_count: [u32; 3],
    /// 68000 cycles not yet turned into samples.
    cycle_carry: u64,
    /// Which interrupt sources the driver enabled, accumulated.
    pub scieb_seen: u16,
    pub key_ons: u64,
    /// Writes to the common registers (0x400+), deduplicated: offset -> (value, count).
    pub common_writes: BTreeMap<usize, (u16, u64)>,
    /// One line per key-on, to show what the driver programmed.
    pub key_log: Vec<String>,
    /// The effect DSP. The BIOS's boot sound is sent to it with the dry path muted, so
    /// without this there is nothing to hear.
    pub dsp: ScspDsp,
    /// Largest effect send and effect return seen, for diagnosis.
    pub max_send: i32,
    pub max_efreg: i32,
    /// Range seen on the effect bus in and out, to tell "no signal" from "stuck".
    pub mixs_min: i32,
    pub mixs_max: i32,
    pub efreg_min: i32,
    pub efreg_max: i32,
    /// Largest absolute value seen on each effect input and output channel.
    pub mixs_peak: [i32; 16],
    pub efreg_peak: [i32; 16],
    pub isel_seen: u16,
    /// Samples produced so far, used as a timestamp for the write timeline.
    pub samples_out: u64,
    /// Timeline of writes to slot 0, to see what the driver does to a playing note.
    pub slot0_log: Vec<String>,
}

impl Default for Scsp {
    fn default() -> Self {
        Self::new()
    }
}

impl Scsp {
    pub fn new() -> Self {
        Self {
            regs: [0; REG_SIZE],
            slots: [Slot::default(); SLOTS],
            timer_count: [0; 3],
            cycle_carry: 0,
            scieb_seen: 0,
            key_ons: 0,
            common_writes: BTreeMap::new(),
            key_log: Vec::new(),
            dsp: ScspDsp::new(),
            max_send: 0,
            max_efreg: 0,
            mixs_min: 0,
            mixs_max: 0,
            efreg_min: 0,
            efreg_max: 0,
            mixs_peak: [0; 16],
            efreg_peak: [0; 16],
            isel_seen: 0,
            samples_out: 0,
            slot0_log: Vec::new(),
        }
    }

    fn word(&self, off: usize) -> u16 {
        u16::from_be_bytes([self.regs[off], self.regs[off + 1]])
    }

    fn set_word(&mut self, off: usize, v: u16) {
        let b = v.to_be_bytes();
        self.regs[off] = b[0];
        self.regs[off + 1] = b[1];
    }

    /// Interrupt level pending for the 68000, or 0.
    ///
    /// The level is not fixed: SCILV0/1/2 hold one bit per source, and the three together
    /// give that source a 3-bit level. A source whose three bits are clear has level 0 and
    /// does not interrupt at all. The BIOS driver relies on this — it gives timer B level 2
    /// and leaves timer A at 0, so the two land on different 68000 autovectors. Raising
    /// everything at a fixed level runs the wrong handler.
    ///
    /// Sources above bit 7 share bit 7's level, which is what the hardware does.
    pub fn irq_level(&self) -> u8 {
        let mut active = self.word(SCIPD) & self.word(SCIEB);
        if active & !0xFF != 0 {
            active = (active & 0xFF) | BIT_TIMER_B;
        }
        let (l0, l1, l2) = (self.word(SCILV0), self.word(SCILV1), self.word(SCILV2));
        let mut level = 0u8;
        for bit in 0..8 {
            let m = 1u16 << bit;
            if active & m == 0 {
                continue;
            }
            let l =
                u8::from(l0 & m != 0) | (u8::from(l1 & m != 0) << 1) | (u8::from(l2 & m != 0) << 2);
            level = level.max(l);
        }
        level
    }

    pub fn active_slots(&self) -> usize {
        self.slots.iter().filter(|s| s.active).count()
    }

    /// KYONEX applies every slot's KYONB at once, the way the hardware does.
    fn apply_keys(&mut self) {
        for i in 0..SLOTS {
            let base = i * 0x20;
            let on = self.word(base) & (1 << 11) != 0;
            if on && !self.slots[i].active {
                self.slots[i] = self.start_slot(i);
                self.key_ons += 1;
                if self.key_log.len() < 16 {
                    self.key_log.push(format!(
                        "slot {i:2}: ctrl={:04X} SA={:05X} LSA={:04X} LEA={:04X} eg1={:04X} eg2={:04X} TL={:02X} pitch={:04X} imxl={:04X} disdl={:04X}",
                        self.word(base),
                        self.slots[i].sa,
                        self.word(base + 0x04),
                        self.word(base + 0x06),
                        self.word(base + 0x08),
                        self.word(base + 0x0A),
                        self.word(base + 0x0C) & 0xFF,
                        self.word(base + 0x10),
                        self.word(base + 0x14),
                        self.word(base + 0x16),
                    ));
                }
            } else if !on {
                self.slots[i].active = false;
            }
        }
    }

    fn start_slot(&self, i: usize) -> Slot {
        let base = i * 0x20;
        let ctrl = self.word(base);
        Slot {
            active: true,
            pos: 0,
            sa: (((ctrl & 0x0F) as u32) << 16) | self.word(base + 0x02) as u32,
            lsa: self.word(base + 0x04) as u32,
            lea: self.word(base + 0x06) as u32,
            loop_mode: ((ctrl >> 5) & 0x03) as u8,
            pcm8: ctrl & (1 << 4) != 0,
        }
    }

    /// Pitch step in 16.16, from OCT (signed exponent) and FNS (fraction in 1024).
    fn step(&self, i: usize) -> u64 {
        let pitch = self.word(i * 0x20 + 0x10);
        let oct = ((pitch >> 11) & 0x0F) as i32;
        let oct = if oct > 7 { oct - 16 } else { oct };
        let fns = (pitch & 0x7FF) as f64;
        (((1.0 + fns / 1024.0) * 2f64.powi(oct)) * 65536.0) as u64
    }

    /// A send level is an attenuation in shifts, not a ratio: level 7 is unity and level 0
    /// is silence, which the hardware documents as minus infinity.
    fn sdl_shift(level: u8) -> u32 {
        if level == 0 { 32 } else { 7 - level as u32 }
    }

    /// Pan attenuates one side and leaves the other alone. Bit 4 picks which side, and the
    /// low four bits are how many shifts it loses.
    fn panning(pan: u8) -> (u32, u32) {
        if pan & 0x10 != 0 {
            (0, (pan & 0x0F) as u32)
        } else {
            ((pan & 0x0F) as u32, 0)
        }
    }

    /// `v >> shift`, with the shifts that mean silence giving zero instead of a panic.
    fn attenuate(v: i32, shift: u32) -> i32 {
        if shift >= 16 { 0 } else { v >> shift }
    }

    /// Advance the timers and generate samples. `m68k_cycles` is how far the 68000 moved.
    pub fn generate(&mut self, m68k_cycles: u64, ram: &mut [u8], out: &mut Vec<(i16, i16)>) {
        self.cycle_carry += m68k_cycles;
        let samples = self.cycle_carry / M68K_CYCLES_PER_SAMPLE;
        self.cycle_carry -= samples * M68K_CYCLES_PER_SAMPLE;
        for _ in 0..samples {
            self.tick_timers();
            let s = self.sample(ram);
            out.push(s);
        }
        self.samples_out += samples;
    }

    /// Each timer counts up once every `1 << prescaler` samples and raises its bit when the
    /// 8-bit count wraps. The driver writes the reload value into the same register.
    fn tick_timers(&mut self) {
        let mut pending = self.word(SCIPD) | BIT_SAMPLE;
        for (i, bit) in [BIT_TIMER_A, BIT_TIMER_B, BIT_TIMER_C]
            .into_iter()
            .enumerate()
        {
            let prescaler = ((self.word(0x418 + i * 2) >> 8) & 0x07) as u32;
            self.timer_count[i] += 256 >> prescaler;
            if self.timer_count[i] >= 0xFF00 {
                self.timer_count[i] -= 0xFF00;
                pending |= bit;
            }
        }
        self.set_word(SCIPD, pending);
    }

    /// One sample of the whole chip: every slot's dry send, every slot's send into the
    /// effect DSP, then the DSP's return, then the master volume.
    ///
    /// All of it is integer, and the order matters: the dry path attenuates by DISDL and
    /// pans, the send into the DSP attenuates by IMXL and picks a mixer channel with ISEL,
    /// and the return attenuates channel `i` by **slot `i`'s** EFSDL and pans by its EFPAN.
    /// Sending with the wrong register, or returning at full level, is the difference
    /// between the boot sound and a clipped roar.
    fn sample(&mut self, ram: &mut [u8]) -> (i16, i16) {
        let (mut left, mut right) = (0i32, 0i32);
        for i in 0..SLOTS {
            if !self.slots[i].active {
                continue;
            }
            let base = i * 0x20;
            let step = self.step(i);
            // TL is the one piece of the envelope this build has: a static attenuation
            // where the hardware has four timed phases. Declared simplification.
            let tl = (self.word(base + 0x0C) & 0xFF) as f32;
            let attenuation = 10f32.powf(-(tl / 255.0) * 2.0);
            let dry = self.regs[base + 0x16];
            let send = self.regs[base + 0x15];

            let slot = &mut self.slots[i];
            let idx = (slot.pos >> 16) as u32;
            let raw = if slot.pcm8 {
                let a = (slot.sa + idx) as usize & 0x7_FFFF;
                (ram[a] as i8 as i32) << 8
            } else {
                let a = (slot.sa + idx * 2) as usize & 0x7_FFFE;
                i16::from_be_bytes([ram[a], ram[a + 1]]) as i32
            };
            slot.pos += step;
            let end = slot.lea.max(1);
            if (slot.pos >> 16) as u32 >= end {
                match slot.loop_mode {
                    0 => slot.active = false, // no loop: stop at the end
                    _ => slot.pos = (slot.lsa as u64) << 16,
                }
            }

            let output = ((raw as f32 * attenuation) as i32).clamp(-32768, 32767);

            let disdl = Self::attenuate(output, Self::sdl_shift((dry >> 5) & 0x07));
            let (pl, pr) = Self::panning(dry & 0x1F);
            left += Self::attenuate(disdl, pl) >> 1;
            right += Self::attenuate(disdl, pr) >> 1;

            // IMXL is the level into the effect bus and ISEL the channel it lands on. The
            // mixer bus is 20 bits wide, which is where the `<< 4` comes from.
            let mixs_input = Self::attenuate(output, Self::sdl_shift(send & 0x07));
            let isel = ((send >> 3) & 0x0F) as usize;
            self.dsp.mixs[isel] = self.dsp.mixs[isel].saturating_add(mixs_input << 4);
            self.max_send = self.max_send.max((mixs_input << 4).abs());
            self.isel_seen |= 1 << isel;
            self.mixs_peak[isel] = self.mixs_peak[isel].max((mixs_input << 4).abs());
        }

        self.mixs_min = self.mixs_min.min(self.dsp.mixs[0]);
        self.mixs_max = self.mixs_max.max(self.dsp.mixs[0]);
        self.dsp.run_sample(ram);
        self.efreg_min = self.efreg_min.min(self.dsp.efreg[0] as i32);
        self.efreg_max = self.efreg_max.max(self.dsp.efreg[0] as i32);

        for i in 0..16 {
            let eff = self.regs[i * 0x20 + 0x17];
            let v = self.dsp.efreg[i] as i32;
            self.efreg_peak[i] = self.efreg_peak[i].max(v.abs());
            self.max_efreg = self.max_efreg.max(v.abs());
            let applied = Self::attenuate(v, Self::sdl_shift((eff >> 5) & 0x07));
            let (pl, pr) = Self::panning(eff & 0x1F);
            left += Self::attenuate(applied, pl) >> 1;
            right += Self::attenuate(applied, pr) >> 1;
        }

        // MVOL is the last stage and it only ever attenuates: 0xF is unity.
        let shift = 0x0F - (self.word(0x400) & 0x0F) as u32;
        let clip = |v: i32| Self::attenuate(v, shift).clamp(-32768, 32767) as i16;
        (clip(left), clip(right))
    }

    /// Write side effects: slot keys, interrupt clearing, and a log of what the driver
    /// programmed in the common registers.
    fn after_write(&mut self, off: usize) {
        if matches!(off, 0x00 | 0x10 | 0x16) && self.slots[0].active && self.slot0_log.len() < 40 {
            self.slot0_log.push(format!(
                "{:7.3}s off={:02X} = {:04X}{}",
                self.samples_out as f64 / SAMPLE_RATE as f64,
                off,
                self.word(off),
                if self.slots[0].active {
                    " (playing)"
                } else {
                    ""
                }
            ));
        }
        if off < SLOTS * 0x20 {
            if off % 0x20 == 0 && self.word(off) & (1 << 12) != 0 {
                self.apply_keys();
                let v = self.word(off) & !(1 << 12);
                self.set_word(off, v);
            }
            return;
        }
        let v = self.word(off);
        // First non-zero write into the DSP area: if a program arrives, this is where.
        if v != 0 && (0x600..0xC00).contains(&off) && self.key_log.len() < 16 {
            self.key_log.push(format!(
                "DSP {:6.3}s off={:03X} = {:04X}",
                self.samples_out as f64 / SAMPLE_RATE as f64,
                off,
                v
            ));
        }
        let entry = self.common_writes.entry(off).or_insert((v, 0));
        entry.0 = v;
        entry.1 += 1;
        match off {
            0x402 => self.dsp.set_ring(v),
            0x700..=0x77F => self.dsp.set_coef((off - 0x700) / 2, v),
            0x780..=0x7BF => self.dsp.set_madrs((off - 0x780) / 2, v),
            0x800..=0xBFF => {
                // One step is four words; reload the whole step whenever part of it moves.
                let step = (off - 0x800) / 8;
                let b = 0x800 + step * 8;
                self.dsp.set_program(
                    step,
                    self.word(b),
                    self.word(b + 2),
                    self.word(b + 4),
                    self.word(b + 6),
                );
            }
            0x418 | 0x41A | 0x41C => self.timer_count[(off - 0x418) / 2] = ((v & 0xFF) as u32) << 8,
            SCIEB => self.scieb_seen |= v,
            SCIRE => {
                let cleared = self.word(SCIPD) & !v;
                self.set_word(SCIPD, cleared);
            }
            MCIRE => {
                let cleared = self.word(MCIPD) & !v;
                self.set_word(MCIPD, cleared);
            }
            _ => {}
        }
    }
}

impl MemoryDevice for Scsp {
    fn read_byte(&mut self, off: u32) -> u8 {
        self.regs[off as usize & (REG_SIZE - 1)]
    }

    fn write_byte(&mut self, off: u32, v: u8) {
        let o = off as usize & (REG_SIZE - 1);
        self.regs[o] = v;
        self.after_write(o & !1);
    }

    fn write_word(&mut self, off: u32, v: u16) {
        let o = off as usize & (REG_SIZE - 1) & !1;
        let b = v.to_be_bytes();
        self.regs[o] = b[0];
        self.regs[o + 1] = b[1];
        self.after_write(o);
    }
}

#[cfg(test)]
mod tests_support {
    use super::*;

    /// Sound RAM filled with a ramp, so a slot reading it produces a predictable sequence.
    pub fn ram_with_ramp() -> Vec<u8> {
        let mut ram = vec![0u8; 0x8_0000];
        // 16-bit big-endian samples at 0x1000: 0x0100, 0x0200, 0x0300, 0x0400.
        for (i, v) in [0x0100u16, 0x0200, 0x0300, 0x0400].into_iter().enumerate() {
            let b = v.to_be_bytes();
            ram[0x1000 + i * 2] = b[0];
            ram[0x1000 + i * 2 + 1] = b[1];
        }
        ram
    }

    /// Programs slot 0 to play the ramp at unity pitch and full direct level.
    pub fn program_slot0(scsp: &mut Scsp, loop_mode: u16) {
        scsp.write_word(0x02, 0x1000); // SA low: byte address 0x1000
        scsp.write_word(0x04, 0x0000); // LSA
        scsp.write_word(0x06, 0x0004); // LEA: four samples
        scsp.write_word(0x0C, 0x0000); // TL = 0, no attenuation
        scsp.write_word(0x10, 0x0000); // OCT = 0, FNS = 0 -> step of exactly one sample
        scsp.write_word(0x16, 0xE000); // DISDL = 7 in the high byte, pan centred
        scsp.write_word(0x400, 0x000F); // MVOL at unity; it boots at zero on hardware
        // ctrl: KYONEX | KYONB | loop mode, SA high nibble 0.
        scsp.write_word(0x00, 0x1800 | (loop_mode << 5));
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::*;
    use super::*;

    #[test]
    fn key_on_plays_the_sample_at_unity_pitch() {
        let mut ram = ram_with_ramp();
        let mut scsp = Scsp::new();
        program_slot0(&mut scsp, 0);
        assert_eq!(scsp.key_ons, 1);
        assert_eq!(scsp.active_slots(), 1);

        let mut out = Vec::new();
        scsp.generate(M68K_CYCLES_PER_SAMPLE * 4, &mut ram, &mut out);
        assert_eq!(out.len(), 4);
        // Hand-derived: DISDL 7 is unity, the pan is centred, and the mixer halves every
        // contribution, so each ramp entry comes out at half its value.
        let left: Vec<i16> = out.iter().map(|(l, _)| *l).collect();
        assert_eq!(left, vec![128, 256, 384, 512]);
    }

    #[test]
    fn a_slot_without_loop_stops_at_the_end_address() {
        let mut ram = ram_with_ramp();
        let mut scsp = Scsp::new();
        program_slot0(&mut scsp, 0); // LPCTL = 0: no loop
        let mut out = Vec::new();
        scsp.generate(M68K_CYCLES_PER_SAMPLE * 8, &mut ram, &mut out);
        assert_eq!(scsp.active_slots(), 0, "the slot must stop at LEA");
        assert!(
            out[4..].iter().all(|(l, r)| *l == 0 && *r == 0),
            "after the end the slot must be silent"
        );
    }

    #[test]
    fn a_looping_slot_returns_to_the_loop_start() {
        let mut ram = ram_with_ramp();
        let mut scsp = Scsp::new();
        program_slot0(&mut scsp, 1); // LPCTL = 1: normal loop
        let mut out = Vec::new();
        scsp.generate(M68K_CYCLES_PER_SAMPLE * 8, &mut ram, &mut out);
        assert_eq!(
            scsp.active_slots(),
            1,
            "a looping slot never ends on its own"
        );
        let left: Vec<i16> = out.iter().map(|(l, _)| *l).collect();
        assert_eq!(left[..4], left[4..8], "the second pass repeats the first");
    }

    #[test]
    fn key_off_silences_the_slot() {
        let mut ram = ram_with_ramp();
        let mut scsp = Scsp::new();
        program_slot0(&mut scsp, 1);
        assert_eq!(scsp.active_slots(), 1);
        scsp.write_word(0x00, 0x1020); // KYONEX with KYONB clear
        assert_eq!(scsp.active_slots(), 0);
    }

    #[test]
    fn one_octave_up_reads_twice_as_fast() {
        let mut ram = ram_with_ramp();
        let mut scsp = Scsp::new();
        program_slot0(&mut scsp, 1);
        scsp.write_word(0x10, 1 << 11); // OCT = 1
        let mut out = Vec::new();
        scsp.generate(M68K_CYCLES_PER_SAMPLE * 2, &mut ram, &mut out);
        let left: Vec<i16> = out.iter().map(|(l, _)| *l).collect();
        // Two samples at double speed land on ramp entries 0 and 2.
        assert_eq!(left, vec![128, 384]);
    }

    #[test]
    fn timer_a_raises_its_interrupt_and_scire_clears_it() {
        let mut ram = vec![0u8; 0x8_0000];
        let mut scsp = Scsp::new();
        scsp.write_word(SCIEB as u32, BIT_TIMER_A);
        scsp.write_word(SCILV2 as u32, BIT_TIMER_A); // level 4 for this source
        scsp.write_word(0x418, 0x00FE); // TIMA two ticks from wrapping, prescaler 0
        assert_eq!(scsp.irq_level(), 0);

        let mut out = Vec::new();
        scsp.generate(M68K_CYCLES_PER_SAMPLE * 2, &mut ram, &mut out);
        assert_eq!(scsp.irq_level(), 4, "the timer must raise its bit on wrap");

        scsp.write_word(SCIRE as u32, BIT_TIMER_A);
        assert_eq!(scsp.irq_level(), 0, "SCIRE must clear the pending bit");
    }

    #[test]
    fn the_level_comes_from_scilv_and_a_source_left_at_zero_never_interrupts() {
        let mut ram = vec![0u8; 0x8_0000];
        let mut scsp = Scsp::new();
        // What the BIOS driver actually programs: timer B at level 2, timer A left at 0.
        scsp.write_word(SCIEB as u32, BIT_TIMER_A | BIT_TIMER_B);
        scsp.write_word(SCILV1 as u32, BIT_TIMER_B);
        scsp.write_word(0x418, 0x00FE); // timer A wraps after 2 samples
        scsp.write_word(0x41A, 0x00F0); // timer B only after 15

        let mut out = Vec::new();
        scsp.generate(M68K_CYCLES_PER_SAMPLE * 2, &mut ram, &mut out);
        assert_ne!(scsp.word(SCIPD) & BIT_TIMER_A, 0, "timer A did wrap");
        assert_eq!(
            scsp.irq_level(),
            0,
            "a source with no SCILV bit set has level 0 and must not interrupt"
        );

        scsp.generate(M68K_CYCLES_PER_SAMPLE * 14, &mut ram, &mut out);
        assert_eq!(
            scsp.irq_level(),
            2,
            "timer B carries the level SCILV1 gives it"
        );
    }

    #[test]
    fn sources_above_bit_seven_borrow_bit_sevens_level() {
        let mut ram = vec![0u8; 0x8_0000];
        let mut scsp = Scsp::new();
        // The per-sample bit is bit 10; the hardware gives it bit 7's level.
        scsp.write_word(SCIEB as u32, BIT_SAMPLE);
        scsp.write_word(SCILV0 as u32, BIT_TIMER_B);
        scsp.write_word(SCILV1 as u32, BIT_TIMER_B);
        let mut out = Vec::new();
        scsp.generate(M68K_CYCLES_PER_SAMPLE, &mut ram, &mut out);
        assert_eq!(scsp.irq_level(), 3);
    }

    #[test]
    fn an_unenabled_source_never_interrupts() {
        let mut ram = vec![0u8; 0x8_0000];
        let mut scsp = Scsp::new();
        scsp.write_word(SCIEB as u32, BIT_TIMER_B); // only timer B enabled
        scsp.write_word(0x418, 0x00FF); // timer A about to wrap
        let mut out = Vec::new();
        scsp.generate(M68K_CYCLES_PER_SAMPLE * 4, &mut ram, &mut out);
        assert_eq!(scsp.irq_level(), 0);
    }

    #[test]
    fn samples_follow_the_68000_clock() {
        let mut ram = vec![0u8; 0x8_0000];
        let mut scsp = Scsp::new();
        let mut out = Vec::new();
        // 256 cycles per sample: 1000 cycles give three samples and 232 left over.
        scsp.generate(1000, &mut ram, &mut out);
        assert_eq!(out.len(), 3);
        scsp.generate(24, &mut ram, &mut out); // 232 + 24 = 256: exactly one more
        assert_eq!(out.len(), 4);
    }
}

#[cfg(test)]
mod routing_tests {
    use super::tests_support::*;
    use super::*;

    /// Three registers, three different jobs, and the BIOS boot sound depends on telling
    /// them apart: DISDL (high byte of 0x16) is the dry path to the output, IMXL/ISEL (byte
    /// 0x15) is the send into the effect DSP, and EFSDL (high bits of byte 0x17) is how much
    /// of the DSP's return comes back. The boot slot arrives with the dry path muted, so
    /// reading the wrong one plays exactly what the hardware silences.
    #[test]
    fn disdl_drives_the_dry_output_and_nothing_else() {
        let mut ram = ram_with_ramp();
        let mut out = Vec::new();

        let mut dry = Scsp::new();
        program_slot0(&mut dry, 1);
        dry.write_word(0x16, 0xE000); // dry full
        dry.generate(M68K_CYCLES_PER_SAMPLE * 2, &mut ram, &mut out);
        assert!(
            out.iter().any(|(l, _)| *l != 0),
            "the dry send has to be audible"
        );
        assert_eq!(
            dry.dsp.mixs, [0; 16],
            "with no IMXL nothing reaches the effect bus"
        );

        let mut muted = Scsp::new();
        out.clear();
        program_slot0(&mut muted, 1);
        muted.write_word(0x16, 0x0000); // the BIOS boot case: dry muted
        muted.generate(M68K_CYCLES_PER_SAMPLE * 2, &mut ram, &mut out);
        assert!(
            out.iter().all(|(l, r)| *l == 0 && *r == 0),
            "with DISDL at zero the dry path is silent"
        );
    }

    #[test]
    fn imxl_sends_to_the_dsp_channel_that_isel_picks() {
        let mut ram = ram_with_ramp();
        let mut out = Vec::new();
        let mut scsp = Scsp::new();
        program_slot0(&mut scsp, 1);
        scsp.write_word(0x16, 0x0000); // dry muted, as the BIOS leaves it
        scsp.write_word(0x14, 0x0027); // ISEL = 4, IMXL = 7
        scsp.generate(M68K_CYCLES_PER_SAMPLE, &mut ram, &mut out);
        // The slot's own peak, on the channel ISEL names and nowhere else.
        assert_eq!(scsp.mixs_peak[4], 0x0100 << 4);
        assert_eq!(scsp.mixs_peak[0], 0, "nothing leaks into another channel");
        assert_eq!(scsp.isel_seen, 1 << 4);
    }

    /// The return is attenuated per channel by **slot i's** EFSDL, not by the slot that
    /// sent the signal in. Returning at full level is what turns the boot chime into a
    /// clipped roar once the driver finally uploads a DSP program.
    #[test]
    fn efsdl_attenuates_the_effect_return() {
        let mut ram = vec![0u8; 0x8_0000];
        let mut out = Vec::new();
        let mut scsp = Scsp::new();
        scsp.write_word(0x400, 0x000F); // MVOL at unity
        // Two steps: read MIXS[0], then write the accumulator to EFREG[0].
        scsp.dsp.set_coef(0, 0x7FF8);
        scsp.dsp.set_program(
            0,
            0x0000,
            0b1010_0000_0000_0000 | (0x20 << 6),
            0x0000,
            0x0000,
        );
        scsp.dsp.set_program(1, 0x0000, 0x0000, 0x1000, 0x0000);

        let returned = |scsp: &mut Scsp, efsdl: u16, ram: &mut [u8], out: &mut Vec<(i16, i16)>| {
            scsp.write_word(0x16, efsdl << 5); // byte 0x17 = EFSDL, pan centred
            scsp.dsp.mixs[0] = 0x4000;
            out.clear();
            scsp.generate(M68K_CYCLES_PER_SAMPLE, ram, out);
            out[0].0 as i32
        };

        let full = returned(&mut scsp, 7, &mut ram, &mut out);
        assert!(full > 8, "EFSDL 7 is unity and has to return the signal");
        // Each level down is one more shift, and level 0 is silence.
        assert_eq!(returned(&mut scsp, 6, &mut ram, &mut out), full / 2);
        assert_eq!(returned(&mut scsp, 5, &mut ram, &mut out), full / 4);
        assert_eq!(returned(&mut scsp, 0, &mut ram, &mut out), 0);
    }
}
