pub mod bus_arbiter;
pub mod shared_buffers;
pub mod sh2;
pub mod sync;
pub mod cdrom;
pub mod scu;
pub mod smpc;
pub mod vdp;
pub mod scsp;
pub mod m68k;

pub use bus_arbiter::BusArbiter;
pub use shared_buffers::{WorkRam, Vram, Framebuffer, DoubleBufferedFramebuffer};
pub use sh2::Sh2;
pub use sync::{LockStepSync, PanicGuard};
pub use cdrom::Cdrom;
pub use scu::Scu;
pub use smpc::Smpc;
pub use vdp::Vdp;
pub use scsp::{Scsp, SoundRingBuffer};
pub use m68k::M68k;

use std::sync::{Arc, RwLock};
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
    /// when it processes those commands; Core 3 (which owns the actual
    /// `M68k` instance, since it's already the VDP1/VDP2/SCSP thread) reads
    /// it each loop iteration to know whether to step the M68K core.
    pub m68k_control: Arc<std::sync::atomic::AtomicBool>,
    /// Real hardware: the SCSP's M68000 requests an interrupt to the SH-2
    /// (SCU "Sound Request", vector 0x46, level 9) by writing its MCIPD
    /// register. Core 3's `M68k` sets this (see `M68k::write_byte`); Core
    /// 0's `Sh2` (via `Sh2::sound_req_irq`) observes and services it.
    pub sound_req_irq: Arc<std::sync::atomic::AtomicBool>,
}

impl SaturnSystem {
    pub fn new() -> Self {
        Self::with_slack(1000)
    }

    pub fn with_slack(slack_limit: u64) -> Self {
        let arbiter = Arc::new(BusArbiter::new());
        let work_ram = Arc::new(WorkRam::new());
        let vram = Arc::new(RwLock::new(Vram::new()));
        let sync = Arc::new(LockStepSync::new(4, slack_limit));
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
        }
    }

    /// Load real BIOS ROM bytes so the master SH-2 actually executes genuine
    /// boot code (from the reset vector) instead of a scaffold no-op loop.
    pub fn load_bios(&mut self, data: Vec<u8>) {
        self.bios = Arc::new(data);
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
        let handle_c0 = thread::spawn(move || {
            let _guard = PanicGuard::new(sync_c0.clone(), arbiter_c0.clone());
            let mut cpu = Sh2::new(false, arbiter_c0, work_ram_c0);
            cpu.sync = Some(sync_c0);
            cpu.core_id = 0;
            cpu.set_bios_arc(bios_c0);
            cpu.reset();
            cpu.pc_reporter = Some(cpu0_pc);
            cpu.m68k_control = Some(m68k_control_c0);
            cpu.sound_req_irq = Some(sound_req_irq_c0);
            cpu.run_loop(shutdown_c0);
        });
        self.handles.push(handle_c0);

        // Spawn Core 1: Slave SH-2. Real hardware keeps this core halted
        // until the Master issues SMPC's SSHON -- not yet implemented
        // anywhere in this codebase (see `.development/TASKS.md`), so this
        // parks at zero CPU (excluded from `LockStepSync`'s drift tracking
        // exactly like a DMA-blocked core, via `set_thread_active`) instead
        // of executing whatever garbage sits at unreset address 0. A future
        // SSHON implementation wakes this core with
        // `sync.set_thread_active(1, true)`, the same call DMA resume
        // already uses -- see `LockStepSync::park_while_inactive`.
        let shutdown_c1 = shutdown.clone();
        let arbiter_c1 = arbiter.clone();
        let work_ram_c1 = work_ram.clone();
        let sync_c1 = sync.clone();
        let bios_c1 = self.bios.clone();
        let handle_c1 = thread::spawn(move || {
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
            cpu.run_loop(shutdown_c1);
        });
        self.handles.push(handle_c1);

        // Spawn Core 2: SCU DSP slot. Nothing targets it yet -- SMPC command
        // processing runs inline inside `Sh2` wherever a core touches its
        // registers, and CD-ROM isn't wired into the CPU address space at
        // all (see `.development/TASKS.md`); real per-component threads for
        // those are separate future work (see
        // `docs/final_architecture_draft.md`'s topology table). Parks at
        // zero CPU instead of spinning a cycle counter with no real effect;
        // a future real DSP implementation wakes this core with
        // `sync.set_thread_active(2, true)`.
        let sync_c2 = sync.clone();
        let arbiter_c2 = arbiter.clone();
        let handle_c2 = thread::spawn(move || {
            let _guard = PanicGuard::new(sync_c2.clone(), arbiter_c2);
            sync_c2.set_thread_active(2, false);
            sync_c2.park_while_inactive(2);
        });
        self.handles.push(handle_c2);

        // Spawn Core 3: VDP1 / VDP2 / SCSP
        let shutdown_c3 = shutdown.clone();
        let sync_c3 = sync.clone();
        let arbiter_c3 = arbiter.clone();
        let work_ram_c3 = work_ram.clone();
        let vdp2_frame = self.vdp2_frame.clone();
        let m68k_control_c3 = self.m68k_control.clone();
        let sound_req_irq_c3 = self.sound_req_irq.clone();
        let handle_c3 = thread::spawn(move || {
            let _guard = PanicGuard::new(sync_c3.clone(), arbiter_c3);
            let mut cycles = 0u64;
            // Real VDP2 output is genuinely paced by the video clock, not
            // free-running: render one frame per ~60Hz tick, same cadence as
            // the CPU's VBLANK-IN interrupt, rather than as fast as this
            // thread can spin.
            let mut next_frame_due = std::time::Instant::now();
            let frame_interval = std::time::Duration::from_micros(16_666);
            let mut last_logged: Option<(u16, u16)> = None;
            // SCSP's onboard M68000: owned by this thread since it's already
            // the VDP1/VDP2/SCSP core. `m68k_control_c3` (flipped by Core 0's
            // SNDON/SNDOFF handling) says whether it should currently be
            // running; `was_running` detects the off->on edge so `reset()`
            // (real SNDON semantics: `M68KStart` -> `M68K->Reset()`) fires
            // exactly once per SNDON, not every loop iteration.
            let mut m68k = m68k::M68k::new(work_ram_c3.clone());
            m68k.sound_req_irq = Some(sound_req_irq_c3);
            let mut was_running = false;
            // Real hardware issues SNDON only after the SH-2 has finished
            // uploading the sound driver into Sound RAM. Core 0 executes
            // SH-2 instructions in strict program order on its own thread,
            // so every Sound RAM write the upload routine makes necessarily
            // completes before the COMREG=0x06 write that flips this flag --
            // the data is always ready by the time SNDON fires. The gap was
            // never timing; it was that `m68k_control` used to be
            // `Ordering::Relaxed` on both ends, which gives no cross-thread
            // visibility guarantee beyond the bool's own atomicity. A wall-
            // clock debounce used to stand in here on the theory that more
            // writes might still be arriving after SNDON -- disproved live
            // (see `.development/current_blocker.md`: the Sound RAM image at
            // reset time was byte-for-byte identical with and without the
            // debounce). The actual fix is `Ordering::Release` on the SNDON/
            // SNDOFF stores (`Sh2::smpc_execute_command`) paired with
            // `Ordering::Acquire` on the load below -- the same publish/
            // observe pair `BusArbiter::lock_for_dma`/`is_locked` already
            // uses for `locked_by_dma` -- which *guarantees* this thread
            // observes every Sound RAM write that preceded the flag flip,
            // not just "usually does in practice." Removing the debounce is
            // not expected to change the current M68K driver self-
            // corruption wall (see `current_blocker.md`); that's a separate,
            // already-tracked bug in the SH-2 upload logic itself.
            while !shutdown_c3.load(Ordering::Relaxed) {
                if sync_c3.is_shutdown() {
                    break;
                }
                let should_run = m68k_control_c3.load(Ordering::Acquire);
                if should_run && !was_running {
                    m68k.reset();
                    if std::env::var("MIMAS_DEBUG_M68K").is_ok() {
                        let ram = work_ram_c3.sound_ram.read().unwrap();
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
                    // Bounded per-iteration step count: this thread also
                    // paces VDP2 frames and yields every loop, so this isn't
                    // real 68000 clock-accurate timing, just enough real
                    // execution progress per wall-clock tick to let the
                    // uploaded sound driver actually run instead of stalling
                    // forever behind an unimplemented CPU.
                    for _ in 0..200 {
                        m68k.step();
                    }
                }
                let now = std::time::Instant::now();
                if now >= next_frame_due {
                    let frame = crate::vdp::render_backdrop(&work_ram_c3);
                    if std::env::var("MIMAS_DEBUG_VDP2").is_ok() {
                        // One held guard for both register pairs below --
                        // `vdp2_regs` is its own lock now, and a Core 0
                        // write landing between two separate acquisitions
                        // here would log a torn TVMD/BKTAL pair.
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
                cycles = cycles.wrapping_add(2);
                sync_c3.sync_core(3, cycles);
                thread::yield_now();
            }
        });
        self.handles.push(handle_c3);
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
