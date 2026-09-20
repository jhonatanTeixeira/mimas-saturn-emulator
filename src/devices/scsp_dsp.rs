//! The SCSP effect DSP: 128 microcode steps per sample over a ring buffer in sound RAM.
//!
//! This exists because the BIOS needs it, not for completeness: the boot sound's slot is
//! sent to the effect bus with its dry level muted (`DISDL = 0`, `EFSDL = 7`), so on real
//! hardware **only what comes out of this DSP is heard**. Without it the boot is either
//! silent or, worse, the raw dry signal the hardware never plays.
//!
//! The instruction is 64 bits, stored as four big-endian words in MPRO. Field layout and
//! execution semantics are hardware facts; the implementation here is ours.

/// 24-bit sign extension, the DSP's working width.
fn sign24(v: u32) -> i32 {
    ((v << 8) as i32) >> 8
}

fn sign_bits(v: u32, bits: u32) -> i32 {
    let shift = 32 - bits;
    ((v << shift) as i32) >> shift
}

/// The 16-bit float the DSP uses when storing to the ring buffer: sign, 4-bit exponent,
/// 11-bit mantissa.
fn float_to_int(f: u16) -> i32 {
    let sign = ((f >> 15) & 1) as u32;
    let mut exponent = ((f >> 11) & 0xF) as u32;
    let mantissa = (f & 0x7FF) as u32;
    let mut v = sign << 31;
    if exponent > 11 {
        exponent = 11;
        v |= sign << 30;
    } else {
        v |= (!sign & 1) << 30;
    }
    v |= mantissa << 19;
    ((v as i32) >> (exponent + 8)) as i32
}

fn int_to_float(mut v: u32) -> u16 {
    let sign = (v >> 23) & 1;
    if sign != 0 {
        v = !v & 0x7F_FFFF;
    }
    let mut exponent = 0u32;
    if v <= 0x1_FFFF {
        v *= 64;
        exponent += 0x3000;
    }
    if v <= 0xF_FFFF {
        v *= 8;
        exponent += 0x1800;
    }
    if v <= 0x3F_FFFF {
        v *= 2;
        exponent += 0x800;
    }
    if v <= 0x3F_FFFF {
        v *= 2;
        exponent += 0x800;
    }
    if v <= 0x3F_FFFF {
        exponent += 0x800;
    }
    v >>= 11;
    v &= 0x7FF;
    v |= exponent;
    if sign != 0 {
        v ^= 0x7FF | (1 << 15);
    }
    v as u16
}

/// One decoded microcode step.
#[derive(Clone, Copy, Default)]
struct Op {
    tra: u8,
    twt: bool,
    twa: u8,
    xsel: bool,
    ysel: u8,
    ira: u8,
    iwt: bool,
    iwa: u8,
    table: bool,
    mwt: bool,
    mrd: bool,
    ewt: bool,
    ewa: u8,
    adrl: bool,
    frcl: bool,
    shift0: bool,
    shift1: bool,
    yrl: bool,
    negb: bool,
    zero: bool,
    bsel: bool,
    nofl: bool,
    coef: u8,
    masa: u8,
    adreb: bool,
    nxadr: bool,
}

impl Op {
    /// Decodes the 64-bit instruction. Bit positions are the chip's, read from the four
    /// 16-bit words in program order.
    fn decode(w0: u16, w1: u16, w2: u16, w3: u16) -> Self {
        Self {
            tra: ((w0 >> 8) & 0x7F) as u8,
            twt: w0 & 0x80 != 0,
            twa: (w0 & 0x7F) as u8,

            xsel: w1 & 0x8000 != 0,
            ysel: ((w1 >> 13) & 0x03) as u8,
            ira: ((w1 >> 6) & 0x3F) as u8,
            iwt: w1 & 0x20 != 0,
            iwa: (w1 & 0x1F) as u8,

            table: w2 & 0x8000 != 0,
            mwt: w2 & 0x4000 != 0,
            mrd: w2 & 0x2000 != 0,
            ewt: w2 & 0x1000 != 0,
            ewa: ((w2 >> 8) & 0x0F) as u8,
            adrl: w2 & 0x80 != 0,
            frcl: w2 & 0x40 != 0,
            shift1: w2 & 0x20 != 0,
            shift0: w2 & 0x10 != 0,
            yrl: w2 & 0x08 != 0,
            negb: w2 & 0x04 != 0,
            zero: w2 & 0x02 != 0,
            bsel: w2 & 0x01 != 0,

            nofl: w3 & 0x8000 != 0,
            coef: ((w3 >> 9) & 0x3F) as u8,
            masa: ((w3 >> 2) & 0x1F) as u8,
            adreb: w3 & 0x02 != 0,
            nxadr: w3 & 0x01 != 0,
        }
    }
}

/// One step's observable state, for comparing against a capture taken from a reference
/// machine. Diagnosis only: nothing in the emulator reads it.
#[derive(Clone, Copy, Debug, Default)]
pub struct StepTrace {
    pub step: usize,
    pub inputs: i32,
    pub x: i32,
    pub y: i32,
    pub product: i64,
    pub shift_reg: u32,
    pub io_addr: u32,
    pub read_value: u32,
    pub efreg_write: Option<(u8, i16)>,
}

pub struct ScspDsp {
    ops: [Op; 128],
    /// One past the last non-zero microcode step. The hardware stops there; running the
    /// trailing empty steps is both wasted work and a different `shift_reg` at the end of
    /// the sample.
    last_step: usize,
    /// Which microcode steps hold a non-zero instruction, to recompute `last_step`.
    filled: [bool; 128],
    /// Working memory: 128 words, addressed relative to the per-sample counter.
    temp: [u32; 128],
    /// Memory inputs, written back from ring-buffer reads.
    mems: [u32; 32],
    /// Mixer inputs: what the slots send to the effect bus.
    pub mixs: [i32; 16],
    /// Effect outputs. Channel 0 and 1 are the stereo pair the BIOS uses.
    pub efreg: [i16; 16],
    coef: [i16; 64],
    madrs: [u16; 32],
    /// Sample counter that rotates TEMP and the ring buffer.
    mdec_ct: u32,
    shift_reg: u32,
    frc_reg: u16,
    y_reg: u32,
    adrs_reg: u16,
    inputs: u32,
    read_pending: u8,
    write_pending: bool,
    write_value: u16,
    read_value: u32,
    io_addr: u32,
    /// Ring buffer position and length, from the common register 0x402.
    rbp: u32,
    rbl: u32,
    /// Steps executed on the last sample, for diagnosis.
    pub steps_run: u32,
    /// When set, each step appends (step, inputs, x, y, product, shift_reg) for one sample.
    pub trace: Option<Vec<StepTrace>>,
}

impl Default for ScspDsp {
    fn default() -> Self {
        Self::new()
    }
}

impl ScspDsp {
    pub fn new() -> Self {
        Self {
            ops: [Op::default(); 128],
            last_step: 0,
            filled: [false; 128],
            temp: [0; 128],
            mems: [0; 32],
            mixs: [0; 16],
            efreg: [0; 16],
            coef: [0; 64],
            madrs: [0; 32],
            mdec_ct: 0,
            shift_reg: 0,
            frc_reg: 0,
            y_reg: 0,
            adrs_reg: 0,
            inputs: 0,
            read_pending: 0,
            write_pending: false,
            write_value: 0,
            read_value: 0,
            io_addr: 0,
            rbp: 0,
            rbl: 0,
            steps_run: 0,
            trace: None,
        }
    }

    /// Ring buffer placement, written by the driver to the common register 0x402.
    pub fn set_ring(&mut self, v: u16) {
        self.rbl = ((v >> 7) & 0x03) as u32;
        self.rbp = (v & 0x7F) as u32;
        if self.mdec_ct == 0 {
            self.mdec_ct = 0x2000 << self.rbl;
        }
    }

    pub fn set_coef(&mut self, index: usize, v: u16) {
        // COEF is 13 bits, left-aligned in the register: the low three bits are not part
        // of the coefficient.
        self.coef[index & 63] = ((v >> 3) & 0x1FFF) as i16;
    }

    pub fn set_madrs(&mut self, index: usize, v: u16) {
        self.madrs[index & 31] = v;
    }

    /// Human-readable form of one decoded step, for diagnosis against a reference capture.
    pub fn describe(&self, step: usize) -> String {
        let o = self.ops[step & 127];
        format!(
            "tra={:3} twt={} twa={:3} xsel={} ysel={} ira={:02X} iwt={} iwa={:02X} ewt={} ewa={} bsel={} zero={} negb={} shift={}{} coef={:2} mrd={} mwt={} masa={:2} table={} adrl={} frcl={} yrl={}",
            o.tra,
            o.twt as u8,
            o.twa,
            o.xsel as u8,
            o.ysel,
            o.ira,
            o.iwt as u8,
            o.iwa,
            o.ewt as u8,
            o.ewa,
            o.bsel as u8,
            o.zero as u8,
            o.negb as u8,
            o.shift1 as u8,
            o.shift0 as u8,
            o.coef,
            o.mrd as u8,
            o.mwt as u8,
            o.masa,
            o.table as u8,
            o.adrl as u8,
            o.frcl as u8,
            o.yrl as u8
        )
    }

    /// One microcode step, as four words of MPRO.
    pub fn set_program(&mut self, step: usize, w0: u16, w1: u16, w2: u16, w3: u16) {
        let step = step & 127;
        self.ops[step] = Op::decode(w0, w1, w2, w3);
        self.filled[step] = (w0 | w1 | w2 | w3) != 0;
        self.last_step = self.filled.iter().rposition(|&f| f).map_or(0, |i| i + 1);
    }

    /// Loads a state captured from a reference machine, so a comparison starts from the
    /// same place instead of from an empty ring buffer. Diagnosis only.
    #[allow(clippy::too_many_arguments)]
    pub fn load_state(
        &mut self,
        mdec_ct: u32,
        shift_reg: u32,
        frc_reg: u16,
        adrs_reg: u16,
        y_reg: u32,
        io_addr: u32,
        read_pending: u8,
        write_pending: bool,
        read_value: u32,
        write_value: u16,
        temp: &[u32; 128],
        mems: &[u32; 32],
    ) {
        self.mdec_ct = mdec_ct;
        self.shift_reg = shift_reg;
        self.frc_reg = frc_reg;
        self.adrs_reg = adrs_reg;
        self.y_reg = y_reg;
        self.io_addr = io_addr;
        self.read_pending = read_pending;
        self.write_pending = write_pending;
        self.read_value = read_value;
        self.write_value = write_value;
        self.temp = *temp;
        self.mems = *mems;
    }

    /// One past the last non-zero microcode step.
    pub fn last_step(&self) -> usize {
        self.last_step
    }

    /// Runs the whole program once — one sample's worth of effect processing.
    pub fn run_sample(&mut self, ram: &mut [u8]) {
        self.steps_run = 0;
        for step in 0..self.last_step {
            self.exec(step, ram);
        }
        // The counter that rotates TEMP and the ring buffer counts DOWN and wraps to the
        // ring size. It must never be decremented from zero: that lands on 0xFFFFFFFF and
        // every address derived from it — TEMP slots and ring buffer alike — is garbage.
        if self.mdec_ct == 0 {
            self.mdec_ct = 0x2000 << self.rbl;
        }
        self.mdec_ct -= 1;
        self.mixs = [0; 16];
    }

    fn ram_read16(ram: &[u8], addr: u32) -> u16 {
        let a = (addr as usize * 2) & 0x7_FFFE;
        u16::from_be_bytes([ram[a], ram[a + 1]])
    }

    fn ram_write16(ram: &mut [u8], addr: u32, v: u16) {
        let a = (addr as usize * 2) & 0x7_FFFE;
        let b = v.to_be_bytes();
        ram[a] = b[0];
        ram[a + 1] = b[1];
    }

    fn exec(&mut self, step: usize, ram: &mut [u8]) {
        let op = self.ops[step];
        // Every step runs, including empty ones: the shifter is a pipeline and each step
        // advances it. Skipping "idle" steps changes the result.
        self.steps_run += 1;

        let temp_w = (op.twa as u32).wrapping_add(self.mdec_ct) as usize & 0x7F;
        let temp_r = (op.tra as u32).wrapping_add(self.mdec_ct) as usize & 0x7F;

        if op.ira & 0x20 != 0 {
            if op.ira & 0x10 != 0 {
                // External inputs (CD audio) are not wired here.
                if op.ira & 0x0E == 0 {
                    self.inputs = 0;
                }
            } else {
                self.inputs = ((self.mixs[(op.ira & 0x0F) as usize] as u32) << 4) & 0xFF_FFFF;
            }
        } else {
            self.inputs = self.mems[(op.ira & 0x1F) as usize];
        }

        let inputs = sign24(self.inputs);
        let temp = sign24(self.temp[temp_r]);
        let x = if op.xsel { inputs } else { temp };
        let y = match op.ysel {
            0 => self.frc_reg,
            1 => self.coef[op.coef as usize] as u16,
            2 => ((self.y_reg >> 11) & 0x1FFF) as u16,
            _ => ((self.y_reg >> 4) & 0x0FFF) as u16,
        };

        if op.yrl {
            self.y_reg = self.inputs & 0xFF_FFFF;
        }

        let mut shifted = (sign_bits(self.shift_reg, 26)) << (op.shift0 ^ op.shift1) as u32;
        if !op.shift1 {
            shifted = shifted.clamp(-0x80_0000, 0x7F_FFFF);
        }
        let shifted = (shifted as u32) & 0xFF_FFFF;

        let mut efreg_write = None;
        if op.ewt {
            self.efreg[op.ewa as usize] = (shifted >> 8) as i16;
            efreg_write = Some((op.ewa, (shifted >> 8) as i16));
        }
        if op.twt {
            self.temp[temp_w] = shifted;
        }
        if op.frcl {
            self.frc_reg = if op.shift0 && op.shift1 {
                (shifted & 0xFFF) as u16
            } else {
                (shifted >> 11) as u16
            };
        }

        let product = ((sign_bits(y as u32, 13) as i64) * x as i64) >> 12;
        let mut b = if op.bsel { self.shift_reg } else { temp as u32 };
        if op.negb {
            b = (b as i32).wrapping_neg() as u32;
        }
        if op.zero {
            b = 0;
        }
        self.shift_reg = ((product as u32).wrapping_add(b)) & 0x3FF_FFFF;

        if op.iwt {
            self.mems[op.iwa as usize] = self.read_value;
        }

        if self.read_pending != 0 {
            let raw = Self::ram_read16(ram, self.io_addr);
            self.read_value = if self.read_pending == 2 {
                ((raw as u32) << 8) & 0xFF_FFFF
            } else {
                float_to_int(raw) as u32 & 0xFF_FFFF
            };
            self.read_pending = 0;
        } else if self.write_pending {
            Self::ram_write16(ram, self.io_addr, self.write_value);
            self.write_pending = false;
        }

        // The address is recomputed on every step, not only on the ones that touch memory:
        // the read and write pending from the previous step are serviced above, before this
        // runs, so an idle step still advances the address the hardware exposes.
        {
            let mut addr = self.madrs[op.masa as usize] as u32 + op.nxadr as u32;
            if op.adreb {
                addr = addr.wrapping_add(sign_bits(self.adrs_reg as u32, 12) as u32);
            }
            if !op.table {
                addr = addr.wrapping_add(self.mdec_ct);
                addr &= (0x2000 << self.rbl) - 1;
            }
            self.io_addr = (addr + (self.rbp << 12)) & 0x3_FFFF;
            if op.mrd {
                self.read_pending = 1 + op.nofl as u8;
            }
            if op.mwt {
                self.write_pending = true;
                self.write_value = if op.nofl {
                    (shifted >> 8) as u16
                } else {
                    int_to_float(shifted)
                };
            }
        }

        if op.adrl {
            self.adrs_reg = if op.shift0 && op.shift1 {
                (shifted >> 12) as u16
            } else {
                ((inputs >> 16) & 0xFFF) as u16
            };
        }

        if let Some(t) = self.trace.as_mut() {
            t.push(StepTrace {
                step,
                inputs,
                x,
                y: sign_bits(y as u32, 13),
                product,
                shift_reg: self.shift_reg,
                io_addr: self.io_addr,
                read_value: self.read_value,
                efreg_write,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_float_format_round_trips_through_zero_and_extremes() {
        // Hand-derived: zero stays zero, and a value converted out and back must not drift
        // by more than the format's own resolution.
        assert_eq!(float_to_int(int_to_float(0)), 0);
        for v in [0x00_1000u32, 0x7F_0000, 0x40_0000] {
            let back = float_to_int(int_to_float(v)) as u32 & 0xFF_FFFF;
            let diff = (sign24(back) - sign24(v)).abs();
            assert!(
                diff < 0x2000,
                "{v:06X} came back {back:06X}, difference {diff:X}"
            );
        }
    }

    #[test]
    fn an_empty_program_does_nothing() {
        let mut dsp = ScspDsp::new();
        let mut ram = vec![0u8; 0x8_0000];
        dsp.mixs[0] = 0x1000;
        dsp.run_sample(&mut ram);
        assert_eq!(dsp.steps_run, 0);
        assert_eq!(dsp.efreg, [0; 16]);
    }

    #[test]
    fn a_step_can_copy_a_mixer_input_to_an_effect_output() {
        let mut dsp = ScspDsp::new();
        let mut ram = vec![0u8; 0x8_0000];
        // Step 0: read MIXS[0] (IRA = 0x20), multiply by COEF[0], write to EFREG[0].
        // Step 1 writes the accumulator out, since the shifter lags one step.
        dsp.set_coef(0, 0x7FF8); // close to 1.0 in 13-bit fixed point
        dsp.set_program(
            0,
            0x0000,
            0b1010_0000_0000_0000 | (0x20 << 6),
            0x0000,
            0x0000,
        );
        dsp.set_program(1, 0x0000, 0x0000, 0x1000, 0x0000); // EWT to EFREG[0]
        dsp.mixs[0] = 0x4000;
        dsp.run_sample(&mut ram);
        assert_eq!(dsp.steps_run, 2);
        assert_ne!(dsp.efreg[0], 0, "the step has to produce effect output");
    }

    /// The hardware runs up to the last non-zero microcode step and stops. Running the
    /// trailing empty steps is not harmless: each one still multiplies and accumulates, so
    /// the sample ends with a different `shift_reg` than the real chip has.
    #[test]
    fn the_program_stops_after_the_last_non_zero_step() {
        let mut dsp = ScspDsp::new();
        let mut ram = vec![0u8; 0x8_0000];
        dsp.set_program(0, 0x0000, 0xA800, 0x0000, 0x0000);
        dsp.set_program(3, 0x0000, 0x0000, 0x1000, 0x0000);
        assert_eq!(dsp.last_step(), 4);

        dsp.run_sample(&mut ram);
        assert_eq!(dsp.steps_run, 4, "the trailing empty steps must not run");

        // Clearing the last step shortens the program again.
        dsp.set_program(3, 0x0000, 0x0000, 0x0000, 0x0000);
        assert_eq!(dsp.last_step(), 1);
    }

    /// The memory address is recomputed on every step, including the ones that touch no
    /// memory. A step's pending read is serviced on the step after it, before that step
    /// recomputes, so the value the hardware exposes keeps moving while the data does not.
    #[test]
    fn the_memory_address_moves_on_every_step_not_only_on_memory_ones() {
        let mut dsp = ScspDsp::new();
        let mut ram = vec![0u8; 0x8_0000];
        dsp.set_ring(0); // rbp 0, rbl 0 -> mdec_ct wraps at 0x2000
        dsp.set_madrs(1, 0x100);
        dsp.set_madrs(2, 0x200);
        // Step 0 reads from MADRS[1]; step 1 touches no memory but names MADRS[2].
        dsp.set_program(0, 0x0000, 0x0000, 0x2000, 0x0004);
        dsp.set_program(1, 0x0000, 0x0000, 0x0000, 0x0008);
        dsp.set_program(2, 0x0000, 0x0000, 0x0000, 0x0000);

        dsp.trace = Some(Vec::new());
        dsp.run_sample(&mut ram);
        let t = dsp.trace.take().unwrap();
        // `set_ring` primes the counter at the ring size, and the decrement lands at the
        // end of the sample, so every step of this sample sees that value.
        let ct = 0x2000u32;
        assert_eq!(t[0].io_addr, (0x100 + ct) & 0x1FFF);
        assert_eq!(
            t[1].io_addr,
            (0x200 + ct) & 0x1FFF,
            "a step with no mrd/mwt still moves the address"
        );
    }
}
