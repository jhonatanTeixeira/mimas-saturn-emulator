pub mod bus_arbiter;
pub mod cdrom;
pub mod cs2;
pub mod m68k;
pub mod peripheral;
pub mod scsp;
pub mod scu;
pub mod scu_dsp;
pub mod sh2;
pub mod sh2_onchip;
pub mod shared_buffers;
pub mod smpc;
pub mod sync;
pub mod telemetry;
pub mod throttle;
pub mod vdp;
pub mod vdp2_regs;

pub use bus_arbiter::BusArbiter;
pub use cdrom::Cdrom;
pub use cs2::Cs2;
pub use m68k::M68k;
pub use scsp::{Scsp, SoundRingBuffer};
pub use scu::Scu;
pub use scu_dsp::ScuDsp;
pub use sh2::Sh2;
pub use shared_buffers::{DoubleBufferedFramebuffer, Framebuffer, Vram, WorkRam};
pub use smpc::Smpc;
pub use sync::{LockStepSync, PanicGuard};
pub use throttle::{ClockThrottle, ThrottleSpeed};
pub use vdp::Vdp;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};

pub struct SaturnSystem {
    pub arbiter: Arc<BusArbiter>,
    pub work_ram: Arc<WorkRam>,
    pub vram: Arc<RwLock<Vram>>,
    pub sync: Arc<LockStepSync>,
    pub shutdown: Arc<AtomicBool>,
    pub handles: Vec<JoinHandle<()>>,
    /// Real BIOS ROM bytes, shared (cheaply, via Arc) with every CPU core.
    /// Empty until `load_bios()` is called.
    pub bios: Arc<Vec<u8>>,
    /// Master SH-2's current PC, updated after every real step so a
    /// frontend can observe genuine boot progress on a thread it no longer
    /// owns once `start()` has spawned it.
    pub cpu0_pc: Arc<AtomicU32>,
    /// Latest completed VDP2 frame, published lock-free by Core 3. A
    /// frontend (e.g. a window's present loop) just loads this on its own
    /// schedule; it never blocks or is blocked by the renderer.
    pub vdp2_frame: Arc<arc_swap::ArcSwap<vdp::Framebuffer>>,
    /// Real hardware: SMPC's SNDON/SNDOFF commands reset/halt the SCSP's
    /// onboard M68000 sound CPU. Core 0 flips this (via `Sh2::m68k_control`)
    /// when it processes those commands, *and* wakes Core 4
    /// (`sync.set_thread_active(4, true)`) at the same moment on SNDON --
    /// Core 4 (`m68k-sound-cpu`, which owns the actual `M68k` instance)
    /// parks via `LockStepSync` while this is false, exactly like Core 6
    /// parks while the DSP/DMA engine are both idle, rather than polling it
    /// in a spin loop.
    pub m68k_control: Arc<std::sync::atomic::AtomicBool>,
    /// How fast every throttled core (both SH-2s, the M68K) paces itself
    /// against real Saturn hardware clock rates -- see `throttle.rs`.
    /// Defaults to `ThrottleSpeed::Unthrottled` (today's existing
    /// as-fast-as-the-host-allows behavior; every existing verification
    /// workflow keeps working unchanged). Live-adjustable via `set_speed`
    /// while the system is running, the same way any emulator's speed
    /// slider works.
    pub speed: Arc<Mutex<ThrottleSpeed>>,
    /// The real SCU: register file, DSP, independent DMA engine, and the
    /// `IMS`/`IST`/`AIACK` interrupt controller
    /// (`docs/implementation-plans/scu.md` Phases 2-4). Core 0 reaches its
    /// register ports via `Sh2::scu`, and its `DnEN` writes only ever mark
    /// a level pending (`Scu::request_dma_trigger`); Core 6 does the real
    /// work, both for the DSP sub-lock (`Scu::dsp`) while `EX` is set and
    /// for the DMA engine (`Scu::step_dma_pass`) while any level is pending
    /// or busy (`Scu::dma_active`); Core 3 and Core 4 call its named
    /// interrupt-source methods (`vblank_in`/`vblank_out`/`sound_request`)
    /// directly, cross-thread, since `Scu`'s own interior locking makes that
    /// safe -- see `Scu::set_master_target`/`set_slave_target`, wired to
    /// `irq_in_c0`/`irq_in_c1` below. See `crate::scu` and `crate::scu_dsp`
    /// (a boot wait loop polling the DSP's Program Control Port's `EX` bit
    /// is why a real DSP interpreter was needed at all).
    pub scu: Arc<Scu>,
    pub scsp: Arc<Mutex<Scsp>>,
    /// The real SMPC command processor. Core 0 reaches it via `Sh2::smpc`
    /// (register storage stays in `WorkRam::smpc_regs`); see `crate::smpc`
    /// and `docs/implementation-plans/smpc-peripheral.md`.
    pub smpc: Arc<Mutex<Smpc>>,
    /// The real CS2 / CD-block subsystem.
    pub cs2: Arc<Mutex<Cs2>>,
    /// The master SH-2's own pending-interrupt queue
    /// (`docs/implementation-plans/sh2-cpu.md` Phase 5) -- also `scu`'s
    /// `master_target` (wired in `with_slack`), so any SCU source Core 3/4
    /// raises lands here once unmasked.
    pub irq_in_c0: Arc<Mutex<sh2::InterruptQueue>>,
    /// The slave SH-2's queue -- also `scu`'s `slave_target`, for the
    /// HBlank-IN/VBlank-IN mirrors (`docs/hardware-reference/scu.md` §4.2).
    pub irq_in_c1: Arc<Mutex<sh2::InterruptQueue>>,
    pub vdp1: Arc<Mutex<crate::vdp::Vdp1State>>,
}

impl SaturnSystem {
    pub fn new() -> Self {
        Self::with_slack(1000)
    }

    pub fn with_slack(slack_limit: u64) -> Self {
        let arbiter = Arc::new(BusArbiter::new());
        let work_ram = Arc::new(WorkRam::new());
        let vram = Arc::new(RwLock::new(Vram::new()));
        let sync = Arc::new(LockStepSync::new(8, slack_limit));
        let shutdown = Arc::new(AtomicBool::new(false));
        let irq_in_c0 = Arc::new(Mutex::new(sh2::InterruptQueue::new()));
        let irq_in_c1 = Arc::new(Mutex::new(sh2::InterruptQueue::new()));
        let scu = Arc::new(Scu::new());
        scu.set_master_target(irq_in_c0.clone());
        scu.set_slave_target(irq_in_c1.clone());
        let cs2 = Arc::new(Mutex::new(Cs2::new()));
        cs2.lock().unwrap().set_scu(scu.clone());
        scu.set_cs2(cs2.clone());

        Self {
            arbiter,
            work_ram,
            vram,
            sync,
            shutdown,
            handles: Vec::new(),
            bios: Arc::new(Vec::new()),
            cpu0_pc: Arc::new(AtomicU32::new(0)),
            vdp2_frame: Arc::new(arc_swap::ArcSwap::new(Arc::new(vdp::Framebuffer::new(
                320, 224,
            )))),
            m68k_control: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            speed: Arc::new(Mutex::new(ThrottleSpeed::Unthrottled)),
            scu,
            scsp: Arc::new(Mutex::new(Scsp::new())),
            smpc: Arc::new(Mutex::new(Smpc::new())),
            cs2,
            vdp1: Arc::new(Mutex::new(crate::vdp::Vdp1State::new())),
            irq_in_c0,
            irq_in_c1,
        }
    }

    pub fn set_pad_state(&self, port: usize, state: crate::peripheral::PadState) {
        self.smpc.lock().unwrap().set_pad_state(port, state);
    }

    /// General form of `set_pad_state` -- drives any connected peripheral's
    /// live state (wheel rotation, mouse motion, gun trigger...), not just
    /// a digital pad's.
    pub fn set_peripheral_state(&self, port: usize, state: crate::peripheral::PeripheralState) {
        self.smpc.lock().unwrap().set_peripheral_state(port, state);
    }

    pub fn set_port_peripheral(
        &mut self,
        port: usize,
        kind: Option<crate::peripheral::PeripheralKind>,
    ) {
        self.smpc.lock().unwrap().set_port_peripheral(port, kind);
    }

    pub fn press_reset_button(&mut self) {
        // §4.16: Reset button.
        let smpc = self.smpc.lock().unwrap();
        if smpc.resd {
            // Inert until the game issues RESENAB.
            return;
        }
        // Fire NMI (vector 0x0B, level 16) to Master SH-2.
        self.irq_in_c0.lock().unwrap().send(0x0B, 16);
        // Note: setting ICR bit 15 here is deferred since it's deep inside Sh2's thread,
        // but the queue delivery will at least vector it correctly.
    }

    /// Load real BIOS ROM bytes so the master SH-2 actually executes genuine
    /// boot code (from the reset vector) instead of a scaffold no-op loop.
    pub fn load_bios(&mut self, data: Vec<u8>) {
        self.bios = Arc::new(data);
    }

    /// Load a disc image into the CD-ROM drive.
    pub fn load_disc(&self, path: &str) -> Result<(), String> {
        self.cs2.lock().unwrap().load_disc(path)?;
        self.sync.set_thread_active(7, true);
        Ok(())
    }

    /// Change how fast every throttled core paces itself, live -- safe to
    /// call before or after `start()`. See `speed`'s field doc comment.
    pub fn set_speed(&self, speed: ThrottleSpeed) {
        *self.speed.lock().unwrap() = speed;
    }

    pub fn get_speed(&self) -> ThrottleSpeed {
        *self.speed.lock().unwrap()
    }

    pub fn start(&mut self) {
        let shutdown = self.shutdown.clone();
        let arbiter = self.arbiter.clone();
        let work_ram = self.work_ram.clone();
        let sync = self.sync.clone();

        // Spawn Core 0: Master SH-2
        let shutdown_c0 = shutdown.clone();
        let arbiter_c0 = arbiter.clone();
        let work_ram_c0 = work_ram.clone();
        let sync_c0 = sync.clone();
        let bios_c0 = self.bios.clone();
        let cpu0_pc = self.cpu0_pc.clone();
        let m68k_control_c0 = self.m68k_control.clone();
        let speed_c0 = self.speed.clone();
        let scu_c0 = self.scu.clone();
        let smpc_c0 = self.smpc.clone();
        let cs2_c0 = self.cs2.clone();
        let vdp1_c0 = self.vdp1.clone();
        let irq_in_c0 = self.irq_in_c0.clone();
        let handle_c0 = thread::Builder::new()
            .name("sh2-master".into())
            .spawn(move || {
                let _guard = PanicGuard::new(sync_c0.clone(), arbiter_c0.clone());
                let mut cpu = Sh2::new(false, arbiter_c0, work_ram_c0);
                cpu.sync = Some(sync_c0);
                cpu.core_id = 0;
                cpu.set_bios_arc(bios_c0);
                cpu.reset();
                cpu.pc_reporter = Some(cpu0_pc);
                cpu.m68k_control = Some(m68k_control_c0);
                cpu.speed = Some(speed_c0);
                cpu.scu = scu_c0;
                cpu.smpc = Some(smpc_c0);
                cpu.cs2 = Some(cs2_c0);
                cpu.vdp1 = Some(vdp1_c0);
                cpu.irq_in = irq_in_c0;
                cpu.run_loop(shutdown_c0);
            })
            .expect("failed to spawn Core 0 (Master SH-2) thread");
        self.handles.push(handle_c0);

        // Spawn Core 1: Slave SH-2.
        let shutdown_c1 = shutdown.clone();
        let arbiter_c1 = arbiter.clone();
        let work_ram_c1 = work_ram.clone();
        let sync_c1 = sync.clone();
        let bios_c1 = self.bios.clone();
        let speed_c1 = self.speed.clone();
        let cs2_c1 = self.cs2.clone();
        let vdp1_c1 = self.vdp1.clone();
        let irq_in_c1 = self.irq_in_c1.clone();
        let handle_c1 = thread::Builder::new()
            .name("sh2-slave".into())
            .spawn(move || {
                let _guard = PanicGuard::new(sync_c1.clone(), arbiter_c1.clone());
                sync_c1.set_thread_active(1, false);
                if !sync_c1.park_while_inactive(1) {
                    return;
                }
                let mut cpu = Sh2::new(true, arbiter_c1, work_ram_c1);
                cpu.sync = Some(sync_c1);
                cpu.core_id = 1;
                cpu.set_bios_arc(bios_c1);
                cpu.reset();
                cpu.speed = Some(speed_c1);
                cpu.cs2 = Some(cs2_c1);
                cpu.vdp1 = Some(vdp1_c1);
                cpu.irq_in = irq_in_c1;
                cpu.run_loop(shutdown_c1);
            })
            .expect("failed to spawn Core 1 (Slave SH-2) thread");
        self.handles.push(handle_c1);

        // Spawn Core 2: VDP1 Drawing Engine.
        //
        // Genuinely parked, not just idle: VDP1 command-list execution
        // actually runs on Core 3 today (`vdp::execute_vdp1`, in the frame
        // closure below), not here -- see CLAUDE.md's "Core 2 vs Core 3"
        // note. This thread has zero real work in the current
        // implementation, so it deactivates itself once and parks forever,
        // exactly like Core 1/Core 6 do before anything wakes them --
        // previously it spun on `thread::yield_now()` at ~100% of a host
        // core for a component that never does anything. Whenever a future
        // `docs/implementation-plans/vdp1.md` phase actually moves VDP1
        // execution here, it will need its own wake call (mirroring the
        // DSP's `EX` bit / DMA's `DnEN` trigger) -- there is nothing to
        // wake it with today.
        let sync_c2 = sync.clone();
        let arbiter_c2 = arbiter.clone();
        let handle_c2 = thread::Builder::new()
            .name("vdp1-draw".into())
            .spawn(move || {
                let _guard = PanicGuard::new(sync_c2.clone(), arbiter_c2);
                sync_c2.set_thread_active(2, false);
                sync_c2.park_while_inactive(2);
            })
            .expect("failed to spawn Core 2 (VDP1 Drawing Engine) thread");
        self.handles.push(handle_c2);

        // Spawn Core 3: VDP2 Compositor & Presentation Loop.
        //
        // Genuinely event-driven, not a wall-clock loop: H-Blank IN /
        // V-Blank IN / V-Blank OUT are generated by Master SH-2's own real
        // cycle progress (`Sh2::step`'s `SH2_CYCLES_PER_LINE` batching ->
        // `Scu::advance_video_line`), exactly mirroring the reference's own
        // main loop (`yabsys.LineCount`/VBlank generation tied to
        // `sh2cycles`, `yabause/src/yabause.c:762-810` -- never a host
        // clock). Core 3 parks (`LockStepSync::park_while_inactive`) until
        // Master's cycle-driven trigger calls `sync.set_thread_active(3,
        // true)` at the exact moment V-Blank IN fires, does the one frame's
        // worth of render work, and re-parks -- the only continuous loop
        // left anywhere in the system is the CPU's own (Core 0/1's
        // `run_loop`); every other component is either parked-until-woken
        // (this, and Cores 1/2/4-idle/6/7) or has continuous *real* work by
        // hardware design (Core 5's SCSP, not touched by this change).
        // Superseded design, kept as history: this used to be its own
        // ~16.6ms wall-clock spin (`Chapter 30`), then briefly a version
        // that additionally spun on purpose to keep `LockStepSync`'s
        // bounded-slack heartbeat frequent (`Chapter 32`) -- both are gone
        // now that the timing source moved to Master SH-2's real cycles,
        // which sidesteps the Chapter 32 problem entirely (a parked core
        // doesn't participate in the slack computation at all).
        let sync_c3 = sync.clone();
        let arbiter_c3 = arbiter.clone();
        let work_ram_c3 = work_ram.clone();
        let vdp1_c3 = self.vdp1.clone();
        let vdp2_frame = self.vdp2_frame.clone();
        let scu_c3 = self.scu.clone();
        let handle_c3 = thread::Builder::new()
            .name("vdp2-composite".into())
            .spawn(move || {
                let _guard = PanicGuard::new(sync_c3.clone(), arbiter_c3);
                sync_c3.set_thread_active(3, false);
                let mut cycles = 0u64;
                let mut last_logged: Option<(u16, u16)> = None;
                loop {
                    if !sync_c3.park_while_inactive(3) {
                        return;
                    }
                    {
                        let mut state = vdp1_c3.lock().unwrap();
                        if state.swap_frame_buffer {
                            crate::vdp::vdp1_swap_frame_buffers(&mut state, &work_ram_c3);
                        }
                        if crate::vdp::execute_vdp1(&mut state, &work_ram_c3) {
                            scu_c3.draw_end();
                        }
                    }
                    let frame = crate::vdp::render_back_screen(&work_ram_c3);
                    if std::env::var("MIMAS_DEBUG_VDP2").is_ok() {
                        let regs = work_ram_c3.vdp2_regs.read().unwrap();
                        let tvmd = u16::from_be_bytes([regs[0], regs[1]]);
                        let bktal = u16::from_be_bytes([regs[0xAE], regs[0xAF]]);
                        drop(regs);
                        if last_logged != Some((tvmd, bktal)) {
                            let px = frame.pixels.first().copied().unwrap_or(0);
                            eprintln!(
                                "[DEBUG VDP2] TVMD={:#06X} BKTAL={:#06X} pixel0={:#08X} res={}x{}",
                                tvmd, bktal, px, frame.width, frame.height
                            );
                            last_logged = Some((tvmd, bktal));
                        }
                    }
                    vdp2_frame.store(Arc::new(frame));
                    cycles = cycles.wrapping_add(1);
                    sync_c3.sync_core(3, cycles);
                    sync_c3.set_thread_active(3, false);
                }
            })
            .expect("failed to spawn Core 3 (VDP2 Compositor) thread");
        self.handles.push(handle_c3);

        // Spawn Core 4: MC68000 Sound CPU.
        //
        // Parks while SNDOFF, exactly like Core 6 parks while the DSP is
        // idle and no DMA level is pending: `Sh2::apply_smpc_effects` (the
        // real, wired-in path) and `smpc_execute_command` (the bare-`Sh2`
        // fallback) both call `sync.set_thread_active(4, true)` at the
        // moment they flip `m68k_control` true on SNDON. Previously this
        // thread polled `m68k_control` in a `thread::yield_now()` spin
        // regardless of whether the M68K was even running -- silent CPU
        // waste identical in shape to the per-instruction sound-request
        // poll `docs/implementation-plans/scu.md` Phase 3 already removed,
        // just on a different thread.
        let sync_c4 = sync.clone();
        let arbiter_c4 = arbiter.clone();
        let work_ram_c4 = work_ram.clone();
        let m68k_control_c4 = self.m68k_control.clone();
        let scu_c4 = self.scu.clone();
        let speed_c4 = self.speed.clone();
        let handle_c4 = thread::Builder::new()
            .name("m68k-sound-cpu".into())
            .spawn(move || {
                let _guard = PanicGuard::new(sync_c4.clone(), arbiter_c4);
                let mut cycles = 0u64;
                let mut m68k = m68k::M68k::new(work_ram_c4.clone());
                // Direct, event-driven call into the SCU on a real MCIPD
                // write (see `M68k::write_byte`) -- no per-sample or
                // per-instruction polling anywhere on this thread.
                m68k.scu = Some(scu_c4);
                // Phase 6: lets `M68k::write_byte` wake Core 6 if
                // `sound_request` armed a factor-5 DMA level.
                m68k.sync = Some(sync_c4.clone());
                let mut m68k_throttle =
                    crate::throttle::ClockThrottle::new(crate::throttle::M68K_CLOCK_HZ, speed_c4);
                sync_c4.set_thread_active(4, false);
                loop {
                    if !sync_c4.park_while_inactive(4) {
                        return;
                    }
                    // Just reactivated -- SNDON fired.
                    m68k.reset();
                    if std::env::var("MIMAS_DEBUG_M68K").is_ok() {
                        let ram = work_ram_c4.sound_ram.read().unwrap();
                        eprintln!(
                            "[M68K] reset: SP={:#010X} PC={:#010X} first16={:02X?}",
                            m68k.a[7],
                            m68k.pc,
                            &ram[0..16]
                        );
                    }
                    while m68k_control_c4.load(Ordering::Acquire) {
                        if sync_c4.is_shutdown() {
                            return;
                        }
                        for _ in 0..200 {
                            m68k.step();
                            m68k_throttle
                                .advance(crate::throttle::M68K_NOMINAL_CYCLES_PER_INSTRUCTION);
                        }
                        cycles = cycles.wrapping_add(2);
                        sync_c4.sync_core(4, cycles);
                        thread::yield_now();
                    }
                    // SNDOFF fired -- stop and re-park.
                    m68k.stop();
                    sync_c4.set_thread_active(4, false);
                }
            })
            .expect("failed to spawn Core 4 (MC68000 Sound CPU) thread");
        self.handles.push(handle_c4);

        // Spawn Core 5: SCSP Sound Synthesizer.
        //
        // Always has real work (real hardware's SCSP synthesizes
        // continuously, independent of the M68K's own run/stop state), so
        // unlike Cores 2/4/7 it can't park -- but it previously ran
        // completely unthrottled regardless of `self.speed`, spinning at
        // ~100% of a host core generating audio far faster than real time
        // for no benefit. Now paced through the same `ClockThrottle`
        // mechanism the SH-2s and M68K already use: a no-op when
        // `ThrottleSpeed::Unthrottled` (the default -- existing
        // verification workflows are unaffected), real pacing otherwise.
        let sync_c5 = sync.clone();
        let arbiter_c5 = arbiter.clone();
        let shutdown_c5 = shutdown.clone();
        let work_ram_c5 = work_ram.clone();
        let scsp_c5 = self.scsp.clone();
        let speed_c5 = self.speed.clone();
        let handle_c5 = thread::Builder::new()
            .name("scsp-synth".into())
            .spawn(move || {
                let _guard = PanicGuard::new(sync_c5.clone(), arbiter_c5);
                let mut cycles = 0u64;
                let mut scsp_throttle = crate::throttle::ClockThrottle::new(
                    crate::throttle::SCSP_SAMPLE_RATE_HZ,
                    speed_c5,
                );
                while !shutdown_c5.load(Ordering::Relaxed) {
                    if sync_c5.is_shutdown() {
                        break;
                    }
                    // Synthesize 128 audio samples per step
                    scsp_c5.lock().unwrap().synthesize(&work_ram_c5, 128);
                    scsp_throttle.advance(128);
                    let step = (sync_c5.slack_limit() / 2).max(2).min(500);
                    cycles = cycles.wrapping_add(step);
                    sync_c5.sync_core(5, cycles);
                    thread::yield_now();
                }
            })
            .expect("failed to spawn Core 5 (SCSP Sound Synthesizer) thread");
        self.handles.push(handle_c5);

        // Spawn Core 6: SCU DMA/DSP Thread. Real, independent DMA engine as
        // of `docs/implementation-plans/scu.md` Phase 4 (`Scu::step_dma_pass`),
        // replacing the old `Sh2::execute_scu_dma` stand-in that ran a
        // whole transfer synchronously inside one SH-2 register write on
        // Core 0. `DMA_BUDGET_PER_PASS` is a Mimas-specific choice, not a
        // translation of the reference's `timing << 4` (Core 6 has no
        // per-instruction SH-2 cycle bus to derive it from) -- picked small
        // enough that the bus lock (`BusArbiter`) is held only briefly per
        // pass, per Phase 4c's "per time-slice burst, not once around a
        // whole transfer" requirement.
        const DMA_BUDGET_PER_PASS: i64 = 512;
        let sync_c6 = sync.clone();
        let arbiter_c6 = arbiter.clone();
        let work_ram_c6 = work_ram.clone();
        let scu_c6 = self.scu.clone();
        let handle_c6 = thread::Builder::new()
            .name("scu-dma-dsp".into())
            .spawn(move || {
                let _guard = PanicGuard::new(sync_c6.clone(), arbiter_c6.clone());
                sync_c6.set_thread_active(6, false);
                let mut cycles = 0u64;
                loop {
                    if !sync_c6.park_while_inactive(6) {
                        return;
                    }
                    while scu_c6.dsp.lock().unwrap().is_executing() || scu_c6.dma_active() {
                        if sync_c6.is_shutdown() {
                            return;
                        }
                        if scu_c6.dsp.lock().unwrap().is_executing() {
                            // `dsp`'s lock is released before calling
                            // `dsp_end()` (which locks `regs`/`irq`),
                            // respecting this crate's own `regs`, `irq`,
                            // `dma`, `timers`, `dsp` lock-ordering rule
                            // (`docs/implementation-plans/scu.md` Phase 6).
                            let dsp_end = scu_c6.dsp.lock().unwrap().step(&work_ram_c6);
                            if dsp_end {
                                scu_c6.dsp_end();
                            }
                        }
                        if scu_c6.dma_active() {
                            scu_c6.step_dma_pass(&work_ram_c6, &arbiter_c6, DMA_BUDGET_PER_PASS);
                        }
                        let step = (sync_c6.slack_limit() / 2).max(2).min(500);
                        cycles = cycles.wrapping_add(step);
                        sync_c6.sync_core(6, cycles);
                        thread::yield_now();
                    }
                    sync_c6.set_thread_active(6, false);
                }
            })
            .expect("failed to spawn Core 6 (SCU DMA/DSP) thread");
        self.handles.push(handle_c6);

        // Spawn Core 7: SMPC & CD-ROM Thread.
        //
        // Parks while inactive; woken when a CD-ROM command is issued,
        // disc is loaded, or playback is active.
        let sync_c7 = sync.clone();
        let arbiter_c7 = arbiter.clone();
        let cs2_c7 = self.cs2.clone();
        let smpc_c7 = self.smpc.clone();
        let work_ram_c7 = work_ram.clone();
        let m68k_control_c7 = self.m68k_control.clone();
        let handle_c7 = thread::Builder::new()
            .name("smpc-cd-block".into())
            .spawn(move || {
                let _guard = PanicGuard::new(sync_c7.clone(), arbiter_c7);
                sync_c7.set_thread_active(7, false);
                let mut cycles = 0u64;
                loop {
                    if !sync_c7.park_while_inactive(7) {
                        return;
                    }
                    // Wait for an explicit wake from Master SH-2.
                    // Master SH-2 tracks cycles and wakes us exactly when the SMPC command
                    // delay expires, or at V-Blank IN.
                    let mut did_work = false;

                    let mut smpc = smpc_c7.lock().unwrap();
                    // Gate on `is_dispatch_ready`, not bare `has_work()` --
                    // this wake may have been CS2's, not ours, while a
                    // command (e.g. INTBACK's ~16ms) is still genuinely
                    // counting down. See `Smpc::dispatch_ready`'s doc
                    // comment.
                    if smpc.is_dispatch_ready() {
                        let effects = smpc.execute_expired_command(&work_ram_c7);
                        if effects.system_manager_irq {
                            work_ram_c7
                                .smpc_irq_pending
                                .store(true, std::sync::atomic::Ordering::Release);
                        }
                        if effects.nmi {
                            work_ram_c7
                                .smpc_nmi_pending
                                .store(true, std::sync::atomic::Ordering::Release);
                        }
                        if effects.system_reset {
                            work_ram_c7
                                .smpc_sysres_pending
                                .store(true, std::sync::atomic::Ordering::Release);
                        }
                        if let Some(is_352) = effects.clock_change {
                            work_ram_c7.smpc_clock_change.store(
                                if is_352 { 2 } else { 1 },
                                std::sync::atomic::Ordering::Release,
                            );
                        }
                        if effects.start_slave {
                            sync_c7.set_thread_active(1, true);
                        }
                        if effects.stop_slave {
                            sync_c7.set_thread_active(1, false);
                        }
                        if effects.sound_on {
                            m68k_control_c7.store(true, std::sync::atomic::Ordering::Release);
                            sync_c7.set_thread_active(4, true);
                        }
                        if effects.sound_off {
                            m68k_control_c7.store(false, std::sync::atomic::Ordering::Release);
                        }
                        did_work = true;
                    }
                    drop(smpc);

                    let mut cs2 = cs2_c7.lock().unwrap();
                    if cs2.command_pending {
                        cs2.execute_command();
                        cs2.command_pending = false;
                        did_work = true;
                    }
                    if cs2.vblank_pending {
                        cs2.exec_vblank();
                        cs2.vblank_pending = false;
                        did_work = true;
                    }
                    drop(cs2);

                    // We are cycle-driven: do the work we were woken to do, sync, and park again.
                    if did_work {
                        cycles = cycles.wrapping_add(100); // Nominal cost for the work done
                        sync_c7.sync_core(7, cycles);
                    }
                    sync_c7.set_thread_active(7, false);
                }
            })
            .expect("failed to spawn Core 7 (SMPC & CD-ROM) thread");
        self.handles.push(handle_c7);
    }

    pub fn shutdown(&mut self) {
        self.arbiter.abort();
        self.shutdown.store(true, Ordering::Relaxed);
        self.sync.request_shutdown();
        let mut join_errors = Vec::new();
        for handle in self.handles.drain(..) {
            if let Err(e) = handle.join() {
                join_errors.push(e);
            }
        }
        if !join_errors.is_empty() {
            let first_error = join_errors.into_iter().next().unwrap();
            std::panic::resume_unwind(first_error);
        }
    }
}

impl Default for SaturnSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SaturnSystem {
    fn drop(&mut self) {
        self.shutdown();
    }
}
