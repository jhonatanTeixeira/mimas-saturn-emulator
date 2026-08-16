//! Real SMPC (System Manager & Peripheral Control) command processor.
//!
//! Extracted from logic that used to live inline in `Sh2::smpc_execute_command`
//! (see `docs/implementation-plans/smpc-peripheral.md` Phase 0) so SMPC state
//! isn't bolted onto the CPU struct and so it's testable in isolation. Register
//! *storage* stays in `WorkRam::smpc_regs`; this type holds only the
//! non-register state (currently none) and the command semantics.
//!
//! Real hardware's register file is a 64-byte struct with each register on the
//! *odd* byte of a 2-byte-aligned pair (Yabause indexes it as `SmpcRegsT[addr
//! >> 1]`, making the even offset an alias of the odd one). Mimas instead
//! stores one independent byte per offset in the full 0x80-byte real address
//! window -- closer to hardware intent (registers only ever live at odd
//! addresses; the even-offset alias is a Yabause storage-layout artifact, not
//! real hardware behavior) -- see `docs/hardware-reference/smpc-peripheral.md`
//! §1.1. This is a deliberate divergence, not an oversight: keep it.
//!
//! **RTC is UTC, not host-local time.** Yabause reads the real-time clock via
//! `localtime_r`, i.e. the host's configured timezone (§7.1). That makes any
//! RTC-dependent test non-reproducible across machines/CI. Mimas reports the
//! real-time clock in UTC instead -- a deliberate, stated deviation (per
//! `CLAUDE.md`'s "state simplifications honestly" rule), not an oversight.

use crate::shared_buffers::WorkRam;

/// Real SMPC register offsets within the 0x80-byte real address window
/// (`0x00100000` + offset), cross-checked against
/// `docs/hardware-reference/smpc-peripheral.md` §1.2 and confirmed live
/// against real BIOS code (`sh2dis.py` trace at BIOS `0x1D38-0x1D66`, see the
/// implementation plan §0.6).
pub mod reg {
    pub const IREG0: usize = 0x01;
    pub const IREG1: usize = 0x03;
    pub const IREG2: usize = 0x05;
    pub const IREG3: usize = 0x07;
    pub const IREG4: usize = 0x09;
    pub const IREG5: usize = 0x0B;
    pub const IREG6: usize = 0x0D;
    // 0x0F..=0x1D: padding (unused, real hardware has nothing here).
    pub const COMREG: usize = 0x1F;
    /// `OREGn = OREG_BASE + 2*n`, n in 0..=31 (`OREG31 == 0x5F`).
    pub const OREG_BASE: usize = 0x21;
    pub const SR: usize = 0x61;
    pub const SF: usize = 0x63;
    // 0x65..=0x73: padding.
    pub const PDR1: usize = 0x75;
    pub const PDR2: usize = 0x77;
    pub const DDR1: usize = 0x79;
    pub const DDR2: usize = 0x7B;
    pub const IOSEL: usize = 0x7D;
    pub const EXLE: usize = 0x7F;

    pub const fn oreg(n: usize) -> usize {
        OREG_BASE + 2 * n
    }
}

/// Real SMPC command IDs (COMREG values), per
/// `docs/hardware-reference/smpc-peripheral.md` §3.4.
pub mod cmd {
    pub const MSHON: u8 = 0x00;
    pub const SSHON: u8 = 0x02;
    pub const SSHOFF: u8 = 0x03;
    pub const SNDON: u8 = 0x06;
    pub const SNDOFF: u8 = 0x07;
    pub const CDON: u8 = 0x08;
    pub const CDOFF: u8 = 0x09;
    pub const SYSRES: u8 = 0x0D;
    pub const CKCHG352: u8 = 0x0E;
    pub const CKCHG320: u8 = 0x0F;
    pub const INTBACK: u8 = 0x10;
    pub const SETSMEM: u8 = 0x17;
    pub const NMIREQ: u8 = 0x18;
    pub const RESENAB: u8 = 0x19;
    pub const RESDISA: u8 = 0x1A;
}

/// Real SMPC region IDs (OREG9), per
/// `docs/hardware-reference/smpc-peripheral.md` §0.3.
pub mod region {
    pub const AUTODETECT: u8 = 0x0;
    pub const JAPAN: u8 = 0x1;
    pub const ASIA_NTSC: u8 = 0x2;
    pub const NORTH_AMERICA: u8 = 0x4;
    pub const CENTRAL_SOUTH_AMERICA_NTSC: u8 = 0x5;
    pub const KOREA: u8 = 0x6;
    pub const ASIA_PAL: u8 = 0xA;
    pub const EUROPE: u8 = 0xC;
    pub const CENTRAL_SOUTH_AMERICA_PAL: u8 = 0xD;
}

/// Side effects an SMPC command has *outside* the SMPC's own register file --
/// `Smpc` has no access to `Sh2`, `LockStepSync`, or `BusArbiter`, so it
/// returns what needs to happen and lets the caller (`Sh2`'s SMPC write path)
/// apply it. Keeps every existing cross-thread handshake (see
/// `Sh2::apply_smpc_effects`) byte-for-byte identical while moving the
/// *decision* into `Smpc`.
#[derive(Debug, Default)]
pub struct SmpcEffects {
    /// Indicates whether `cmd::SSHON` was executed.
    pub start_slave: bool,
    /// Indicates whether `cmd::SSHOFF` was executed.
    pub stop_slave: bool,
    /// Indicates whether `cmd::SNDON` was executed.
    pub sound_on: bool,
    /// Indicates whether `cmd::SNDOFF` was executed.
    pub sound_off: bool,
    /// Indicates whether `cmd::INTBACK` requests a System Manager IRQ (SH-2
    /// level 8) when a command completes -- today, only INTBACK requests it
    /// (§2.3).
    pub system_manager_irq: bool,
    /// NMIREQ §4.13, reset button §4.16.
    pub nmi: bool,
    /// SYSRES §4.8.
    pub system_reset: bool,
    /// CKCHG352/320 §4.9/§4.10. true = 352, false = 320.
    pub clock_change: Option<bool>,
}

/// Where INTBACK's RTC bytes (OREG1-7) come from. Mirrors §7.1's
/// `clocksync` distinction but named for what each variant actually is,
/// rather than transliterating Yabause's flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockSource {
    /// Re-read the host's wall clock (UTC) on every INTBACK status call.
    HostWallClock,
    /// A fixed UNIX timestamp (UTC seconds) -- deterministic, for tests and
    /// future deterministic replay. Skips Yabause's
    /// `basetime + frame_count * 1001/60000` formula entirely: §7.1 flags
    /// that formula as NTSC-only and ~20% slow under PAL, so it isn't a
    /// faithful "real time" source to begin with. A deterministic
    /// *advancing* clock, if wanted later, should derive from the real frame
    /// period for the active video mode instead.
    Fixed(u64),
}

impl Default for ClockSource {
    fn default() -> Self {
        ClockSource::HostWallClock
    }
}

impl ClockSource {
    /// Resolve to `(year, month, day, hour, minute, second, weekday)` in
    /// UTC -- `month` is 1-12, `weekday` is 0=Sunday..6=Saturday (§5.6's
    /// layout). No `chrono`/`time` dependency: this project has none today,
    /// and proleptic-Gregorian-in-UTC civil-calendar math is a handful of
    /// lines via the well-known `civil_from_days` algorithm (Howard
    /// Hinnant, public domain,
    /// http://howardhinnant.github.io/date_algorithms.html) -- exact for
    /// every real date, not an approximation.
    fn now_utc(&self) -> (u64, u8, u8, u8, u8, u8, u8) {
        let unix_seconds = match *self {
            ClockSource::Fixed(secs) => secs,
            ClockSource::HostWallClock => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        };
        let days = (unix_seconds / 86_400) as i64;
        let secs_of_day = unix_seconds % 86_400;
        let hour = (secs_of_day / 3600) as u8;
        let minute = ((secs_of_day % 3600) / 60) as u8;
        let second = (secs_of_day % 60) as u8;
        // 1970-01-01 (day 0) was a Thursday; weekday 0=Sunday per §5.6.
        let weekday = ((days + 4).rem_euclid(7)) as u8;
        let (year, month, day) = civil_from_days(days);
        (year as u64, month, day, hour, minute, second, weekday)
    }
}

/// `civil_from_days`: days-since-1970-01-01 -> `(year, month, day)` in the
/// proleptic Gregorian calendar. See `ClockSource::now_utc`'s doc comment
/// for provenance.
fn civil_from_days(z: i64) -> (i64, u8, u8) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

/// Encode `0..=99` as two BCD nibbles, e.g. `59 -> 0x59`.
fn bcd(v: u8) -> u8 {
    ((v / 10) << 4) | (v % 10)
}

/// Non-register SMPC state -- Phase 4 adds the peripheral port state.
pub struct Smpc {
    /// Reset-disable flag (§0.4 step 4: `true` at power-on/reset). Gates
    /// OREG0 bit 6 and, later (Phase 3), whether the reset button is inert.
    pub resd: bool,
    /// Shadow of "whatever byte value is currently sitting on the shared
    /// internal SMPC data bus" (§1.3). Every byte write to any SMPC offset
    /// *other than SF itself* latches this; reading SF folds its own bit 0
    /// into it. Exists purely so SF's high 7 bits reflect real bus-hold
    /// behavior instead of always reading zero.
    bustmp: u8,
    /// SMPC internal scratch/backup memory (§7.3, `0x17` SETSMEM's target).
    /// Not persisted to disk -- lives only here, for the process's lifetime.
    smem: [u8; 4],
    /// Reported region (OREG9). `region::AUTODETECT` resolves to
    /// `region::JAPAN` at read time (§0.2's `SmpcRecheckRegion` no-CD
    /// fallback, matching what the pre-Phase-0 inline code already did).
    regionid: u8,
    /// OREG10 bit 6: real hardware's horizontal dot clock select
    /// (352 = `true` / 320 = `false`), flipped by CKCHG352/CKCHG320
    /// (Phase 3).
    dotsel: bool,
    /// OREG10 bit 3 (§5.6). No current write path -- Phase 3+ territory;
    /// kept as real, inspectable state rather than a hardcoded bit.
    mshnmi: bool,
    /// OREG10 bit 1 (§5.6). Flipped by SYSRES (Phase 3).
    sysres: bool,
    /// OREG10 bit 0 (§5.6). No current write path yet.
    sndres: bool,
    /// OREG11 bit 6 (§5.6). No current write path yet (CD block isn't
    /// integrated into the system at all -- see
    /// `docs/implementation-plans/cs2-cdblock.md`).
    cdres: bool,
    /// Where INTBACK's RTC bytes come from; see `ClockSource`.
    clock: ClockSource,
    /// Peripheral port 1.
    pub port1: std::sync::Arc<std::sync::Mutex<crate::peripheral::PeripheralState>>,
    /// Peripheral port 2.
    pub port2: std::sync::Arc<std::sync::Mutex<crate::peripheral::PeripheralState>>,
    /// Cached port 1 data for chunking continuation.
    pub snap1: Option<crate::peripheral::PortData>,
    /// Cached port 2 data for chunking continuation.
    pub snap2: Option<crate::peripheral::PortData>,
    /// Whether the next INTBACK peripheral fetch is the first in a sequence.
    pub first_peri: bool,
    /// Whether an INTBACK command is currently active.
    pub intback: bool,
    /// The pending command, its delay in microseconds, and whether it waits for line 207.
    pub pending_command: Option<(u8, u32, bool)>,
    /// Whether the pending command has met its dispatch criteria.
    pub dispatch_ready: bool,
}

impl Default for Smpc {
    fn default() -> Self {
        Self::new()
    }
}

impl Smpc {
    pub fn new() -> Self {
        Self::with_language(0)
    }

    /// Real hardware seeds `SMEM[3]` with the system's configured language
    /// ID at reset (§0.4 step 2) -- `SMEM[0..3]` stay zero regardless (that
    /// step's double-`memset` derivation: the *net* effect is one language
    /// byte, not four copies of it, which a plausible-looking
    /// reimplementation gets wrong). No locale-selection mechanism exists
    /// yet, so callers that care can pass a language ID directly; `new()`
    /// defaults to `0`.
    pub fn with_language(language_id: u8) -> Self {
        Self {
            resd: true,
            bustmp: 0,
            smem: [0, 0, 0, language_id],
            regionid: region::AUTODETECT,
            dotsel: false,
            mshnmi: false,
            sysres: false,
            sndres: false,
            cdres: false,
            clock: ClockSource::HostWallClock,
            // Port 1: a digital pad is connected by default (the common
            // single-player case). Port 2: genuinely disconnected --
            // `PeripheralState::default()` (`Disconnected`) is correct
            // there, but was wrongly also used for port 1 here at one
            // point, making the "connected" default port look
            // indistinguishable from the "nothing here" port to both the
            // INTBACK peripheral path and the DDR1 ID-nibble path.
            port1: std::sync::Arc::new(std::sync::Mutex::new(
                crate::peripheral::PeripheralState::Pad(crate::peripheral::PadState::default()),
            )),
            port2: std::sync::Arc::new(std::sync::Mutex::new(
                crate::peripheral::PeripheralState::default(),
            )),
            snap1: None,
            snap2: None,
            first_peri: false,
            intback: false,
            pending_command: None,
            dispatch_ready: false,
        }
    }

    /// Frontend-facing region override (OREG9 on the next INTBACK status
    /// call). `region::AUTODETECT` falls back to `region::JAPAN` (§0.2, no
    /// CD-based autodetection exists since the CD block isn't integrated at
    /// all yet).
    pub fn set_region(&mut self, regionid: u8) {
        self.regionid = regionid;
    }

    pub fn set_port_peripheral(
        &mut self,
        port: usize,
        kind: Option<crate::peripheral::PeripheralKind>,
    ) {
        use crate::peripheral::{
            GunState, KeyboardState, MissionStickState, MouseState, Pad3DState, PadState,
            PeripheralKind, PeripheralState, TwinSticksState, WheelState,
        };
        let state = match kind {
            None => PeripheralState::Disconnected,
            Some(PeripheralKind::Pad) => PeripheralState::Pad(PadState::default()),
            Some(PeripheralKind::Wheel) => PeripheralState::Wheel(WheelState::default()),
            Some(PeripheralKind::MissionStick) => {
                PeripheralState::MissionStick(MissionStickState::default())
            }
            Some(PeripheralKind::Pad3D) => PeripheralState::Pad3D(Pad3DState::default()),
            Some(PeripheralKind::TwinSticks) => {
                PeripheralState::TwinSticks(TwinSticksState::default())
            }
            Some(PeripheralKind::Gun) => PeripheralState::Gun(GunState::default()),
            Some(PeripheralKind::Keyboard) => PeripheralState::Keyboard(KeyboardState::default()),
            Some(PeripheralKind::Mouse) => PeripheralState::Mouse(MouseState::default()),
        };
        if port == 1 {
            *self.port1.lock().unwrap() = state;
        } else if port == 2 {
            *self.port2.lock().unwrap() = state;
        }
    }

    pub fn set_pad_state(&mut self, port: usize, state: crate::peripheral::PadState) {
        self.set_peripheral_state(port, crate::peripheral::PeripheralState::Pad(state));
    }

    /// General form of `set_pad_state` -- sets any connected peripheral's
    /// *live* state directly (button presses, axis values, mouse deltas...),
    /// as opposed to `set_port_peripheral`, which only changes *what kind*
    /// is connected (resetting it to that kind's idle default).
    pub fn set_peripheral_state(&mut self, port: usize, state: crate::peripheral::PeripheralState) {
        if port == 1 {
            *self.port1.lock().unwrap() = state;
        } else if port == 2 {
            *self.port2.lock().unwrap() = state;
        }
    }

    pub fn is_resd(&self) -> bool {
        self.resd
    }

    /// Pin the RTC to a fixed UNIX timestamp (UTC seconds) instead of the
    /// host wall clock -- for deterministic tests/replay. See `ClockSource`.
    pub fn set_clock(&mut self, clock: ClockSource) {
        self.clock = clock;
    }

    /// Called for every byte write to *any* SMPC offset (§1.3, `smpc.c:756`)
    /// -- except SF itself, which has its own dedicated write path (see
    /// `docs/implementation-plans/smpc-peripheral.md` Phase 1's derivation
    /// of the `sf_read_returns_bustmp_high_bits` test: a write to SF must
    /// not clobber `bustmp`, or a subsequent SF read couldn't recover the
    /// high bits a prior non-SF write latched).
    pub fn on_register_write(
        &mut self,
        off: usize,
        val: u8,
        old_val: u8,
        work_ram: &WorkRam,
    ) -> Option<(u32, bool)> {
        if off != reg::SF {
            self.bustmp = val;
        }

        if off == reg::IREG0 && self.intback {
            if val & 0x40 != 0 {
                // Break
                self.intback = false;
                work_ram.smpc_regs.write().unwrap()[reg::SR] &= 0x0F;
                self.snap1 = None;
                self.snap2 = None;
                return None;
            } else if val & 0x80 != 0 {
                // Continue
                return Some(self.arm_command(cmd::INTBACK, work_ram));
            }
        }

        // §6: the direct-access port. `write_byte` (`sh2.rs`) already stored
        // the raw written byte into `work_ram` unconditionally before
        // calling here -- every arm below that says "leave unchanged"
        // means overwriting that raw store back to `old_val`, matching real
        // hardware's actual PDR/DDR *response synthesis* (§6, opening
        // paragraph), not "whatever the game happened to write".
        match off {
            reg::PDR1 => self.write_pdr(1, val, old_val, work_ram),
            reg::PDR2 => self.write_pdr(2, val, old_val, work_ram),
            reg::DDR1 => {
                let port1 = *self.port1.lock().unwrap();
                if let Some(nibble) = Self::ddr_id_nibble(&port1) {
                    work_ram.smpc_regs.write().unwrap()[reg::PDR1] = nibble;
                }
            }
            reg::DDR2 => {
                // §10.1 #28: real hardware has no DDR2 handler at all --
                // Mimas deliberately keeps a symmetric one against port 2
                // (a pre-existing, documented divergence, not new to Phase 7).
                let port2 = *self.port2.lock().unwrap();
                if let Some(nibble) = Self::ddr_id_nibble(&port2) {
                    work_ram.smpc_regs.write().unwrap()[reg::PDR2] = nibble;
                }
            }
            reg::IOSEL => {
                // §6.4 [QUIRK]: stored only -- real hardware never reads it
                // either. The raw store `write_byte` already did is correct
                // as-is.
            }
            reg::EXLE => {
                // §6.4/§6.5: bit 0, combined with VDP2's `EXTEN & 0x200`,
                // latches the gun's X/Y into VDP2's HCNT/VCNT at V-Blank
                // OUT. Needs both a live gun-position input source and
                // VDP2's external-latch registers, neither of which exist
                // yet -- deliberately deferred (see `GunState`'s doc
                // comment). Stored only for now, same as `IOSEL`.
            }
            _ => {}
        }
        None
    }

    /// §6.2: PDR1 (`port == 1`) / PDR2 (`port == 2`) write. Dispatch is on
    /// the *currently stored* `DDR[n] & 0x7F` (the control method the game
    /// selected earlier), not on `val` itself -- see `on_register_write`'s
    /// own doc comment for why `old_val` matters here.
    fn write_pdr(&mut self, port: usize, val: u8, old_val: u8, work_ram: &WorkRam) {
        let (ddr_off, pdr_off, state) = if port == 1 {
            (reg::DDR1, reg::PDR1, *self.port1.lock().unwrap())
        } else {
            (reg::DDR2, reg::PDR2, *self.port2.lock().unwrap())
        };
        let ddr = work_ram.smpc_regs.read().unwrap()[ddr_off] & 0x7F;
        let write = |b: u8, work_ram: &WorkRam| {
            work_ram.smpc_regs.write().unwrap()[pdr_off] = b;
        };
        match ddr {
            0x00 => {
                // Only meaningful for a light gun: when the game floats all
                // seven lines high, the gun's trigger/start byte appears in
                // PDR (`data[2]` in real hardware's flat array == the
                // first data byte here). Otherwise PDR keeps whatever it
                // already held.
                if let crate::peripheral::PeripheralState::Gun(gun) = state {
                    if val & 0x7F == 0x7F {
                        let mut b = 0xFF;
                        if gun.trigger {
                            b &= !(1 << 4);
                        }
                        if gun.start {
                            b &= !(1 << 5);
                        }
                        write(b, work_ram);
                    } else {
                        write(old_val, work_ram);
                    }
                } else {
                    write(old_val, work_ram);
                }
            }
            0x40 if port == 1 => {
                // `do_th_mode` -- Mega Drive ID acquisition [HACK] (§6.2).
                // PDR2 has no equivalent case at all (§6.1's table).
                let pd = state.to_port_data();
                let (b2, b3) = (pd.data[0], pd.data[1]);
                let res = if val & 0x40 != 0 {
                    0x70 | (b3 & 0x0C)
                } else {
                    0x30 | ((b2 >> 4) & 0x0F)
                };
                write(res, work_ram);
            }
            0x60 => {
                let pd = state.to_port_data();
                let (b2, b3) = (pd.data[0], pd.data[1]);
                let res = match val & 0x60 {
                    0x60 => (val & 0x80) | 0x14 | (b3 & 0x08),
                    0x20 => (val & 0x80) | 0x10 | ((b2 >> 4) & 0x0F),
                    0x40 => (val & 0x80) | 0x10 | (b2 & 0x0F),
                    0x00 => (val & 0x80) | 0x10 | ((b3 >> 4) & 0x0F),
                    _ => unreachable!("val & 0x60 has exactly four values"),
                };
                write(res, work_ram);
            }
            _ => {
                // Unrecognised control method (§6.1) -- real hardware logs
                // and leaves PDR untouched.
                write(old_val, work_ram);
            }
        }
    }

    /// §6.3's DDR1/DDR2 ID-nibble table: `0xC` = Saturn digital pad or gun,
    /// `0x1` = analog-incapable-of-TH-ID pad-shaped device (3D pad,
    /// keyboard) or multi-tap, `0x0` = mouse, `0xF` = nothing connected.
    /// `None` means "real hardware logs an error and leaves PDR
    /// untouched" -- the wheel/mission-stick/twin-sticks row.
    fn ddr_id_nibble(state: &crate::peripheral::PeripheralState) -> Option<u8> {
        use crate::peripheral::PeripheralState::*;
        match state {
            Disconnected => Some(0x7F),
            Gun(_) => Some(0x7C),
            Pad(_) => Some(0x7C),
            Pad3D(_) | Keyboard(_) => Some(0x71),
            Mouse(_) => Some(0x70),
            Wheel(_) | MissionStick(_) | TwinSticks(_) => None,
        }
    }

    /// SF read (§1.3): `(bustmp & 0xFE) | (SF & 1)`, folding the stored SF
    /// bit into `bustmp` as a side effect of the read itself. Replaces the
    /// old hardcoded `0x00`.
    pub fn read_sf(&mut self, work_ram: &WorkRam) -> u8 {
        let sf_bit = work_ram.smpc_regs.read().unwrap()[reg::SF] & 1;
        self.bustmp = (self.bustmp & 0xFE) | sf_bit;
        self.bustmp
    }

    pub fn arm_command(&mut self, command: u8, work_ram: &WorkRam) -> (u32, bool) {
        let delay_us = if command == cmd::INTBACK {
            let ireg0 = work_ram.smpc_regs.read().unwrap()[reg::IREG0];
            let ireg1 = work_ram.smpc_regs.read().unwrap()[reg::IREG1];
            if self.intback {
                16000
            } else if ireg0 == 0x01 {
                250
            } else if ireg0 == 0x00 && (ireg1 & 0x08) != 0 {
                16000
            } else {
                1
            }
        } else {
            1
        };

        let wait_for_line = if command == cmd::INTBACK {
            let ireg0 = work_ram.smpc_regs.read().unwrap()[reg::IREG0];
            let ireg1 = work_ram.smpc_regs.read().unwrap()[reg::IREG1];
            if self.intback {
                true
            } else if ireg0 == 0x01 {
                false
            } else if ireg0 == 0x00 && (ireg1 & 0x08) != 0 {
                true
            } else {
                false
            }
        } else {
            false
        };

        work_ram.smpc_regs.write().unwrap()[reg::SF] = 1;
        self.pending_command = Some((command, delay_us, wait_for_line));
        self.dispatch_ready = false;
        (delay_us, wait_for_line)
    }

    pub fn mark_dispatch_ready(&mut self) {
        self.dispatch_ready = true;
    }

    pub fn is_dispatch_ready(&self) -> bool {
        self.dispatch_ready
    }

    pub fn execute_expired_command(&mut self, work_ram: &WorkRam) -> SmpcEffects {
        if let Some((cmd, _, _)) = self.pending_command {
            self.pending_command = None;
            // Without this, a stale `true` from the command that just
            // dispatched would make Core 7 try (harmlessly, but pointlessly)
            // to dispatch again on every later unrelated wake -- e.g. CS2's
            // `vblank_pending` -- until the *next* `arm_command` reset it.
            self.dispatch_ready = false;
            work_ram.smpc_regs.write().unwrap()[reg::SF] = 0;
            return self.execute_command(cmd, work_ram);
        }
        SmpcEffects::default()
    }

    /// Execute the command just latched into COMREG, mutating `work_ram`'s
    /// SMPC register file the way real hardware would and returning whatever
    /// needs to happen outside the SMPC itself.
    pub fn execute_command(&mut self, command: u8, work_ram: &WorkRam) -> SmpcEffects {
        let mut effects = SmpcEffects::default();
        match command {
            cmd::MSHON => {
                // §4.2: no inputs, no outputs, no OREG31. Real hardware turns
                // the master SH-2 on; there is no state in which Mimas's
                // master is off. Explicit accepted no-op, not a silent
                // fall-through, so it's distinguishable from an unrecognised
                // command in the log.
            }
            cmd::SSHON => {
                effects.start_slave = true;
            }
            cmd::SSHOFF => {
                // §4.4: real hardware fully *resets* the slave, not merely
                // halts it. `SmpcEffects::stop_slave` today only deactivates
                // it in `LockStepSync` (see `Sh2::apply_smpc_effects`) --
                // making a later SSHON re-enter `Sh2::reset()` requires
                // restructuring Core 1's thread body in `lib.rs` (it has no
                // outer re-park loop today, unlike Core 6's), which is
                // deliberately deferred out of this phase; tracked in
                // `docs/implementation-plans/smpc-peripheral.md` Phase 1.
                effects.stop_slave = true;
            }
            cmd::SNDON => {
                effects.sound_on = true;
                Self::echo_oreg31(work_ram, cmd::SNDON);
            }
            cmd::SNDOFF => {
                effects.sound_off = true;
                Self::echo_oreg31(work_ram, cmd::SNDOFF);
            }
            cmd::CDON | cmd::CDOFF => {
                // §4.7: accepted no-op; revisit once the CD block is
                // integrated into the system at all (it currently is not --
                // see `docs/implementation-plans/cs2-cdblock.md`). No OREG31
                // (§4.1).
            }
            cmd::SYSRES => {
                effects.system_reset = true;
            }
            cmd::CKCHG352 => {
                effects.clock_change = Some(true);
                effects.stop_slave = true; // §4.9 step 3
                self.dotsel = true;
                effects.nmi = true;
            }
            cmd::CKCHG320 => {
                effects.clock_change = Some(false);
                effects.stop_slave = true; // §4.10 step 3
                self.dotsel = false;
                effects.nmi = true;
            }
            cmd::INTBACK => {
                effects.system_manager_irq = true;
                let mut ram = work_ram.smpc_regs.write().unwrap();
                let ireg0 = ram[reg::IREG0];
                let ireg1 = ram[reg::IREG1];
                if ireg0 & 1 != 0 {
                    // Status path (§5.2, §5.6). `wants_peripheral` (bit 3 of
                    // IREG1) selects the low nibble of SR, per §5.3 --
                    // confirmed live against the real BIOS trace (§0.6 #4:
                    // IREG1 = 0x02 ⇒ SR = 0x4F).
                    let wants_peripheral = (ireg1 >> 3) & 1;
                    ram[reg::oreg(0)] = 0x80 | ((self.resd as u8) << 6);
                    let (year, month, day, hour, minute, second, weekday) = self.clock.now_utc();
                    ram[reg::oreg(1)] = ((year / 1000) << 4) as u8 | ((year % 1000) / 100) as u8;
                    ram[reg::oreg(2)] = (((year % 100) / 10) << 4) as u8 | (year % 10) as u8;
                    ram[reg::oreg(3)] = (weekday << 4) | month; // month: 1-12, NOT BCD (§5.6)
                    ram[reg::oreg(4)] = bcd(day);
                    ram[reg::oreg(5)] = bcd(hour);
                    ram[reg::oreg(6)] = bcd(minute);
                    ram[reg::oreg(7)] = bcd(second);
                    // Cartridge code: Yabause's own reference hardcodes 0
                    // with a "FIXME: random value" comment (§5.6) -- no real
                    // cartridge model exists here either, so 0 is the honest
                    // simplification, not a guess dressed up as a value.
                    ram[reg::oreg(8)] = 0;
                    ram[reg::oreg(9)] = if self.regionid == region::AUTODETECT {
                        region::JAPAN
                    } else {
                        self.regionid
                    };
                    ram[reg::oreg(10)] = 0x34
                        | ((self.dotsel as u8) << 6)
                        | ((self.mshnmi as u8) << 3)
                        | ((self.sysres as u8) << 1)
                        | (self.sndres as u8);
                    ram[reg::oreg(11)] = (self.cdres as u8) << 6;
                    for i in 0..4 {
                        ram[reg::oreg(12 + i)] = self.smem[i];
                    }
                    // OREG16-30: real hardware presumably has *some* defined
                    // meaning here, but §5.6 tags Yabause itself as leaving
                    // these 15 bytes stale garbage ([QUIRK] #20) -- since the
                    // real values aren't recoverable from this source,
                    // zeroing is the deliberate divergence over propagating
                    // whatever happened to be there before. Revisit only if
                    // a real BIOS/game is found to depend on a specific
                    // value here.
                    for i in 16..=30 {
                        ram[reg::oreg(i)] = 0;
                    }
                    ram[reg::oreg(31)] = cmd::INTBACK;
                    ram[reg::SR] = 0x4F | (wants_peripheral << 5);
                } else if (ireg1 >> 3) & 1 != 0 {
                    if ireg0 & 0x40 != 0 {
                        self.intback = false;
                        ram[reg::SR] &= 0x0F;
                        return effects;
                    }
                    if ireg0 == 0 {
                        self.intback = true;
                        self.first_peri = true;
                    }
                    let is_continuation = !self.first_peri;

                    let (mut p1_data, mut p2_data);
                    if !is_continuation {
                        let mut pad1 = self.port1.lock().unwrap();
                        let mut pad2 = self.port2.lock().unwrap();

                        p1_data = pad1.to_port_data();
                        p2_data = pad2.to_port_data();
                        // §5.4 step 2: `PerFlush` runs on the *live* port
                        // state immediately after the snapshot is taken, so
                        // the mouse's accumulated deltas don't carry over
                        // into the next accumulation period. Applied after
                        // snapshotting, not before -- this frame's already-
                        // captured deltas must still reach the OREGs.
                        pad1.flush_mouse_deltas();
                        pad2.flush_mouse_deltas();
                        self.snap1 = Some(p1_data);
                        self.snap2 = Some(p2_data);
                    } else {
                        p1_data = self.snap1.unwrap_or_default();
                        p2_data = self.snap2.unwrap_or_default();
                    }

                    Self::chunk_port_data(&mut p1_data, &mut p2_data, &mut ram);

                    self.snap1 = Some(p1_data);
                    self.snap2 = Some(p2_data);

                    // §5.3: the peripheral-chunk SR formula is its own,
                    // separate from the status-block path above -- 0xC0 for
                    // the first chunk of a sequence, 0x80 for every
                    // subsequent one, both OR'd with IREG1's own high
                    // nibble. There is deliberately no "more chunks remain"
                    // bit here (§5.3's own [QUIRK]) -- do not synthesize one
                    // by checking `offset < size`; real hardware genuinely
                    // never signals exhaustion, and the game is expected to
                    // derive it from the port-status/size bytes instead.
                    ram[reg::SR] = (if self.first_peri { 0xC0 } else { 0x80 }) | (ireg1 >> 4);
                    self.first_peri = false;
                }
            }
            cmd::SETSMEM => {
                // §4.12: IREG0..IREG3 -> SMEM[0..3], no validity check. SMEM
                // isn't persisted to disk (§7.3) -- it lives only in this
                // `Smpc` for the process's lifetime.
                let ram = work_ram.smpc_regs.read().unwrap();
                let smem = [
                    ram[reg::IREG0],
                    ram[reg::IREG1],
                    ram[reg::IREG2],
                    ram[reg::IREG3],
                ];
                drop(ram);
                self.smem = smem;
                Self::echo_oreg31(work_ram, cmd::SETSMEM);
            }
            cmd::NMIREQ => {
                effects.nmi = true;
                Self::echo_oreg31(work_ram, cmd::NMIREQ);
            }
            cmd::RESENAB => {
                self.resd = false;
                Self::echo_oreg31(work_ram, cmd::RESENAB);
            }
            cmd::RESDISA => {
                self.resd = true;
                Self::echo_oreg31(work_ram, cmd::RESDISA);
            }
            _ => {
                // Unrecognised COMREG (§2.2, `smpc.c:727`): no dispatch, no
                // OREG touched -- only the unconditional SF clear below
                // applies.
            }
        }
        // §2.2 (`smpc.c:628`): every dispatched command, recognised or not,
        // clears SF once finished. This is what actually lets the real
        // BIOS's SF-polling loop (§0.6, BIOS `0x1D60-0x1D64`) exit --
        // splitting this from the SF-read fix above would hang it.
        work_ram.smpc_regs.write().unwrap()[reg::SF] = 0;
        effects
    }

    /// §5.4/§5.5's 32-byte-at-a-time chunker. The OREG stream is simply
    /// port 1's own status byte, then (if connected) its ID byte and data
    /// bytes, then port 2's status byte and the same -- confirmed against
    /// §5.5's own worked examples (`F1 02 FF FF | F0` for one idle pad on
    /// port 1, nothing on port 2). There is deliberately **no** separate
    /// combined header byte anywhere in this stream; an earlier version of
    /// this code invented one (`0xF0` fixed + a `(p1.size<<4|p2.size)`
    /// byte) that has no basis in the real format and does not appear in
    /// any of §5.5's worked examples.
    fn chunk_port_data(
        p1: &mut crate::peripheral::PortData,
        p2: &mut crate::peripheral::PortData,
        ram: &mut [u8; 128],
    ) {
        let mut oreg_idx = 0;

        // Port 1: status byte, then (for a real single peripheral -- not
        // "nothing connected" and not a gun, which has no ID byte at all)
        // its ID byte, then its data bytes.
        if p1.offset == 0 && oreg_idx < 32 {
            ram[reg::oreg(oreg_idx)] = p1.status;
            oreg_idx += 1;
            if p1.status != crate::peripheral::status::NOT_CONNECTED
                && p1.status != crate::peripheral::status::GUN_DIRECT
                && oreg_idx < 32
            {
                ram[reg::oreg(oreg_idx)] = p1.id;
                oreg_idx += 1;
            }
        }

        while p1.offset < p1.size && oreg_idx < 32 {
            ram[reg::oreg(oreg_idx)] = p1.data[p1.offset];
            oreg_idx += 1;
            p1.offset += 1;
        }

        // Port 2: same shape.
        if p2.offset == 0 && oreg_idx < 32 {
            ram[reg::oreg(oreg_idx)] = p2.status;
            oreg_idx += 1;
            if p2.status != crate::peripheral::status::NOT_CONNECTED
                && p2.status != crate::peripheral::status::GUN_DIRECT
                && oreg_idx < 32
            {
                ram[reg::oreg(oreg_idx)] = p2.id;
                oreg_idx += 1;
            }
        }

        while p2.offset < p2.size && oreg_idx < 32 {
            ram[reg::oreg(oreg_idx)] = p2.data[p2.offset];
            oreg_idx += 1;
            p2.offset += 1;
        }
    }

    fn echo_oreg31(work_ram: &WorkRam, command: u8) {
        work_ram.smpc_regs.write().unwrap()[reg::oreg(31)] = command;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn execute_smpc(cpu: &mut crate::sh2::Sh2, work_ram: &crate::shared_buffers::WorkRam) {
        let effects = cpu
            .smpc
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .execute_expired_command(work_ram);
        cpu.apply_smpc_effects(effects);
    }

    use std::sync::Arc;

    #[test]
    fn fresh_register_view_is_all_zero() {
        let work_ram = WorkRam::new();
        let ram = work_ram.smpc_regs.read().unwrap();
        assert!(
            ram.iter().all(|&b| b == 0),
            "SmpcReset zeroes all 64 real registers"
        );
    }

    /// Wiring proof: a `Sh2` with a real `Smpc` attached (mirroring exactly
    /// how `SaturnSystem::start` wires Core 0) must route COMREG writes
    /// through `Smpc::execute_command`, not the old inline fallback -- and
    /// therefore see the Phase 1-correct values, not the historical ones.
    fn wired_cpu() -> (crate::sh2::Sh2, Arc<WorkRam>, Arc<std::sync::Mutex<Smpc>>) {
        use crate::bus_arbiter::BusArbiter;
        use crate::sh2::Sh2;

        let arbiter = Arc::new(BusArbiter::new());
        let work_ram = Arc::new(WorkRam::new());
        let smpc = Arc::new(std::sync::Mutex::new(Smpc::new()));
        let mut cpu = Sh2::new(false, arbiter, work_ram.clone());
        cpu.smpc = Some(smpc.clone());
        (cpu, work_ram, smpc)
    }

    const SMPC_BASE: u32 = 0x0010_0000;

    #[test]
    fn oreg0_reports_reset_disable_state() {
        // §5.6 x §0.4 step 4 x §4.14/§4.15: 0x80 | (resd << 6), resd = true
        // at reset. IREG0 bit 0 must be set to request the status block
        // (§5.2) -- otherwise INTBACK is the genuine `smpc.c:499` no-op and
        // OREG0 is never touched at all.
        let (mut cpu, work_ram, _smpc) = wired_cpu();
        cpu.write_byte(SMPC_BASE + reg::IREG0 as u32, 0x01);
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::INTBACK);
        execute_smpc(&mut cpu, &work_ram);
        assert_eq!(
            work_ram.smpc_regs.read().unwrap()[reg::oreg(0)],
            0xC0,
            "fresh resd=true"
        );

        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::RESENAB);
        execute_smpc(&mut cpu, &work_ram);
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::INTBACK);
        execute_smpc(&mut cpu, &work_ram);
        assert_eq!(
            work_ram.smpc_regs.read().unwrap()[reg::oreg(0)],
            0x80,
            "after RESENAB"
        );

        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::RESDISA);
        execute_smpc(&mut cpu, &work_ram);
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::INTBACK);
        execute_smpc(&mut cpu, &work_ram);
        assert_eq!(
            work_ram.smpc_regs.read().unwrap()[reg::oreg(0)],
            0xC0,
            "after RESDISA"
        );
    }

    #[test]
    fn intback_status_sr_tracks_ireg1_bit3() {
        // §5.3: 0x4F | (intback << 5), where intback = (IREG1 >> 3) & 1.
        let (mut cpu, work_ram, _smpc) = wired_cpu();
        cpu.write_byte(SMPC_BASE + reg::IREG1 as u32, 0x02);
        cpu.write_byte(SMPC_BASE + reg::IREG0 as u32, 0x01);
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::INTBACK);
        execute_smpc(&mut cpu, &work_ram);
        assert_eq!(
            work_ram.smpc_regs.read().unwrap()[reg::SR],
            0x4F,
            "IREG1=0x02 -> SR=0x4F"
        );

        cpu.write_byte(SMPC_BASE + reg::IREG1 as u32, 0x0A);
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::INTBACK);
        execute_smpc(&mut cpu, &work_ram);
        assert_eq!(
            work_ram.smpc_regs.read().unwrap()[reg::SR],
            0x6F,
            "IREG1=0x0A -> SR=0x6F"
        );
    }

    #[test]
    fn oreg31_echo_matrix() {
        // §4.1: echoed for exactly these seven; left untouched for everyone
        // else (pre-seeded with a sentinel that must survive).
        let echoed = [
            cmd::SNDON,
            cmd::SNDOFF,
            cmd::INTBACK,
            cmd::SETSMEM,
            cmd::NMIREQ,
            cmd::RESENAB,
            cmd::RESDISA,
        ];
        let not_echoed = [
            cmd::SSHON,
            cmd::SSHOFF,
            cmd::CKCHG352,
            cmd::CKCHG320,
            cmd::MSHON,
            cmd::CDON,
            cmd::CDOFF,
            cmd::SYSRES,
        ];

        for &command in &echoed {
            let (mut cpu, work_ram, _smpc) = wired_cpu();
            // INTBACK only echoes on the status path (§5.2) -- harmless for
            // every other command in this list.
            cpu.write_byte(SMPC_BASE + reg::IREG0 as u32, 0x01);
            cpu.write_byte(SMPC_BASE + reg::COMREG as u32, command);
            execute_smpc(&mut cpu, &work_ram);
            assert_eq!(
                work_ram.smpc_regs.read().unwrap()[reg::oreg(31)],
                command,
                "command {:#04X} must echo into OREG31",
                command
            );
        }
        for &command in &not_echoed {
            let (mut cpu, work_ram, _smpc) = wired_cpu();
            work_ram.smpc_regs.write().unwrap()[reg::oreg(31)] = 0x5A; // sentinel
            cpu.write_byte(SMPC_BASE + reg::COMREG as u32, command);
            execute_smpc(&mut cpu, &work_ram);
            assert_eq!(
                work_ram.smpc_regs.read().unwrap()[reg::oreg(31)],
                0x5A,
                "command {:#04X} must not touch OREG31",
                command
            );
        }
    }

    #[test]
    fn sf_handshake_matches_real_bios_sequence() {
        // Replays the real BIOS's own INTBACK handshake byte for byte (§0.6,
        // BIOS 0x1D48-0x1D64) -- the exact loop that hangs the machine if
        // "clear SF after dispatch" is missing.
        let (mut cpu, work_ram, _smpc) = wired_cpu();
        cpu.write_byte(SMPC_BASE + reg::SF as u32, 0x01);
        cpu.write_byte(SMPC_BASE + reg::IREG0 as u32, 0x01);
        cpu.write_byte(SMPC_BASE + reg::IREG1 as u32, 0x02);
        cpu.write_byte(SMPC_BASE + reg::IREG2 as u32, 0xF0);
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::INTBACK);
        execute_smpc(&mut cpu, &work_ram);
        let sf = cpu.read_byte(SMPC_BASE + reg::SF as u32);
        assert_eq!(
            sf & 1,
            0,
            "SF bit 0 must clear once INTBACK finishes, or the real BIOS spins forever"
        );
    }

    #[test]
    fn sf_read_returns_bustmp_high_bits() {
        // §1.3: write 0xF0 to a padding offset -> bustmp = 0xF0. Write 0x01
        // to SF (must NOT clobber bustmp, only SF's own storage). Read SF ->
        // (0xF0 & 0xFE) | 1 == 0xF1.
        let (mut cpu, _work_ram, _smpc) = wired_cpu();
        cpu.write_byte(SMPC_BASE + 0x0F, 0xF0); // padding offset
        cpu.write_byte(SMPC_BASE + reg::SF as u32, 0x01);
        assert_eq!(cpu.read_byte(SMPC_BASE + reg::SF as u32), 0xF1);
    }

    #[test]
    fn unrecognised_comreg_clears_sf_without_dispatching() {
        // §2.2: an unrecognised command clears SF immediately without
        // dispatching -- OREG0 (pre-seeded) must be untouched.
        let (mut cpu, work_ram, _smpc) = wired_cpu();
        work_ram.smpc_regs.write().unwrap()[reg::oreg(0)] = 0x5A; // sentinel
        cpu.write_byte(SMPC_BASE + reg::SF as u32, 0x01);
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, 0x05);
        execute_smpc(&mut cpu, &work_ram); // not in §3.4's table
        assert_eq!(
            cpu.read_byte(SMPC_BASE + reg::SF as u32) & 1,
            0,
            "SF must clear"
        );
        assert_eq!(
            work_ram.smpc_regs.read().unwrap()[reg::oreg(0)],
            0x5A,
            "OREG0 must be untouched"
        );
    }

    fn wired_cpu_with_clock(clock: ClockSource) -> (crate::sh2::Sh2, Arc<WorkRam>) {
        use crate::bus_arbiter::BusArbiter;
        use crate::sh2::Sh2;

        let arbiter = Arc::new(BusArbiter::new());
        let work_ram = Arc::new(WorkRam::new());
        let mut smpc = Smpc::new();
        smpc.set_clock(clock);
        let smpc = Arc::new(std::sync::Mutex::new(smpc));
        let mut cpu = Sh2::new(false, arbiter, work_ram.clone());
        cpu.smpc = Some(smpc);
        (cpu, work_ram)
    }

    fn do_intback_status(cpu: &mut crate::sh2::Sh2, work_ram: &crate::shared_buffers::WorkRam) {
        cpu.write_byte(SMPC_BASE + reg::IREG0 as u32, 0x01);
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::INTBACK);
        execute_smpc(cpu, work_ram);
    }

    #[test]
    fn intback_rtc_bcd_layout() {
        // 2000-01-01 00:00:00 UTC, a Saturday -- hand-derived from §5.6's
        // formulas (see docs/implementation-plans/smpc-peripheral.md
        // Phase 2 testing).
        let (mut cpu, work_ram) = wired_cpu_with_clock(ClockSource::Fixed(946_684_800));
        do_intback_status(&mut cpu, &work_ram);
        {
            let ram = work_ram.smpc_regs.read().unwrap();
            assert_eq!(ram[reg::oreg(1)], 0x20, "OREG1 (year thousands/hundreds)");
            assert_eq!(ram[reg::oreg(2)], 0x00, "OREG2 (year tens/units)");
            assert_eq!(
                ram[reg::oreg(3)],
                0x61,
                "OREG3 (weekday<<4 | month), Saturday=6, Jan=1"
            );
            assert_eq!(ram[reg::oreg(4)], 0x01, "OREG4 (day BCD)");
            assert_eq!(ram[reg::oreg(5)], 0x00, "OREG5 (hour BCD)");
            assert_eq!(ram[reg::oreg(6)], 0x00, "OREG6 (minute BCD)");
            assert_eq!(ram[reg::oreg(7)], 0x00, "OREG7 (second BCD)");
        }

        // 2001-12-25 13:45:59 UTC -- independently hand-derived unix
        // timestamp (1_009_287_959: 11681 days since epoch x 86400, plus
        // 49559 seconds-of-day), cross-checked two ways: the weekday it
        // implies (Tuesday) matches the historical record for that date,
        // and every BCD byte below matches what
        // docs/implementation-plans/smpc-peripheral.md Phase 2 independently
        // states the formulas must produce. Exercises the awkward cases:
        // month >= 10 (nibble 0xC, NOT BCD-carried) and two-digit
        // day/hour/minute/second.
        let (mut cpu, work_ram) = wired_cpu_with_clock(ClockSource::Fixed(1_009_287_959));
        do_intback_status(&mut cpu, &work_ram);
        {
            let ram = work_ram.smpc_regs.read().unwrap();
            assert_eq!(
                ram[reg::oreg(3)] & 0x0F,
                0x0C,
                "OREG3 low nibble: December is nibble 0xC, not BCD"
            );
            assert_eq!(
                ram[reg::oreg(3)] >> 4,
                0x2,
                "OREG3 high nibble: Tuesday = 2"
            );
            assert_eq!(ram[reg::oreg(4)], 0x25, "OREG4 (day 25, BCD)");
            assert_eq!(ram[reg::oreg(5)], 0x13, "OREG5 (hour 13, BCD)");
            assert_eq!(ram[reg::oreg(6)], 0x45, "OREG6 (minute 45, BCD)");
            assert_eq!(ram[reg::oreg(7)], 0x59, "OREG7 (second 59, BCD)");
        }
    }

    #[test]
    fn oreg10_encodes_dot_clock() {
        let (mut cpu, work_ram) = wired_cpu_with_clock(ClockSource::Fixed(0));
        do_intback_status(&mut cpu, &work_ram);
        assert_eq!(
            work_ram.smpc_regs.read().unwrap()[reg::oreg(10)],
            0x34,
            "all flags clear"
        );
        // No CKCHG352 command exists yet (Phase 3) to flip `dotsel` through
        // the public command path -- this pins the OREG10 formula itself
        // against a directly-constructed `Smpc` instead.
        let mut smpc = Smpc::new();
        smpc.dotsel = true;
        let work_ram2 = WorkRam::new();
        work_ram2.smpc_regs.write().unwrap()[reg::IREG0] = 0x01;
        smpc.execute_command(cmd::INTBACK, &work_ram2);
        assert_eq!(
            work_ram2.smpc_regs.read().unwrap()[reg::oreg(10)],
            0x74,
            "dotsel=true"
        );
    }

    #[test]
    fn setsmem_round_trips_through_oreg12_15() {
        let (mut cpu, work_ram) = wired_cpu_with_clock(ClockSource::Fixed(0));
        cpu.write_byte(SMPC_BASE + reg::IREG0 as u32, 0xDE);
        cpu.write_byte(SMPC_BASE + reg::IREG1 as u32, 0xAD);
        cpu.write_byte(SMPC_BASE + reg::IREG2 as u32, 0xBE);
        cpu.write_byte(SMPC_BASE + reg::IREG3 as u32, 0xEF);
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::SETSMEM);
        execute_smpc(&mut cpu, &work_ram);
        assert_eq!(
            work_ram.smpc_regs.read().unwrap()[reg::oreg(31)],
            cmd::SETSMEM
        );

        // IREG0's SETSMEM value (0xDE) would otherwise look like a status
        // request on the next INTBACK (bit 0 set) -- clear it first so this
        // test isolates the SMEM round-trip.
        cpu.write_byte(SMPC_BASE + reg::IREG0 as u32, 0x01);
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::INTBACK);
        execute_smpc(&mut cpu, &work_ram);
        let ram = work_ram.smpc_regs.read().unwrap();
        assert_eq!(ram[reg::oreg(12)], 0xDE, "OREG12");
        assert_eq!(ram[reg::oreg(13)], 0xAD, "OREG13");
        assert_eq!(ram[reg::oreg(14)], 0xBE, "OREG14");
        assert_eq!(ram[reg::oreg(15)], 0xEF, "OREG15");
    }

    #[test]
    fn smem_reset_state() {
        // §0.4 step 2: SMEM = [0, 0, 0, language_id] -- not four copies of
        // the language id, which a plausible-looking reimplementation
        // (naively memset-ing the whole array) gets wrong.
        let smpc = Smpc::with_language(5); // Japanese
        assert_eq!(smpc.smem, [0x00, 0x00, 0x00, 0x05]);
    }

    #[test]
    fn region_reported_in_oreg9() {
        let regions = [
            region::JAPAN,
            region::ASIA_NTSC,
            region::NORTH_AMERICA,
            region::CENTRAL_SOUTH_AMERICA_NTSC,
            region::KOREA,
            region::ASIA_PAL,
            region::EUROPE,
            region::CENTRAL_SOUTH_AMERICA_PAL,
        ];
        for &r in &regions {
            let (mut cpu, work_ram) = wired_cpu_with_clock(ClockSource::Fixed(0));
            // Reach into the wired-in Smpc via the CPU to set the region --
            // there's no COMREG-level "set region" on real hardware either
            // (§0.2: it's autodetected from the CD, or configured out of
            // band), so this is a direct API call, not a register write.
            cpu.smpc.as_ref().unwrap().lock().unwrap().set_region(r);
            do_intback_status(&mut cpu, &work_ram);
            assert_eq!(
                work_ram.smpc_regs.read().unwrap()[reg::oreg(9)],
                r,
                "region {:#04X}",
                r
            );
        }

        // AUTODETECT with no CD present (the only state this codebase can
        // be in -- the CD block isn't integrated at all) falls back to
        // JAPAN, matching what the pre-Phase-0 inline code already did.
        let (mut cpu, work_ram) = wired_cpu_with_clock(ClockSource::Fixed(0));
        cpu.smpc
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .set_region(region::AUTODETECT);
        do_intback_status(&mut cpu, &work_ram);
        assert_eq!(
            work_ram.smpc_regs.read().unwrap()[reg::oreg(9)],
            region::JAPAN,
            "AUTODETECT -> JAPAN fallback"
        );
    }

    // ---- Restored/extended for Phase 7: PDR/DDR direct-access port,
    // CKCHG, port defaults, and INTBACK peripheral chunking against the
    // real §5.5 byte stream (no fabricated header). ----

    #[test]
    fn port1_defaults_to_a_connected_pad_port2_to_disconnected() {
        let smpc = Smpc::new();
        assert_eq!(
            *smpc.port1.lock().unwrap(),
            crate::peripheral::PeripheralState::Pad(crate::peripheral::PadState::default()),
            "port 1 defaults to a connected, idle pad"
        );
        assert_eq!(
            *smpc.port2.lock().unwrap(),
            crate::peripheral::PeripheralState::Disconnected,
            "port 2 must default to disconnected, not a phantom idle pad"
        );
    }

    #[test]
    fn set_port_peripheral_and_set_peripheral_state_both_reach_ddr_and_intback() {
        use crate::peripheral::{PeripheralKind, PeripheralState};
        let mut smpc = Smpc::new();
        let work_ram = WorkRam::new();

        smpc.set_port_peripheral(2, Some(PeripheralKind::Mouse));
        assert_eq!(
            *smpc.port2.lock().unwrap(),
            PeripheralState::Mouse(crate::peripheral::MouseState::default())
        );
        let old = work_ram.smpc_regs.read().unwrap()[reg::DDR2];
        smpc.on_register_write(reg::DDR2, 0x00, old, &work_ram);
        assert_eq!(
            work_ram.smpc_regs.read().unwrap()[reg::PDR2],
            0x70,
            "mouse's own DDR nibble must show up on port 2 too"
        );

        smpc.set_port_peripheral(2, None);
        let old = work_ram.smpc_regs.read().unwrap()[reg::DDR2];
        smpc.on_register_write(reg::DDR2, 0x00, old, &work_ram);
        assert_eq!(work_ram.smpc_regs.read().unwrap()[reg::PDR2], 0x7F);
    }

    #[test]
    fn ckchg352_sets_dotsel_and_ckchg320_clears_it() {
        let (mut cpu, work_ram) = wired_cpu_with_clock(ClockSource::Fixed(0));
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::CKCHG352);
        execute_smpc(&mut cpu, &work_ram);
        do_intback_status(&mut cpu, &work_ram);
        assert_eq!(
            work_ram.smpc_regs.read().unwrap()[reg::oreg(10)] & 0x40,
            0x40,
            "OREG10 bit 6 (dotsel) must be set after CKCHG352"
        );

        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::CKCHG320);
        execute_smpc(&mut cpu, &work_ram);
        do_intback_status(&mut cpu, &work_ram);
        assert_eq!(
            work_ram.smpc_regs.read().unwrap()[reg::oreg(10)] & 0x40,
            0,
            "OREG10 bit 6 must clear after CKCHG320"
        );
    }

    #[test]
    fn pdr1_four_phase_idle_pad() {
        // §6.2 mode 0x60, idle pad: b2 = b3 = 0xFF, hand-derived per phase.
        let mut smpc = Smpc::new();
        let work_ram = WorkRam::new();
        work_ram.smpc_regs.write().unwrap()[reg::DDR1] = 0x60;

        let cases: &[(u8, u8)] = &[
            (0x60, 0x1C), // 1st Data: 0x14 | (0xFF & 0x08)
            (0x20, 0x1F), // 2nd Data: 0x10 | ((0xFF >> 4) & 0xF)
            (0x40, 0x1F), // 3rd Data: 0x10 | (0xFF & 0xF)
            (0x00, 0x1F), // 4th Data: 0x10 | ((0xFF >> 4) & 0xF)
        ];
        for &(val, expected) in cases {
            let old = work_ram.smpc_regs.read().unwrap()[reg::PDR1];
            smpc.on_register_write(reg::PDR1, val, old, &work_ram);
            assert_eq!(
                work_ram.smpc_regs.read().unwrap()[reg::PDR1],
                expected,
                "phase select {val:#04x}"
            );
        }
    }

    #[test]
    fn pdr1_four_phase_a_and_right() {
        // A + Right pressed: b2 = 0xFF & !(1<<7) & !(1<<2) = 0x7B, b3 = 0xFF.
        let mut smpc = Smpc::new();
        let work_ram = WorkRam::new();
        let mut pad = crate::peripheral::PadState::default();
        pad.a = true;
        pad.right = true;
        smpc.set_pad_state(1, pad);
        work_ram.smpc_regs.write().unwrap()[reg::DDR1] = 0x60;

        let cases: &[(u8, u8)] = &[
            (0x60, 0x1C), // 0x14 | (0xFF & 0x08)
            (0x20, 0x17), // 0x10 | ((0x7B >> 4) & 0xF) = 0x10 | 0x07
            (0x40, 0x1B), // 0x10 | (0x7B & 0xF) = 0x10 | 0x0B
            (0x00, 0x1F), // 0x10 | ((0xFF >> 4) & 0xF)
        ];
        for &(val, expected) in cases {
            let old = work_ram.smpc_regs.read().unwrap()[reg::PDR1];
            smpc.on_register_write(reg::PDR1, val, old, &work_ram);
            assert_eq!(
                work_ram.smpc_regs.read().unwrap()[reg::PDR1],
                expected,
                "phase select {val:#04x}"
            );
        }
    }

    #[test]
    fn pdr1_preserves_written_bit7() {
        let mut smpc = Smpc::new();
        let work_ram = WorkRam::new();
        work_ram.smpc_regs.write().unwrap()[reg::DDR1] = 0x60;
        let old = work_ram.smpc_regs.read().unwrap()[reg::PDR1];
        smpc.on_register_write(reg::PDR1, 0x80 | 0x60, old, &work_ram); // bit7 set + phase 1
        assert_eq!(
            work_ram.smpc_regs.read().unwrap()[reg::PDR1],
            0x80 | 0x1C,
            "the written bit 7 must survive into the result"
        );
    }

    #[test]
    fn pdr1_gun_button_read_at_mode_0x00() {
        // §6.2 mode 0x00: only meaningful for a gun, only when all seven
        // lines are floated high (`val & 0x7F == 0x7F`).
        let mut smpc = Smpc::new();
        let work_ram = WorkRam::new();
        let mut gun = crate::peripheral::GunState::default();
        gun.trigger = true;
        smpc.set_peripheral_state(1, crate::peripheral::PeripheralState::Gun(gun));
        work_ram.smpc_regs.write().unwrap()[reg::DDR1] = 0x00;

        let old = work_ram.smpc_regs.read().unwrap()[reg::PDR1];
        smpc.on_register_write(reg::PDR1, 0x7F, old, &work_ram);
        assert_eq!(
            work_ram.smpc_regs.read().unwrap()[reg::PDR1],
            0xFF & !(1 << 4),
            "trigger pressed must clear bit 4"
        );
    }

    #[test]
    fn unknown_control_method_leaves_pdr_untouched() {
        let mut smpc = Smpc::new();
        let work_ram = WorkRam::new();
        work_ram.smpc_regs.write().unwrap()[reg::DDR1] = 0x20; // not 0x00/0x40/0x60
        work_ram.smpc_regs.write().unwrap()[reg::PDR1] = 0xAA;

        let old = work_ram.smpc_regs.read().unwrap()[reg::PDR1];
        smpc.on_register_write(reg::PDR1, 0x55, old, &work_ram);
        assert_eq!(
            work_ram.smpc_regs.read().unwrap()[reg::PDR1],
            0xAA,
            "an unrecognised control method must leave PDR1 exactly as it was"
        );
    }

    #[test]
    fn intback_peripheral_one_pad_port1_matches_the_worked_example() {
        // §5.5's own worked example, verbatim: "One digital pad on port 1,
        // nothing on port 2: F1 02 FF FF | F0" -- no header of any kind.
        let (mut cpu, work_ram) = wired_cpu_with_clock(ClockSource::Fixed(0));
        cpu.write_byte(SMPC_BASE + reg::IREG1 as u32, 0x08); // peripheral data wanted
        cpu.write_byte(SMPC_BASE + reg::IREG0 as u32, 0x00); // peripheral-only path
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::INTBACK);
        execute_smpc(&mut cpu, &work_ram);

        let ram = work_ram.smpc_regs.read().unwrap();
        assert_eq!(ram[reg::oreg(0)], 0xF1);
        assert_eq!(ram[reg::oreg(1)], 0x02);
        assert_eq!(ram[reg::oreg(2)], 0xFF);
        assert_eq!(ram[reg::oreg(3)], 0xFF);
        assert_eq!(ram[reg::oreg(4)], 0xF0);
    }

    #[test]
    fn intback_peripheral_both_ports_empty_matches_the_worked_example() {
        // §5.5: "Nothing on either port: F0 | F0 -- OREG0 = 0xF0, OREG1 = 0xF0."
        let (mut cpu, work_ram) = wired_cpu_with_clock(ClockSource::Fixed(0));
        cpu.smpc
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .set_port_peripheral(1, None);
        cpu.write_byte(SMPC_BASE + reg::IREG1 as u32, 0x08);
        cpu.write_byte(SMPC_BASE + reg::IREG0 as u32, 0x00);
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::INTBACK);
        execute_smpc(&mut cpu, &work_ram);

        let ram = work_ram.smpc_regs.read().unwrap();
        assert_eq!(ram[reg::oreg(0)], 0xF0);
        assert_eq!(ram[reg::oreg(1)], 0xF0);
    }

    #[test]
    fn intback_peripheral_sr_first_vs_subsequent() {
        // §5.3: SR's high nibble is 0xC on the first chunk of a sequence,
        // 0x8 on every later one (0x40's low bits come from IREG1's own
        // high nibble, held at 0 here to isolate the bit under test).
        let (mut cpu, work_ram) = wired_cpu_with_clock(ClockSource::Fixed(0));
        cpu.write_byte(SMPC_BASE + reg::IREG1 as u32, 0x08);
        cpu.write_byte(SMPC_BASE + reg::IREG0 as u32, 0x00);
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::INTBACK);
        execute_smpc(&mut cpu, &work_ram);
        assert_eq!(
            work_ram.smpc_regs.read().unwrap()[reg::SR] & 0xC0,
            0xC0,
            "first chunk: bit7|bit6 set"
        );

        // Continue: IREG0 bit 7 set.
        cpu.write_byte(SMPC_BASE + reg::IREG0 as u32, 0x80);
        execute_smpc(&mut cpu, &work_ram);
        assert_eq!(
            work_ram.smpc_regs.read().unwrap()[reg::SR] & 0xC0,
            0x80,
            "subsequent chunk: only bit7 set"
        );
    }

    #[test]
    fn intback_break_clears_sr_high_nibble_and_drops_the_snapshot() {
        let (mut cpu, work_ram) = wired_cpu_with_clock(ClockSource::Fixed(0));
        cpu.write_byte(SMPC_BASE + reg::IREG1 as u32, 0x08);
        cpu.write_byte(SMPC_BASE + reg::IREG0 as u32, 0x00);
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::INTBACK);
        execute_smpc(&mut cpu, &work_ram);

        // Break: IREG0 bit 6 set.
        cpu.write_byte(SMPC_BASE + reg::IREG0 as u32, 0x40);
        assert_eq!(
            work_ram.smpc_regs.read().unwrap()[reg::SR] & 0xF0,
            0,
            "break must clear SR's high nibble immediately (synchronous, no dispatch needed)"
        );
        assert!(
            cpu.smpc.as_ref().unwrap().lock().unwrap().snap1.is_none(),
            "the in-progress snapshot must be dropped, not resumed by a later INTBACK"
        );
    }
}
