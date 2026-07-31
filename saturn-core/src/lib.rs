pub mod bus_arbiter;
pub mod shared_buffers;
pub mod sh2;
pub mod sync;
pub mod cdrom;
pub mod scu;
pub mod scu_dsp;
pub mod smpc;
pub mod vdp;
pub mod scsp;
pub mod m68k;
pub mod throttle;
pub mod telemetry;

pub use bus_arbiter::BusArbiter;
pub use shared_buffers::{WorkRam, Vram, Framebuffer, DoubleBufferedFramebuffer};
pub use sh2::Sh2;
pub use sync::{LockStepSync, PanicGuard};
pub use cdrom::Cdrom;
pub use scu::Scu;
pub use scu_dsp::ScuDsp;
pub use smpc::Smpc;
pub use vdp::Vdp;
pub use scsp::{Scsp, SoundRingBuffer};
pub use m68k::M68k;
pub use throttle::{ClockThrottle, ThrottleSpeed};

use std::sync::{Arc, Mutex, RwLock};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
    /// when it processes those commands; Core 4 (`m68k-sound-cpu`, which
    /// owns the actual `M68k` instance) reads it each loop iteration to know
    /// whether to step the M68K core.
    pub m68k_control: Arc<std::sync::atomic::AtomicBool>,
    /// Real hardware: the SCSP's M68000 requests an interrupt to the SH-2
    /// (SCU "Sound Request", vector 0x46, level 9) by writing its MCIPD
    /// register. Core 3's `M68k` sets this (see `M68k::write_byte`); Core
    /// 0's `Sh2` (via `Sh2::sound_req_irq`) observes and services it.
    pub sound_req_irq: Arc<std::sync::atomic::AtomicBool>,
    /// How fast every throttled core (both SH-2s, the M68K) paces itself
    /// against real Saturn hardware clock rates -- see `throttle.rs`.
    /// Defaults to `ThrottleSpeed::Unthrottled` (today's existing
    /// as-fast-as-the-host-allows behavior; every existing verification
    /// workflow keeps working unchanged). Live-adjustable via `set_speed`
    /// while the system is running, the same way any emulator's speed
    /// slider works.
    pub speed: Arc<Mutex<ThrottleSpeed>>,
    /// The SCU DSP (Core 2's slot). Core 0 writes/reads its register ports
    /// (via `Sh2::scu_dsp`); Core 2 actually steps it while `EX` is set.
    /// See `crate::scu_dsp` for why a real DSP interpreter was needed (a
    /// boot wait loop polling the Program Control Port's `EX` bit).
    pub scu_dsp: Arc<Mutex<ScuDsp>>,
    pub scsp: Arc<Mutex<Scsp>>,
    /// The real SMPC command processor. Core 0 reaches it via `Sh2::smpc`
    /// (register storage stays in `WorkRam::smpc_regs`); see `crate::smpc`
    /// and `docs/implementation-plans/smpc-peripheral.md`.
    pub smpc: Arc<Mutex<Smpc>>,
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

        Self {
            arbiter,
            work_ram,
            vram,
            sync,
            shutdown,
            handles: Vec::new(),
            bios: Arc::new(Vec::new()),
            cpu0_pc: Arc::new(AtomicU32::new(0)),
            vdp2_frame: Arc::new(arc_swap::ArcSwap::new(Arc::new(vdp::Framebuffer::new(320, 224)))),
            m68k_control: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sound_req_irq: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            speed: Arc::new(Mutex::new(ThrottleSpeed::Unthrottled)),
            scu_dsp: Arc::new(Mutex::new(ScuDsp::new())),
            scsp: Arc::new(Mutex::new(Scsp::new())),
            smpc: Arc::new(Mutex::new(Smpc::new())),
        }
    }

    /// Load real BIOS ROM bytes so the master SH-2 actually executes genuine
    /// boot code (from the reset vector) instead of a scaffold no-op loop.
    pub fn load_bios(&mut self, data: Vec<u8>) {
        self.bios = Arc::new(data);
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
        let sound_req_irq_c0 = self.sound_req_irq.clone();
        let speed_c0 = self.speed.clone();
        let scu_dsp_c0 = self.scu_dsp.clone();
        let smpc_c0 = self.smpc.clone();
        let handle_c0 = thread::Builder::new().name("sh2-master".into()).spawn(move || {
            let _guard = PanicGuard::new(sync_c0.clone(), arbiter_c0.clone());
            let mut cpu = Sh2::new(false, arbiter_c0, work_ram_c0);
            cpu.sync = Some(sync_c0);
            cpu.core_id = 0;
            cpu.set_bios_arc(bios_c0);
            cpu.reset();
            cpu.pc_reporter = Some(cpu0_pc);
            cpu.m68k_control = Some(m68k_control_c0);
            cpu.sound_req_irq = Some(sound_req_irq_c0);
            cpu.speed = Some(speed_c0);
            cpu.scu_dsp = Some(scu_dsp_c0);
            cpu.smpc = Some(smpc_c0);
            cpu.run_loop(shutdown_c0);
        }).expect("failed to spawn Core 0 (Master SH-2) thread");
        self.handles.push(handle_c0);

        // Spawn Core 1: Slave SH-2.
        let shutdown_c1 = shutdown.clone();
        let arbiter_c1 = arbiter.clone();
        let work_ram_c1 = work_ram.clone();
        let sync_c1 = sync.clone();
        let bios_c1 = self.bios.clone();
        let speed_c1 = self.speed.clone();
        let handle_c1 = thread::Builder::new().name("sh2-slave".into()).spawn(move || {
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
            cpu.run_loop(shutdown_c1);
        }).expect("failed to spawn Core 1 (Slave SH-2) thread");
        self.handles.push(handle_c1);

        // Spawn Core 2: VDP1 Drawing Engine.
        let sync_c2 = sync.clone();
        let arbiter_c2 = arbiter.clone();
        let shutdown_c2 = shutdown.clone();
        let handle_c2 = thread::Builder::new().name("vdp1-draw".into()).spawn(move || {
            let _guard = PanicGuard::new(sync_c2.clone(), arbiter_c2);
            let mut cycles = 0u64;
            while !shutdown_c2.load(Ordering::Relaxed) {
                if sync_c2.is_shutdown() {
                    break;
                }
                let step = (sync_c2.slack_limit() / 2).max(2).min(500);
                cycles = cycles.wrapping_add(step);
                sync_c2.sync_core(2, cycles);
                thread::yield_now();
            }
        }).expect("failed to spawn Core 2 (VDP1 Drawing Engine) thread");
        self.handles.push(handle_c2);

        // Spawn Core 3: VDP2 Compositor & Presentation Loop.
        let shutdown_c3 = shutdown.clone();
        let sync_c3 = sync.clone();
        let arbiter_c3 = arbiter.clone();
        let work_ram_c3 = work_ram.clone();
        let vdp2_frame = self.vdp2_frame.clone();
        let handle_c3 = thread::Builder::new().name("vdp2-composite".into()).spawn(move || {
            let _guard = PanicGuard::new(sync_c3.clone(), arbiter_c3);
            let mut cycles = 0u64;
            let mut next_frame_due = std::time::Instant::now();
            let frame_interval = std::time::Duration::from_micros(16_666);
            let mut last_logged: Option<(u16, u16)> = None;
            while !shutdown_c3.load(Ordering::Relaxed) {
                if sync_c3.is_shutdown() {
                    break;
                }
                let now = std::time::Instant::now();
                if now >= next_frame_due {
                    crate::vdp::execute_vdp1(&work_ram_c3);
                    let frame = crate::vdp::render_backdrop(&work_ram_c3);
                    if std::env::var("MIMAS_DEBUG_VDP2").is_ok() {
                        let regs = work_ram_c3.vdp2_regs.read().unwrap();
                        let tvmd = u16::from_be_bytes([regs[0], regs[1]]);
                        let bktal = u16::from_be_bytes([regs[0xAE], regs[0xAF]]);
                        drop(regs);
                        if last_logged != Some((tvmd, bktal)) {
                            let px = frame.pixels.first().copied().unwrap_or(0);
                            eprintln!("[DEBUG VDP2] TVMD={:#06X} BKTAL={:#06X} pixel0={:#08X} res={}x{}",
                                tvmd, bktal,
                                px, frame.width, frame.height);
                            last_logged = Some((tvmd, bktal));
                        }
                    }
                    vdp2_frame.store(Arc::new(frame));
                    next_frame_due = now + frame_interval;
                }
                let step = (sync_c3.slack_limit() / 2).max(2).min(500);
                cycles = cycles.wrapping_add(step);
                sync_c3.sync_core(3, cycles);
                thread::yield_now();
            }
        }).expect("failed to spawn Core 3 (VDP2 Compositor) thread");
        self.handles.push(handle_c3);

        // Spawn Core 4: MC68000 Sound CPU.
        let shutdown_c4 = shutdown.clone();
        let sync_c4 = sync.clone();
        let arbiter_c4 = arbiter.clone();
        let work_ram_c4 = work_ram.clone();
        let m68k_control_c4 = self.m68k_control.clone();
        let sound_req_irq_c4 = self.sound_req_irq.clone();
        let speed_c4 = self.speed.clone();
        let handle_c4 = thread::Builder::new().name("m68k-sound-cpu".into()).spawn(move || {
            let _guard = PanicGuard::new(sync_c4.clone(), arbiter_c4);
            let mut cycles = 0u64;
            let mut m68k = m68k::M68k::new(work_ram_c4.clone());
            m68k.sound_req_irq = Some(sound_req_irq_c4);
            let mut m68k_throttle = crate::throttle::ClockThrottle::new(
                crate::throttle::M68K_CLOCK_HZ,
                speed_c4,
            );
            let mut was_running = false;
            while !shutdown_c4.load(Ordering::Relaxed) {
                if sync_c4.is_shutdown() {
                    break;
                }
                let should_run = m68k_control_c4.load(Ordering::Acquire);
                if should_run && !was_running {
                    m68k.reset();
                    if std::env::var("MIMAS_DEBUG_M68K").is_ok() {
                        let ram = work_ram_c4.sound_ram.read().unwrap();
                        eprintln!(
                            "[M68K] reset: SP={:#010X} PC={:#010X} first16={:02X?}",
                            m68k.a[7], m68k.pc,
                            &ram[0..16]
                        );
                    }
                } else if !should_run && was_running {
                    m68k.stop();
                }
                was_running = should_run;
                if should_run {
                    for _ in 0..200 {
                        m68k.step();
                        m68k_throttle.advance(crate::throttle::M68K_NOMINAL_CYCLES_PER_INSTRUCTION);
                    }
                }
                cycles = cycles.wrapping_add(2);
                sync_c4.sync_core(4, cycles);
                thread::yield_now();
            }
        }).expect("failed to spawn Core 4 (MC68000 Sound CPU) thread");
        self.handles.push(handle_c4);

        // Spawn Core 5: SCSP Sound Synthesizer.
        let sync_c5 = sync.clone();
        let arbiter_c5 = arbiter.clone();
        let shutdown_c5 = shutdown.clone();
        let work_ram_c5 = work_ram.clone();
        let scsp_c5 = self.scsp.clone();
        let handle_c5 = thread::Builder::new().name("scsp-synth".into()).spawn(move || {
            let _guard = PanicGuard::new(sync_c5.clone(), arbiter_c5);
            let mut cycles = 0u64;
            while !shutdown_c5.load(Ordering::Relaxed) {
                if sync_c5.is_shutdown() {
                    break;
                }
                // Synthesize 128 audio samples per step
                scsp_c5.lock().unwrap().synthesize(&work_ram_c5, 128);
                let step = (sync_c5.slack_limit() / 2).max(2).min(500);
                cycles = cycles.wrapping_add(step);
                sync_c5.sync_core(5, cycles);
                thread::yield_now();
            }
        }).expect("failed to spawn Core 5 (SCSP Sound Synthesizer) thread");
        self.handles.push(handle_c5);

        // Spawn Core 6: SCU DMA/DSP Thread.
        let sync_c6 = sync.clone();
        let arbiter_c6 = arbiter.clone();
        let work_ram_c6 = work_ram.clone();
        let scu_dsp_c6 = self.scu_dsp.clone();
        let handle_c6 = thread::Builder::new().name("scu-dma-dsp".into()).spawn(move || {
            let _guard = PanicGuard::new(sync_c6.clone(), arbiter_c6);
            sync_c6.set_thread_active(6, false);
            let mut cycles = 0u64;
            loop {
                if !sync_c6.park_while_inactive(6) {
                    return;
                }
                while scu_dsp_c6.lock().unwrap().is_executing() {
                    if sync_c6.is_shutdown() {
                        return;
                    }
                    scu_dsp_c6.lock().unwrap().step(&work_ram_c6);
                    let step = (sync_c6.slack_limit() / 2).max(2).min(500);
                    cycles = cycles.wrapping_add(step);
                    sync_c6.sync_core(6, cycles);
                    thread::yield_now();
                }
                sync_c6.set_thread_active(6, false);
            }
        }).expect("failed to spawn Core 6 (SCU DMA/DSP) thread");
        self.handles.push(handle_c6);

        // Spawn Core 7: SMPC & CD-ROM Thread.
        let sync_c7 = sync.clone();
        let arbiter_c7 = arbiter.clone();
        let shutdown_c7 = shutdown.clone();
        let handle_c7 = thread::Builder::new().name("smpc-cd-block".into()).spawn(move || {
            let _guard = PanicGuard::new(sync_c7.clone(), arbiter_c7);
            let mut cycles = 0u64;
            while !shutdown_c7.load(Ordering::Relaxed) {
                if sync_c7.is_shutdown() {
                    break;
                }
                let step = (sync_c7.slack_limit() / 2).max(2).min(500);
                cycles = cycles.wrapping_add(step);
                sync_c7.sync_core(7, cycles);
                thread::yield_now();
            }
        }).expect("failed to spawn Core 7 (SMPC & CD-ROM) thread");
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
