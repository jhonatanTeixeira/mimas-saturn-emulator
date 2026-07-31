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
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmpcEffects {
    /// SSHON §4.3.
    pub start_slave: bool,
    /// SSHOFF §4.4 -- real hardware fully resets the slave, not merely halts
    /// it (see `docs/implementation-plans/smpc-peripheral.md` Phase 1).
    pub stop_slave: bool,
    /// SNDON §4.5.
    pub sound_on: bool,
    /// SNDOFF §4.6.
    pub sound_off: bool,
    /// Real hardware fires the SCU "System Manager" interrupt (vector 0x47,
    /// level 8) when a command completes -- today, only INTBACK requests it
    /// (§2.3).
    pub system_manager_irq: bool,
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
    resd: bool,
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
            clock: ClockSource::default(),
        }
    }

    /// Frontend-facing region override (OREG9 on the next INTBACK status
    /// call). `region::AUTODETECT` falls back to `region::JAPAN` (§0.2, no
    /// CD-based autodetection exists since the CD block isn't integrated at
    /// all yet).
    pub fn set_region(&mut self, regionid: u8) {
        self.regionid = regionid;
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
    pub fn on_register_write(&mut self, off: usize, val: u8) {
        if off != reg::SF {
            self.bustmp = val;
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
                // §4.8: Yabause's reference does nothing here either. Real
                // hardware performs a full system reset -- deferred to
                // Phase 3 (`SmpcEffects::system_reset` doesn't exist yet).
                // No OREG31 (§4.1).
            }
            cmd::CKCHG352 | cmd::CKCHG320 => {
                // §4.9/§4.10: VDP/SCU/SCSP reset + slave stop + clock change
                // + NMI, deferred to Phase 3. No OREG31 (§4.1).
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
                    // Peripheral-only path (IREG0&1==0, IREG1&8!=0): real
                    // hardware returns a peripheral report here
                    // (`SmpcINTBACKPeripheral`). Deferred to Phase 4.
                } // else: genuine no-op (`smpc.c:499` fall-through, §5.2).
            }
            cmd::SETSMEM => {
                // §4.12: IREG0..IREG3 -> SMEM[0..3], no validity check. SMEM
                // isn't persisted to disk (§7.3) -- it lives only in this
                // `Smpc` for the process's lifetime.
                let ram = work_ram.smpc_regs.read().unwrap();
                let smem = [ram[reg::IREG0], ram[reg::IREG1], ram[reg::IREG2], ram[reg::IREG3]];
                drop(ram);
                self.smem = smem;
                Self::echo_oreg31(work_ram, cmd::SETSMEM);
            }
            cmd::NMIREQ => {
                // §4.13: full NMI-raise (`Sh2` vector 0x0B, level 16)
                // deferred to Phase 3 (no NMI plumbing in `Sh2` yet).
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

    fn echo_oreg31(work_ram: &WorkRam, command: u8) {
        work_ram.smpc_regs.write().unwrap()[reg::oreg(31)] = command;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn fresh_register_view_is_all_zero() {
        let work_ram = WorkRam::new();
        let ram = work_ram.smpc_regs.read().unwrap();
        assert!(ram.iter().all(|&b| b == 0), "SmpcReset zeroes all 64 real registers");
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
        assert_eq!(work_ram.smpc_regs.read().unwrap()[reg::oreg(0)], 0xC0, "fresh resd=true");

        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::RESENAB);
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::INTBACK);
        assert_eq!(work_ram.smpc_regs.read().unwrap()[reg::oreg(0)], 0x80, "after RESENAB");

        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::RESDISA);
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::INTBACK);
        assert_eq!(work_ram.smpc_regs.read().unwrap()[reg::oreg(0)], 0xC0, "after RESDISA");
    }

    #[test]
    fn intback_status_sr_tracks_ireg1_bit3() {
        // §5.3: 0x4F | (intback << 5), where intback = (IREG1 >> 3) & 1.
        let (mut cpu, work_ram, _smpc) = wired_cpu();
        cpu.write_byte(SMPC_BASE + reg::IREG1 as u32, 0x02);
        cpu.write_byte(SMPC_BASE + reg::IREG0 as u32, 0x01);
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::INTBACK);
        assert_eq!(work_ram.smpc_regs.read().unwrap()[reg::SR], 0x4F, "IREG1=0x02 -> SR=0x4F");

        cpu.write_byte(SMPC_BASE + reg::IREG1 as u32, 0x0A);
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::INTBACK);
        assert_eq!(work_ram.smpc_regs.read().unwrap()[reg::SR], 0x6F, "IREG1=0x0A -> SR=0x6F");
    }

    #[test]
    fn oreg31_echo_matrix() {
        // §4.1: echoed for exactly these seven; left untouched for everyone
        // else (pre-seeded with a sentinel that must survive).
        let echoed = [cmd::SNDON, cmd::SNDOFF, cmd::INTBACK, cmd::SETSMEM, cmd::NMIREQ, cmd::RESENAB, cmd::RESDISA];
        let not_echoed = [cmd::SSHON, cmd::SSHOFF, cmd::CKCHG352, cmd::CKCHG320, cmd::MSHON, cmd::CDON, cmd::CDOFF, cmd::SYSRES];

        for &command in &echoed {
            let (mut cpu, work_ram, _smpc) = wired_cpu();
            // INTBACK only echoes on the status path (§5.2) -- harmless for
            // every other command in this list.
            cpu.write_byte(SMPC_BASE + reg::IREG0 as u32, 0x01);
            cpu.write_byte(SMPC_BASE + reg::COMREG as u32, command);
            assert_eq!(
                work_ram.smpc_regs.read().unwrap()[reg::oreg(31)], command,
                "command {:#04X} must echo into OREG31", command
            );
        }
        for &command in &not_echoed {
            let (mut cpu, work_ram, _smpc) = wired_cpu();
            work_ram.smpc_regs.write().unwrap()[reg::oreg(31)] = 0x5A; // sentinel
            cpu.write_byte(SMPC_BASE + reg::COMREG as u32, command);
            assert_eq!(
                work_ram.smpc_regs.read().unwrap()[reg::oreg(31)], 0x5A,
                "command {:#04X} must not touch OREG31", command
            );
        }
    }

    #[test]
    fn sf_handshake_matches_real_bios_sequence() {
        // Replays the real BIOS's own INTBACK handshake byte for byte (§0.6,
        // BIOS 0x1D48-0x1D64) -- the exact loop that hangs the machine if
        // "clear SF after dispatch" is missing.
        let (mut cpu, _work_ram, _smpc) = wired_cpu();
        cpu.write_byte(SMPC_BASE + reg::SF as u32, 0x01);
        cpu.write_byte(SMPC_BASE + reg::IREG0 as u32, 0x01);
        cpu.write_byte(SMPC_BASE + reg::IREG1 as u32, 0x02);
        cpu.write_byte(SMPC_BASE + reg::IREG2 as u32, 0xF0);
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::INTBACK);
        let sf = cpu.read_byte(SMPC_BASE + reg::SF as u32);
        assert_eq!(sf & 1, 0, "SF bit 0 must clear once INTBACK finishes, or the real BIOS spins forever");
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
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, 0x05); // not in §3.4's table
        assert_eq!(cpu.read_byte(SMPC_BASE + reg::SF as u32) & 1, 0, "SF must clear");
        assert_eq!(work_ram.smpc_regs.read().unwrap()[reg::oreg(0)], 0x5A, "OREG0 must be untouched");
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

    fn do_intback_status(cpu: &mut crate::sh2::Sh2) {
        cpu.write_byte(SMPC_BASE + reg::IREG0 as u32, 0x01);
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::INTBACK);
    }

    #[test]
    fn intback_rtc_bcd_layout() {
        // 2000-01-01 00:00:00 UTC, a Saturday -- hand-derived from §5.6's
        // formulas (see docs/implementation-plans/smpc-peripheral.md
        // Phase 2 testing).
        let (mut cpu, work_ram) = wired_cpu_with_clock(ClockSource::Fixed(946_684_800));
        do_intback_status(&mut cpu);
        {
            let ram = work_ram.smpc_regs.read().unwrap();
            assert_eq!(ram[reg::oreg(1)], 0x20, "OREG1 (year thousands/hundreds)");
            assert_eq!(ram[reg::oreg(2)], 0x00, "OREG2 (year tens/units)");
            assert_eq!(ram[reg::oreg(3)], 0x61, "OREG3 (weekday<<4 | month), Saturday=6, Jan=1");
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
        do_intback_status(&mut cpu);
        {
            let ram = work_ram.smpc_regs.read().unwrap();
            assert_eq!(ram[reg::oreg(3)] & 0x0F, 0x0C, "OREG3 low nibble: December is nibble 0xC, not BCD");
            assert_eq!(ram[reg::oreg(3)] >> 4, 0x2, "OREG3 high nibble: Tuesday = 2");
            assert_eq!(ram[reg::oreg(4)], 0x25, "OREG4 (day 25, BCD)");
            assert_eq!(ram[reg::oreg(5)], 0x13, "OREG5 (hour 13, BCD)");
            assert_eq!(ram[reg::oreg(6)], 0x45, "OREG6 (minute 45, BCD)");
            assert_eq!(ram[reg::oreg(7)], 0x59, "OREG7 (second 59, BCD)");
        }
    }

    #[test]
    fn oreg10_encodes_dot_clock() {
        let (mut cpu, work_ram) = wired_cpu_with_clock(ClockSource::Fixed(0));
        do_intback_status(&mut cpu);
        assert_eq!(work_ram.smpc_regs.read().unwrap()[reg::oreg(10)], 0x34, "all flags clear");
        // No CKCHG352 command exists yet (Phase 3) to flip `dotsel` through
        // the public command path -- this pins the OREG10 formula itself
        // against a directly-constructed `Smpc` instead.
        let mut smpc = Smpc::new();
        smpc.dotsel = true;
        let work_ram2 = WorkRam::new();
        work_ram2.smpc_regs.write().unwrap()[reg::IREG0] = 0x01;
        smpc.execute_command(cmd::INTBACK, &work_ram2);
        assert_eq!(work_ram2.smpc_regs.read().unwrap()[reg::oreg(10)], 0x74, "dotsel=true");
    }

    #[test]
    fn setsmem_round_trips_through_oreg12_15() {
        let (mut cpu, work_ram) = wired_cpu_with_clock(ClockSource::Fixed(0));
        cpu.write_byte(SMPC_BASE + reg::IREG0 as u32, 0xDE);
        cpu.write_byte(SMPC_BASE + reg::IREG1 as u32, 0xAD);
        cpu.write_byte(SMPC_BASE + reg::IREG2 as u32, 0xBE);
        cpu.write_byte(SMPC_BASE + reg::IREG3 as u32, 0xEF);
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::SETSMEM);
        assert_eq!(work_ram.smpc_regs.read().unwrap()[reg::oreg(31)], cmd::SETSMEM);

        // IREG0's SETSMEM value (0xDE) would otherwise look like a status
        // request on the next INTBACK (bit 0 set) -- clear it first so this
        // test isolates the SMEM round-trip.
        cpu.write_byte(SMPC_BASE + reg::IREG0 as u32, 0x01);
        cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::INTBACK);
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
            region::JAPAN, region::ASIA_NTSC, region::NORTH_AMERICA,
            region::CENTRAL_SOUTH_AMERICA_NTSC, region::KOREA, region::ASIA_PAL,
            region::EUROPE, region::CENTRAL_SOUTH_AMERICA_PAL,
        ];
        for &r in &regions {
            let (mut cpu, work_ram) = wired_cpu_with_clock(ClockSource::Fixed(0));
            // Reach into the wired-in Smpc via the CPU to set the region --
            // there's no COMREG-level "set region" on real hardware either
            // (§0.2: it's autodetected from the CD, or configured out of
            // band), so this is a direct API call, not a register write.
            cpu.smpc.as_ref().unwrap().lock().unwrap().set_region(r);
            do_intback_status(&mut cpu);
            assert_eq!(work_ram.smpc_regs.read().unwrap()[reg::oreg(9)], r, "region {:#04X}", r);
        }

        // AUTODETECT with no CD present (the only state this codebase can
        // be in -- the CD block isn't integrated at all) falls back to
        // JAPAN, matching what the pre-Phase-0 inline code already did.
        let (mut cpu, work_ram) = wired_cpu_with_clock(ClockSource::Fixed(0));
        cpu.smpc.as_ref().unwrap().lock().unwrap().set_region(region::AUTODETECT);
        do_intback_status(&mut cpu);
        assert_eq!(work_ram.smpc_regs.read().unwrap()[reg::oreg(9)], region::JAPAN, "AUTODETECT -> JAPAN fallback");
    }
}
