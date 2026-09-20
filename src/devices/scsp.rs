//! SCSP: real register file, 32 PCM slots and 44.1 kHz mixing.
//!
//! Real here: the register file (the 68000 reads back what it wrote), the slot fields that
//! actually move sound (start address, loop, pitch, level and pan), the three timers, and
//! the interrupts they raise to the 68000.
//!
//! Declared simplification: the envelope. Real hardware times four phases (attack, two
//! decays, release) from rate registers (AR/D1R/D2R/RR, scaled by KRS/OCT/FNS). We do not
//! know those registers' exact bit layout and have no captured envelope curve to check a
//! guess against, so instead of encoding an unverified guess as fact, a slot holds at full
//! gain for 50 ms after key-on and then decays on a fixed clock (`EG_HOLD_SAMPLES`,
//! `EG_DECAY_PER_SAMPLE` in `sample()`). That turns a held tone that drones forever into one
//! that fades, which is the audible defect this exists to fix — it is not the hardware's
//! envelope, and the timing will not match a real capture sample for sample.
//!
//! Nothing here came from another emulator: the fields were derived from the hardware map
//! and checked against what the real BIOS driver writes (see `docs/sound.md`).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::rc::Rc;

use crate::bus::MemoryDevice;
use crate::devices::scsp_dsp::ScspDsp;

/// Tier 2c cache key: everything that determines a non-looping voice's per-sample core
/// output (RAM fetch, TL, the time-based envelope — none of which reads a register besides
/// these). Pan, send level and ISEL are deliberately left out: they are applied *after* the
/// cached core, and caching them too would hide the part of a repeat play that is actually
/// allowed to differ (a game can send the same hit sound to a different pan/channel).
type SfxKey = (u32, u32, u32, u16, u8, bool); // sa, lsa, lea, pitch_reg, tl, pcm8

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

/// The envelope: held at full gain for this many samples after key-on (50 ms), then decays.
/// Real hardware times this from AR/D1R/D2R/RR/KRS, decoded from the slot's two envelope
/// registers (0x08/0x0A). We do not know their exact bit layout — no capture of a real
/// envelope curve exists to check a guess against, and this project's rule is not to encode
/// an unverified guess as if it were a fact (see `docs/sound.md`). This hold-then-decay
/// shape is a declared, time-based stand-in: it is not derived from those registers at all.
/// It replaces silence-never (a held tone that drones) with silence-eventually, which is the
/// audible defect this exists to fix; it is not the hardware's envelope.
const EG_HOLD_SAMPLES: u64 = 2_205;
/// Per-sample multiplier during decay, chosen so a held note reaches -60 dB about two
/// seconds after the hold ends.
const EG_DECAY_PER_SAMPLE: f32 = 0.999_921_7;
/// Below this the slot is inaudible; snap to exactly zero so nothing lingers below the
/// resolution of an i16 sample.
const EG_SILENCE_FLOOR: f32 = 1e-4;

/// Tier 2c: how many one-shot voices (menu blips, hit sounds) stay cached at once. Small on
/// purpose — a scene has dozens of distinct effects, not thousands; oldest entry evicted
/// first when full.
const SFX_CACHE_MAX_ENTRIES: usize = 64;
/// A voice longer than this (in output samples, roughly 1.5s at 44.1kHz) is not worth
/// caching: the odds of an exact-parameter repeat drop as a sound gets longer, and the
/// buffer itself gets big enough to matter.
const SFX_CACHE_MAX_SAMPLES: usize = 0x1_0000;

/// Playback state of one slot. Only what changes per sample lives here; level, pan and
/// pitch are read from the registers every sample, because a sound driver keeps writing
/// them while a note plays.
#[derive(Clone, Default)]
struct Slot {
    active: bool,
    /// Read position in sound RAM, in samples, 16.16 fixed point.
    pos: u64,
    sa: u32,
    lsa: u32,
    lea: u32,
    loop_mode: u8,
    pcm8: bool,
    /// Envelope multiplier, 1.0 at key-on down to 0.0 at silence. See `sample()` for the
    /// hold-then-decay shape and why it is time-based rather than register-rate-based.
    eg_gain: f32,
    /// Samples since key-on, used to time the hold-then-decay envelope.
    eg_age: u64,
    /// Everything below is derived from registers that usually do not change between two
    /// samples of the same note — cached at key-on and refreshed in `after_write` when the
    /// owning register moves, instead of recomputed 44,100 times a second regardless. A game
    /// driving the mixer hard (hundreds of register writes a frame) rewrites these far less
    /// often than it renders samples, so this turns "recompute every sample" into "recompute
    /// on the write that actually changed something."
    attenuation: f32,
    pitch_step: u64,
    dry_shift: u32,
    dry_pan: (u32, u32),
    send_shift: u32,
    isel: usize,
    /// Tier 2c. `Some` when this key-on hit the cache: the core output replays from here
    /// instead of touching RAM or the envelope at all. `sfx_cursor` is the read position.
    sfx_playback: Option<Rc<Vec<i32>>>,
    sfx_cursor: usize,
    /// `Some` while building a new cache candidate (a loop-mode-0 voice short enough to be
    /// worth caching, with no cached entry yet) — the core output is pushed here every
    /// sample and handed to `Scsp::sfx_cache` when the voice reaches its natural end.
    sfx_capture: Option<Vec<i32>>,
    sfx_key: Option<SfxKey>,
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
    /// Off by default: `common_writes`/`key_log`/`slot0_log` exist only for `--sound-profile`
    /// to report on, and a real game rewrites registers hundreds of times a frame — bookkeeping
    /// a `String` and a `BTreeMap` entry on every one of those writes for a report nobody asked
    /// for is wasted work in the hottest path this chip has. `set_diagnostics` turns it on.
    pub diag_enabled: bool,
    /// Tier 2c: cached core output for one-shot (non-looping) voices, keyed by the register
    /// tuple that determines it. `sfx_cache_order` tracks insertion order for eviction — a
    /// `VecDeque` of keys, oldest first, since a plain `HashMap` has no order of its own.
    sfx_cache: HashMap<SfxKey, Rc<Vec<i32>>>,
    sfx_cache_order: VecDeque<SfxKey>,
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
            slots: std::array::from_fn(|_| Slot::default()),
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
            diag_enabled: false,
            sfx_cache: HashMap::new(),
            sfx_cache_order: VecDeque::new(),
        }
    }

    /// Inserts a finished one-shot capture into the cache, evicting the oldest entry first if
    /// full. A no-op if the key is already cached (can happen if two slots finish an
    /// identical, previously-uncached voice on the same sample).
    fn sfx_cache_insert(&mut self, key: SfxKey, buf: Vec<i32>) {
        if self.sfx_cache.contains_key(&key) {
            return;
        }
        if self.sfx_cache_order.len() >= SFX_CACHE_MAX_ENTRIES
            && let Some(oldest) = self.sfx_cache_order.pop_front()
        {
            self.sfx_cache.remove(&oldest);
        }
        self.sfx_cache_order.push_back(key);
        self.sfx_cache.insert(key, Rc::new(buf));
    }

    /// Turns the `--sound-profile` bookkeeping (`common_writes`, `key_log`, `slot0_log`) on
    /// or off. Off by default; nothing that plays sound reads these fields back.
    pub fn set_diagnostics(&mut self, on: bool) {
        self.diag_enabled = on;
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
                if self.diag_enabled && self.key_log.len() < 16 {
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
        let (dry_shift, dry_pan) = self.slot_dry(i);
        let (send_shift, isel) = self.slot_send(i);
        let sa = (((ctrl & 0x0F) as u32) << 16) | self.word(base + 0x02) as u32;
        let lsa = self.word(base + 0x04) as u32;
        let lea = self.word(base + 0x06) as u32;
        let loop_mode = ((ctrl >> 5) & 0x03) as u8;
        let pcm8 = ctrl & (1 << 4) != 0;
        let pitch_step = self.step(i);

        // Tier 2c: only non-looping voices are candidates — a looping one never reaches a
        // "finished, cache it" moment the way this is wired. On a hit, replay instead of
        // computing; on a miss short enough to be worth it, start building one.
        let (sfx_playback, sfx_capture, sfx_key) = if loop_mode == 0 {
            let key: SfxKey = (
                sa,
                lsa,
                lea,
                self.word(base + 0x10),
                (self.word(base + 0x0C) & 0xFF) as u8,
                pcm8,
            );
            if let Some(cached) = self.sfx_cache.get(&key) {
                (Some(cached.clone()), None, None)
            } else {
                // Rough estimate of how many output samples this voice will take: LEA in
                // 16.16 fixed point divided by the per-sample position step. Only used to
                // decide whether to bother — `sample()` still guards the real push.
                let estimate = (lea as u64).saturating_mul(0x1_0000) / pitch_step.max(1) + 1;
                if estimate <= SFX_CACHE_MAX_SAMPLES as u64 {
                    (None, Some(Vec::with_capacity(estimate as usize)), Some(key))
                } else {
                    (None, None, None)
                }
            }
        } else {
            (None, None, None)
        };

        Slot {
            active: true,
            pos: 0,
            sa,
            lsa,
            lea,
            loop_mode,
            pcm8,
            eg_gain: 1.0,
            eg_age: 0,
            attenuation: self.slot_attenuation(i),
            pitch_step,
            dry_shift,
            dry_pan,
            send_shift,
            isel,
            sfx_playback,
            sfx_cursor: 0,
            sfx_capture,
            sfx_key,
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

    /// TL is the one piece of the envelope this build has: a static attenuation where the
    /// hardware has four timed phases. Declared simplification.
    fn slot_attenuation(&self, i: usize) -> f32 {
        let tl = (self.word(i * 0x20 + 0x0C) & 0xFF) as f32;
        10f32.powf(-(tl / 255.0) * 2.0)
    }

    /// DISDL/pan for the dry path, decoded once instead of every sample.
    fn slot_dry(&self, i: usize) -> (u32, (u32, u32)) {
        let dry = self.regs[i * 0x20 + 0x16];
        (
            Self::sdl_shift((dry >> 5) & 0x07),
            Self::panning(dry & 0x1F),
        )
    }

    /// IMXL/ISEL for the effect-bus send, decoded once instead of every sample.
    fn slot_send(&self, i: usize) -> (u32, usize) {
        let send = self.regs[i * 0x20 + 0x15];
        (Self::sdl_shift(send & 0x07), ((send >> 3) & 0x0F) as usize)
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
            // Tier 2c: filled in below only on the branch that finishes building a new cache
            // entry, then handed to `sfx_cache_insert` after `slot`'s borrow ends — inserting
            // needs `&mut self` as a whole, which cannot happen while `slot` still borrows
            // `self.slots[i]`.
            let mut new_cache_entry: Option<(SfxKey, Vec<i32>)> = None;

            let slot = &mut self.slots[i];
            let (dry_shift, (pl, pr)) = (slot.dry_shift, slot.dry_pan);
            let (send_shift, isel) = (slot.send_shift, slot.isel);

            let output = if let Some(cached) = slot.sfx_playback.clone() {
                // Same (SA, LSA, LEA, pitch, TL, pcm8) played before: the core output is a
                // pure function of those and of samples-since-key-on, so replay it instead of
                // touching RAM or the envelope at all.
                let v = cached.get(slot.sfx_cursor).copied().unwrap_or(0);
                slot.sfx_cursor += 1;
                if slot.sfx_cursor >= cached.len() {
                    slot.active = false; // matches reaching LEA on the non-cached path
                }
                v
            } else {
                let step = slot.pitch_step;
                let attenuation = slot.attenuation;
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
                let mut finished = false;
                if (slot.pos >> 16) as u32 >= end {
                    match slot.loop_mode {
                        0 => {
                            slot.active = false; // no loop: stop at the end
                            finished = true;
                        }
                        _ => slot.pos = (slot.lsa as u64) << 16,
                    }
                }

                slot.eg_age += 1;
                if slot.eg_age > EG_HOLD_SAMPLES && slot.eg_gain > 0.0 {
                    slot.eg_gain *= EG_DECAY_PER_SAMPLE;
                    if slot.eg_gain < EG_SILENCE_FLOOR {
                        slot.eg_gain = 0.0;
                    }
                }
                let eg_gain = slot.eg_gain;
                let v = ((raw as f32 * attenuation * eg_gain) as i32).clamp(-32768, 32767);

                if let Some(buf) = slot.sfx_capture.as_mut() {
                    if buf.len() < SFX_CACHE_MAX_SAMPLES {
                        buf.push(v);
                    } else {
                        // The length estimate at key-on was wrong (non-integer pitch step
                        // rounding can do that) — abandon the capture rather than grow
                        // unbounded for a voice that turned out too long to be worth caching.
                        slot.sfx_capture = None;
                        slot.sfx_key = None;
                    }
                }
                if finished {
                    if let (Some(buf), Some(key)) = (slot.sfx_capture.take(), slot.sfx_key.take()) {
                        new_cache_entry = Some((key, buf));
                    }
                }
                v
            };

            let disdl = Self::attenuate(output, dry_shift);
            left += Self::attenuate(disdl, pl) >> 1;
            right += Self::attenuate(disdl, pr) >> 1;

            // IMXL is the level into the effect bus and ISEL the channel it lands on. The
            // mixer bus is 20 bits wide, which is where the `<< 4` comes from.
            let mixs_input = Self::attenuate(output, send_shift);
            self.dsp.mixs[isel] = self.dsp.mixs[isel].saturating_add(mixs_input << 4);
            self.max_send = self.max_send.max((mixs_input << 4).abs());
            self.isel_seen |= 1 << isel;
            self.mixs_peak[isel] = self.mixs_peak[isel].max((mixs_input << 4).abs());

            if let Some((key, buf)) = new_cache_entry {
                self.sfx_cache_insert(key, buf);
            }
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
        if self.diag_enabled
            && matches!(off, 0x00 | 0x10 | 0x16)
            && self.slots[0].active
            && self.slot0_log.len() < 40
        {
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
            let i = off / 0x20;
            match off % 0x20 {
                0x00 => {
                    if self.word(off) & (1 << 12) != 0 {
                        self.apply_keys();
                        let v = self.word(off) & !(1 << 12);
                        self.set_word(off, v);
                    }
                }
                // Refresh whatever `sample()` cached from this register, instead of leaving
                // it stale until the next key-on. A slot playing keeps its position and
                // envelope, so key-on is the only place those get reset — but level, pitch
                // and routing keep getting rewritten by the driver while a note plays, and
                // the whole point of caching them is that they must not go stale.
                0x0C if self.slots[i].active => {
                    self.slots[i].attenuation = self.slot_attenuation(i)
                }
                0x10 if self.slots[i].active => self.slots[i].pitch_step = self.step(i),
                0x14 if self.slots[i].active => {
                    let (send_shift, isel) = self.slot_send(i);
                    self.slots[i].send_shift = send_shift;
                    self.slots[i].isel = isel;
                }
                0x16 if self.slots[i].active => {
                    let (dry_shift, dry_pan) = self.slot_dry(i);
                    self.slots[i].dry_shift = dry_shift;
                    self.slots[i].dry_pan = dry_pan;
                }
                _ => {}
            }
            return;
        }
        let v = self.word(off);
        if self.diag_enabled {
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
        }
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

    /// Proof that the second play actually comes from the cache, not just that the numbers
    /// happen to match: the RAM changes between the two plays. A recompute would read the new
    /// content and produce a different answer; a cache hit replays the first play's output
    /// regardless.
    #[test]
    fn a_repeated_one_shot_replays_from_cache_instead_of_rereading_ram() {
        let mut ram = ram_with_ramp();
        let mut scsp = Scsp::new();
        program_slot0(&mut scsp, 0); // LPCTL = 0: one-shot, the only mode Tier 2c caches
        let mut out = Vec::new();
        scsp.generate(M68K_CYCLES_PER_SAMPLE * 4, &mut ram, &mut out);
        let first: Vec<i16> = out.iter().map(|(l, _)| *l).collect();
        assert_eq!(first, vec![128, 256, 384, 512]);
        assert_eq!(
            scsp.active_slots(),
            0,
            "a one-shot must have finished by now"
        );

        // Same exact parameters, but the RAM this slot reads from now holds something else.
        for b in ram[0x1000..0x1008].iter_mut() {
            *b = 0;
        }
        scsp.write_word(0x00, 0x1800); // KYONEX | KYONB, same SA/LSA/LEA/pitch/TL as before
        out.clear();
        scsp.generate(M68K_CYCLES_PER_SAMPLE * 4, &mut ram, &mut out);
        let second: Vec<i16> = out.iter().map(|(l, _)| *l).collect();
        assert_eq!(
            second, first,
            "a cache hit must replay the first play's output, not read the RAM that changed"
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
        let mut scsp = Scsp::new();
        program_slot0(&mut scsp, 1);
        assert_eq!(scsp.active_slots(), 1);
        scsp.write_word(0x00, 0x1020); // KYONEX with KYONB clear
        assert_eq!(scsp.active_slots(), 0);
    }

    #[test]
    fn diagnostics_are_off_by_default_and_only_the_flag_turns_them_on() {
        let mut scsp = Scsp::new();
        assert!(!scsp.diag_enabled);
        program_slot0(&mut scsp, 1); // several register writes, including a key-on
        assert!(
            scsp.common_writes.is_empty(),
            "bookkeeping must stay off unless set_diagnostics(true) was called"
        );
        assert!(scsp.key_log.is_empty());

        scsp.set_diagnostics(true);
        scsp.write_word(0x400, 0x0001); // any common-register write
        assert!(
            !scsp.common_writes.is_empty(),
            "once on, the same write path must record again"
        );

        // With diagnostics on and slot 0 playing, the timeline and the DSP-area log record
        // too — the two branches the previous checks did not reach.
        scsp.write_word(0x10, 0x0001); // slot 0's own pitch register, while it is active
        assert!(!scsp.slot0_log.is_empty());
        scsp.write_word(0x600, 0x0001); // non-zero write inside the DSP area
        assert!(scsp.key_log.iter().any(|l| l.starts_with("DSP")));
    }

    /// TL is cached at key-on (Tier 2a) and must be refreshed whenever the driver rewrites it
    /// mid-note, not just read once — a driver that fades a note by lowering TL depends on
    /// this.
    #[test]
    fn tl_written_while_a_note_plays_changes_the_cached_attenuation() {
        let mut ram = ram_with_ramp();
        let mut scsp = Scsp::new();
        program_slot0(&mut scsp, 1); // TL = 0, no attenuation
        let mut out = Vec::new();
        scsp.generate(M68K_CYCLES_PER_SAMPLE, &mut ram, &mut out);
        let full = out[0].0;

        scsp.write_word(0x0C, 0x0080); // raise TL well past halfway while the note is playing
        out.clear();
        scsp.generate(M68K_CYCLES_PER_SAMPLE, &mut ram, &mut out);
        assert!(
            out[0].0.abs() < full.abs(),
            "raising TL after key-on must quiet the note, got {} against {full}",
            out[0].0
        );
    }

    #[test]
    fn the_envelope_holds_then_decays_a_slot_that_is_never_key_offed() {
        let mut ram = ram_with_ramp();
        let mut scsp = Scsp::new();
        program_slot0(&mut scsp, 1); // looping, TL = 0, never key-offed

        let mut out = Vec::new();
        scsp.generate(M68K_CYCLES_PER_SAMPLE * 4, &mut ram, &mut out);
        let early_peak = out.iter().map(|(l, _)| l.unsigned_abs()).max().unwrap();
        assert_eq!(
            early_peak, 512,
            "inside the hold window the level must be untouched"
        );

        out.clear();
        scsp.generate(M68K_CYCLES_PER_SAMPLE * 300_000, &mut ram, &mut out);
        let late_peak = out[out.len() - 4..]
            .iter()
            .map(|(l, _)| l.unsigned_abs())
            .max()
            .unwrap();
        assert!(
            late_peak < early_peak / 10,
            "a note held this long must have decayed, got {late_peak} against {early_peak}"
        );
        assert_eq!(
            scsp.active_slots(),
            1,
            "the envelope silences the output, it does not stop the slot"
        );
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
