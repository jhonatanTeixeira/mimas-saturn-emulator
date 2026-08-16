//! Real SCU (System Control Unit) register file, plus the scaffolding for
//! its DMA controller, interrupt controller, and timers.
//!
//! `docs/implementation-plans/scu.md` Phase 2 replaces what used to live
//! here -- a 25-line stub (`start_dma`/`run_dsp_instruction`) referenced
//! only by unit tests and never constructed by `SaturnSystem` (see
//! `CLAUDE.md`'s "Known architecture debt"). This module now owns the real,
//! typed 256-byte register file (`docs/hardware-reference/scu.md` §1),
//! matching its reset values (§0.1) and read/write visibility rules
//! (§1.1-§1.4) exactly -- and gives the DMA/interrupt/timer machinery Phase
//! 3-5 will build a real home (`irq`/`dma`/`timers`), rather than
//! retrofitting them into `Sh2` a second time.
//!
//! Each field group is behind its own lock (mirroring `WorkRam`'s
//! per-region design, `docs/mimas-architecture-spec.md` §1.3) so that Core
//! 0 (register access) and Core 6 (DSP execution) don't serialize against
//! each other for no reason. Lock-ordering rule, for whenever something
//! needs two of these at once (nothing does yet): acquire in
//! field-declaration order (`regs`, `irq`, `dma`, `timers`, `dsp`), and
//! always `Scu`'s own locks before any `WorkRam` lock, never the reverse
//! (`docs/implementation-plans/scu.md` §7 call-out 4).
//!
//! `irq` is real as of Phase 3: `IMS`/`IST`/`AIACK` genuinely gate whether a
//! source's interrupt reaches the SH-2 immediately or waits, latched, until
//! unmasked -- see `ScuIrq`'s own doc comment for exactly how that relates
//! to `Sh2`'s separate, already-real `InterruptQueue`. `dma` is real as of
//! Phase 4: a genuine, independent DMA controller (`step_dma_pass`) driven
//! from Core 6 (`SaturnSystem`'s `scu-dma-dsp` thread), replacing the old
//! `Sh2::execute_scu_dma` stand-in that ran a whole transfer synchronously
//! inside one SH-2 register write. `timers` is real as of Phase 5: Timer 0
//! (a scanline counter compared against `T0C`) and Timer 1 (a down-counter
//! reloaded from `T1S`, ticking on real Master SH-2 cycles via
//! `Scu::timer1_tick`) both raise their vectors through the Phase 3
//! controller -- see `hblank_in`/`vblank_out`'s own doc comments for the
//! exact H-Blank IN / V-Blank OUT bookkeeping.

use crate::bus_arbiter::BusArbiter;
use crate::scu_dsp::ScuDsp;
use crate::shared_buffers::WorkRam;
use std::sync::{Arc, Mutex};

/// Dedup-logs an overrun of `ScuIrq`'s own masked-interrupt queue (its cap
/// is 30, a Mimas-specific bound the reference has none of -- see
/// `ScuIrq::queue_interrupt`). Mirrors `sh2.rs`'s own
/// `log_interrupt_overrun_once` for the SH-2-level queue's identical cap
/// pattern, kept as a separate log since the two queues are different
/// things (see `ScuIrq`'s doc comment).
fn log_scu_irq_overrun_once(vector: u8, level: u8) {
    static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let key = format!("vector={vector:#04X} level={level}");
    let mut log = LOG.lock().unwrap();
    if !log.contains(&key) {
        eprintln!("[SCU_IRQ_OVERRUN] {key}");
        log.push(key);
    }
}

/// Mimas-only safety valve, not present in the reference: a malformed or
/// non-terminating indirect descriptor chain (no descriptor ever setting
/// the end bit) would otherwise loop forever inside `Scu::run_level`,
/// hanging Core 6 -- and, through `LockStepSync`'s bounded-slack lockstep,
/// eventually the whole system. Logged once per level so a real occurrence
/// is visible rather than silently swallowed.
fn log_scu_dma_malformed_chain_once(level: usize) {
    static LOGGED: Mutex<Vec<usize>> = Mutex::new(Vec::new());
    let mut log = LOGGED.lock().unwrap();
    if !log.contains(&level) {
        eprintln!(
            "[SCU_DMA] level {level}: indirect descriptor chain exceeded 4096 entries \
             without an end marker -- forcing completion (malformed chain?)"
        );
        log.push(level);
    }
}

/// Dedup-logs the (inert on real hardware too, deviation #13) `DSTP` write
/// -- a real BIOS/game attempt to abort a DMA is visible in the logs
/// instead of silently doing nothing.
fn log_scu_dstp_write_once() {
    static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!("[SCU] DSTP written (DMA abort requested) -- inert on real hardware too");
    }
}

#[derive(PartialEq, Eq, Debug)]
pub enum VideoLineEvent {
    None,
    VBlankIn,
    VBlankOut,
    Line207,
}

/// The 256-byte memory-mapped register file, `docs/hardware-reference/scu.md`
/// §1.1-§1.4. Field names are taken directly from that table rather than
/// transliterated from Yabause's C struct layout.
#[derive(Debug, Clone, Copy)]
pub struct ScuRegisters {
    pub d0r: u32,
    pub d0w: u32,
    pub d0c: u32,
    pub d0ad: u32,
    pub d0en: u32,
    pub d0md: u32,
    pub d1r: u32,
    pub d1w: u32,
    pub d1c: u32,
    pub d1ad: u32,
    pub d1en: u32,
    pub d1md: u32,
    pub d2r: u32,
    pub d2w: u32,
    pub d2c: u32,
    pub d2ad: u32,
    pub d2en: u32,
    pub d2md: u32,
    pub dstp: u32,
    pub dsta: u32,
    pub t0c: u32,
    pub t1s: u32,
    pub t1md: u32,
    pub ims: u32,
    pub ist: u32,
    pub aiack: u32,
    pub asr0: u32,
    pub asr1: u32,
    pub aref: u32,
    pub rsel: u32,
    pub ver: u32,
}

impl ScuRegisters {
    /// §0.1's reset table (`ScuReset`, `scu.c:121-158`). `DnAD` resets to
    /// `0x101` (read-add disabled, write-add = 2 bytes -- see
    /// `hardware-reference/scu.md` §1.2), `DnMD` to `0x7` (start factor
    /// "immediate", indirect bit clear), `IMS` to `0xBFFF` (every real
    /// interrupt source masked -- no source in this file uses mask bit
    /// 14), `VER` to `0x04`. Deliberately *not* reproduced here: DSP
    /// `ProgramRam`/`MD[][]`/`PC`/`CT[]`/`RA0`/`WA0`/`jmpaddr` all survive a
    /// reset on real hardware -- that's `ScuDsp::new()`'s own concern
    /// (constructed fresh alongside this, but never re-zeroed by *this*
    /// reset).
    pub fn reset() -> Self {
        Self {
            d0r: 0,
            d0w: 0,
            d0c: 0,
            d0ad: 0x101,
            d0en: 0,
            d0md: 0x7,
            d1r: 0,
            d1w: 0,
            d1c: 0,
            d1ad: 0x101,
            d1en: 0,
            d1md: 0x7,
            d2r: 0,
            d2w: 0,
            d2c: 0,
            d2ad: 0x101,
            d2en: 0,
            d2md: 0x7,
            dstp: 0,
            dsta: 0,
            t0c: 0,
            t1s: 0,
            t1md: 0,
            ims: 0xBFFF,
            ist: 0,
            aiack: 0,
            asr0: 0,
            asr1: 0,
            aref: 0,
            rsel: 0,
            ver: 0x04,
        }
    }
}

impl Default for ScuRegisters {
    fn default() -> Self {
        Self::reset()
    }
}

/// Per-level DMA working-copy (`scudmainfo_struct`,
/// `docs/hardware-reference/scu.md` §2.1) -- a *snapshot*, taken at trigger
/// time from the memory-mapped `DnR`/`DnW`/`DnC`/`DnAD`/`DnMD` registers and
/// then mutated as the transfer proceeds; the visible registers themselves
/// are never written back (§2.1's own framing). `transfer_number > 0` is
/// the real busy predicate, consulted live by `DSTA`'s read path below.
///
/// **Not reproduced from §2.1**: the reference's `mode` field (the literal
/// level number 0/1/2) is only ever used to pick which completion interrupt
/// to raise -- Mimas already knows that from the array index `dma[level]`
/// is stored at, so carrying a redundant copy of it here would just be
/// another thing that could get out of sync.
#[derive(Debug, Clone, Copy, Default)]
pub struct DmaLevel {
    /// Current source pointer. May carry indirect mode's end-of-list marker
    /// (bit 31) unstripped -- see `is_last_indirect_descriptor` below.
    pub read_address: u32,
    /// Current destination pointer (direct mode) / next descriptor's
    /// `+0x4` field, refreshed on every descriptor load (indirect mode).
    pub write_address: u32,
    /// Remaining bytes; `> 0` is the busy predicate (§2.1, §1.3).
    pub transfer_number: u32,
    /// Snapshot of `DnAD` at trigger time.
    pub add_value: u32,
    /// Snapshot of `DnMD` at trigger time -- decides direct vs. indirect
    /// (bit 24) for the whole transfer; never re-read from the live
    /// register once triggered.
    pub mode_address_update: u32,
    /// Decoded from `add_value` bit 8: `4` or `0` (`0` = fill mode, §2.4).
    pub read_add: u32,
    /// Decoded from `add_value[2:0]`: 0/2/4/8/16/32/64/128 bytes (§1.2).
    pub write_add: u32,
    /// Indirect mode only: pointer to the *next* descriptor (§2.5). Unused
    /// in direct mode.
    pub indirect_address: u32,
    /// Fill mode only (§2.4): the source long-word, read once at trigger
    /// (or at each descriptor load, for indirect fill transfers) when
    /// `read_address` falls in one of the four "constant source" regions.
    /// Re-read live from `read_address` every iteration when this is
    /// `false` (real hardware treats a non-constant fill source as a live
    /// register that can change between reads).
    pub fill_cached: bool,
    pub fill_value: u32,
    /// Set by `Scu::request_dma_trigger` (`Sh2::write_long`'s `DnEN`
    /// handling) when the CPU asks for an immediate start (§2.2 path (a));
    /// cleared once Core 6 (`Scu::step_dma_pass`) services it. Keeping the
    /// request as a flag -- rather than snapshotting the registers inline
    /// on the CPU's own thread -- is what actually moves the whole engine
    /// onto Core 6: the CPU write only ever sets a bit and wakes the
    /// thread, exactly like the DSP's own `EX` control-port bit already
    /// works (`Sh2::write_scu_dsp_port`).
    pub trigger_pending: bool,
    /// Set alongside `trigger_pending`, but only by the *factor* trigger
    /// path (§2.3, `docs/implementation-plans/scu.md` Phase 6 --
    /// `Scu::check_dma_start_factor`), never by the immediate path
    /// (`Scu::request_dma_trigger`). Real hardware clears `DnEN` to `0`
    /// after a factor-triggered start (`ScuChekIntrruptDMA`'s own
    /// `D0EN = 0` at the end) -- a genuine, one-shot-arming asymmetry with
    /// the immediate path, which Phase 4 already found and tested leaves
    /// `DnEN` untouched. This flag is how `Scu::service_trigger` tells the
    /// two triggers apart once it's ready to act on a pending level.
    pub clear_den_after_trigger: bool,
}

/// Interrupt-controller scaffold (§4). Empty until Phase 3 replaces `Sh2`'s
/// four ad hoc pending-interrupt flags (`vblank_pending`,
/// `vblank_out_pending`, `smpc_irq_pending`, `sound_req_irq`) with a real
/// pending queue here (`docs/implementation-plans/scu.md` Phase 3).
///
/// **How this relates to `Sh2`'s own `InterruptQueue`** (`sh2-cpu.md` Phase
/// 5, already real and load-bearing): that queue is the CPU-side "about to
/// be delivered" list the SH-2 itself polls once per `step()` -- real
/// hardware's own per-CPU `interrupts[]`. `ScuIrq`'s `queue` here is a
/// *separate*, upstream staging list that only ever holds interrupts the
/// SCU's own `IMS` mask is currently blocking (`docs/hardware-reference/scu.md`
/// §4.2/§4.3): an unmasked source goes straight into the SH-2's queue via
/// `master_target`/`slave_target`; a masked one waits here, with its `IST`
/// status bit latched, until an `IMS`/`IST`/`AIACK` write asks
/// `Scu::test_interrupt_mask` to look again.
#[derive(Debug, Clone, Copy)]
pub struct QueuedIrq {
    pub vector: u8,
    pub level: u8,
    pub mask: u16,
    pub statusbit: u32,
}

pub struct ScuIrq {
    queue: Vec<QueuedIrq>,
    /// Where an *unmasked* interrupt actually lands: the master SH-2's own
    /// `InterruptQueue`. Always present -- `Scu::new()` gives it a fresh,
    /// private queue nobody else looks at by default (matching `Scu`
    /// itself always having a real, if private, instance); `SaturnSystem`
    /// rewires it (`set_master_target`) to the shared queue Core 0 also
    /// holds, exactly like `Scu` is rewired to a shared instance
    /// post-construction.
    master_target: Arc<Mutex<crate::sh2::InterruptQueue>>,
    /// The slave SH-2's queue, for the two hard-wired mirrors
    /// (`docs/hardware-reference/scu.md` §4.2: HBlank-IN -> slave vector
    /// 0x41 level 1, VBlank-IN -> slave vector 0x43 level 2). `None` for a
    /// bare `Scu` (no slave concept); wired by `SaturnSystem::new()`.
    ///
    /// [Simplification] real hardware only mirrors "when the slave is
    /// running" (`yabsys.IsSSH2Running`). Mimas mirrors whenever a slave
    /// queue is wired at all, regardless of whether SSHON has actually run
    /// yet -- checking "is Core 1 genuinely active" would need `Scu` to
    /// also hold a `LockStepSync` handle for this one narrow case. Revisit
    /// if a real boot trace ever shows this mattering (a stray HBlank/VBlank
    /// mirror landing in the slave's queue before SSHON is harmless in
    /// practice: Core 1 stays parked and never calls `step()` until then).
    slave_target: Option<Arc<Mutex<crate::sh2::InterruptQueue>>>,
}

impl ScuIrq {
    fn new() -> Self {
        Self {
            queue: Vec::new(),
            master_target: Arc::new(Mutex::new(crate::sh2::InterruptQueue::new())),
            slave_target: None,
        }
    }

    /// §4.3: dedupe by vector (the caller's `IST |= statusbit` still
    /// happens either way), then keep the queue sorted ascending by level.
    /// Capped at 30 -- the reference has no bounds check; Mimas saturates
    /// and logs once rather than growing unboundedly (same shape as
    /// `InterruptQueue::send`'s own cap, just a different, SCU-specific
    /// limit).
    fn queue_interrupt(&mut self, entry: QueuedIrq) {
        if self.queue.iter().any(|q| q.vector == entry.vector) {
            return;
        }
        if self.queue.len() >= 30 {
            log_scu_irq_overrun_once(entry.vector, entry.level);
            return;
        }
        self.queue.push(entry);
        self.queue.sort_by_key(|q| q.level);
    }
}

/// Timers 0 and 1 (§5.1's `scu.h` internal state). Reset values match the
/// reference implicitly -- `ScuReset` never explicitly zeroes any of these
/// (`scu.c:120-152`), relying on the struct's zero-allocation, exactly what
/// `#[derive(Default)]` gives every field here too.
///
/// **Not reproduced from `scu.h`**: a `timer1: u32` field exists in the real
/// struct but is written only once, at reset, to `0`, and never read or
/// written anywhere else in `scu.c` -- genuinely dead state, so Mimas
/// doesn't carry a matching field.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScuTimers {
    /// Current scanline counter (§5.2): incremented on every H-Blank IN,
    /// reset to `0` on V-Blank OUT, compared against `T0C` on both edges.
    pub timer0: u32,
    /// "Timer 0 matched on this line" -- consumed by Timer 1 mode 1 (`T1MD`
    /// bit 7 set, §5.3).
    pub timer0_set: bool,
    /// Timer 1's down-counter (§5.3). Signed to match the reference's
    /// `s32 timer1_counter` -- the expiry test (`<= 0`) can see it go
    /// slightly negative when `decrement` overshoots.
    pub timer1_counter: i32,
    /// Armed by a `T1S` write (`Scu::write_long`'s `0x94` arm); consumed
    /// (cleared) the next time Timer 1 reloads at H-Blank IN.
    pub timer1_set: bool,
    /// Timer 1's reload value, latched from the last `T1S` write.
    pub timer1_preset: i32,
}

/// The real SCU: register file, DMA/interrupt/timer scaffolding, and the
/// DSP submodule, each behind its own lock so Core 0 (register access) and
/// Core 6 (DSP execution) don't serialize against each other. Constructed
/// once by `SaturnSystem` and shared (`Arc<Scu>`) with the cores that touch
/// it; a bare `Sh2::new()` used in plain unit tests gets its own private
/// instance by default (see `Sh2::new`'s struct literal), so every SCU
/// register behaves the same way whether or not a `SaturnSystem` is
/// involved -- there is no "unwired, fall back to a raw byte array" mode
/// the way `Smpc`/`ScuDsp` still have, because `WorkRam::scu_regs` (the
/// byte array those fallbacks used) has been retired; this module is the
/// only home for SCU register storage now.
pub struct Scu {
    pub regs: Mutex<ScuRegisters>,
    pub irq: Mutex<ScuIrq>,
    pub dma: Mutex<[DmaLevel; 3]>,
    pub timers: Mutex<ScuTimers>,
    pub dsp: Mutex<ScuDsp>,
    pub cs2: Mutex<Option<Arc<Mutex<crate::cs2::Cs2>>>>,
}

impl Scu {
    pub fn new() -> Self {
        Self {
            regs: Mutex::new(ScuRegisters::reset()),
            irq: Mutex::new(ScuIrq::new()),
            dma: Mutex::new([DmaLevel::default(); 3]),
            timers: Mutex::new(ScuTimers::default()),
            dsp: Mutex::new(ScuDsp::new()),
            cs2: Mutex::new(None),
        }
    }

    pub fn set_cs2(&self, cs2: Arc<Mutex<crate::cs2::Cs2>>) {
        *self.cs2.lock().unwrap() = Some(cs2);
    }

    pub fn dma_read_long(&self, work_ram: &WorkRam, addr: u32) -> u32 {
        let a = addr & 0x0FFF_FFFF;
        if (0x0580_0000..0x0590_0000).contains(&a) {
            let guard = self.cs2.lock().unwrap();
            if let Some(ref cs2) = *guard {
                let off = (a - 0x0580_0000) as usize;
                return cs2.lock().unwrap().read_long(off);
            }
            0
        } else {
            crate::scu_dsp::read_long(work_ram, a)
        }
    }

    pub fn dma_read_word(&self, work_ram: &WorkRam, addr: u32) -> u16 {
        let a = addr & 0x0FFF_FFFF;
        if (0x0580_0000..0x0590_0000).contains(&a) {
            let guard = self.cs2.lock().unwrap();
            if let Some(ref cs2) = *guard {
                let off = (a - 0x0580_0000) as usize;
                return cs2.lock().unwrap().read_word(off);
            }
            0
        } else {
            crate::scu_dsp::read_word(work_ram, a)
        }
    }

    /// Rewire where an unmasked interrupt actually lands -- the shared
    /// `InterruptQueue` the master SH-2 also holds (`Sh2::irq_in`).
    /// `SaturnSystem::new()` calls this once, right after constructing both,
    /// so the default private queue `Scu::new()` gave `ScuIrq` is replaced
    /// before anything can observe the mismatch.
    pub fn set_master_target(&self, target: Arc<Mutex<crate::sh2::InterruptQueue>>) {
        self.irq.lock().unwrap().master_target = target;
    }

    /// Wire the slave SH-2's queue for the two hard-wired HBlank-IN/
    /// VBlank-IN mirrors (§4.2). `None` (the default) means no slave
    /// concept exists at all -- a bare `Scu` simply never mirrors.
    pub fn set_slave_target(&self, target: Arc<Mutex<crate::sh2::InterruptQueue>>) {
        self.irq.lock().unwrap().slave_target = Some(target);
    }

    /// `docs/hardware-reference/scu.md` §4.2 (`SendInterrupt`), the dispatch
    /// every named source method below funnels into.
    fn send(&self, vector: u8, level: u8, mask: u32, statusbit: u32) {
        if mask & 0x8000 != 0 {
            // External / A-Bus source: consumes AIACK unconditionally, and
            // is dropped entirely (not queued, IST untouched) if AIACK is
            // already 0 -- deviation #16.
            let mut regs = self.regs.lock().unwrap();
            if regs.aiack == 0 {
                return;
            }
            regs.aiack = 0;
            let masked = regs.ims & 0x8000 != 0;
            drop(regs);
            if !masked {
                self.deliver_to_master(vector, level);
            }
        } else {
            let mut regs = self.regs.lock().unwrap();
            if regs.ims & mask == 0 {
                // Unmasked: deliver now. IST is left untouched -- an
                // interrupt that never had to wait never latches a status
                // bit (§4.2 consequence 1, the "asymmetry" the reference
                // itself calls the most surprising part of this design).
                drop(regs);
                self.deliver_to_master(vector, level);
            } else {
                // Masked: stage it and latch IST.
                regs.ist |= statusbit;
                drop(regs);
                self.irq.lock().unwrap().queue_interrupt(QueuedIrq {
                    vector,
                    level,
                    mask: mask as u16,
                    statusbit,
                });
            }
        }

        // Slave mirrors (§4.2's closing paragraph): unconditional on the
        // mask branch taken above, gated only on whether a slave queue is
        // wired at all -- see `ScuIrq::slave_target`'s doc comment for the
        // documented simplification ("is the slave running" isn't checked).
        if vector == 0x42 {
            self.deliver_to_slave(0x41, 1);
        } else if vector == 0x40 {
            self.deliver_to_slave(0x43, 2);
        }
    }

    fn deliver_to_master(&self, vector: u8, level: u8) {
        let target = self.irq.lock().unwrap().master_target.clone();
        target.lock().unwrap().send(vector, level);
    }

    fn deliver_to_slave(&self, vector: u8, level: u8) {
        let target = self.irq.lock().unwrap().slave_target.clone();
        if let Some(target) = target {
            target.lock().unwrap().send(vector, level);
        }
    }

    /// `docs/hardware-reference/scu.md` §4.3 (`ScuTestInterruptMask`), the
    /// drain run on every write to `IMS`/`IST`/`AIACK`.
    fn test_interrupt_mask(&self) {
        let mut regs = self.regs.lock().unwrap();
        let mut irq = self.irq.lock().unwrap();

        // Deviation #17 (§9): the reference's own `ScuRemoveInterruptByCPU`
        // is dead code (a C precedence bug always evaluates its guard to
        // false), so a queued entry whose IST bit the CPU cleared by hand
        // leaks in the real queue forever -- skipped every drain, never
        // removed. Mimas implements the *intended* behavior instead: drop
        // any queued entry whose status bit is no longer set in IST.
        irq.queue.retain(|q| regs.ist & q.statusbit != 0);

        // Walk from the end (highest level first, since the queue is kept
        // sorted ascending by level). External entries consume AIACK and
        // never `break` -- keep scanning past them. The first deliverable
        // non-external entry breaks after exactly one delivery (§4.3: "at
        // most one queued non-external interrupt per register write").
        let mut i = irq.queue.len();
        while i > 0 {
            i -= 1;
            let entry = irq.queue[i];
            let mask = entry.mask as u32;
            if mask & 0x8000 != 0 {
                if regs.aiack != 0 {
                    regs.aiack = 0;
                    if regs.ims & 0x8000 == 0 {
                        irq.master_target
                            .lock()
                            .unwrap()
                            .send(entry.vector, entry.level);
                        regs.ist &= !entry.statusbit;
                        irq.queue.remove(i);
                    }
                }
            } else if regs.ims & mask == 0 {
                irq.master_target
                    .lock()
                    .unwrap()
                    .send(entry.vector, entry.level);
                regs.ist &= !entry.statusbit;
                irq.queue.remove(i);
                break;
            }
        }
    }

    // ---- Interrupt sources, §4.1's complete table. One thin method per
    // source; each is the only place its (vector, level, mask, statusbit)
    // tuple is spelled out. ----

    pub fn vblank_in(&self) {
        self.send(0x40, 15, 0x0001, 0x0000_0001);
        self.check_dma_start_factor(0);
    }
    /// §5.2's `ScuSendVBlankOUT`: dispatches the interrupt first, *then*
    /// unconditionally resets the Timer 0 scanline counter to `0` -- and
    /// only when `T1MD` bit 0 (global timer enable) is set, runs the exact
    /// same `T0C` compare `hblank_in` uses, so `T0C == 0` fires Timer 0 at
    /// V-Blank OUT. The §2.3 DMA start-factor check runs last, after that
    /// bookkeeping, matching `ScuSendVBlankOUT`'s real order exactly.
    pub fn vblank_out(&self) {
        self.send(0x41, 14, 0x0002, 0x0000_0002);
        self.reset_timer0_at_vblank_out();
        self.check_dma_start_factor(1);
    }
    /// §5.2/§5.3's `ScuSendHBlankIN`: dispatches the interrupt first, *then*
    /// unconditionally increments the Timer 0 scanline counter -- and only
    /// when `T1MD` bit 0 is set, compares it against `T0C` and reloads
    /// Timer 1 if armed (`timer1_set`, from the last `T1S` write). The §2.3
    /// DMA start-factor check runs last, matching `ScuSendHBlankIN`'s real
    /// order.
    pub fn hblank_in(&self) {
        self.send(0x42, 13, 0x0004, 0x0000_0004);
        self.tick_timer0_and_arm_timer1();
        self.check_dma_start_factor(2);
    }
    pub fn timer0(&self) {
        self.send(0x43, 12, 0x0008, 0x0000_0008);
        self.check_dma_start_factor(3);
    }
    pub fn timer1(&self) {
        self.send(0x44, 11, 0x0010, 0x0000_0010);
        self.check_dma_start_factor(4);
    }

    // ---- Timers 0 and 1 (`docs/implementation-plans/scu.md` Phase 5,
    // `docs/hardware-reference/scu.md` §5). ----

    /// §5.2's V-Blank OUT half: `timer0 = 0` is unconditional; the compare
    /// (and therefore `timer0_set`/`Scu::timer0()`) only runs when `T1MD`
    /// bit 0 is set -- confirmed against `ScuSendVBlankOUT`'s literal body
    /// (`yabause/src/scu.c:3250-3265`), not inferred from prose.
    fn reset_timer0_at_vblank_out(&self) {
        let t1md = self.raw_read_long(0x98);
        let t0c = self.raw_read_long(0x90);
        let matched = {
            let mut timers = self.timers.lock().unwrap();
            timers.timer0 = 0;
            if t1md & 1 == 0 {
                false
            } else {
                let matched = timers.timer0 == t0c; // i.e. t0c == 0
                timers.timer0_set = matched;
                matched
            }
        };
        if matched {
            self.timer0();
        }
    }

    /// §5.2/§5.3's H-Blank IN half: `timer0 += 1` is unconditional; the
    /// compare, Timer 0's own dispatch, and the Timer 1 reload-arm check
    /// only run when `T1MD` bit 0 is set -- confirmed against
    /// `ScuSendHBlankIN`'s literal body (`yabause/src/scu.c:3278-3301`),
    /// including the exact statement order: Timer 0's dispatch (which runs
    /// its own §2.3 factor-3 DMA check) happens *before* the Timer 1
    /// reload-arm check, not after. The `timers` lock is dropped before
    /// calling `Scu::timer0()` (which itself locks `regs`/`irq`) and
    /// re-acquired afterward for the reload check, keeping this module's
    /// documented lock-ordering rule (`regs`, `irq`, `dma`, `timers`,
    /// `dsp`) intact rather than acquiring `timers` first and `regs`/`irq`
    /// second.
    fn tick_timer0_and_arm_timer1(&self) {
        let t1md = self.raw_read_long(0x98);
        let t0c = self.raw_read_long(0x90);
        let matched = {
            let mut timers = self.timers.lock().unwrap();
            timers.timer0 = timers.timer0.wrapping_add(1);
            if t1md & 1 == 0 {
                None
            } else {
                let matched = timers.timer0 == t0c;
                timers.timer0_set = matched;
                Some(matched)
            }
        };
        let Some(matched) = matched else {
            return; // T1MD bit 0 clear -- neither the compare nor the reload check run.
        };
        if matched {
            self.timer0(); // ScuSendTimer0(): dispatch + its own §2.3 factor-3 check.
        }
        let mut timers = self.timers.lock().unwrap();
        if timers.timer1_set {
            timers.timer1_set = false;
            timers.timer1_counter = timers.timer1_preset;
        }
    }

    /// §5.3's `ScuTimer1Exec`: decrements Timer 1's down-counter by real
    /// elapsed SH-2 cycles and fires on expiry, gated on `T1MD` bit 0 (the
    /// same global enable `hblank_in`/`vblank_out` check) and, on expiry,
    /// on bit 7 (Timer 1 mode). `sh2_cycles` is the real delta Master SH-2
    /// executed since the last call -- see `Sh2::step`'s own call site
    /// (`SCU_TIMER_BATCH_CYCLES`) for why this is batched rather than
    /// called every instruction, and for why only the Master ever calls it.
    ///
    /// **Deviation #18, fixed not copied**: the reference's outer gate in
    /// `ScuExec` (`if (T1MD & 0x80 == 0)`) is a C operator-precedence bug --
    /// `0x80 == 0` binds first and is always false, so `ScuTimer1Exec` is in
    /// practice only ever called on the scanline where `LineCount == T0C`
    /// (or unconditionally past `T0C > 500`), not "every tick when bit 7 is
    /// clear" like the code clearly intended. Mimas ticks Timer 1 every call
    /// (subject only to the real, correctly-written inner gate below) and
    /// implements the *intended* reading of bit 7, per
    /// `docs/hardware-reference/scu.md` §5.4.
    pub fn timer1_tick(&self, sh2_cycles: u32) {
        let t1md = self.raw_read_long(0x98);
        if t1md & 1 == 0 {
            return;
        }
        let timing = (sh2_cycles >> 1) as i32;
        let decrement = timing >> 1;
        let should_fire = {
            let mut timers = self.timers.lock().unwrap();
            if timers.timer1_counter <= 0 {
                false
            } else {
                timers.timer1_counter -= decrement;
                if timers.timer1_counter <= 0 {
                    timers.timer1_set = true;
                    if t1md & 0x80 == 0 {
                        true
                    } else {
                        timers.timer0_set
                    }
                } else {
                    false
                }
            }
        };
        if should_fire {
            self.timer1();
        }
    }

    /// Drives H-Blank IN / V-Blank IN / V-Blank OUT generation from real
    /// Master SH-2 progress (`Sh2::step`'s `SH2_CYCLES_PER_LINE` batching)
    /// instead of a wall-clock timer -- see that constant's doc comment for
    /// the reference citation (`yabause/src/yabause.c:762-810` ties
    /// `yabsys.LineCount`/VBlank generation to `sh2cycles`, never a host
    /// clock, and this mirrors that structure exactly). Reuses
    /// `timers.timer0` as the running line counter -- real hardware's own
    /// `LineCount` and the SCU's Timer 0 both increment on the same
    /// H-Blank IN edge, so there is only one counter to keep here too, not
    /// two that could drift apart.
    ///
    /// `225`/`263` are NTSC's real `VBlankLineCount`/`MaxLineCount`
    /// (`yabause/src/vdp2.cpp:515`, `yabause/src/yabause.c:1027`) -- not
    /// re-derived from Mimas's own frame-tick constants, cross-checked
    /// independently against the same reference `hblank_in`'s own doc
    /// comment cites for the 263-lines-per-frame figure.
    ///
    /// Returns `true` exactly when V-Blank IN just fired -- the signal the
    /// caller (`Sh2::step`) uses to wake Core 3 (`lib.rs`) for the actual
    /// frame render, since `Scu` deliberately holds no `LockStepSync`
    /// handle of its own.
    pub fn advance_video_line(&self, work_ram: &WorkRam) -> VideoLineEvent {
        self.hblank_in();
        let line = self.timers.lock().unwrap().timer0;
        if line == 225 {
            self.vblank_in();
            // Release: pairs with `Sh2::tvstat_word`'s Acquire load.
            work_ram
                .vblank_active
                .store(true, std::sync::atomic::Ordering::Release);
            VideoLineEvent::VBlankIn
        } else if line == 263 {
            self.vblank_out(); // resets timers.timer0 to 0 internally
            work_ram
                .vblank_active
                .store(false, std::sync::atomic::Ordering::Release);
            VideoLineEvent::VBlankOut
        } else if line == 207 {
            VideoLineEvent::Line207
        } else {
            VideoLineEvent::None
        }
    }

    /// Wired to `ScuDsp`'s `ENDI` (`docs/implementation-plans/scu.md` Phase
    /// 6): Core 6 calls this when `ScuDsp::step` reports the just-executed
    /// `END` instruction had its interrupt-request bit set. **Not** in
    /// §2.3's DMA start-factor table -- `ScuSendDSPEnd` never calls
    /// `ScuChekIntrruptDMA`, so no `check_dma_start_factor` call here.
    pub fn dsp_end(&self) {
        self.send(0x45, 10, 0x0020, 0x0000_0020);
    }
    pub fn sound_request(&self) {
        self.send(0x46, 9, 0x0040, 0x0000_0040);
        self.check_dma_start_factor(5);
    }
    pub fn system_manager(&self) {
        self.send(0x47, 8, 0x0080, 0x0000_0080);
    }
    /// Never raised yet -- coordinate with `docs/implementation-plans/smpc-peripheral.md`.
    pub fn pad(&self) {
        self.send(0x48, 8, 0x0100, 0x0000_0100);
    }
    pub fn level2_dma_end(&self) {
        self.send(0x49, 6, 0x0200, 0x0000_0200);
    }
    pub fn level1_dma_end(&self) {
        self.send(0x4A, 6, 0x0400, 0x0000_0400);
    }
    pub fn level0_dma_end(&self) {
        self.send(0x4B, 5, 0x0800, 0x0000_0800);
    }
    /// Never called anywhere -- illegal-DMA detection is unimplemented in
    /// the reference too (deviation #14). Kept reachable for whenever
    /// Phase 4 grows address-validity checks.
    pub fn dma_illegal(&self) {
        self.send(0x4C, 3, 0x1000, 0x0000_1000);
    }
    /// Never raised yet -- `docs/implementation-plans/vdp1.md` owns the
    /// VDP1-side trigger condition; this is only the SCU-side entry point.
    /// Its §2.3 factor-6 DMA check is implemented and tested here (like
    /// every other source), ready for whenever that trigger is wired --
    /// whoever wires it will also need to wake Core 6 the same way
    /// `Sh2::step`/`M68k::write_byte` already do for the other six sources
    /// (`Scu` deliberately holds no `LockStepSync` handle of its own).
    pub fn draw_end(&self) {
        self.send(0x4D, 2, 0x2000, 0x0000_2000);
        self.check_dma_start_factor(6);
    }
    /// External interrupts 00-15 (`0x50`-`0x5F`). No A-Bus device is
    /// emulated, so these have no producer -- kept reachable, called from
    /// nowhere.
    pub fn external(&self, n: u8) {
        debug_assert!(n < 16, "only 16 external interrupt lines exist");
        let vector = 0x50 + n;
        let level = match n {
            0..=3 => 7,
            4..=7 => 4,
            _ => 1,
        };
        let statusbit = 0x0001_0000u32 << n;
        self.send(vector, level, 0x8000, statusbit);
    }

    // ---- DMA engine (`docs/implementation-plans/scu.md` Phase 4,
    // `docs/hardware-reference/scu.md` §2). Retires `Sh2::execute_scu_dma`
    // (the synchronous stand-in that ran a whole transfer inside one SH-2
    // register write) in favor of a real, budgeted engine driven from Core
    // 6 (`SaturnSystem`'s `scu-dma-dsp` thread, `lib.rs`). ----

    /// §1.2: `DnAD` bit 8 (read-add enable) and bits `[2:0]` (write-add
    /// select) decode independently of everything else in the register.
    fn decode_add_value(add_value: u32) -> (u32, u32) {
        let read_add = if add_value & 0x100 != 0 { 4 } else { 0 };
        let write_add = match add_value & 0x7 {
            0 => 0,
            1 => 2,
            2 => 4,
            3 => 8,
            4 => 16,
            5 => 32,
            6 => 64,
            _ => 128, // 7
        };
        (read_add, write_add)
    }

    /// §1.2's count clamp -- skipped entirely for indirect mode (the count
    /// there comes from each descriptor instead, see `load_descriptor_at`).
    fn clamp_direct_count(level: usize, raw_count: u32) -> u32 {
        if level == 0 {
            if raw_count == 0 {
                0x0010_0000
            } else {
                raw_count
            }
        } else {
            let c = raw_count & 0xFFF;
            if c == 0 {
                0x1000
            } else {
                c
            }
        }
    }

    /// §2.4's B-Bus range test for this engine -- **not** the same boundary
    /// as the DSP DMA's own B-Bus test (§3.8.6 uses `< 0x06000000`); the
    /// hardware reference explicitly warns not to unify them.
    fn is_b_bus_dma(addr: u32) -> bool {
        let a = addr & 0x1FFF_FFFF;
        (0x05A0_0000..0x05FF_0000).contains(&a)
    }

    /// §2.4's "constant source" test for fill mode: Low WRAM, High WRAM,
    /// Sound RAM or VDP1/VDP2 RAM. Tested against the raw (possibly
    /// indirect-mode end-bit-tagged) address, exactly as the reference does.
    fn is_constant_fill_source(addr: u32) -> bool {
        (addr & 0x1FF0_0000) == 0x0020_0000 // Low WRAM
            || (addr & 0x1E00_0000) == 0x0600_0000 // High WRAM
            || (addr & 0x1FF0_0000) == 0x05A0_0000 // Sound RAM
            || (addr & 0x1DF0_0000) == 0x05C0_0000 // VDP1/VDP2 RAM
    }

    /// Fill mode only (§2.4): cache the source long-word once when it falls
    /// in a constant region, so a transfer that spans multiple Core-6
    /// budget passes (see `step_dma_pass`) reads it exactly once overall --
    /// matching real hardware's single synchronous read -- instead of
    /// re-reading a possibly-since-modified address on a later pass. A
    /// non-constant source (some live register) is deliberately left
    /// uncached and re-read every iteration in `fill_iteration`.
    fn prime_fill_cache(&self, lvl: &mut DmaLevel, work_ram: &WorkRam) {
        if lvl.read_add != 0 {
            return; // copy mode: no cache, `read_address` genuinely advances.
        }
        if Self::is_constant_fill_source(lvl.read_address) {
            lvl.fill_value = self.dma_read_long(work_ram, lvl.read_address);
            lvl.fill_cached = true;
        } else {
            lvl.fill_cached = false;
        }
    }

    /// §2.5: load one descriptor's three fields (count / dst / src) from
    /// `table_addr` into `lvl`. Shared by the initial load at trigger time
    /// and every subsequent chain step (`load_next_descriptor`) -- both are
    /// "load whatever `table_addr` currently points at", just with a
    /// different address source.
    fn load_descriptor_at(&self, lvl: &mut DmaLevel, work_ram: &WorkRam, table_addr: u32) {
        let count = self.dma_read_long(work_ram, table_addr);
        let dst = self.dma_read_long(work_ram, table_addr.wrapping_add(4));
        let src = self.dma_read_long(work_ram, table_addr.wrapping_add(8));
        lvl.transfer_number = count;
        lvl.write_address = dst;
        lvl.read_address = src;
        self.prime_fill_cache(lvl, work_ram);
    }

    /// §2.5's chain-advance step: `InDirectAdress` always points at the
    /// *next* descriptor to load, advanced by `0xC` bytes after each load.
    fn load_next_descriptor(&self, lvl: &mut DmaLevel, work_ram: &WorkRam) {
        let table_ptr = lvl.indirect_address;
        self.load_descriptor_at(lvl, work_ram, table_ptr);
        lvl.indirect_address = table_ptr.wrapping_add(0xC);
    }

    /// §2.2 trigger path (a): CPU write to `DnEN` (offsets `0x10`/`0x30`/
    /// `0x50`) with bit 0 set, live only when `DnMD[2:0] == 7`
    /// ("immediate"). Called by `Sh2::write_long` right after
    /// `Scu::write_long` has already stored the raw value. Deliberately
    /// does no transfer work here -- only ever marks the level pending, so
    /// `Sh2::write_long` can wake Core 6 (`LockStepSync::set_thread_active`)
    /// exactly like the DSP's `EX` control-port bit already does; see
    /// `DmaLevel::trigger_pending`'s doc comment. Factor-triggered starts
    /// (path (b), `DnEN` bit 8) are `check_dma_start_factor`, below.
    pub fn request_dma_trigger(&self, level: usize, val: u32) -> bool {
        let base = level * 0x20;
        let mode3 = self.raw_read_long(base + 0x14) & 0x7;
        if val & 1 == 0 || mode3 != 7 {
            return false;
        }
        self.dma.lock().unwrap()[level].trigger_pending = true;
        true
    }

    /// §2.3 trigger path (b), `ScuChekIntrruptDMA(id)`: called by every
    /// interrupt source that has a DMA start factor (§2.3's table --
    /// vblank_in/out, hblank_in, timer0/1, sound_request, draw_end; *not*
    /// dsp_end, system_manager, pad, the three DMA-end senders, dma_illegal,
    /// or the 16 externals), immediately after that source's own interrupt
    /// dispatch (and any other bookkeeping it does -- see each source
    /// method's own doc comment for the exact real order). Deliberately
    /// **unconditional on `IMS`**: a masked interrupt still arms and starts
    /// a DMA level on a matching factor -- §2.3's own explicit callout, and
    /// the single most counterintuitive part of this mechanism (dedicated
    /// regression test: `masked_vblank_in_still_starts_a_factor_armed_dma`).
    /// Checks all 3 levels independently, exactly like `request_dma_trigger`
    /// but through a different precondition (`DnEN` bit 8 "armed" + `DnMD`
    /// matching `factor_id`, instead of bit 0 "go" + `DnMD == 7`). Marks the
    /// level pending -- Core 6's existing `service_trigger` does the actual
    /// snapshot/start/busy-flush work, unchanged from the immediate path --
    /// and additionally sets `clear_den_after_trigger`, since (unlike an
    /// immediate trigger) a factor-triggered start really does clear `DnEN`
    /// to 0 once serviced (one-shot arming, §2.2's own text).
    fn check_dma_start_factor(&self, factor_id: u8) {
        for level in 0..3 {
            let base = level * 0x20;
            let den = self.raw_read_long(base + 0x10);
            let dmd = self.raw_read_long(base + 0x14);
            if den & 0x100 != 0 && (dmd & 0x7) as u8 == factor_id {
                let mut dma = self.dma.lock().unwrap();
                dma[level].trigger_pending = true;
                dma[level].clear_den_after_trigger = true;
            }
        }
    }

    /// Whether Core 6 has any real work outstanding for the DMA engine --
    /// a pending trigger, or a level still mid-transfer. Paired with
    /// `ScuDsp::is_executing` in `SaturnSystem`'s Core 6 loop to decide
    /// whether to keep running or re-park (`LockStepSync::park_while_inactive`).
    pub fn dma_active(&self) -> bool {
        self.dma
            .lock()
            .unwrap()
            .iter()
            .any(|d| d.transfer_number != 0 || d.trigger_pending)
    }

    /// §2.2's setup: snapshot `DnR`/`DnW`/`DnC`/`DnAD`/`DnMD` into a fresh
    /// working copy. For indirect mode, `DnW`'s value is the descriptor
    /// *table* pointer, not a destination -- descriptor 0 is loaded
    /// immediately and `indirect_address` points past it (§2.5 step 1).
    fn snapshot_and_start(&self, level: usize, work_ram: &WorkRam) {
        let base = level * 0x20;
        let read_address = self.raw_read_long(base);
        let write_address_reg = self.raw_read_long(base + 0x04);
        let raw_count = self.raw_read_long(base + 0x08);
        let add_value = self.raw_read_long(base + 0x0C);
        let mode_address_update = self.raw_read_long(base + 0x14);
        let indirect = mode_address_update & 0x0100_0000 != 0;
        let (read_add, write_add) = Self::decode_add_value(add_value);

        let mut lvl = DmaLevel {
            read_address,
            write_address: write_address_reg,
            transfer_number: 0,
            add_value,
            mode_address_update,
            read_add,
            write_add,
            indirect_address: 0,
            fill_cached: false,
            fill_value: 0,
            trigger_pending: false,
            clear_den_after_trigger: false,
        };

        if indirect {
            lvl.indirect_address = write_address_reg.wrapping_add(0xC);
            self.load_descriptor_at(&mut lvl, work_ram, write_address_reg);
        } else {
            lvl.transfer_number = Self::clamp_direct_count(level, raw_count);
            self.prime_fill_cache(&mut lvl, work_ram);
        }

        self.dma.lock().unwrap()[level] = lvl;
    }

    /// §2.4 fill mode (`read_add == 0`): B-Bus destinations write two
    /// 16-bit halves (high half first) with `write_address` advancing by
    /// `write_add` *twice*; everything else writes one 32-bit long. The
    /// source pointer is masked to `0x0FFF_FFFF` at the access (§1.2) but
    /// otherwise never advances in fill mode (`read_add == 0`).
    fn fill_iteration(&self, lvl: &mut DmaLevel, work_ram: &WorkRam) {
        let value = if lvl.fill_cached {
            lvl.fill_value
        } else {
            self.dma_read_long(work_ram, lvl.read_address)
        };
        lvl.read_address = lvl.read_address.wrapping_add(lvl.read_add);

        if Self::is_b_bus_dma(lvl.write_address) {
            let dst_hi = lvl.write_address & 0x0FFF_FFFF;
            crate::scu_dsp::write_word(work_ram, dst_hi, (value >> 16) as u16);
            lvl.write_address = lvl.write_address.wrapping_add(lvl.write_add);
            let dst_lo = lvl.write_address & 0x0FFF_FFFF;
            crate::scu_dsp::write_word(work_ram, dst_lo, value as u16);
            lvl.write_address = lvl.write_address.wrapping_add(lvl.write_add);
        } else {
            let dst = lvl.write_address & 0x0FFF_FFFF;
            crate::scu_dsp::write_long(work_ram, dst, value);
            lvl.write_address = lvl.write_address.wrapping_add(lvl.write_add);
        }
        lvl.transfer_number = lvl.transfer_number.saturating_sub(4);
    }

    /// §2.4 copy mode (`read_add != 0`): destination-on-B-Bus and
    /// source-on-B-Bus each transfer one 16-bit unit (destination wins if
    /// both are true, matching the reference's case order); neither
    /// transfers one 32-bit unit. **[QUIRK] preserved deliberately**:
    /// `read_add`'s decoded value (always 4) is never actually used as the
    /// source stride here -- the source always advances by the natural
    /// access width (2 or 4) instead, exactly as §2.4 documents.
    fn copy_iteration(&self, lvl: &mut DmaLevel, work_ram: &WorkRam) {
        let dst_is_bbus = Self::is_b_bus_dma(lvl.write_address);
        let src_is_bbus = Self::is_b_bus_dma(lvl.read_address);
        if dst_is_bbus {
            let val = self.dma_read_word(work_ram, lvl.read_address);
            let dst = lvl.write_address & 0x0FFF_FFFF;
            crate::scu_dsp::write_word(work_ram, dst, val);
            lvl.read_address = lvl.read_address.wrapping_add(2);
            lvl.write_address = lvl.write_address.wrapping_add(lvl.write_add);
            lvl.transfer_number = lvl.transfer_number.saturating_sub(2);
        } else if src_is_bbus {
            let val = self.dma_read_word(work_ram, lvl.read_address);
            let dst = lvl.write_address & 0x0FFF_FFFF;
            crate::scu_dsp::write_word(work_ram, dst, val);
            lvl.read_address = lvl.read_address.wrapping_add(2);
            lvl.write_address = lvl.write_address.wrapping_add(lvl.write_add >> 1);
            lvl.transfer_number = lvl.transfer_number.saturating_sub(2);
        } else {
            let val = self.dma_read_long(work_ram, lvl.read_address);
            let dst = lvl.write_address & 0x0FFF_FFFF;
            crate::scu_dsp::write_long(work_ram, dst, val);
            lvl.read_address = lvl.read_address.wrapping_add(4);
            lvl.write_address = lvl.write_address.wrapping_add(lvl.write_add);
            lvl.transfer_number = lvl.transfer_number.saturating_sub(4);
        }
    }

    /// §2.8: the one completion interrupt for `level`.
    fn raise_dma_end(&self, level: usize) {
        match level {
            0 => self.level0_dma_end(),
            1 => self.level1_dma_end(),
            2 => self.level2_dma_end(),
            _ => unreachable!("only 3 DMA levels exist"),
        }
    }

    /// §2.6: steps `level` up to `budget` iterations (one fill/copy
    /// iteration = one unit, matching the reference's `*time -= 1` cost
    /// model exactly). A direct-mode transfer or one indirect descriptor
    /// finishing consumes the budget the same way; an indirect chain keeps
    /// walking descriptors within the same call as long as budget remains.
    /// `budget = i64::MAX` runs to completion -- used only by the rare
    /// re-trigger-while-busy flush (§2.2), never by Core 6's steady-state
    /// per-pass stepping.
    fn run_level(&self, level: usize, work_ram: &WorkRam, mut budget: i64) {
        while budget > 0 {
            let mut lvl = self.dma.lock().unwrap()[level];
            if lvl.transfer_number == 0 {
                return; // idle -- matches `SucDmaCheck`'s early return.
            }
            let indirect = lvl.mode_address_update & 0x0100_0000 != 0;
            if lvl.read_add == 0 {
                self.fill_iteration(&mut lvl, work_ram);
            } else {
                self.copy_iteration(&mut lvl, work_ram);
            }
            budget -= 1;

            // §2.5 step 3: a descriptor (or, in direct mode, the whole
            // transfer) just finished. Chase zero-cost descriptor chains
            // (a descriptor whose count is legitimately 0) without
            // consuming budget, matching the reference's own re-entrant
            // check -- bounded by a Mimas-only safety cap (`chase_guard`,
            // not present in the reference) against a malformed/
            // non-terminating chain hanging Core 6 forever.
            let mut chase_guard = 0u32;
            while lvl.transfer_number == 0 {
                if !indirect {
                    self.raise_dma_end(level);
                    break;
                }
                if lvl.read_address & 0x8000_0000 != 0 {
                    self.raise_dma_end(level);
                    break;
                }
                chase_guard += 1;
                if chase_guard > 4096 {
                    log_scu_dma_malformed_chain_once(level);
                    self.raise_dma_end(level);
                    break;
                }
                self.load_next_descriptor(&mut lvl, work_ram);
            }
            self.dma.lock().unwrap()[level] = lvl;
        }
    }

    /// §2.2's "re-trigger while busy" hack, then §2.2's setup + immediate
    /// 128-unit burst, for whichever level(s) Core 6 finds `trigger_pending`
    /// this pass.
    fn service_trigger(&self, level: usize, work_ram: &WorkRam) {
        let pending = self.dma.lock().unwrap()[level].trigger_pending;
        if !pending {
            return;
        }
        let busy = self.dma.lock().unwrap()[level].transfer_number != 0;
        if busy {
            // §2.2: re-triggering a still-busy level flushes *all three*
            // levels to completion first -- a documented Yabause hack (real
            // hardware has no such rule), kept bug-for-bug per the plan.
            // [Simplification] this runs synchronously within this one
            // Core-6 pass rather than across several bursts like the
            // steady-state path below (`step_dma_pass`) -- re-triggering a
            // level that's still mid-transfer essentially never happens in
            // real BIOS/game code (not observed in this phase's own
            // boot-watch run), so trading strict per-burst granularity for
            // a much simpler synchronous flush was judged the right
            // tradeoff for this one rare corner case.
            for l in 0..3 {
                self.run_level(l, work_ram, i64::MAX);
            }
        }
        // Read *before* `snapshot_and_start`, which constructs a brand new
        // `DmaLevel` and would otherwise silently wipe this back to its
        // default (`false`).
        let clear_den = self.dma.lock().unwrap()[level].clear_den_after_trigger;
        self.snapshot_and_start(level, work_ram);
        self.dma.lock().unwrap()[level].trigger_pending = false;
        if clear_den {
            // §2.2/§2.3: a factor-triggered start clears `DnEN` to `0` once
            // serviced (one-shot arming) -- unlike an immediate trigger,
            // which real hardware leaves untouched (Phase 4's own
            // regression test, `immediate_trigger_leaves_den_bit_set_...`,
            // covers that asymmetry).
            self.raw_write_long(level * 0x20 + 0x10, 0);
        }
        // §2.2: a fresh trigger also runs an immediate 128-unit burst
        // (`ScuDmaProc(ScuRegs, 128)`) on top of Core 6's normal per-pass
        // budget below -- keeps short transfers (the overwhelming majority
        // of real BIOS DMA usage) visibly complete without waiting an
        // extra Core-6 wakeup latency.
        self.run_level(level, work_ram, 128);
    }

    /// One Core-6 pass over all three levels (§2.6: each level gets its own
    /// private copy of the budget; strict textual order 0 -> 1 -> 2; no
    /// priority arbitration between levels, matching the reference
    /// exactly). Locks the bus once for the whole pass and releases it
    /// before returning -- "per time-slice burst", not once around a whole
    /// transfer, so a large DMA doesn't stall Core 0/1 for its entire
    /// duration (docs/implementation-plans/scu.md Phase 4c).
    pub fn step_dma_pass(&self, work_ram: &WorkRam, arbiter: &BusArbiter, budget_per_level: i64) {
        arbiter.lock_for_dma();
        for level in 0..3 {
            self.service_trigger(level, work_ram);
            self.run_level(level, work_ram, budget_per_level);
        }
        arbiter.unlock_from_dma();
    }

    /// The register's true stored value for `aligned_off` (a 4-byte-aligned
    /// offset into the 256-byte file), regardless of whether that offset
    /// has a *read* handler on the CPU-facing long-access path (see
    /// `read_long`). Shared by `read_long`'s readable-offset arms and by
    /// the byte-access fallback (`read_byte`/`write_byte`), which -- per
    /// deviation #2 in `docs/implementation-plans/scu.md` §9 -- is allowed
    /// to see storage the long-access path itself won't surface, and by
    /// `raw_read_long`/`raw_write_long`, the internal-engine accessor real
    /// DMA logic needs (see that method's own doc comment).
    fn raw_get(regs: &ScuRegisters, aligned_off: usize) -> u32 {
        match aligned_off {
            0x00 => regs.d0r,
            0x04 => regs.d0w,
            0x08 => regs.d0c,
            0x0C => regs.d0ad,
            0x10 => regs.d0en,
            0x14 => regs.d0md,
            0x20 => regs.d1r,
            0x24 => regs.d1w,
            0x28 => regs.d1c,
            0x2C => regs.d1ad,
            0x30 => regs.d1en,
            0x34 => regs.d1md,
            0x40 => regs.d2r,
            0x44 => regs.d2w,
            0x48 => regs.d2c,
            0x4C => regs.d2ad,
            0x50 => regs.d2en,
            0x54 => regs.d2md,
            0x60 => regs.dstp,
            0x7C => regs.dsta,
            0x90 => regs.t0c,
            0x94 => regs.t1s,
            0x98 => regs.t1md,
            0xA0 => regs.ims,
            0xA4 => regs.ist,
            0xA8 => regs.aiack,
            0xB0 => regs.asr0,
            0xB4 => regs.asr1,
            0xB8 => regs.aref,
            0xC4 => regs.rsel,
            0xC8 => regs.ver,
            _ => 0,
        }
    }

    /// Sets `aligned_off`'s stored value. `0xC8` (`VER`) falls through to
    /// `_` and is silently dropped -- §1.1: "Writes unhandled" for VER, a
    /// hardwired-read-only register.
    fn raw_set(regs: &mut ScuRegisters, aligned_off: usize, val: u32) {
        match aligned_off {
            0x00 => regs.d0r = val,
            0x04 => regs.d0w = val,
            0x08 => regs.d0c = val,
            0x0C => regs.d0ad = val,
            0x10 => regs.d0en = val,
            0x14 => regs.d0md = val,
            0x20 => regs.d1r = val,
            0x24 => regs.d1w = val,
            0x28 => regs.d1c = val,
            0x2C => regs.d1ad = val,
            0x30 => regs.d1en = val,
            0x34 => regs.d1md = val,
            0x40 => regs.d2r = val,
            0x44 => regs.d2w = val,
            0x48 => regs.d2c = val,
            0x4C => regs.d2ad = val,
            0x50 => regs.d2en = val,
            0x54 => regs.d2md = val,
            // Inert on real hardware too (deviation #13) -- kept as plain
            // storage; Phase 4 adds a one-time "[SCU] DSTP written" log at
            // the call site instead of behavior.
            0x60 => regs.dstp = val,
            0x7C => regs.dsta = val,
            0x90 => regs.t0c = val,
            // Plain storage only -- `write_long`'s dedicated `0x94` arm is
            // where the real timer1_set/timer1_preset side effect lives
            // (§5.1); this generic path is only reached via `raw_write_long`
            // (the internal-engine accessor, which never runs CPU-write
            // side effects) or the byte-access fallback.
            0x94 => regs.t1s = val,
            0x98 => regs.t1md = val,
            0xA0 => regs.ims = val, // Phase 3 adds the interrupt-queue drain
            0xA4 => regs.ist = val, // §4.4: real hardware ANDs; Phase 3 adds the drain
            0xA8 => regs.aiack = val,
            0xB0 => regs.asr0 = val,
            0xB4 => regs.asr1 = val,
            0xB8 => regs.aref = val,
            0xC4 => regs.rsel = val,
            _ => {}
        }
    }

    /// §1.1: CPU-facing long read, at an already-masked (`& 0xFF`) offset.
    /// Implements the R column exactly -- most of `DnAD`/`DnEN`/`DnMD`,
    /// `DSTP`, `T0C`/`T1S`/`T1MD`, `ASR0`/`ASR1`/`AREF` have **no read
    /// handler** in the reference (deviation #19) and return 0 here too,
    /// even though the write side stores a real value (see `raw_read_long`
    /// for the internal-engine accessor that does see it).
    pub fn read_long(&self, offset: usize) -> u32 {
        let off = offset & 0xFF;
        if off == 0x7C {
            return self.read_dsta();
        }
        let regs = self.regs.lock().unwrap();
        match off {
            0x00 | 0x04 | 0x08 | 0x20 | 0x24 | 0x28 | 0x40 | 0x44 | 0x48 | 0xA4 | 0xA8 | 0xC4
            | 0xC8 => Self::raw_get(&regs, off),
            _ => 0,
        }
    }

    /// `DSTA` (`0x7C`, §1.3): bits 4/8/12 are recomputed live from each
    /// level's `transfer_number` (`> 0` = that level's transfer is in
    /// progress); every other bit is last-written storage.
    fn read_dsta(&self) -> u32 {
        let stored = self.regs.lock().unwrap().dsta;
        let dma = self.dma.lock().unwrap();
        let mut val = stored & !0x1110;
        if dma[0].transfer_number > 0 {
            val |= 0x0010;
        }
        if dma[1].transfer_number > 0 {
            val |= 0x0100;
        }
        if dma[2].transfer_number > 0 {
            val |= 0x1000;
        }
        val
    }

    /// §1.1: CPU-facing long write, at an already-masked offset. Every one
    /// of the 24 writable registers accepts a plain store here, except:
    /// the three interrupt-controller registers Phase 3 gives real
    /// semantics (`IST` `0xA4` is AND-only, "write 0s to clear", §4.4; all
    /// three of `IMS`/`IST`/`AIACK` run the §4.3 drain afterward); `DSTP`
    /// (`0x60`), which additionally logs once (deviation #13: inert on real
    /// hardware too, so no other behavior); `T1S` (`0x94`), which also arms
    /// the Timer 1 reload (§5.1). The `DnEN` DMA trigger (§2.2) is
    /// deliberately **not** handled here -- it needs `Sh2::write_long`'s own
    /// `LockStepSync` handle to wake Core 6, so that call site invokes
    /// `Scu::request_dma_trigger` itself, right after this plain store runs.
    pub fn write_long(&self, offset: usize, val: u32) {
        let off = offset & 0xFF;
        match off {
            0xA4 => {
                self.regs.lock().unwrap().ist &= val;
                self.test_interrupt_mask();
            }
            0xA0 | 0xA8 => {
                {
                    let mut regs = self.regs.lock().unwrap();
                    Self::raw_set(&mut regs, off, val);
                }
                self.test_interrupt_mask();
            }
            0x60 => {
                log_scu_dstp_write_once();
                let mut regs = self.regs.lock().unwrap();
                Self::raw_set(&mut regs, off, val);
            }
            0x94 => {
                {
                    let mut regs = self.regs.lock().unwrap();
                    Self::raw_set(&mut regs, off, val);
                }
                let mut timers = self.timers.lock().unwrap();
                timers.timer1_preset = val as i32;
                timers.timer1_set = true;
            }
            _ => {
                let mut regs = self.regs.lock().unwrap();
                Self::raw_set(&mut regs, off, val);
            }
        }
    }

    /// The true stored value for `offset`'s containing register, bypassing
    /// the CPU-facing long-access visibility rules `read_long` enforces --
    /// i.e. this *does* return real data for `D0AD`/`D0EN`/`D0MD` and
    /// friends, which `read_long` reports as 0 per §1.1. For internal
    /// engine use only: `Scu::snapshot_and_start`/`request_dma_trigger`
    /// (Phase 4's real DMA engine) need the actual configured transfer
    /// parameters, exactly as Yabause's own C reads `ScuRegs->D0AD` as a
    /// plain struct field rather than through the CPU-facing register-read
    /// dispatch function.
    pub fn raw_read_long(&self, offset: usize) -> u32 {
        let off = offset & 0xFF;
        let aligned = off & !3;
        let regs = self.regs.lock().unwrap();
        Self::raw_get(&regs, aligned)
    }

    /// The internal-engine counterpart of `raw_read_long` -- a plain store
    /// with no CPU-write side effects, for an engine (not the CPU) to
    /// update its own register block (e.g. clearing `DnEN` once a transfer
    /// completes).
    pub fn raw_write_long(&self, offset: usize, val: u32) {
        let off = offset & 0xFF;
        let aligned = off & !3;
        let mut regs = self.regs.lock().unwrap();
        Self::raw_set(&mut regs, aligned, val);
    }

    /// §1.4: the one byte-addressable register in the reference -- the low
    /// byte of `IST`. Read is a plain extraction (identical to what the
    /// generic byte fallback below would compute anyway); write is
    /// AND-only ("write 0s to clear", matching the long-write semantics at
    /// byte granularity) -- **not** a plain overwrite, which is why it
    /// needs its own case rather than falling into the generic fallback.
    pub fn read_ist_byte(&self) -> u8 {
        (self.regs.lock().unwrap().ist & 0xFF) as u8
    }

    pub fn write_ist_byte(&self, val: u8) {
        {
            let mut regs = self.regs.lock().unwrap();
            regs.ist &= 0xFFFF_FF00 | (val as u32);
        }
        self.test_interrupt_mask();
    }

    /// §1.4 + deviation #2: only `0xA7` is a real hardware byte register --
    /// every other byte offset logs `"Unhandled SCU Register... read"` and
    /// returns 0 in the reference. Mimas deliberately does not copy that
    /// hard-0 behavior: it exposes the real (visibility-unfiltered) stored
    /// value a byte at a time instead, so a plain `MOV.B` probe against any
    /// SCU offset sees genuine data rather than a constant. The `Sh2` call
    /// site still tags this through `log_reg_access_once`, so real BIOS
    /// traffic through this path stays visible in a `[REGACCESS]` sweep.
    pub fn read_byte(&self, offset: usize) -> u8 {
        let off = offset & 0xFF;
        if off == 0xA7 {
            return self.read_ist_byte();
        }
        let aligned = off & !3;
        let idx = off & 3;
        let regs = self.regs.lock().unwrap();
        let long = Self::raw_get(&regs, aligned);
        ((long >> ((3 - idx) * 8)) & 0xFF) as u8
    }

    pub fn write_byte(&self, offset: usize, val: u8) {
        let off = offset & 0xFF;
        if off == 0xA7 {
            self.write_ist_byte(val);
            return;
        }
        let aligned = off & !3;
        let idx = off & 3;
        let mut regs = self.regs.lock().unwrap();
        let long = Self::raw_get(&regs, aligned);
        let shift = (3 - idx) * 8;
        let new_long = (long & !(0xFFu32 << shift)) | ((val as u32) << shift);
        Self::raw_set(&mut regs, aligned, new_long);
    }
}

impl Default for Scu {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_values_match_hardware_reference_table() {
        // §0.1 -- every field, independently derived from the hardware
        // reference's own table, not from this file's implementation.
        let scu = Scu::new();
        assert_eq!(scu.read_long(0x00), 0, "D0R");
        assert_eq!(scu.raw_read_long(0x0C), 0x101, "D0AD");
        assert_eq!(scu.raw_read_long(0x14), 0x7, "D0MD");
        assert_eq!(scu.raw_read_long(0x2C), 0x101, "D1AD");
        assert_eq!(scu.raw_read_long(0x34), 0x7, "D1MD");
        assert_eq!(scu.raw_read_long(0x4C), 0x101, "D2AD");
        assert_eq!(scu.raw_read_long(0x54), 0x7, "D2MD");
        assert_eq!(scu.raw_read_long(0x10), 0, "D0EN");
        assert_eq!(scu.raw_read_long(0x60), 0, "DSTP");
        assert_eq!(scu.read_long(0x7C), 0, "DSTA");
        assert_eq!(scu.raw_read_long(0x90), 0, "T0C");
        assert_eq!(scu.raw_read_long(0x94), 0, "T1S");
        assert_eq!(scu.raw_read_long(0x98), 0, "T1MD");
        assert_eq!(scu.raw_read_long(0xA0), 0xBFFF, "IMS");
        assert_eq!(scu.read_long(0xA4), 0, "IST");
        assert_eq!(scu.read_long(0xA8), 0, "AIACK");
        assert_eq!(scu.raw_read_long(0xB0), 0, "ASR0");
        assert_eq!(scu.raw_read_long(0xB4), 0, "ASR1");
        assert_eq!(scu.raw_read_long(0xB8), 0, "AREF");
        assert_eq!(scu.read_long(0xC4), 0, "RSEL");
        assert_eq!(scu.read_long(0xC8), 0x04, "VER");
        assert!(!scu.dsp.lock().unwrap().is_executing(), "DSP starts idle");
    }

    #[test]
    fn register_file_mirrors_every_256_bytes() {
        // §1.1: mask & 0xFF -> the 256-byte file mirrors 256x across the
        // 64KiB page. Confirmed via the long-write/long-read pair on D0R.
        let scu = Scu::new();
        scu.write_long(0x00, 0xCAFEBABE);
        assert_eq!(scu.read_long(0x100), 0xCAFEBABE);
        assert_eq!(scu.read_long(0xFF00), 0xCAFEBABE);
    }

    #[test]
    fn version_register_reads_4_and_rejects_writes() {
        let scu = Scu::new();
        assert_eq!(scu.read_long(0xC8), 0x04);
        scu.write_long(0xC8, 0xFFFFFFFF);
        assert_eq!(scu.read_long(0xC8), 0x04, "VER must stay hardwired to 4");
    }

    #[test]
    fn dnad_dnen_dnmd_have_no_long_read_handler_but_do_store() {
        // Deviation #19: reading D0AD/D0EN/D0MD returns 0 regardless of
        // what was written -- but the write really did land, observable
        // only through raw_read_long (the internal-engine accessor).
        let scu = Scu::new();
        scu.write_long(0x0C, 0x1234);
        scu.write_long(0x10, 1);
        scu.write_long(0x14, 0x0700_0007);
        assert_eq!(scu.read_long(0x0C), 0, "D0AD has no read handler");
        assert_eq!(scu.read_long(0x10), 0, "D0EN has no read handler");
        assert_eq!(scu.read_long(0x14), 0, "D0MD has no read handler");
        assert_eq!(scu.raw_read_long(0x0C), 0x1234);
        assert_eq!(scu.raw_read_long(0x10), 1);
        assert_eq!(scu.raw_read_long(0x14), 0x0700_0007);
    }

    #[test]
    fn dsta_busy_bits_recompute_live_from_dma_state() {
        let scu = Scu::new();
        assert_eq!(scu.read_long(0x7C), 0);
        scu.dma.lock().unwrap()[0].transfer_number = 5;
        assert_eq!(scu.read_long(0x7C) & 0x0010, 0x0010, "level 0 busy bit");
        scu.dma.lock().unwrap()[0].transfer_number = 0;
        scu.dma.lock().unwrap()[1].transfer_number = 3;
        assert_eq!(scu.read_long(0x7C) & 0x0100, 0x0100, "level 1 busy bit");
        scu.dma.lock().unwrap()[1].transfer_number = 0;
        scu.dma.lock().unwrap()[2].transfer_number = 1;
        assert_eq!(scu.read_long(0x7C) & 0x1000, 0x1000, "level 2 busy bit");
    }

    #[test]
    fn ist_byte_write_is_and_only_clear() {
        // §1.4: byte write to 0xA7 ANDs the low byte of IST -- writing
        // 0xFFFF_FFFF then clearing only bit 0 via the byte port must leave
        // the other bits set. Seeded via `raw_write_long` (a plain store)
        // rather than the CPU-facing `write_long`, since Phase 3 makes the
        // latter itself AND-only (§4.4) -- ANDing `0xFFFF_FFFF` against a
        // freshly-reset `IST` of `0` would just stay `0`.
        let scu = Scu::new();
        scu.raw_write_long(0xA4, 0xFFFF_FFFF);
        scu.write_byte(0xA7, 0xFE); // clear bit 0 only
        assert_eq!(scu.read_byte(0xA7), 0xFE);
        assert_eq!(scu.raw_read_long(0xA4), 0xFFFF_FFFE);
        scu.write_byte(0xA7, 0xFF); // writing 1s must not set anything back
        assert_eq!(scu.raw_read_long(0xA4), 0xFFFF_FFFE);
    }

    #[test]
    fn byte_fallback_reads_and_writes_individual_bytes_big_endian() {
        // deviation #2: every other byte offset is a real (permissive)
        // per-byte view of the actual register, not a hard 0.
        let scu = Scu::new();
        scu.write_byte(0x00, 0x11);
        scu.write_byte(0x01, 0x22);
        scu.write_byte(0x02, 0x33);
        scu.write_byte(0x03, 0x44);
        assert_eq!(scu.raw_read_long(0x00), 0x1122_3344);
        assert_eq!(scu.read_byte(0x00), 0x11);
        assert_eq!(scu.read_byte(0x01), 0x22);
        assert_eq!(scu.read_byte(0x02), 0x33);
        assert_eq!(scu.read_byte(0x03), 0x44);
    }

    // ---- Phase 3 (scu.md): the interrupt controller ----

    /// Test-only helper: wire a fresh, externally-held queue as `scu`'s
    /// master target so the test can inspect exactly what got delivered,
    /// without needing a `Sh2` at all.
    fn wire_master(scu: &Scu) -> Arc<Mutex<crate::sh2::InterruptQueue>> {
        let target = Arc::new(Mutex::new(crate::sh2::InterruptQueue::new()));
        scu.set_master_target(target.clone());
        target
    }

    #[test]
    fn source_table_matches_hardware_reference_section_4_1() {
        // Every tuple independently derived from §4.1's table (extracted
        // from `yabause/src/scu.c:3236-3481`, a different codebase from
        // this one) -- not from this file's own implementation.
        let cases: &[(&str, fn(&Scu), u8, u8, u32)] = &[
            ("vblank_in", Scu::vblank_in, 0x40, 15, 0x0001),
            ("vblank_out", Scu::vblank_out, 0x41, 14, 0x0002),
            ("hblank_in", Scu::hblank_in, 0x42, 13, 0x0004),
            ("timer0", Scu::timer0, 0x43, 12, 0x0008),
            ("timer1", Scu::timer1, 0x44, 11, 0x0010),
            ("dsp_end", Scu::dsp_end, 0x45, 10, 0x0020),
            ("sound_request", Scu::sound_request, 0x46, 9, 0x0040),
            ("system_manager", Scu::system_manager, 0x47, 8, 0x0080),
            ("pad", Scu::pad, 0x48, 8, 0x0100),
            ("level2_dma_end", Scu::level2_dma_end, 0x49, 6, 0x0200),
            ("level1_dma_end", Scu::level1_dma_end, 0x4A, 6, 0x0400),
            ("level0_dma_end", Scu::level0_dma_end, 0x4B, 5, 0x0800),
            ("dma_illegal", Scu::dma_illegal, 0x4C, 3, 0x1000),
            ("draw_end", Scu::draw_end, 0x4D, 2, 0x2000),
        ];
        for &(name, f, vector, level, mask) in cases {
            let scu = Scu::new();
            let target = wire_master(&scu);
            // §0.1: IMS resets to 0xBFFF -- every real source masked by
            // default. Unmask everything first so this test exercises the
            // (vector, level) tuple itself, not the masking behavior
            // (covered separately below and by the masked-latch tests).
            scu.write_long(0xA0, 0x0000);
            f(&scu);
            let q = target.lock().unwrap();
            assert_eq!(
                q.pending.len(),
                1,
                "{name}: expected exactly one delivered interrupt"
            );
            assert_eq!(q.pending[0].vector, vector, "{name}: wrong vector");
            assert_eq!(q.pending[0].level, level, "{name}: wrong level");
            drop(q);
            // Re-derive the mask/statusbit relationship independently: an
            // unmasked send must leave IST untouched regardless of what the
            // mask bit is, so masking that exact bit and re-sending must
            // now latch it -- proving `send`'s mask argument really is
            // `mask`, not some other bit pattern.
            let scu2 = Scu::new();
            let _t2 = wire_master(&scu2);
            scu2.write_long(0xA0, mask);
            f(&scu2);
            assert_eq!(
                scu2.read_long(0xA4) & mask,
                mask,
                "{name}: masking bit {mask:#06X} must latch its own IST status bit"
            );
        }
    }

    #[test]
    fn external_source_table_matches_section_4_1() {
        let expected_levels = [7u8, 7, 7, 7, 4, 4, 4, 4, 1, 1, 1, 1, 1, 1, 1, 1];
        for n in 0..16u8 {
            let scu = Scu::new();
            let target = wire_master(&scu);
            scu.write_long(0xA0, 0x0000); // unmask (§0.1's reset masks everything)
            scu.write_long(0xA8, 1); // arm AIACK -- unarmed, externals are dropped (§4.2)
            scu.external(n);
            let q = target.lock().unwrap();
            assert_eq!(q.pending.len(), 1, "external {n}");
            assert_eq!(q.pending[0].vector, 0x50 + n, "external {n} vector");
            assert_eq!(
                q.pending[0].level, expected_levels[n as usize],
                "external {n} level"
            );
        }
    }

    #[test]
    fn masked_external_interrupt_is_lost_not_queued() {
        // §4.2: unlike the non-external `else` branch, a *masked* external
        // interrupt is not staged in `ScuIrq`'s own queue or latched into
        // IST at all -- it still consumes AIACK and then simply vanishes.
        // Real hardware's IST bits 16-31 exist as a bit-position label in
        // §4.1's table, but this dispatch path never actually sets them.
        let scu = Scu::new();
        let target = wire_master(&scu);
        scu.write_long(0xA0, 0x8000); // mask all externals
        scu.write_long(0xA8, 1); // arm AIACK
        scu.external(0);
        assert!(
            target.lock().unwrap().pending.is_empty(),
            "a masked external interrupt must not be delivered"
        );
        assert_eq!(
            scu.read_long(0xA4) & 0x0001_0000,
            0,
            "a masked external interrupt must not latch IST either -- it's simply lost"
        );
        assert_eq!(scu.read_long(0xA8), 0, "AIACK is still consumed either way");
    }

    #[test]
    fn masked_interrupt_latches_ist_and_dedupes_then_delivers_once_unmasked() {
        // §4.2 consequence 1 + §4.3's dedupe.
        let scu = Scu::new();
        let target = wire_master(&scu);
        scu.write_long(0xA0, 0x0001); // mask VBlank-IN
        scu.vblank_in();
        assert!(
            target.lock().unwrap().pending.is_empty(),
            "a masked interrupt must not reach the SH-2 queue"
        );
        assert_eq!(scu.read_long(0xA4) & 0x0001, 0x0001, "IST bit 0 must latch");

        scu.vblank_in(); // sent again while still masked -- must dedupe
        assert!(target.lock().unwrap().pending.is_empty());

        scu.write_long(0xA0, 0x0000); // unmask -> drain runs
        let q = target.lock().unwrap();
        assert_eq!(q.pending.len(), 1, "exactly one delivery, not two");
        assert_eq!(q.pending[0].vector, 0x40);
        drop(q);
        assert_eq!(
            scu.read_long(0xA4) & 0x0001,
            0,
            "IST bit 0 must clear on delivery"
        );
    }

    #[test]
    fn unmasked_interrupt_delivers_without_latching_ist() {
        // §4.2 consequence 1's other half -- the asymmetry the reference
        // itself calls the single most surprising part of this design.
        let scu = Scu::new();
        let target = wire_master(&scu);
        scu.write_long(0xA0, 0x0000); // nothing masked
        scu.vblank_in();
        assert_eq!(target.lock().unwrap().pending.len(), 1);
        assert_eq!(
            scu.read_long(0xA4) & 0x0001,
            0,
            "an interrupt that never had to wait must not latch IST"
        );
    }

    #[test]
    fn external_interrupt_dropped_when_aiack_is_zero() {
        // §4.2 consequence 2 (deviation #16): not queued, IST untouched.
        let scu = Scu::new();
        let target = wire_master(&scu);
        scu.write_long(0xA8, 0); // AIACK = 0
        scu.external(0);
        assert!(target.lock().unwrap().pending.is_empty());
        assert_eq!(scu.read_long(0xA4) & 0x0001_0000, 0);
    }

    #[test]
    fn level_ordering_drains_highest_first() {
        let scu = Scu::new();
        let target = wire_master(&scu);
        // Mask Timer1 (11), VBlank-IN (15) and Draw End (2) together.
        scu.write_long(0xA0, 0x0001 | 0x0010 | 0x2000);
        scu.timer1();
        scu.vblank_in();
        scu.draw_end();
        assert!(target.lock().unwrap().pending.is_empty());

        scu.write_long(0xA0, 0x0000); // unmask all at once
        let q = target.lock().unwrap();
        assert_eq!(
            q.pending.len(),
            1,
            "the drain delivers at most one non-external interrupt per write"
        );
        assert_eq!(
            q.pending[0].vector, 0x40,
            "VBlank-IN (level 15) must drain before Timer1 (11) or Draw End (2)"
        );
    }

    #[test]
    fn stale_entry_is_removed_when_the_cpu_clears_its_ist_bit_by_hand() {
        // The deliberate divergence from the reference's dead-code
        // `ScuRemoveInterruptByCPU` (deviation #17): under the reference's
        // actual (buggy) behavior this entry would leak in the queue
        // forever; here it's actually removed.
        let scu = Scu::new();
        let target = wire_master(&scu);
        scu.write_long(0xA0, 0x0001);
        scu.vblank_in();
        assert_eq!(scu.read_long(0xA4) & 0x0001, 0x0001);

        // CPU clears the status bit by hand without unmasking.
        scu.write_long(0xA4, 0xFFFF_FFFE); // AND-clear bit 0 only
        assert_eq!(scu.read_long(0xA4) & 0x0001, 0);

        // Now unmask -- if the stale entry had survived, this would
        // deliver it. It must not.
        scu.write_long(0xA0, 0x0000);
        assert!(
            target.lock().unwrap().pending.is_empty(),
            "a queue entry whose IST bit was cleared by hand must not survive to be delivered"
        );
    }

    #[test]
    fn slave_mirror_fires_regardless_of_master_mask_state() {
        let scu = Scu::new();
        let master = wire_master(&scu);
        let slave = Arc::new(Mutex::new(crate::sh2::InterruptQueue::new()));
        scu.set_slave_target(slave.clone());

        scu.write_long(0xA0, 0x0001); // mask VBlank-IN on the master
        scu.vblank_in();
        assert!(
            master.lock().unwrap().pending.is_empty(),
            "master delivery is masked"
        );
        let sq = slave.lock().unwrap();
        assert_eq!(sq.pending.len(), 1, "slave mirror must still fire");
        assert_eq!(
            sq.pending[0].vector, 0x43,
            "VBlank-IN mirrors to slave vector 0x43"
        );
        assert_eq!(sq.pending[0].level, 2);
    }

    #[test]
    fn hblank_in_mirrors_to_slave_vector_0x41_level_1() {
        let scu = Scu::new();
        let _master = wire_master(&scu);
        let slave = Arc::new(Mutex::new(crate::sh2::InterruptQueue::new()));
        scu.set_slave_target(slave.clone());
        scu.hblank_in();
        let sq = slave.lock().unwrap();
        assert_eq!(sq.pending.len(), 1);
        assert_eq!(sq.pending[0].vector, 0x41);
        assert_eq!(sq.pending[0].level, 1);
    }

    #[test]
    fn no_slave_target_wired_is_a_silent_no_op() {
        // A bare `Scu::new()` (no `set_slave_target` call) must not panic
        // when a mirrorable vector fires.
        let scu = Scu::new();
        let _master = wire_master(&scu);
        scu.vblank_in();
        scu.hblank_in();
    }

    // ---- Phase 4: DMA engine (`docs/implementation-plans/scu.md` Phase 4,
    // `docs/hardware-reference/scu.md` §2). ----

    /// Big-endian long write into Low WRAM (`0x0020_0000`-`0x002F_FFFF`),
    /// the region every test below uses for source/destination/descriptor
    /// data -- avoids repeating the offset arithmetic at every call site.
    fn write_lr(work_ram: &WorkRam, addr: u32, val: u32) {
        let off = (addr - 0x0020_0000) as usize;
        let mut ram = work_ram.low_ram.write().unwrap();
        ram[off..off + 4].copy_from_slice(&val.to_be_bytes());
    }

    fn read_lr(work_ram: &WorkRam, addr: u32) -> u32 {
        let off = (addr - 0x0020_0000) as usize;
        let ram = work_ram.low_ram.read().unwrap();
        u32::from_be_bytes(ram[off..off + 4].try_into().unwrap())
    }

    /// Mirrors the real two-step production sequence
    /// (`Sh2::write_long`'s `ScuRegs` arm): the CPU write stores the raw
    /// `DnEN` value first (`Scu::write_long`), *then* `request_dma_trigger`
    /// decides whether it actually starts anything. Calling
    /// `request_dma_trigger` alone (skipping the store) would leave `DnEN`
    /// un-set even though a real write always does both.
    fn trigger_dma(scu: &Scu, level: usize) -> bool {
        scu.write_long(level * 0x20 + 0x10, 1);
        scu.request_dma_trigger(level, 1)
    }

    #[test]
    fn immediate_trigger_requires_mode_3to0_equal_7_regression_guard_d_dma_7() {
        // The pre-Phase-4 stand-in triggered on any DnEN bit-0 write,
        // regardless of DnMD -- D-DMA-7. §2.2 requires DnMD[2:0] == 7.
        let scu = Scu::new();
        scu.write_long(0x14, 0x0); // D0MD = 0 -- not the immediate factor code
        assert!(
            !scu.request_dma_trigger(0, 1),
            "DnEN bit 0 must not trigger unless DnMD[2:0] == 7"
        );
        assert!(!scu.dma_active());

        scu.write_long(0x14, 0x7);
        assert!(scu.request_dma_trigger(0, 1));
        assert!(scu.dma_active());
    }

    #[test]
    fn count_clamp_matches_hardware_reference_table() {
        // §1.2, independently derived: level 0 keeps the full 32 bits (0
        // means 0x100000); levels 1/2 clamp to 12 bits (0 means 0x1000).
        assert_eq!(Scu::clamp_direct_count(0, 0), 0x0010_0000);
        assert_eq!(Scu::clamp_direct_count(0, 0x1234), 0x1234);
        assert_eq!(Scu::clamp_direct_count(1, 0), 0x1000);
        assert_eq!(Scu::clamp_direct_count(1, 0x1_2345), 0x345);
        assert_eq!(Scu::clamp_direct_count(2, 0), 0x1000);
        assert_eq!(Scu::clamp_direct_count(2, 0x1_2345), 0x345);
    }

    #[test]
    fn count_clamp_of_zero_takes_effect_on_a_real_trigger() {
        let scu = Scu::new();
        let work_ram = WorkRam::new();
        scu.write_long(0x20, 0x0020_0000); // D1R
        scu.write_long(0x24, 0x0020_1000); // D1W
        scu.write_long(0x28, 0); // D1C = 0 -> clamps to 0x1000 (level 1)
        scu.write_long(0x2C, 0x102); // D1AD: copy mode, write_add = 4
        scu.write_long(0x34, 0x7); // D1MD: direct, factor = 7
        assert!(trigger_dma(&scu, 1));
        let arbiter = BusArbiter::new();
        // §2.2: a fresh trigger also runs its own immediate 128-unit burst
        // (`service_trigger`) *in addition to* whatever budget this call
        // passes in -- so this one `step_dma_pass` call consumes
        // `(128 + 10)` iterations of 4 bytes each, not just 10.
        scu.step_dma_pass(&work_ram, &arbiter, 10);
        let remaining = scu.dma.lock().unwrap()[1].transfer_number;
        assert_eq!(
            remaining,
            0x1000 - (128 + 10) * 4,
            "level 1's DnC=0 must clamp to 0x1000, not 0"
        );
    }

    #[test]
    fn direct_copy_hand_derived_stride_from_write_add_table() {
        // §1.2's write_add table, independently derived: DnAD[2:0] == 3 ->
        // write_add = 8 bytes. A stride wider than the natural 4-byte copy
        // unit makes the gaps this test checks for meaningful -- a bug
        // that instead advanced by the natural width would silently pass a
        // same-stride test.
        let scu = Scu::new();
        let work_ram = WorkRam::new();
        let src = 0x0020_0000u32;
        let dst = 0x0020_1000u32;
        let generator = |i: u32| -> u8 { ((i * 7 + 3) & 0xFF) as u8 };
        {
            let mut ram = work_ram.low_ram.write().unwrap();
            for i in 0..16u32 {
                ram[i as usize] = generator(i);
            }
        }

        scu.write_long(0x00, src); // D0R
        scu.write_long(0x04, dst); // D0W
        scu.write_long(0x08, 16); // D0C
        scu.write_long(0x0C, 0x103); // D0AD: bit8 (read_add=4) | sel=3 (write_add=8)
        scu.write_long(0x14, 0x7); // D0MD: direct, factor=7

        assert!(trigger_dma(&scu, 0));
        let arbiter = BusArbiter::new();
        scu.step_dma_pass(&work_ram, &arbiter, 128);
        assert!(
            !scu.dma_active(),
            "4 iterations of 4 bytes fit well within a 128-unit burst"
        );

        let ram = work_ram.low_ram.read().unwrap();
        let dst_off = (dst - 0x0020_0000) as usize;
        for iter in 0..4u32 {
            let s = iter * 4;
            let expected = u32::from_be_bytes([
                generator(s),
                generator(s + 1),
                generator(s + 2),
                generator(s + 3),
            ]);
            let got_off = dst_off + (iter * 8) as usize;
            let got = u32::from_be_bytes(ram[got_off..got_off + 4].try_into().unwrap());
            assert_eq!(got, expected, "iteration {iter}");
        }
        for iter in 0..3u32 {
            let gap_off = dst_off + (iter * 8 + 4) as usize;
            assert_eq!(
                ram[gap_off], 0,
                "write_add=8 must leave a 4-byte gap between 4-byte writes"
            );
        }
    }

    #[test]
    fn fill_mode_constant_source_writes_count_over_4_copies_of_one_long() {
        let scu = Scu::new();
        let work_ram = WorkRam::new();
        let src = 0x0020_0000u32; // Low WRAM -- a constant-source region (§2.4)
        let dst = 0x0020_2000u32;
        write_lr(&work_ram, src, 0xDEAD_BEEF);

        scu.write_long(0x00, src);
        scu.write_long(0x04, dst);
        scu.write_long(0x08, 16); // 4 copies of one long
        scu.write_long(0x0C, 0x002); // bit8 clear = fill mode; write_add sel=2 -> 4
        scu.write_long(0x14, 0x7);

        assert!(trigger_dma(&scu, 0));
        let arbiter = BusArbiter::new();
        scu.step_dma_pass(&work_ram, &arbiter, 128);
        assert!(!scu.dma_active());

        for copy in 0..4u32 {
            assert_eq!(
                read_lr(&work_ram, dst + copy * 4),
                0xDEAD_BEEF,
                "copy {copy}"
            );
        }
    }

    #[test]
    fn fill_mode_constant_source_is_cached_once_not_re_read_across_passes() {
        let scu = Scu::new();
        let work_ram = WorkRam::new();
        let src = 0x0020_0000u32; // Low WRAM -- constant source
        let dst = 0x0020_4000u32;
        write_lr(&work_ram, src, 0xAAAA_AAAA);

        // A fresh trigger's own immediate burst always runs 128 iterations
        // (`service_trigger`) before any budget this test passes in --
        // use 130 total iterations so 2 remain, controllable one at a time,
        // after that embedded burst.
        scu.write_long(0x00, src);
        scu.write_long(0x04, dst);
        scu.write_long(0x08, (128 + 2) * 4);
        scu.write_long(0x0C, 0x002);
        scu.write_long(0x14, 0x7);
        assert!(trigger_dma(&scu, 0));
        let arbiter = BusArbiter::new();
        scu.step_dma_pass(&work_ram, &arbiter, 0); // only the embedded 128-unit burst runs
        assert!(
            scu.dma_active(),
            "2 iterations remain after the initial burst"
        );

        // Mutate the constant-region source *after* the cache should have
        // been primed at trigger time -- real hardware's single
        // synchronous read means this must never show up, even now.
        write_lr(&work_ram, src, 0xBBBB_BBBB);
        scu.step_dma_pass(&work_ram, &arbiter, 1); // iteration 129 (index 128)
        assert!(scu.dma_active());
        scu.step_dma_pass(&work_ram, &arbiter, 1); // iteration 130 (index 129)
        assert!(!scu.dma_active());

        assert_eq!(
            read_lr(&work_ram, dst + 128 * 4),
            0xAAAA_AAAA,
            "a constant source must stay cached at its trigger-time value, not the later mutation"
        );
        assert_eq!(read_lr(&work_ram, dst + 129 * 4), 0xAAAA_AAAA);
    }

    #[test]
    fn fill_mode_non_constant_source_is_re_read_live_every_iteration() {
        // §2.4: only Low/High WRAM, Sound RAM and VDP1/VDP2 RAM are
        // "constant" sources cached once; everything else (here, a VDP2
        // register) is a live register re-read on every iteration -- a
        // value changed between two Core-6 passes must show up.
        let scu = Scu::new();
        let work_ram = WorkRam::new();
        let src = 0x05F8_0000u32; // VDP2 regs -- NOT in the constant-source list
        let dst = 0x0020_0000u32;
        {
            let mut regs = work_ram.vdp2_regs.write().unwrap();
            regs[0..4].copy_from_slice(&0x1111_1111u32.to_be_bytes());
        }

        scu.write_long(0x00, src);
        scu.write_long(0x04, dst);
        scu.write_long(0x08, (128 + 2) * 4); // 130 iterations -- see the cached-source test above
        scu.write_long(0x0C, 0x002); // fill mode, write_add=4
        scu.write_long(0x14, 0x7);
        assert!(trigger_dma(&scu, 0));
        let arbiter = BusArbiter::new();
        scu.step_dma_pass(&work_ram, &arbiter, 0); // only the embedded 128-unit burst runs
        assert!(
            scu.dma_active(),
            "2 iterations remain after the initial burst"
        );

        scu.step_dma_pass(&work_ram, &arbiter, 1); // iteration 129 (index 128)
        assert!(scu.dma_active());
        assert_eq!(read_lr(&work_ram, dst + 128 * 4), 0x1111_1111);

        {
            let mut regs = work_ram.vdp2_regs.write().unwrap();
            regs[0..4].copy_from_slice(&0x2222_2222u32.to_be_bytes());
        }
        scu.step_dma_pass(&work_ram, &arbiter, 1); // iteration 130 (index 129)
        assert!(!scu.dma_active());
        assert_eq!(
            read_lr(&work_ram, dst + 129 * 4),
            0x2222_2222,
            "the final iteration must observe the new live value, not a cached one"
        );
    }

    #[test]
    fn fill_mode_b_bus_destination_writes_high_half_first_with_double_stride() {
        let scu = Scu::new();
        let work_ram = WorkRam::new();
        let src = 0x0020_0000u32; // constant source
        let dst = 0x05E0_0000u32; // VDP2 VRAM -- inside the B-Bus range
        write_lr(&work_ram, src, 0x1234_5678);

        scu.write_long(0x00, src);
        scu.write_long(0x04, dst);
        scu.write_long(0x08, 4); // one fill iteration
        scu.write_long(0x0C, 0x002); // fill mode, write_add sel=2 -> 4
        scu.write_long(0x14, 0x7);
        assert!(trigger_dma(&scu, 0));
        let arbiter = BusArbiter::new();
        scu.step_dma_pass(&work_ram, &arbiter, 128);
        assert!(!scu.dma_active());

        let vram = work_ram.vdp2_vram.read().unwrap();
        assert_eq!(&vram[0..2], &[0x12, 0x34], "high half written first");
        assert_eq!(
            &vram[4..6],
            &[0x56, 0x78],
            "low half lands write_add bytes later, not immediately after"
        );
        assert_eq!(&vram[2..4], &[0, 0], "the write_add gap must be untouched");
    }

    #[test]
    fn indirect_three_descriptor_chain_hand_built() {
        let scu = Scu::new();
        let work_ram = WorkRam::new();
        let table = 0x0021_0000u32;
        let s0 = 0x0022_0000u32;
        let s1 = 0x0022_1000u32;
        let s2 = 0x0022_2000u32;
        let d0 = 0x0023_0000u32;
        let d1 = 0x0023_1000u32;
        let d2 = 0x0023_2000u32;

        write_lr(&work_ram, s0, 0xAAAA_AAAA);
        write_lr(&work_ram, s1, 0xBBBB_BBBB);
        write_lr(&work_ram, s2, 0xCCCC_CCCC);

        write_lr(&work_ram, table, 4);
        write_lr(&work_ram, table + 4, d0);
        write_lr(&work_ram, table + 8, s0);
        write_lr(&work_ram, table + 12, 4);
        write_lr(&work_ram, table + 16, d1);
        write_lr(&work_ram, table + 20, s1);
        write_lr(&work_ram, table + 24, 4);
        write_lr(&work_ram, table + 28, d2);
        write_lr(&work_ram, table + 32, s2 | 0x8000_0000); // last descriptor

        scu.write_long(0xA0, 0); // unmask so the completion interrupt delivers directly
        scu.write_long(0x04, table); // D0W = descriptor table pointer
        scu.write_long(0x0C, 0x102); // D0AD: copy mode (irrelevant within one 4-byte descriptor)
        scu.write_long(0x14, 0x0100_0007); // D0MD: indirect bit + factor=7

        let master = wire_master(&scu);
        assert!(trigger_dma(&scu, 0));
        let arbiter = BusArbiter::new();
        scu.step_dma_pass(&work_ram, &arbiter, 128);
        assert!(
            !scu.dma_active(),
            "3 tiny descriptors fit well within one 128-unit burst"
        );

        assert_eq!(read_lr(&work_ram, d0), 0xAAAA_AAAA);
        assert_eq!(read_lr(&work_ram, d1), 0xBBBB_BBBB);
        assert_eq!(read_lr(&work_ram, d2), 0xCCCC_CCCC);

        let q = master.lock().unwrap();
        assert_eq!(
            q.pending.len(),
            1,
            "completion interrupt must fire exactly once, at end-of-list, not per descriptor"
        );
        assert_eq!(q.pending[0].vector, 0x4B);
        assert_eq!(q.pending[0].level, 5);
    }

    #[test]
    fn indirect_table_pointer_comes_from_dnw_not_dnr_regression_guard_d_dma_2_3() {
        // D-DMA-2/3: the pre-Phase-4 stand-in read the descriptor table
        // from DnR, not DnW. Point DnR at a well-formed but *different*
        // table (would write to `wrong_dst`) and DnW at the real one
        // (writes to `dst`) -- only DnW's table may be honored.
        let scu = Scu::new();
        let work_ram = WorkRam::new();
        let real_table = 0x0021_0000u32;
        let wrong_table = 0x0024_0000u32;
        let src = 0x0022_0000u32;
        let dst = 0x0023_0000u32;
        let wrong_dst = 0x0025_0000u32;

        write_lr(&work_ram, src, 0x1234_5678);
        write_lr(&work_ram, real_table, 4);
        write_lr(&work_ram, real_table + 4, dst);
        write_lr(&work_ram, real_table + 8, src | 0x8000_0000);
        write_lr(&work_ram, wrong_table, 4);
        write_lr(&work_ram, wrong_table + 4, wrong_dst);
        write_lr(&work_ram, wrong_table + 8, src | 0x8000_0000);

        scu.write_long(0x00, wrong_table); // D0R -- must be ignored for indirect mode
        scu.write_long(0x04, real_table); // D0W -- the real table pointer
        scu.write_long(0x0C, 0x102);
        scu.write_long(0x14, 0x0100_0007);

        assert!(trigger_dma(&scu, 0));
        let arbiter = BusArbiter::new();
        scu.step_dma_pass(&work_ram, &arbiter, 128);
        assert!(!scu.dma_active());

        assert_eq!(
            read_lr(&work_ram, dst),
            0x1234_5678,
            "DnW's table must be honored"
        );
        assert_eq!(
            read_lr(&work_ram, wrong_dst),
            0,
            "DnR's table must be ignored entirely"
        );
    }

    #[test]
    fn direct_transfer_raises_completion_interrupt_exactly_once() {
        let scu = Scu::new();
        let work_ram = WorkRam::new();
        scu.write_long(0xA0, 0); // unmask
        scu.write_long(0x40, 0x0020_0000); // D2R
        scu.write_long(0x44, 0x0020_1000); // D2W
        scu.write_long(0x48, 8); // D2C
        scu.write_long(0x4C, 0x102); // D2AD: copy mode, write_add=4
        scu.write_long(0x54, 0x7); // D2MD

        let master = wire_master(&scu);
        assert!(trigger_dma(&scu, 2));
        let arbiter = BusArbiter::new();
        scu.step_dma_pass(&work_ram, &arbiter, 128);
        assert!(!scu.dma_active());

        let q = master.lock().unwrap();
        assert_eq!(q.pending.len(), 1);
        assert_eq!(
            q.pending[0].vector, 0x49,
            "level 2 completion is vector 0x49"
        );
        assert_eq!(q.pending[0].level, 6);
    }

    #[test]
    fn dsta_bit_reflects_live_transfer_state_not_a_snapshot() {
        let scu = Scu::new();
        let work_ram = WorkRam::new();
        scu.write_long(0x00, 0x0020_0000);
        scu.write_long(0x04, 0x0020_1000);
        // A fresh trigger's own immediate burst always runs 128 iterations
        // (`service_trigger`) before any budget this test's first pass
        // passes in -- use more than that so the level is still genuinely
        // mid-transfer afterward.
        scu.write_long(0x08, (128 + 16) * 4);
        scu.write_long(0x0C, 0x102);
        scu.write_long(0x14, 0x7);
        assert!(trigger_dma(&scu, 0));
        let arbiter = BusArbiter::new();
        scu.step_dma_pass(&work_ram, &arbiter, 0); // only the embedded 128-unit burst runs
        assert_ne!(
            scu.read_long(0x7C) & 0x0010,
            0,
            "DSTA bit 4 must be set while level 0 is mid-transfer"
        );
        assert!(scu.dma_active());

        scu.step_dma_pass(&work_ram, &arbiter, 128); // finish the remaining 16 iterations
        assert!(!scu.dma_active());
        assert_eq!(
            scu.read_long(0x7C) & 0x0010,
            0,
            "DSTA bit 4 must clear once the transfer completes"
        );
    }

    #[test]
    fn immediate_trigger_leaves_den_bit_set_real_hardware_never_auto_clears_it() {
        // §1.2: "The whole written value is stored to DnEN after the
        // go-check" -- only the factor-triggered path (b), not wired until
        // Phase 6, clears DnEN afterward. The pre-Phase-4 stand-in
        // incorrectly cleared it unconditionally on every trigger; this
        // guards against that regression.
        let scu = Scu::new();
        let work_ram = WorkRam::new();
        scu.write_long(0x00, 0x0020_0000);
        scu.write_long(0x04, 0x0020_1000);
        scu.write_long(0x08, 4);
        scu.write_long(0x0C, 0x102);
        scu.write_long(0x14, 0x7);
        assert!(trigger_dma(&scu, 0));
        let arbiter = BusArbiter::new();
        scu.step_dma_pass(&work_ram, &arbiter, 128);
        assert!(!scu.dma_active());
        assert_eq!(
            scu.raw_read_long(0x10),
            1,
            "DnEN must retain its written value after an immediate transfer"
        );
    }

    #[test]
    fn retrigger_while_busy_flushes_all_three_levels_to_completion_first() {
        let scu = Scu::new();
        let work_ram = WorkRam::new();
        // 138 iterations total (552 bytes): a fresh trigger's own immediate
        // burst always runs 128 of them (`service_trigger`), leaving 10
        // genuinely in-flight after the first (budget=0) pass below.
        const ITERS: usize = 128 + 10;
        {
            let mut ram = work_ram.low_ram.write().unwrap();
            for (i, byte) in ram.iter_mut().take(ITERS * 4).enumerate() {
                *byte = (i as u8).wrapping_add(1);
            }
        }

        scu.write_long(0x00, 0x0020_0000);
        scu.write_long(0x04, 0x0020_1000);
        scu.write_long(0x08, (ITERS * 4) as u32);
        scu.write_long(0x0C, 0x102);
        scu.write_long(0x14, 0x7);
        assert!(trigger_dma(&scu, 0));
        let arbiter = BusArbiter::new();
        scu.step_dma_pass(&work_ram, &arbiter, 0); // only the embedded 128-unit burst runs
        assert!(
            scu.dma_active(),
            "10 iterations remain after the initial burst"
        );
        assert_ne!(scu.dma.lock().unwrap()[0].transfer_number, 0);

        // Re-trigger level 0 while it's still busy, with fresh parameters.
        write_lr(&work_ram, 0x0020_2000, 0xFFEE_DDCC);
        scu.write_long(0x00, 0x0020_2000);
        scu.write_long(0x04, 0x0020_3000);
        scu.write_long(0x08, 4);
        assert!(trigger_dma(&scu, 0));
        scu.step_dma_pass(&work_ram, &arbiter, 128);
        assert!(
            !scu.dma_active(),
            "the old transfer must have been flushed to completion, plus the new one finished"
        );

        // The original transfer's remaining 10 iterations must have been
        // flushed, not abandoned mid-way -- check its very last one.
        let ram = work_ram.low_ram.read().unwrap();
        let last_iter = ITERS - 1;
        let dst_off = 0x1000usize + last_iter * 4;
        let src_off = last_iter * 4;
        let expected: Vec<u8> = (src_off..src_off + 4)
            .map(|i: usize| (i as u8).wrapping_add(1))
            .collect();
        assert_eq!(&ram[dst_off..dst_off + 4], expected.as_slice());
        drop(ram);
        // The new (re-triggered) transfer's data landed too.
        assert_eq!(read_lr(&work_ram, 0x0020_3000), 0xFFEE_DDCC);
    }

    // ---- Phase 5: Timers 0 and 1 (`docs/implementation-plans/scu.md`
    // Phase 5, `docs/hardware-reference/scu.md` §5). ----

    #[test]
    fn hand_traced_scanline_sequence_timer0_fires_once_at_line_100() {
        let scu = Scu::new();
        scu.write_long(0xA0, 0); // unmask so the completion interrupt delivers directly
        scu.write_long(0x98, 1); // T1MD: global timer enable
        scu.write_long(0x90, 100); // T0C = 100
        let master = wire_master(&scu);
        for _ in 0..263 {
            scu.hblank_in();
        }
        scu.vblank_out();
        let q = master.lock().unwrap();
        let timer0_hits = q.pending.iter().filter(|p| p.vector == 0x43).count();
        assert_eq!(
            timer0_hits, 1,
            "Timer 0 must fire exactly once across a full 263-line scanline sequence"
        );
    }

    #[test]
    fn t0c_zero_fires_timer0_at_vblank_out() {
        let scu = Scu::new();
        scu.write_long(0xA0, 0);
        scu.write_long(0x98, 1); // T1MD enable
        scu.write_long(0x90, 0); // T0C = 0
        let master = wire_master(&scu);
        scu.vblank_out();
        let q = master.lock().unwrap();
        // `vblank_out()` itself also delivers its own vector 0x41 -- so
        // this asserts Timer 0's vector specifically, not the total count.
        assert_eq!(q.pending.len(), 2, "V-Blank OUT itself, plus Timer 0");
        let timer0 = q.pending.iter().find(|p| p.vector == 0x43);
        assert!(
            timer0.is_some(),
            "Timer 0 must fire when T0C == 0 at V-Blank OUT"
        );
        assert_eq!(timer0.unwrap().level, 12);
    }

    #[test]
    fn t1md_global_enable_clear_disables_both_timers() {
        let scu = Scu::new();
        scu.write_long(0xA0, 0);
        // T1MD left at its reset value (0) -- bit 0 clear.
        scu.write_long(0x90, 0); // T0C = 0 -- would fire at V-Blank OUT if enabled
        scu.write_long(0x94, 1); // T1S = 1 -- would expire almost immediately if enabled
        let master = wire_master(&scu);
        scu.hblank_in();
        scu.vblank_out();
        scu.timer1_tick(1000); // plenty of cycles -- must still not fire
        let q = master.lock().unwrap();
        assert!(
            q.pending
                .iter()
                .all(|p| p.vector != 0x43 && p.vector != 0x44),
            "no timer interrupt may fire while T1MD bit 0 is clear"
        );
    }

    #[test]
    fn t1md_bit7_set_gates_timer1_on_timer0_match_intended_reading_deviation_18() {
        let scu = Scu::new();
        scu.write_long(0xA0, 0);
        scu.write_long(0x90, 0xFFFF_FFFF); // T0C effectively unreachable at first
        scu.write_long(0x98, 0x81); // T1MD: bit0 enable + bit7 set (mode 1)
        scu.write_long(0x94, 4); // T1S = 4 -- arm Timer 1 with a tiny preset
        let master = wire_master(&scu);

        scu.hblank_in(); // timer0 -> 1 (no T0C match); reloads timer1_counter = 4
        scu.timer1_tick(64); // 64 >> 2 = 16 >= 4 -- expires, but Timer 0 didn't match
        {
            let q = master.lock().unwrap();
            assert!(
                q.pending.iter().all(|p| p.vector != 0x44),
                "Timer 1 must not fire: Timer 0 did not match on this line"
            );
        }

        scu.write_long(0x90, 2); // T0C = 2 -- matches on the next H-Blank IN
        scu.write_long(0x94, 4); // re-arm Timer 1
        scu.hblank_in(); // timer0 -> 2 == T0C -> timer0_set = true; also reloads timer1
        scu.timer1_tick(64); // expires again -- timer0_set is now true, must fire
        let q = master.lock().unwrap();
        assert!(
            q.pending.iter().any(|p| p.vector == 0x44),
            "Timer 1 must fire once Timer 0 matched on the same line"
        );
    }

    #[test]
    fn t1md_bit7_clear_fires_timer1_every_expiry_regardless_of_timer0() {
        let scu = Scu::new();
        scu.write_long(0xA0, 0);
        scu.write_long(0x90, 0xFFFF_FFFF); // Timer 0 never matches
        scu.write_long(0x98, 0x01); // T1MD: bit0 enable, bit7 clear (mode 0)
        scu.write_long(0x94, 4);
        let master = wire_master(&scu);
        scu.hblank_in(); // reload
        scu.timer1_tick(64); // expire
        let q = master.lock().unwrap();
        assert!(
            q.pending.iter().any(|p| p.vector == 0x44),
            "with T1MD bit 7 clear, Timer 1 must fire on every expiry regardless of Timer 0"
        );
    }

    #[test]
    fn timer1_reload_rearms_at_the_next_hblank_in() {
        let scu = Scu::new();
        scu.write_long(0xA0, 0);
        scu.write_long(0x98, 0x01); // T1MD: enable, mode 0 (fire every line)
        scu.write_long(0x94, 8); // T1S = 8
        let master = wire_master(&scu);

        scu.hblank_in(); // reload: timer1_counter = 8, timer1_set cleared
        assert_eq!(scu.timers.lock().unwrap().timer1_counter, 8);
        assert!(!scu.timers.lock().unwrap().timer1_set);

        scu.timer1_tick(64); // expires -- timer1_set re-armed for the next reload
        assert!(scu.timers.lock().unwrap().timer1_set);
        {
            let q = master.lock().unwrap();
            assert!(q.pending.iter().any(|p| p.vector == 0x44));
        }

        scu.hblank_in(); // the just-armed timer1_set must reload again here
        assert!(
            !scu.timers.lock().unwrap().timer1_set,
            "the reload must consume timer1_set"
        );
        assert_eq!(
            scu.timers.lock().unwrap().timer1_counter,
            8,
            "reload must reuse the latched preset"
        );
    }

    #[test]
    fn timer1_countdown_decrements_by_cycles_shifted_right_twice_hand_derived() {
        // §5.3: timing = sh2_cycles >> 1; decrement = timing >> 1 -- i.e.
        // sh2_cycles >> 2 overall, independently re-derived here rather
        // than trusting the implementation's own two nested shifts.
        let scu = Scu::new();
        scu.write_long(0x98, 0x01);
        scu.write_long(0x94, 1000); // large preset so it won't expire mid-test
        scu.hblank_in(); // arm the reload
        scu.timer1_tick(40); // 40 >> 2 = 10
        assert_eq!(scu.timers.lock().unwrap().timer1_counter, 1000 - 10);
        scu.timer1_tick(4); // 4 >> 2 = 1
        assert_eq!(scu.timers.lock().unwrap().timer1_counter, 1000 - 10 - 1);
    }

    #[test]
    fn timer0_increments_and_timer1_reload_are_unconditional_only_the_compare_is_gated() {
        // Confirmed against `ScuSendHBlankIN`'s literal body
        // (`yabause/src/scu.c:3278-3301`): `timer0++` and the Timer 1
        // reload-arm check both run *unconditionally*; only the T0C compare
        // (and therefore `Scu::timer0()`/`timer0_set`) is gated on T1MD bit
        // 0. A first draft of this phase gated the whole thing on bit 0,
        // which would have silently frozen `timer0` whenever the CPU
        // temporarily disabled the global timer enable -- wrong per the
        // reference.
        let scu = Scu::new();
        // T1MD left at 0 (bit 0 clear).
        scu.hblank_in();
        scu.hblank_in();
        scu.hblank_in();
        assert_eq!(
            scu.timers.lock().unwrap().timer0,
            3,
            "timer0 must keep incrementing even while T1MD bit 0 is clear"
        );
    }

    #[test]
    fn advance_video_line_fires_vblank_in_at_line_225_hand_derived() {
        // Real hardware: `VBlankLineCount = 225` (`yabause/src/vdp2.cpp:515`).
        let scu = Scu::new();
        scu.write_long(0xA0, 0); // unmask
        let work_ram = WorkRam::new();
        let master = wire_master(&scu);
        for i in 1..=224u32 {
            let event = scu.advance_video_line(&work_ram);
            if i == 207 {
                assert!(
                    event == VideoLineEvent::Line207,
                    "line 207 must fire Line207"
                );
            } else {
                assert!(
                    event == VideoLineEvent::None,
                    "line {i} must not be VBlankLineCount yet"
                );
            }
        }
        assert!(!work_ram
            .vblank_active
            .load(std::sync::atomic::Ordering::Acquire));
        let entered_vblank = scu.advance_video_line(&work_ram); // line 225
        assert!(
            entered_vblank == VideoLineEvent::VBlankIn,
            "line 225 must fire V-Blank IN"
        );
        assert!(work_ram
            .vblank_active
            .load(std::sync::atomic::Ordering::Acquire));
        let q = master.lock().unwrap();
        assert!(
            q.pending.iter().any(|p| p.vector == 0x40),
            "V-Blank IN's own vector must fire"
        );
    }

    #[test]
    fn advance_video_line_fires_vblank_out_at_line_263_and_wraps_hand_derived() {
        // Real hardware: `MaxLineCount = 263` for NTSC (`yabause/src/yabause.c:1027`).
        let scu = Scu::new();
        scu.write_long(0xA0, 0);
        let work_ram = WorkRam::new();
        let master = wire_master(&scu);
        for _ in 1..=262u32 {
            scu.advance_video_line(&work_ram);
        }
        assert!(work_ram
            .vblank_active
            .load(std::sync::atomic::Ordering::Acquire));
        let entered_vblank = scu.advance_video_line(&work_ram); // line 263
        assert!(
            entered_vblank == VideoLineEvent::VBlankOut,
            "line 263 fires V-Blank OUT, not V-Blank IN"
        );
        assert!(!work_ram
            .vblank_active
            .load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(
            scu.timers.lock().unwrap().timer0,
            0,
            "the line counter must wrap to 0"
        );
        let q = master.lock().unwrap();
        assert!(
            q.pending.iter().any(|p| p.vector == 0x41),
            "V-Blank OUT's own vector must fire"
        );
    }

    // ---- Phase 6: DMA start factors, DSP End
    // (`docs/implementation-plans/scu.md` Phase 6,
    // `docs/hardware-reference/scu.md` §2.3, §3.12). ----

    /// Arms level 0 on `factor_id`, programs a tiny 4-byte direct copy,
    /// fires `source`, and asserts the transfer ran and `DnEN` was cleared
    /// to `0` (one-shot arming, §2.2). Covers every row of §2.3's table
    /// except the exclusions (DSP End, System Manager, Pad, the three
    /// DMA-end senders, DMA Illegal, the 16 externals -- none of which call
    /// `ScuChekIntrruptDMA` in the reference).
    fn assert_factor_arms_and_starts_dma(source: fn(&Scu), factor_id: u8) {
        let scu = Scu::new();
        let work_ram = WorkRam::new();
        let src = 0x0020_0000u32;
        let dst = 0x0020_1000u32;
        write_lr(&work_ram, src, 0xCAFE_BABE);
        scu.write_long(0x00, src); // D0R
        scu.write_long(0x04, dst); // D0W
        scu.write_long(0x08, 4); // D0C
        scu.write_long(0x0C, 0x102); // D0AD: copy mode, write_add = 4
        scu.write_long(0x14, factor_id as u32); // D0MD: direct, start factor = id
        scu.write_long(0x10, 0x100); // D0EN: armed (bit 8) -- *not* "go" (bit 0)
        source(&scu);
        assert!(
            scu.dma_active(),
            "factor {factor_id}: the level must be pending once the matching source fires"
        );
        let arbiter = BusArbiter::new();
        scu.step_dma_pass(&work_ram, &arbiter, 128);
        assert!(!scu.dma_active());
        assert_eq!(
            read_lr(&work_ram, dst),
            0xCAFE_BABE,
            "factor {factor_id}: the transfer must have run"
        );
        assert_eq!(
            scu.raw_read_long(0x10),
            0,
            "factor {factor_id}: DnEN must be cleared to 0 after servicing (one-shot arming)"
        );
    }

    #[test]
    fn dma_start_factor_0_vblank_in() {
        assert_factor_arms_and_starts_dma(Scu::vblank_in, 0);
    }
    #[test]
    fn dma_start_factor_1_vblank_out() {
        assert_factor_arms_and_starts_dma(Scu::vblank_out, 1);
    }
    #[test]
    fn dma_start_factor_2_hblank_in() {
        assert_factor_arms_and_starts_dma(Scu::hblank_in, 2);
    }
    #[test]
    fn dma_start_factor_3_timer0() {
        assert_factor_arms_and_starts_dma(Scu::timer0, 3);
    }
    #[test]
    fn dma_start_factor_4_timer1() {
        assert_factor_arms_and_starts_dma(Scu::timer1, 4);
    }
    #[test]
    fn dma_start_factor_5_sound_request() {
        assert_factor_arms_and_starts_dma(Scu::sound_request, 5);
    }
    #[test]
    fn dma_start_factor_6_draw_end() {
        assert_factor_arms_and_starts_dma(Scu::draw_end, 6);
    }

    #[test]
    fn masked_vblank_in_still_starts_a_factor_armed_dma() {
        // §2.3's own explicit callout, and the single most counterintuitive
        // part of this mechanism: the factor check is unconditional on
        // `IMS`. IMS is left at its default (`0xBFFF`) -- V-Blank IN's own
        // mask bit (`0x0001`) is set, so the interrupt itself is
        // masked/queued, never delivered -- but the DMA must still start.
        let scu = Scu::new();
        let work_ram = WorkRam::new();
        let src = 0x0020_0000u32;
        let dst = 0x0020_1000u32;
        write_lr(&work_ram, src, 0x1234_5678);
        scu.write_long(0x00, src);
        scu.write_long(0x04, dst);
        scu.write_long(0x08, 4);
        scu.write_long(0x0C, 0x102);
        scu.write_long(0x14, 0); // D0MD: factor 0 (V-Blank IN)
        scu.write_long(0x10, 0x100); // armed
        scu.vblank_in();
        assert!(
            scu.dma_active(),
            "the DMA must still start even though V-Blank IN's own interrupt is masked"
        );
        let arbiter = BusArbiter::new();
        scu.step_dma_pass(&work_ram, &arbiter, 128);
        assert_eq!(read_lr(&work_ram, dst), 0x1234_5678);
    }

    #[test]
    fn hblank_in_can_start_two_different_dma_levels_on_factors_2_and_3_in_one_call() {
        // Confirms the real statement order inside `ScuSendHBlankIN`:
        // Timer 0's own dispatch (and its factor-3 check) runs *before*
        // `hblank_in`'s own factor-2 check, but both must land within the
        // same call when Timer 0 matches on this exact line.
        let scu = Scu::new();
        let work_ram = WorkRam::new();
        write_lr(&work_ram, 0x0020_0000, 0xAAAA_AAAA);
        scu.write_long(0x00, 0x0020_0000); // D0R
        scu.write_long(0x04, 0x0020_1000); // D0W
        scu.write_long(0x08, 4);
        scu.write_long(0x0C, 0x102);
        scu.write_long(0x14, 2); // D0MD: factor 2 (H-Blank IN itself)
        scu.write_long(0x10, 0x100);

        scu.write_long(0x98, 1); // T1MD: global timer enable
        scu.write_long(0x90, 1); // T0C = 1 -- matches after exactly one H-Blank IN
        write_lr(&work_ram, 0x0020_2000, 0xBBBB_BBBB);
        scu.write_long(0x20, 0x0020_2000); // D1R
        scu.write_long(0x24, 0x0020_3000); // D1W
        scu.write_long(0x28, 4);
        scu.write_long(0x2C, 0x102);
        scu.write_long(0x34, 3); // D1MD: factor 3 (Timer 0)
        scu.write_long(0x30, 0x100);

        scu.hblank_in(); // timer0 -> 1 == T0C: Timer 0 fires (arms level 1); hblank_in's own factor-2 check arms level 0

        assert!(scu.dma_active());
        let arbiter = BusArbiter::new();
        scu.step_dma_pass(&work_ram, &arbiter, 128);
        assert!(!scu.dma_active());
        assert_eq!(read_lr(&work_ram, 0x0020_1000), 0xAAAA_AAAA);
        assert_eq!(read_lr(&work_ram, 0x0020_3000), 0xBBBB_BBBB);
    }

    #[test]
    fn immediate_trigger_never_sets_clear_den_after_trigger() {
        // Regression guard for the asymmetry this phase's new field exists
        // to encode: an immediate trigger (§2.2 path (a)) must never clear
        // `DnEN` after servicing, unlike a factor trigger. `DmaLevel`'s own
        // struct-literal in `snapshot_and_start` defaults the field to
        // `false`, but this asserts the *observable* behavior end to end.
        let scu = Scu::new();
        let work_ram = WorkRam::new();
        scu.write_long(0x00, 0x0020_0000);
        scu.write_long(0x04, 0x0020_1000);
        scu.write_long(0x08, 4);
        scu.write_long(0x0C, 0x102);
        scu.write_long(0x14, 0x7); // immediate
        assert!(trigger_dma(&scu, 0));
        let arbiter = BusArbiter::new();
        scu.step_dma_pass(&work_ram, &arbiter, 128);
        assert!(!scu.dma_active());
        assert_eq!(
            scu.raw_read_long(0x10),
            1,
            "an immediate trigger must leave DnEN's written value untouched"
        );
    }

    #[test]
    fn endi_raises_dsp_end_and_leaves_e_set_across_a_subsequent_control_port_write() {
        // Mirrors what Core 6 (`lib.rs`) actually does: pump `ScuDsp::step`,
        // and if it reports an `ENDI` just executed, call `Scu::dsp_end()`
        // from *outside* the `dsp` lock `step` ran under.
        let scu = Scu::new();
        scu.write_long(0xA0, 0); // unmask
        let master = wire_master(&scu);
        let work_ram = WorkRam::new();

        {
            let mut dsp = scu.dsp.lock().unwrap();
            dsp.program_ram[0] = 0xF800_0000; // End with interrupt (bit 27 set)
            dsp.write_control_port(0x0001_8000); // EX + LE, P = 0
        }
        let dsp_end = scu.dsp.lock().unwrap().step(&work_ram);
        assert!(
            dsp_end,
            "ENDI must report DSP End on the instruction it executes"
        );
        scu.dsp_end();

        let q = master.lock().unwrap();
        assert_eq!(q.pending.len(), 1);
        assert_eq!(q.pending[0].vector, 0x45);
        assert_eq!(q.pending[0].level, 10);
        drop(q);

        assert_ne!(
            scu.dsp.lock().unwrap().prog_control & 0x0004_0000,
            0,
            "E must be set right after ENDI"
        );
        // §3.9.1: E is sticky -- no Program Control Port write mask
        // includes bit 18, so only a reset clears it.
        scu.dsp.lock().unwrap().write_control_port(0x0000_0000);
        assert_ne!(
            scu.dsp.lock().unwrap().prog_control & 0x0004_0000,
            0,
            "E must stay set across a subsequent Program Control Port write"
        );
    }
}
