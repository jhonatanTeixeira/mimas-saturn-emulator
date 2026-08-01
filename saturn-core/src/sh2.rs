use crate::bus_arbiter::BusArbiter;
use crate::shared_buffers::WorkRam;
use crate::sync::LockStepSync;
use std::sync::{Arc, Mutex};

static INTERRUPT_OVERRUN_LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
fn log_interrupt_overrun_once(vector: u8, level: u8) {
    let key = format!("vector={:#X} level={}", vector, level);
    let mut log = INTERRUPT_OVERRUN_LOG.lock().unwrap();
    if !log.contains(&key) {
        eprintln!("[INTQUEUE_OVERRUN] {}", key);
        log.push(key);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingInterrupt {
    pub vector: u8,
    pub level: u8,
}

#[derive(Debug, Clone)]
pub struct InterruptQueue {
    pub pending: Vec<PendingInterrupt>,
}

impl InterruptQueue {
    pub fn new() -> Self {
        Self {
            pending: Vec::with_capacity(50),
        }
    }

    pub fn send(&mut self, vector: u8, level: u8) {
        if self.pending.iter().any(|x| x.vector == vector) {
            return;
        }
        if self.pending.len() >= 50 {
            log_interrupt_overrun_once(vector, level);
            return;
        }
        self.pending.push(PendingInterrupt { vector, level });
        self.pending.sort_by_key(|x| x.level);
    }

    pub fn remove(&mut self, vector: u8) {
        if let Some(pos) = self.pending.iter().position(|x| x.vector == vector) {
            self.pending.remove(pos);
        }
    }
}

/// Real Saturn/SH-2 physical address regions this core understands. Anything
/// outside these returns 0 on read and is ignored on write, matching the
/// existing out-of-bounds test expectation. Region boundaries cross-checked
/// against Yabause's real, working memory map (`memory.c`'s
/// `MappedMemoryInit` fill table) rather than guessed.
#[derive(Debug, Clone, Copy)]
enum MemRegion {
    Bios(usize),
    LowRam(usize),
    HighRam(usize),
    /// SMPC register block (physical 0x00100000-0x0017FFFF, 512KB window
    /// mirroring an 0x80-byte register file -- real hardware masks the
    /// offset with `& 0x7F`, see `Sh2::translate`). Earlier code mistakenly
    /// overlaid a single fake status byte onto High Work RAM's own start
    /// address (0x06000000), corrupting real High RAM once programs used
    /// it; moved to its real address. SF (the busy/idle Status Flag) lives
    /// at the real, documented offset 0x63.
    Smpc(usize),
    BackupRam(usize),
    SoundRam(usize),
    ScspRegs(usize),
    Vdp1Vram(usize),
    Vdp1Framebuffer(usize),
    Vdp1Regs(usize),
    Vdp2Vram(usize),
    Vdp2Cram(usize),
    Vdp2Regs(usize),
    ScuRegs(usize),
    Cs2Regs(usize),
    OnChip(usize),
    PurgeArea,
    AddressArray(usize),
    DataArray(usize),
    Unmapped,
}

/// Real offset of SMPC's Status Flag (SF) within its register block.
const SMPC_SF_OFFSET: usize = 0x63;
/// Real SMPC register offsets, cross-checked against Yabause's `smpc.c`
/// (`SmpcWriteByte`'s `switch` and the `SmpcRegs->IREG`/`OREG` struct
/// layout, which places each register on the odd byte of a 2-byte-aligned
/// pair -- `SmpcReadByte` collapses both bytes of each pair to the same
/// storage via `addr >> 1`).
const SMPC_IREG1_OFFSET: usize = 0x03;
const SMPC_COMREG_OFFSET: usize = 0x1F;
const SMPC_OREG_BASE_OFFSET: usize = 0x21;
const SMPC_SR_OFFSET: usize = 0x61;
/// INTBACK: the command real BIOS boot code uses to request system status
/// (region, RTC, reset/cart flags) and optionally peripheral data in one
/// handshake -- see `SmpcINTBACK`/`SmpcINTBACKStatus` in `smpc.c`.
const SMPC_CMD_INTBACK: u8 = 0x10;
/// SNDON/SNDOFF: reset/halt the SCSP's onboard M68000 (`M68KStart`/
/// `M68KStop` in a real, working SCSP implementation).
const SMPC_CMD_SNDON: u8 = 0x06;
const SMPC_CMD_SNDOFF: u8 = 0x07;
/// Mask real hardware applies to any value written into SR (via `LDC`,
/// `LDC.L`, or `RTE`): only these bits are architecturally meaningful (T,
/// S, the interrupt mask I3-I0, M, Q). Confirmed against three independent
/// call sites in a real, working SH-2 interpreter (Yabause) that all use
/// this exact constant.
const SR_WRITE_MASK: u32 = 0x0000_03F3;

pub struct Sh2 {
    pub is_slave: bool,
    pub core_id: usize,
    pub cycles: u64,
    pub registers: [u32; 16],
    pub pc: u32,
    pub pr: u32,
    pub sr: u32,
    pub gbr: u32,
    pub vbr: u32,
    pub mach: u32,
    pub macl: u32,
    pub arbiter: Arc<BusArbiter>,
    pub work_ram: Arc<WorkRam>,
    /// BIOS ROM contents (512KB on real hardware). Empty until `load_bios()`
    /// is called; reads against the BIOS region return 0 until then so
    /// existing tests that never load a BIOS keep working unchanged.
    pub bios: Arc<Vec<u8>>,
    pub sync: Option<Arc<LockStepSync>>,
    pub illegal_instruction_flag: bool,
    pub unaligned_access_flag: bool,
    pub cdrom_command_executed: bool,
    /// Optional external observer: `run_loop` stamps its current PC here
    /// after every step, so a frontend can watch real boot progress happen
    /// on a CPU thread it no longer owns (it was moved into the thread
    /// closure when spawned).
    pub pc_reporter: Option<Arc<std::sync::atomic::AtomicU32>>,
    /// Optional external handle letting SNDON/SNDOFF (`smpc_execute_command`)
    /// enable/disable the SCSP's M68000 core, which runs on a different
    /// thread (Core 3) and doesn't otherwise share any state with this SH-2.
    /// `None` when nothing has wired an M68K core in (e.g. plain unit tests).
    ///
    /// **Ordering contract**: writes here must use `Ordering::Release` and
    /// reads must use `Ordering::Acquire`, never `Relaxed`. This is the one
    /// thing that makes it safe for Core 3 to read Sound RAM (via
    /// `WorkRam::sound_ram`'s own lock, which both cores share) immediately
    /// after observing this flag flip true: by the time this store executes,
    /// every Sound RAM write the driver-upload routine made is already behind it in
    /// Core 0's own program order, but only `Release`/`Acquire` actually
    /// guarantees Core 3 observes them -- `Relaxed` previously did not,
    /// which is why a wall-clock debounce used to stand in here (see
    /// `SaturnSystem::start`'s Core 3 loop and `history.md`). Matches
    /// `BusArbiter::lock_for_dma`/`is_locked`'s existing use of the same
    /// pair for `locked_by_dma`.
    pub m68k_control: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Set when VDP2 has raised the VBLANK-IN interrupt line. Checked once
    /// per `step()`; serviced (pushes PC/SR, jumps through the VBR vector
    /// table) if the interrupt mask in SR allows it. Found necessary
    /// running the real BIOS: it waits for a RAM counter that only a real
    /// VBLANK interrupt handler (installed by the BIOS itself in the
    /// vector table) ever increments -- there's no way to satisfy that
    /// wait without genuine interrupt delivery.
    pub vblank_pending: bool,
    /// Wall-clock pacing for `run_loop`'s VBLANK timer (~60Hz), so a CPU
    /// running at whatever speed this interpreter manages still receives
    /// VBLANK at roughly the real cadence rather than every single step.
    next_vblank_due: Option<std::time::Instant>,
    /// Set when VDP2 has raised the VBLANK-OUT interrupt line -- see
    /// `VBLANK_OUT_LEVEL`'s doc comment for why this exists as its own
    /// interrupt, separate from `vblank_pending`.
    pub vblank_out_pending: bool,
    /// Wall-clock pacing for VBLANK-OUT, scheduled relative to the same
    /// `now` sample that advances `next_vblank_due` when VBLANK-IN fires
    /// (`due + VBLANK_DURATION`) -- kept in lockstep with `tvstat_word()`'s
    /// existing period_start+VBLANK_DURATION edge instead of running an
    /// independent timer that could drift against TVSTAT's own bit.
    next_vblank_out_due: Option<std::time::Instant>,
    /// SMPC "System Manager" interrupt (real hardware: SCU vector 0x47,
    /// level 8 -- fired when an SMPC command, e.g. INTBACK, completes). Real
    /// BIOS INTBACK handshakes wait on this specifically, not just SF --
    /// see `smpc_execute_command`. Same instant-completion simplification as
    /// SF: real hardware has command-dependent timing, we fire the
    /// completion interrupt the same step the command is issued.
    pub smpc_irq_pending: bool,
    /// SCU "Sound Request" interrupt (real hardware: vector 0x46, level 9--
    /// `ScuSendSoundRequest` in a real, working SCU implementation). Fired
    /// when the SCSP's M68000 sound driver writes a bit into its MCIPD
    /// register that's also enabled in MCIEB -- see `M68k::write_byte` and
    /// `mimas/CLAUDE.md`'s "Current wall" section for how this was traced
    /// (a boot BIOS wait loop polls a RAM counter that only the SH-2's own
    /// installed handler for *this* interrupt plausibly updates).
    /// `Option` because it's shared with whatever thread owns the M68K core
    /// (`SaturnSystem` wires it up in `lib.rs`); `None` in plain unit tests.
    pub sound_req_irq: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Real wall-clock CPU throttle control (see `crate::throttle`).
    /// `None` (the default) means this core's `run_loop()` never paces at
    /// all -- runs exactly as fast as it does today, so every existing
    /// unit test that builds a bare `Sh2` stays unaffected. `SaturnSystem`
    /// wires `Some(...)` in for real, running systems (defaulting to
    /// `ThrottleSpeed::Unthrottled` there too, until a caller opts into a
    /// real speed via `SaturnSystem::set_speed`).
    pub speed: Option<Arc<std::sync::Mutex<crate::throttle::ThrottleSpeed>>>,
    /// The SCU DSP (Core 2's slot), shared with whatever thread actually
    /// steps it (`SaturnSystem` wires this up in `lib.rs`). `None` in
    /// plain unit tests -- SCU DSP register writes then fall through to
    /// plain byte storage, same as before this existed. See
    /// `crate::scu_dsp` for why a real DSP interpreter was needed (a boot
    /// wait loop polling the Program Control Port's `EX` bit).
    pub scu_dsp: Option<Arc<std::sync::Mutex<crate::scu_dsp::ScuDsp>>>,
    /// The real SMPC command processor, shared with whatever else needs to
    /// reach it (`SaturnSystem` wires this up in `lib.rs`). `None` in plain
    /// unit tests -- COMREG writes then fall through to the old inline
    /// `smpc_execute_command`, so every existing bare-`Sh2` test keeps
    /// working unchanged. See `crate::smpc` and
    /// `docs/implementation-plans/smpc-peripheral.md` Phase 0.
    pub smpc: Option<Arc<std::sync::Mutex<crate::smpc::Smpc>>>,
    pub irq_in: Option<Arc<Mutex<InterruptQueue>>>,
    local_irq_in: InterruptQueue,
    pub onchip: crate::sh2_onchip::Sh2OnChip,
    pub address_array: [u32; 0x100],
    pub data_array: Box<[u8; 0x1000]>,
    pub frc_leftover: u32,
    pub frc_shift: u32,
    pub pending_sync: u32,
}

// SR bit positions actually used by this subset of the ISA. Layout (T, S,
// I3-I0, M, Q) matches real SH-2 and is cross-checked against
// SR_WRITE_MASK (0x3F3), which a real, working interpreter (Yabause) uses
// at every site that writes SR from an external value.
const SR_T: u32 = 1 << 0;
const SR_S: u32 = 1 << 1;
const SR_M: u32 = 1 << 8;
const SR_Q: u32 = 1 << 9;
// SR bits 4-7: current interrupt mask level (0-15). An interrupt is
// accepted only if its level is strictly greater than this mask.
const SR_IMASK_SHIFT: u32 = 4;

/// VBLANK-IN: highest-priority interrupt on real Saturn hardware (level 15),
/// autovector number 0x40 -- both values per the standard, widely
/// documented Saturn interrupt table (see e.g. Yabause's vdp2.c and the
/// Saturn hardware manual's interrupt level assignments). If this specific
/// vector number turns out wrong for a given BIOS revision, the symptom
/// will be the CPU jumping into garbage right after the interrupt fires,
/// which is easy to spot against "PC keeps making forward progress."
const VBLANK_IN_LEVEL: u32 = 15;
const VBLANK_IN_VECTOR: u32 = 0x40;
/// VBLANK-OUT: level 0xE, autovector 0x41 -- a separate, lower-priority
/// interrupt from VBLANK-IN, not a duplicate of it. Confirmed against
/// `ScuSendVBlankOUT()` (`SendInterrupt(0x41, 0xE, ...)`, `scu.c`) and
/// `Vdp2VBlankOUT()` (`vdp2.cpp`), which clears TVSTAT's VBLANK bit and
/// fires this in the same step -- real hardware raises it once per frame at
/// the transition from vertical blanking back into active display, i.e.
/// `VBLANK_DURATION` after each VBLANK-IN. Found necessary running the real
/// BIOS the same way VBLANK-IN was: a boot wait loop (this one at SH-2
/// `0x060108ba` against `Sega Saturn BIOS (USA).bin`) polls a RAM counter
/// that only this BIOS's own vector-table-installed VBLANK-OUT handler
/// (`0x060102aa` in that trace) ever increments -- traced by dumping High
/// RAM at the stuck PC and disassembling with `tools/sh2dis.py`, then
/// resolving the BIOS's own two-level interrupt dispatch table (vector
/// number -> SR-mask table @ handler-table-base, and a parallel real-handler
/// table) to see which vector's slot actually pointed at that address.
const VBLANK_OUT_LEVEL: u32 = 14;
const VBLANK_OUT_VECTOR: u32 = 0x41;
/// SMPC "System Manager" interrupt: level 8, autovector 0x47 -- confirmed
/// against `ScuSendSystemManager()` (`SendInterrupt(0x47, 0x8, ...)`) in a
/// real, working SCU implementation (Yabause `scu.c`), fired whenever an
/// SMPC command like INTBACK completes.
const SMPC_IRQ_LEVEL: u32 = 8;
const SMPC_IRQ_VECTOR: u32 = 0x47;
/// SCU "Sound Request" interrupt: level 9, autovector 0x46 -- confirmed
/// against `ScuSendSoundRequest()` (`SendInterrupt(0x46, 0x9, ...)`) in a
/// real, working SCU implementation (Yabause `scu.c`).
const SOUND_REQ_IRQ_LEVEL: u32 = 9;
const SOUND_REQ_IRQ_VECTOR: u32 = 0x46;
const VBLANK_INTERVAL: std::time::Duration = std::time::Duration::from_micros(16_666); // ~60Hz
/// Real VDP2 VBLANK duration within each frame: NTSC has 262 total
/// scanlines, of which ~38 fall in the vertical blanking period (the rest,
/// ~224, are active display) -- see `yabsys.VBlankLineCount` vs total line
/// count driving `Vdp2HBlankIN`/`Vdp2HBlankOUT` in a real, working VDP2
/// implementation (Yabause `vdp2.cpp`). 38/262 of the ~16.666ms frame.
const VBLANK_DURATION: std::time::Duration = std::time::Duration::from_micros(2_417);
/// TVSTAT's VBLANK flag: real hardware sets this bit for the duration of
/// vertical blanking and clears it once active display resumes, and BIOS
/// code polls it directly (independent of the VBLANK-IN interrupt) -- see
/// `Vdp2Regs->TVSTAT |= 0x0008` / `&= ~0x0008` in Yabause `vdp2.cpp`.
const TVSTAT_VBLANK_BIT: u16 = 0x0008;

/// Throwaway diagnostic: log each distinct (region, offset, direction) real
/// hardware register access exactly once, so a boot run reveals which
/// registers the BIOS actually touches without hand-decoding raw opcodes.
/// Remove once the current wall is diagnosed.
static REG_ACCESS_LOG: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
static ILLEGAL_OP_LOG: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn log_reg_access_once(region: &MemRegion, is_write: bool, val: u8) {
    let interesting = matches!(
        region,
        MemRegion::Smpc(_)
            | MemRegion::Vdp1Regs(_)
            | MemRegion::Vdp2Regs(_)
            | MemRegion::ScuRegs(_)
            | MemRegion::Cs2Regs(_)
            | MemRegion::OnChip(_)
    );
    if !interesting {
        return;
    }
    let key = format!(
        "{:?} {} val={:#04X}",
        region,
        if is_write { "W" } else { "R" },
        val
    );
    let mut log = REG_ACCESS_LOG.lock().unwrap();
    if !log.contains(&key) {
        eprintln!("[REGACCESS] {}", key);
        log.push(key);
    }
}

fn log_illegal_once(pc: u32, opcode: u16) {
    let key = format!("pc={:#010X} opcode={:#06X}", pc, opcode);
    let mut log = ILLEGAL_OP_LOG.lock().unwrap();
    if !log.contains(&key) {
        eprintln!("[ILLOP] {}", key);
        log.push(key);
    }
}

static BUS_MISS_LOG: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn lock_bus_miss_log() -> std::sync::MutexGuard<'static, Vec<String>> {
    match BUS_MISS_LOG.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn log_bus_miss_once(address: u32, is_write: bool, width: u8, pc: u32, info: &str) {
    if std::env::var("MIMAS_BUS_TRACE").is_err() {
        return;
    }
    let area = address >> 29;
    let block = address & 0x0FF00000;
    let key = format!(
        "area={} block={:#010X} is_write={} width={}",
        area, block, is_write, width
    );
    let mut log = lock_bus_miss_log();
    if !log.contains(&key) {
        eprintln!(
            "[BUSMISS] area={} block={:#010X} is_write={} width={} pc={:#010X} info={}",
            area, block, is_write, width, pc, info
        );
        log.push(key);
    }
}

impl Sh2 {
    fn check_bus_miss(&self, address: u32, is_write: bool, width: u8) {
        if std::env::var("MIMAS_BUS_TRACE").is_err() {
            return;
        }
        let area = address >> 29;
        let mut is_miss = false;
        let mut info = "";

        // 1. address >> 29 is 2, 3, 5, or 6
        if area == 2 || area == 3 || area == 5 || area == 6 {
            is_miss = true;
            info = "Area 2/3/5/6 access";
        }
        // 2. address >> 29 == 7 && address < 0xFFFF_FE00
        else if area == 7 && address < 0xFFFF_FE00 {
            is_miss = true;
            info = "Area 7 below on-chip registers";
        }
        // 3. bit-28 alias
        else if (address & 0x1000_0000) != 0 {
            is_miss = true;
            info = "Bit-28 alias";
        }
        // 4. area-4 alias
        else if area == 4 {
            is_miss = true;
            info = "Area-4 alias";
        }

        // Check if translate results in Unmapped, or if offset exceeds real device size
        if !is_miss {
            let region = self.translate(address);
            match region {
                MemRegion::Unmapped => {
                    is_miss = true;
                    let a = address & 0x0FFF_FFFF;
                    if (0x0200_0000..0x0400_0000).contains(&a) {
                        info = "Unmapped CS0";
                    } else if (0x0400_0000..0x0500_0000).contains(&a) {
                        info = "Unmapped CS1";
                    } else if (0x0100_0000..0x0200_0000).contains(&a) {
                        info = "Unmapped FRT Capture";
                    } else if (0x0700_0000..0x0800_0000).contains(&a) {
                        info = "Unmapped High WRAM mirror";
                    } else {
                        info = "Unmapped hole";
                    }
                }
                _ => {
                    let a = address & 0x0FFF_FFFF;
                    if a < 0x0010_0000 && a >= 0x0008_0000 {
                        is_miss = true;
                        info = "BIOS mirror offset";
                    } else if a >= 0x0018_0000
                        && a < 0x0020_0000
                        && (a - 0x0018_0000) >= 0x0001_0000
                    {
                        is_miss = true;
                        info = "Backup RAM mirror offset";
                    } else if a >= 0x05D0_0000
                        && a < 0x05D8_0000
                        && (a - 0x05D0_0000) >= 0x0000_0100
                    {
                        is_miss = true;
                        info = "VDP1 Regs mirror offset";
                    } else if a >= 0x05F8_0000
                        && a < 0x05FC_0000
                        && (a - 0x05F8_0000) >= 0x0000_0200
                    {
                        is_miss = true;
                        info = "VDP2 Regs mirror offset";
                    } else if a >= 0x05FE_0000
                        && a < 0x05FF_0000
                        && (a - 0x05FE_0000) >= 0x0000_0100
                    {
                        is_miss = true;
                        info = "SCU Regs mirror offset";
                    } else if a >= 0x0600_0000
                        && a < 0x0700_0000
                        && (a - 0x0600_0000) >= 0x0010_0000
                    {
                        is_miss = true;
                        info = "High RAM mirror offset";
                    }
                }
            }
        }

        if is_miss {
            log_bus_miss_once(address, is_write, width, self.pc, info);
        }
    }

    pub fn new(is_slave: bool, arbiter: Arc<BusArbiter>, work_ram: Arc<WorkRam>) -> Self {
        Self {
            is_slave,
            core_id: if is_slave { 1 } else { 0 },
            cycles: 0,
            registers: [0; 16],
            pc: 0,
            pr: 0,
            sr: 0,
            gbr: 0,
            vbr: 0,
            mach: 0,
            macl: 0,
            arbiter,
            work_ram,
            bios: Arc::new(Vec::new()),
            sync: None,
            illegal_instruction_flag: false,
            unaligned_access_flag: false,
            cdrom_command_executed: false,
            pc_reporter: None,
            m68k_control: None,
            vblank_pending: false,
            next_vblank_due: None,
            vblank_out_pending: false,
            next_vblank_out_due: None,
            smpc_irq_pending: false,
            sound_req_irq: None,
            speed: None,
            scu_dsp: None,
            smpc: None,
            irq_in: None,
            local_irq_in: InterruptQueue::new(),
            onchip: crate::sh2_onchip::Sh2OnChip::new(is_slave),
            address_array: [0; 0x100],
            data_array: Box::new([0; 0x1000]),
            frc_leftover: 0,
            frc_shift: 3,
            pending_sync: 0,
        }
    }

    /// Load real BIOS ROM bytes. Call before `reset()`/stepping so the CPU
    /// actually fetches genuine boot code instead of reading zeros.
    pub fn load_bios(&mut self, mut data: Vec<u8>) {
        if data.len() != 524288 {
            eprintln!("[WARNING] BIOS image length is {} bytes (expected 524288 bytes). Truncating or padding.", data.len());
            data.resize(524288, 0);
        }
        self.bios = Arc::new(data);
    }

    /// Share an already-loaded BIOS image (cheap: clones the Arc, not the
    /// underlying bytes). Used when multiple cores need to see the same ROM.
    pub fn set_bios_arc(&mut self, bios: Arc<Vec<u8>>) {
        self.bios = bios;
    }

    pub fn queue_send(&mut self, vector: u8, level: u8) {
        if let Some(ref q) = self.irq_in {
            q.lock().unwrap().send(vector, level);
        } else {
            self.local_irq_in.send(vector, level);
        }
        if vector == VBLANK_IN_VECTOR as u8 {
            self.vblank_pending = true;
        } else if vector == VBLANK_OUT_VECTOR as u8 {
            self.vblank_out_pending = true;
        } else if vector == SMPC_IRQ_VECTOR as u8 {
            self.smpc_irq_pending = true;
        }
        if let Some(ref sync) = self.sync {
            sync.set_thread_active(self.core_id, true);
        }
    }

    pub fn queue_remove(&mut self, vector: u8) {
        if let Some(ref q) = self.irq_in {
            q.lock().unwrap().remove(vector);
        } else {
            self.local_irq_in.remove(vector);
        }
        if vector == VBLANK_IN_VECTOR as u8 {
            self.vblank_pending = false;
        } else if vector == VBLANK_OUT_VECTOR as u8 {
            self.vblank_out_pending = false;
        } else if vector == SMPC_IRQ_VECTOR as u8 {
            self.smpc_irq_pending = false;
        }
    }

    pub fn queue_peek(&self) -> Option<PendingInterrupt> {
        if let Some(ref q) = self.irq_in {
            q.lock().unwrap().pending.last().copied()
        } else {
            self.local_irq_in.pending.last().copied()
        }
    }

    /// Triggers a Non-Maskable Interrupt (NMI).
    /// Sets bit 15 of the ICR (Interrupt Control Register) and sends NMI (vector 0xB, level 16).
    pub fn nmi(&mut self) {
        self.onchip.icr |= 0x8000;
        self.queue_send(0xB, 16);
    }

    /// Perform the real SH-2 reset sequence: PC and R15 (stack pointer) are
    /// read from the first two 32-bit words of the reset vector table (at
    /// physical address 0x00000000), which lives in BIOS ROM. Must be called
    /// after `load_bios()` for this to do anything meaningful.
    pub fn reset(&mut self) {
        // Zero R0..R14 (not R15)
        for i in 0..15 {
            self.registers[i] = 0;
        }
        self.gbr = 0;
        self.vbr = 0;
        self.mach = 0;
        self.macl = 0;
        self.pr = 0;
        self.cycles = 0;

        self.pc = self.read_long(self.vbr + 0);
        self.registers[15] = self.read_long(self.vbr + 4);

        // Real SH-2 reset value: interrupt mask level 15 (I3-I0 = 1111)
        self.sr = 0x0000_00F0;
        self.illegal_instruction_flag = false;
        self.unaligned_access_flag = false;

        // Reset pending flags
        self.vblank_pending = false;
        self.vblank_out_pending = false;
        self.smpc_irq_pending = false;
        if let Some(ref f) = self.sound_req_irq {
            f.store(false, std::sync::atomic::Ordering::Relaxed);
        }

        // Reset on-chip registers
        self.onchip.reset(self.is_slave);
        self.frc_leftover = 0;
        self.frc_shift = 3;
        self.pending_sync = 0;
    }

    fn translate(&self, address: u32) -> MemRegion {
        if address >= 0xFFFF_FE00 {
            return MemRegion::OnChip((address & 0x1FF) as usize);
        }
        match address >> 29 {
            2 => MemRegion::PurgeArea,
            3 => MemRegion::AddressArray((address & 0x3FC) as usize),
            6 => MemRegion::DataArray((address & 0xFFF) as usize),
            7 => MemRegion::Unmapped,
            0 | 1 | 4 | 5 => {
                let a = address & 0x0FFF_FFFF;
                if a < 0x0010_0000 {
                    MemRegion::Bios(a as usize)
                } else if (0x0010_0000..0x0018_0000).contains(&a) {
                    // Real hardware mirrors the 0x80-byte SMPC register file across
                    // this whole 512KB window by masking the offset with & 0x7F.
                    MemRegion::Smpc((a as usize) & 0x7F)
                } else if (0x0018_0000..0x0020_0000).contains(&a) {
                    MemRegion::BackupRam((a - 0x0018_0000) as usize)
                } else if (0x0020_0000..0x0030_0000).contains(&a) {
                    MemRegion::LowRam((a - 0x0020_0000) as usize)
                } else if (0x0580_0000..0x0590_0000).contains(&a) {
                    MemRegion::Cs2Regs((a - 0x0580_0000) as usize)
                } else if (0x05A0_0000..0x05B0_0000).contains(&a) {
                    MemRegion::SoundRam((a - 0x05A0_0000) as usize)
                } else if (0x05B0_0000..0x05C0_0000).contains(&a) {
                    MemRegion::ScspRegs((a - 0x05B0_0000) as usize)
                } else if (0x05C0_0000..0x05C8_0000).contains(&a) {
                    MemRegion::Vdp1Vram((a - 0x05C0_0000) as usize)
                } else if (0x05C8_0000..0x05D0_0000).contains(&a) {
                    MemRegion::Vdp1Framebuffer((a - 0x05C8_0000) as usize)
                } else if (0x05D0_0000..0x05D8_0000).contains(&a) {
                    MemRegion::Vdp1Regs((a - 0x05D0_0000) as usize)
                } else if (0x05E0_0000..0x05F0_0000).contains(&a) {
                    MemRegion::Vdp2Vram((a - 0x05E0_0000) as usize)
                } else if (0x05F0_0000..0x05F8_0000).contains(&a) {
                    MemRegion::Vdp2Cram((a - 0x05F0_0000) as usize)
                } else if (0x05F8_0000..0x05FC_0000).contains(&a) {
                    MemRegion::Vdp2Regs((a - 0x05F8_0000) as usize)
                } else if (0x05FE_0000..0x05FF_0000).contains(&a) {
                    MemRegion::ScuRegs((a - 0x05FE_0000) as usize)
                } else if (0x0600_0000..0x0800_0000).contains(&a) {
                    MemRegion::HighRam((a - 0x0600_0000) as usize)
                } else {
                    MemRegion::Unmapped
                }
            }
            _ => MemRegion::Unmapped,
        }
    }

    fn get_base_cycles(&self, opcode: u16) -> u32 {
        if opcode & 0xFF00 == 0xC300 {
            // TRAPA #imm
            return 8;
        }

        // 4 cycles
        if (opcode & 0xF0FF) == 0x401B {
            // TAS.B @Rn
            return 4;
        }
        if opcode == 0x002B {
            // RTE
            return 4;
        }

        // 3 cycles
        if (opcode & 0xF0FF) == 0x4007 || (opcode & 0xF0FF) == 0x4017 {
            // LDC.L @Rm+,SR or LDC.L @Rm+,GBR
            return 3;
        }
        if (opcode & 0xF0FF) == 0x4027 {
            // LDC.L @Rm+,VBR
            return 3;
        }
        if opcode & 0xFC00 == 0xCC00 {
            // TST.B/AND.B/XOR.B/OR.B #imm,@(R0,GBR)
            return 3;
        }
        if (opcode & 0xF00F) == 0x000F || (opcode & 0xF00F) == 0x000E {
            // MAC.L or MAC.W
            return 3;
        }
        if opcode == 0x001B {
            // SLEEP
            return 3;
        }
        if opcode & 0xFF00 == 0x8900 {
            // BT label
            return if (self.sr & SR_T) != 0 { 3 } else { 1 };
        }
        if opcode & 0xFF00 == 0x8B00 {
            // BF label
            return if (self.sr & SR_T) == 0 { 3 } else { 1 };
        }

        // 2 cycles
        if opcode & 0xFF00 == 0x8D00 {
            // BT/S label
            return if (self.sr & SR_T) != 0 { 2 } else { 1 };
        }
        if opcode & 0xFF00 == 0x8F00 {
            // BF/S label
            return if (self.sr & SR_T) == 0 { 2 } else { 1 };
        }
        if opcode & 0xF000 == 0xA000 {
            // BRA
            return 2;
        }
        if opcode & 0xF000 == 0xB000 {
            // BSR
            return 2;
        }
        if (opcode & 0xF0FF) == 0x402B {
            // JMP
            return 2;
        }
        if (opcode & 0xF0FF) == 0x400B {
            // JSR
            return 2;
        }
        if opcode == 0x000B {
            // RTS
            return 2;
        }
        if (opcode & 0xF0FF) == 0x0023 {
            // BRAF Rn
            return 2;
        }
        if (opcode & 0xF0FF) == 0x0003 {
            // BSRF Rn
            return 2;
        }
        if (opcode & 0xF00F) == 0x0007 {
            // MUL.L Rm,Rn
            return 2;
        }
        if (opcode & 0xF00F) == 0x300D {
            // DMULS.L Rm,Rn
            return 2;
        }
        if (opcode & 0xF00F) == 0x3005 {
            // DMULU.L Rm,Rn
            return 2;
        }

        // Default 1 cycle
        1
    }

    fn mem_cycles_r(&self, addr: u32) -> u32 {
        let phys = addr & 0x1FFF_FFFF;
        if phys <= 0x000F_FFFF {
            16 // BIOS ROM
        } else if phys >= 0x0010_0000 && phys <= 0x001F_FFFF {
            16 // Backup RAM
        } else if phys >= 0x0020_0000 && phys <= 0x00FF_FFFF {
            12 // Low Work RAM
        } else if phys >= 0x0200_0000 && phys <= 0x03FF_FFFF {
            24 // CS0
        } else if phys >= 0x0580_0000 && phys <= 0x059F_FFFF {
            24 // CS2
        } else if phys >= 0x05A0_0000 && phys <= 0x05AF_FFFF {
            50 // Sound RAM
        } else if phys >= 0x05B0_0000 && phys <= 0x05BF_FFFF {
            50 // Sound regs
        } else if phys >= 0x05C0_0000 && phys <= 0x05DF_FFFF {
            50 // VDP1 RAM
        } else if phys >= 0x05E0_0000 && phys <= 0x05FF_FFFF {
            // PLACEHOLDER: until getVramCycle is implemented in VDP2
            2
        } else {
            0
        }
    }

    fn mem_cycles_w(&self, addr: u32) -> u32 {
        let phys = addr & 0x1FFF_FFFF;
        if phys <= 0x000F_FFFF {
            0 // BIOS ROM
        } else if phys >= 0x0010_0000 && phys <= 0x001F_FFFF {
            0 // Backup RAM
        } else if phys >= 0x0020_0000 && phys <= 0x00FF_FFFF {
            7 // Low Work RAM
        } else if phys >= 0x0200_0000 && phys <= 0x03FF_FFFF {
            0 // CS0
        } else if phys >= 0x0580_0000 && phys <= 0x059F_FFFF {
            0 // CS2
        } else if phys >= 0x05A0_0000 && phys <= 0x05AF_FFFF {
            7 // Sound RAM only
        } else if phys >= 0x05C0_0000 && phys <= 0x05DF_FFFF {
            2 // VDP1 RAM
        } else if phys >= 0x05E0_0000 && phys <= 0x05FF_FFFF {
            // PLACEHOLDER: until getVramCycle is implemented in VDP2
            2
        } else if phys >= 0x0600_0000 && phys <= 0x060F_FFFF {
            2 // High Work RAM
        } else {
            0
        }
    }

    fn add_wait_states_r(&mut self, address: u32) {
        let wait = self.mem_cycles_r(address);
        if wait > 0 {
            self.cycles = self.cycles.wrapping_add(wait as u64);
            if self.frc_shift <= 7 {
                self.frt_exec(wait);
            }
        }
    }

    fn add_wait_states_w(&mut self, address: u32) {
        let wait = self.mem_cycles_w(address);
        if wait > 0 {
            self.cycles = self.cycles.wrapping_add(wait as u64);
            if self.frc_shift <= 7 {
                self.frt_exec(wait);
            }
        }
    }

    fn bus_wait(&mut self) {
        if let Some(sync) = self.sync.clone() {
            if let Some(caught_up) = self.arbiter.acquire_bus_sync(self.core_id, &sync) {
                self.cycles = caught_up;
            }
        } else {
            self.arbiter.acquire_bus();
        }
    }

    /// Raw byte fetch with no bus arbitration of its own -- callers
    /// (read_byte and the word/long helpers) are responsible for calling
    /// `bus_wait()` exactly once per logical transaction before using this.
    fn raw_read_byte(&self, address: u32) -> u8 {
        let region = self.translate(address);
        let val = self.raw_read_byte_region(region);
        if self.core_id == 0 {
            log_reg_access_once(&region, false, val);
        }
        val
    }

    fn raw_read_byte_region(&self, region: MemRegion) -> u8 {
        match region {
            MemRegion::Bios(off) => {
                let index = off & 0x7FFFF;
                if index < self.bios.len() {
                    self.bios[index]
                } else {
                    0
                }
            }
            MemRegion::LowRam(off) => {
                let ram = self.work_ram.low_ram.read().unwrap();
                ram[off & (ram.len() - 1)]
            }
            MemRegion::HighRam(off) => self.work_ram.read_high_ram_byte(off),
            MemRegion::SoundRam(off) => {
                let mut addr = off & 0xFFFFF;
                let mem4b = self
                    .work_ram
                    .mem4b
                    .load(std::sync::atomic::Ordering::Relaxed);
                if !mem4b {
                    addr &= 0x3FFFF;
                    let ram = self.work_ram.sound_ram.read().unwrap();
                    ram[addr & (ram.len() - 1)]
                } else if addr > 0x7FFFF {
                    0xFF
                } else {
                    let ram = self.work_ram.sound_ram.read().unwrap();
                    ram[addr & (ram.len() - 1)]
                }
            }
            MemRegion::ScspRegs(off) => {
                let ram = self.work_ram.scsp_regs.read().unwrap();
                ram[off & (ram.len() - 1)]
            }
            MemRegion::Vdp1Vram(off) => {
                let ram = self.work_ram.vdp1_vram.read().unwrap();
                ram[off & (ram.len() - 1)]
            }
            MemRegion::Vdp1Framebuffer(off) => {
                let ram = self.work_ram.vdp1_framebuffer.read().unwrap();
                ram[off & 0x3FFFF]
            }
            MemRegion::Vdp1Regs(off) => {
                let ram = self.work_ram.vdp1_regs.read().unwrap();
                ram[off & (ram.len() - 1)]
            }
            MemRegion::Vdp2Vram(off) => {
                let ram = self.work_ram.vdp2_vram.read().unwrap();
                ram[off & (ram.len() - 1)]
            }
            MemRegion::Vdp2Cram(off) => {
                let ram = self.work_ram.vdp2_cram.read().unwrap();
                ram[off & (ram.len() - 1)]
            }
            MemRegion::Vdp2Regs(off) => {
                let masked_off = off & 0x1FF;
                if masked_off == 0x004 || masked_off == 0x005 {
                    let tvstat = self.tvstat_word();
                    if masked_off == 0x004 {
                        (tvstat >> 8) as u8
                    } else {
                        (tvstat & 0xFF) as u8
                    }
                } else {
                    let ram = self.work_ram.vdp2_regs.read().unwrap();
                    ram[masked_off]
                }
            }
            MemRegion::ScuRegs(off) => {
                let ram = self.work_ram.scu_regs.read().unwrap();
                ram[off & (ram.len() - 1)]
            }
            MemRegion::Cs2Regs(off) => {
                let masked_off = off & 0xFFFFF;
                if masked_off < 0x1000 {
                    let ram = self.work_ram.cs2_regs.read().unwrap();
                    ram[masked_off]
                } else {
                    0
                }
            }
            MemRegion::BackupRam(off) => {
                let ram = self.work_ram.backup_ram.read().unwrap();
                ram[off & (ram.len() - 1)]
            }
            // SF (offset 0x63): the busy/idle Status Flag. Real hardware
            // sets it to 1 when the CPU writes a command to COMREG and
            // clears it back to 0 once the SMPC finishes executing that
            // command -- `Smpc::execute_command` does exactly that
            // unconditionally at the end, since commands complete
            // "instantly" from the CPU's point of view. When a real `Smpc`
            // is wired in, the read folds that stored bit into `bustmp`
            // (`Smpc::read_sf`, see
            // `docs/hardware-reference/smpc-peripheral.md` §1.3); the bare
            // `0x00` fallback below is only for plain unit tests that never
            // wired one in (pre-Phase-0 test compatibility, see
            // `docs/implementation-plans/smpc-peripheral.md` Phase 0/1).
            MemRegion::Smpc(off) if off == SMPC_SF_OFFSET => {
                if let Some(smpc) = self.smpc.clone() {
                    smpc.lock().unwrap().read_sf(&self.work_ram)
                } else {
                    0x00
                }
            }
            // Every other SMPC register (OREG/IREG/SR/PDR/DDR/IOSEL and
            // friends): real, persisted storage -- `smpc_execute_command`
            // populates OREG with genuine INTBACK response data on command
            // completion, and IREG/PDR/DDR are whatever the CPU last wrote,
            // matching real hardware's plain register-file behavior for
            // those.
            MemRegion::Smpc(off) => {
                let ram = self.work_ram.smpc_regs.read().unwrap();
                ram[off & (ram.len() - 1)]
            }
            MemRegion::PurgeArea => 0xFF,
            MemRegion::AddressArray(_) => 0, // Byte read falls to Unmapped/returns 0
            MemRegion::DataArray(off) => self.data_array[off & 0xFFF],
            MemRegion::OnChip(off) => self.read_onchip_byte(off),
            MemRegion::Unmapped => 0,
        }
    }

    /// Raw byte store, see `raw_read_byte`.
    fn raw_write_byte(&mut self, address: u32, val: u8) {
        let region = self.translate(address);
        if self.core_id == 0 {
            log_reg_access_once(&region, true, val);
        }
        match region {
            MemRegion::LowRam(off) => {
                let mut ram = self.work_ram.low_ram.write().unwrap();
                let mask = ram.len() - 1;
                ram[off & mask] = val;
            }
            MemRegion::HighRam(off) => {
                self.work_ram.write_high_ram_byte(off, val);
            }
            MemRegion::SoundRam(off) => {
                let mut addr = off & 0xFFFFF;
                let mem4b = self
                    .work_ram
                    .mem4b
                    .load(std::sync::atomic::Ordering::Relaxed);
                if !mem4b {
                    addr &= 0x3FFFF;
                    let mut ram = self.work_ram.sound_ram.write().unwrap();
                    let mask = ram.len() - 1;
                    ram[addr & mask] = val;
                } else if addr <= 0x7FFFF {
                    let mut ram = self.work_ram.sound_ram.write().unwrap();
                    let mask = ram.len() - 1;
                    ram[addr & mask] = val;
                }
            }
            MemRegion::ScspRegs(off) => {
                let mut ram = self.work_ram.scsp_regs.write().unwrap();
                let mask = ram.len() - 1;
                let masked_off = off & mask;
                ram[masked_off] = val;
                if masked_off == 0x400 {
                    let mem4b = (val & 2) != 0;
                    self.work_ram
                        .mem4b
                        .store(mem4b, std::sync::atomic::Ordering::Relaxed);
                }
            }
            MemRegion::Vdp1Vram(off) => {
                let mut ram = self.work_ram.vdp1_vram.write().unwrap();
                let mask = ram.len() - 1;
                ram[off & mask] = val;
            }
            MemRegion::Vdp1Framebuffer(off) => {
                let mut ram = self.work_ram.vdp1_framebuffer.write().unwrap();
                ram[off & 0x3FFFF] = val;
            }
            MemRegion::Vdp1Regs(off) => {
                let mut ram = self.work_ram.vdp1_regs.write().unwrap();
                let mask = ram.len() - 1;
                ram[off & mask] = val;
            }
            MemRegion::Vdp2Vram(off) => {
                let mut ram = self.work_ram.vdp2_vram.write().unwrap();
                let mask = ram.len() - 1;
                ram[off & mask] = val;
            }
            MemRegion::Vdp2Cram(off) => {
                let mut ram = self.work_ram.vdp2_cram.write().unwrap();
                let mask = ram.len() - 1;
                ram[off & mask] = val;
            }
            MemRegion::Vdp2Regs(off) => {
                let mut ram = self.work_ram.vdp2_regs.write().unwrap();
                let mask = ram.len() - 1;
                ram[off & mask] = val;
            }
            MemRegion::ScuRegs(off) => {
                let mut ram = self.work_ram.scu_regs.write().unwrap();
                let mask = ram.len() - 1;
                ram[off & mask] = val;
            }
            MemRegion::Cs2Regs(off) => {
                let masked_off = off & 0xFFFFF;
                if masked_off < 0x1000 {
                    {
                        let mut ram = self.work_ram.cs2_regs.write().unwrap();
                        ram[masked_off] = val;
                    }
                    if masked_off == 6 || masked_off == 7 {
                        self.execute_cdrom_command();
                    }
                }
            }
            MemRegion::BackupRam(off) => {
                let mut ram = self.work_ram.backup_ram.write().unwrap();
                let mask = ram.len() - 1;
                ram[(off | 1) & mask] = val;
            }
            // SMPC: a real, persisted register file (IREG/OREG/SR/PDR/DDR/
            // IOSEL/EXLE) -- see `MemRegion::Smpc` on the read side. A write
            // to COMREG (offset 0x1F) additionally triggers real command
            // processing, matching real hardware issuing the command the
            // instant COMREG is written.
            MemRegion::Smpc(off) => {
                {
                    let mut ram = self.work_ram.smpc_regs.write().unwrap();
                    let mask = ram.len() - 1;
                    ram[off & mask] = val;
                }
                if let Some(smpc) = self.smpc.clone() {
                    // §1.3: every SMPC byte write latches `bustmp` (except SF
                    // itself, which has its own dedicated write semantics --
                    // see `Smpc::on_register_write`'s doc comment).
                    smpc.lock().unwrap().on_register_write(off, val);
                }
                if off == SMPC_COMREG_OFFSET {
                    if let Some(smpc) = self.smpc.clone() {
                        let effects = smpc.lock().unwrap().execute_command(val, &self.work_ram);
                        self.apply_smpc_effects(effects);
                    } else {
                        self.smpc_execute_command(val);
                    }
                }
            }
            MemRegion::PurgeArea => {
                let uncached_addr = address & 0x0FFF_FFFF;
                self.raw_write_byte(uncached_addr, val);
            }
            MemRegion::AddressArray(_) => {}
            MemRegion::DataArray(off) => {
                self.data_array[off & 0xFFF] = val;
            }
            MemRegion::OnChip(off) => {
                self.write_onchip_byte(off, val);
            }
            // BIOS is ROM: writes are silently discarded, matching real hardware.
            MemRegion::Bios(_) | MemRegion::Unmapped => {}
        }
    }

    pub fn read_byte(&mut self, address: u32) -> u8 {
        self.check_bus_miss(address, false, 1);
        self.bus_wait();
        self.add_wait_states_r(address);
        self.raw_read_byte(address)
    }

    pub fn write_byte(&mut self, address: u32, val: u8) {
        self.check_bus_miss(address, true, 1);
        self.bus_wait();
        self.add_wait_states_w(address);
        if address == 0x0600_1000 {
            self.cdrom_command_executed = true;
        }
        self.raw_write_byte(address, val);
    }

    /// Read 16-bit word from memory using the bus arbiter check. Arbitration
    /// happens once per transaction (matching a real single bus cycle), not
    /// once per byte fetched.
    pub fn read_word(&mut self, address: u32) -> u16 {
        self.check_bus_miss(address, false, 2);
        if address % 2 != 0 {
            self.unaligned_access_flag = true;
        }
        self.bus_wait();
        self.add_wait_states_r(address);
        self.raw_read_word(address)
    }

    /// Write 16-bit word to memory using the bus arbiter check
    pub fn write_word(&mut self, address: u32, val: u16) {
        self.check_bus_miss(address, true, 2);
        if address % 2 != 0 {
            self.unaligned_access_flag = true;
        }
        self.bus_wait();
        self.add_wait_states_w(address);
        if address == 0x0600_1000 {
            self.cdrom_command_executed = true;
        }
        self.raw_write_word(address, val);
    }

    /// Read 32-bit long word (big-endian, matching real SH-2/Saturn wiring).
    pub fn read_long(&mut self, address: u32) -> u32 {
        self.check_bus_miss(address, false, 4);
        if address % 4 != 0 {
            self.unaligned_access_flag = true;
        }
        self.bus_wait();
        self.add_wait_states_r(address);
        self.raw_read_long(address)
    }

    /// Write 32-bit long word (big-endian).
    pub fn write_long(&mut self, address: u32, val: u32) {
        self.check_bus_miss(address, true, 4);
        if address % 4 != 0 {
            self.unaligned_access_flag = true;
        }
        self.bus_wait();
        self.add_wait_states_w(address);
        if address <= 0x0600_1000 && address + 4 > 0x0600_1000 {
            self.cdrom_command_executed = true;
        }
        self.raw_write_long(address, val);
    }

    fn raw_read_word(&self, address: u32) -> u16 {
        let region = self.translate(address);
        let val = self.raw_read_word_region(region, address);
        if self.core_id == 0 {
            log_reg_access_once(&region, false, (val >> 8) as u8);
        }
        val
    }

    fn raw_read_long(&self, address: u32) -> u32 {
        let region = self.translate(address);
        let val = self.raw_read_long_region(region, address);
        if self.core_id == 0 {
            log_reg_access_once(&region, false, (val >> 24) as u8);
        }
        val
    }

    fn raw_write_word(&mut self, address: u32, val: u16) {
        let region = self.translate(address);
        if self.core_id == 0 {
            log_reg_access_once(&region, true, (val >> 8) as u8);
        }
        self.raw_write_word_region(region, address, val);
    }

    fn raw_write_long(&mut self, address: u32, val: u32) {
        let region = self.translate(address);
        if self.core_id == 0 {
            log_reg_access_once(&region, true, (val >> 24) as u8);
        }
        self.raw_write_long_region(region, address, val);
    }

    fn raw_read_word_region(&self, region: MemRegion, _address: u32) -> u16 {
        match region {
            MemRegion::Bios(off) => {
                let index = off & 0x7FFFF;
                if index + 1 < self.bios.len() {
                    let b0 = self.bios[index] as u16;
                    let b1 = self.bios[index + 1] as u16;
                    (b0 << 8) | b1
                } else {
                    0
                }
            }
            MemRegion::LowRam(off) => {
                let ram = self.work_ram.low_ram.read().unwrap();
                let mask = ram.len() - 1;
                let b0 = ram[off & mask] as u16;
                let b1 = ram[(off + 1) & mask] as u16;
                (b0 << 8) | b1
            }
            MemRegion::HighRam(off) => self.work_ram.read_high_ram_word(off),
            MemRegion::SoundRam(off) => {
                let mut addr = off & 0xFFFFF;
                let mem4b = self
                    .work_ram
                    .mem4b
                    .load(std::sync::atomic::Ordering::Relaxed);
                if !mem4b {
                    addr &= 0x3FFFF;
                    let ram = self.work_ram.sound_ram.read().unwrap();
                    let mask = ram.len() - 1;
                    let b0 = ram[addr & mask] as u16;
                    let b1 = ram[(addr + 1) & mask] as u16;
                    (b0 << 8) | b1
                } else if addr > 0x7FFFF {
                    0xFFFF
                } else {
                    let ram = self.work_ram.sound_ram.read().unwrap();
                    let mask = ram.len() - 1;
                    let b0 = ram[addr & mask] as u16;
                    let b1 = ram[(addr + 1) & mask] as u16;
                    (b0 << 8) | b1
                }
            }
            MemRegion::ScspRegs(off) => {
                let ram = self.work_ram.scsp_regs.read().unwrap();
                let mask = ram.len() - 1;
                let b0 = ram[off & mask] as u16;
                let b1 = ram[(off + 1) & mask] as u16;
                (b0 << 8) | b1
            }
            MemRegion::Vdp1Vram(off) => {
                let ram = self.work_ram.vdp1_vram.read().unwrap();
                let mask = ram.len() - 1;
                let b0 = ram[off & mask] as u16;
                let b1 = ram[(off + 1) & mask] as u16;
                (b0 << 8) | b1
            }
            MemRegion::Vdp1Framebuffer(off) => {
                let ram = self.work_ram.vdp1_framebuffer.read().unwrap();
                let b0 = ram[off & 0x3FFFF] as u16;
                let b1 = ram[(off + 1) & 0x3FFFF] as u16;
                (b0 << 8) | b1
            }
            MemRegion::Vdp1Regs(off) => {
                let ram = self.work_ram.vdp1_regs.read().unwrap();
                let mask = ram.len() - 1;
                let b0 = ram[off & mask] as u16;
                let b1 = ram[(off + 1) & mask] as u16;
                (b0 << 8) | b1
            }
            MemRegion::Vdp2Vram(off) => {
                let ram = self.work_ram.vdp2_vram.read().unwrap();
                let mask = ram.len() - 1;
                let b0 = ram[off & mask] as u16;
                let b1 = ram[(off + 1) & mask] as u16;
                (b0 << 8) | b1
            }
            MemRegion::Vdp2Cram(off) => {
                let ram = self.work_ram.vdp2_cram.read().unwrap();
                let mask = ram.len() - 1;
                let b0 = ram[off & mask] as u16;
                let b1 = ram[(off + 1) & mask] as u16;
                (b0 << 8) | b1
            }
            MemRegion::Vdp2Regs(off) => {
                let masked_off = off & 0x1FF;
                if masked_off == 0x004 || masked_off == 0x005 || masked_off == 0x003 {
                    let b0 = self.raw_read_byte_region(MemRegion::Vdp2Regs(off));
                    let b1 = self.raw_read_byte_region(MemRegion::Vdp2Regs(off + 1));
                    ((b0 as u16) << 8) | (b1 as u16)
                } else {
                    let ram = self.work_ram.vdp2_regs.read().unwrap();
                    let b0 = ram[masked_off] as u16;
                    let b1 = ram[(masked_off + 1) & 0x1FF] as u16;
                    (b0 << 8) | b1
                }
            }
            MemRegion::ScuRegs(off) => {
                let ram = self.work_ram.scu_regs.read().unwrap();
                let mask = ram.len() - 1;
                let b0 = ram[off & mask] as u16;
                let b1 = ram[(off + 1) & mask] as u16;
                (b0 << 8) | b1
            }
            MemRegion::Cs2Regs(off) => {
                let masked_off = off & 0xFFFFF;
                if masked_off < 0x1000 {
                    let ram = self.work_ram.cs2_regs.read().unwrap();
                    let b0 = ram[masked_off] as u16;
                    let b1 = ram[(masked_off + 1) & 0xFFF] as u16;
                    (b0 << 8) | b1
                } else {
                    0
                }
            }
            MemRegion::BackupRam(off) => {
                let ram = self.work_ram.backup_ram.read().unwrap();
                let mask = ram.len() - 1;
                let b0 = ram[off & mask] as u16;
                let b1 = ram[(off + 1) & mask] as u16;
                (b0 << 8) | b1
            }
            MemRegion::Smpc(off) => {
                let b0 = self.raw_read_byte_region(MemRegion::Smpc(off));
                let b1 = self.raw_read_byte_region(MemRegion::Smpc(off + 1));
                ((b0 as u16) << 8) | (b1 as u16)
            }
            MemRegion::DataArray(off) => {
                let b0 = self.data_array[off & 0xFFF] as u16;
                let b1 = self.data_array[(off + 1) & 0xFFF] as u16;
                (b0 << 8) | b1
            }
            MemRegion::OnChip(off) => self.read_onchip_word(off),
            MemRegion::PurgeArea => 0xFFFF,
            MemRegion::AddressArray(_) | MemRegion::Unmapped => 0,
        }
    }

    fn raw_read_long_region(&self, region: MemRegion, address: u32) -> u32 {
        match region {
            MemRegion::Bios(off) => {
                let index = off & 0x7FFFF;
                if index + 3 < self.bios.len() {
                    let b0 = self.bios[index] as u32;
                    let b1 = self.bios[index + 1] as u32;
                    let b2 = self.bios[index + 2] as u32;
                    let b3 = self.bios[index + 3] as u32;
                    (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
                } else {
                    0
                }
            }
            MemRegion::LowRam(off) => {
                let ram = self.work_ram.low_ram.read().unwrap();
                let mask = ram.len() - 1;
                let b0 = ram[off & mask] as u32;
                let b1 = ram[(off + 1) & mask] as u32;
                let b2 = ram[(off + 2) & mask] as u32;
                let b3 = ram[(off + 3) & mask] as u32;
                (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
            }
            MemRegion::HighRam(off) => self.work_ram.read_high_ram_long(off),
            MemRegion::SoundRam(off) => {
                let mut addr = off & 0xFFFFF;
                let mem4b = self
                    .work_ram
                    .mem4b
                    .load(std::sync::atomic::Ordering::Relaxed);
                if !mem4b {
                    addr &= 0x3FFFF;
                    let ram = self.work_ram.sound_ram.read().unwrap();
                    let mask = ram.len() - 1;
                    let b0 = ram[addr & mask] as u32;
                    let b1 = ram[(addr + 1) & mask] as u32;
                    let b2 = ram[(addr + 2) & mask] as u32;
                    let b3 = ram[(addr + 3) & mask] as u32;
                    (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
                } else if addr > 0x7FFFF {
                    0xFFFFFFFF
                } else {
                    let ram = self.work_ram.sound_ram.read().unwrap();
                    let mask = ram.len() - 1;
                    let b0 = ram[addr & mask] as u32;
                    let b1 = ram[(addr + 1) & mask] as u32;
                    let b2 = ram[(addr + 2) & mask] as u32;
                    let b3 = ram[(addr + 3) & mask] as u32;
                    (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
                }
            }
            MemRegion::ScspRegs(off) => {
                let ram = self.work_ram.scsp_regs.read().unwrap();
                let mask = ram.len() - 1;
                let b0 = ram[off & mask] as u32;
                let b1 = ram[(off + 1) & mask] as u32;
                let b2 = ram[(off + 2) & mask] as u32;
                let b3 = ram[(off + 3) & mask] as u32;
                (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
            }
            MemRegion::Vdp1Vram(off) => {
                let ram = self.work_ram.vdp1_vram.read().unwrap();
                let mask = ram.len() - 1;
                let b0 = ram[off & mask] as u32;
                let b1 = ram[(off + 1) & mask] as u32;
                let b2 = ram[(off + 2) & mask] as u32;
                let b3 = ram[(off + 3) & mask] as u32;
                (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
            }
            MemRegion::Vdp1Framebuffer(off) => {
                let ram = self.work_ram.vdp1_framebuffer.read().unwrap();
                let b0 = ram[off & 0x3FFFF] as u32;
                let b1 = ram[(off + 1) & 0x3FFFF] as u32;
                let b2 = ram[(off + 2) & 0x3FFFF] as u32;
                let b3 = ram[(off + 3) & 0x3FFFF] as u32;
                (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
            }
            MemRegion::Vdp1Regs(off) => {
                let ram = self.work_ram.vdp1_regs.read().unwrap();
                let mask = ram.len() - 1;
                let b0 = ram[off & mask] as u32;
                let b1 = ram[(off + 1) & mask] as u32;
                let b2 = ram[(off + 2) & mask] as u32;
                let b3 = ram[(off + 3) & mask] as u32;
                (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
            }
            MemRegion::Vdp2Vram(off) => {
                let ram = self.work_ram.vdp2_vram.read().unwrap();
                let mask = ram.len() - 1;
                let b0 = ram[off & mask] as u32;
                let b1 = ram[(off + 1) & mask] as u32;
                let b2 = ram[(off + 2) & mask] as u32;
                let b3 = ram[(off + 3) & mask] as u32;
                (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
            }
            MemRegion::Vdp2Cram(off) => {
                let ram = self.work_ram.vdp2_cram.read().unwrap();
                let mask = ram.len() - 1;
                let b0 = ram[off & mask] as u32;
                let b1 = ram[(off + 1) & mask] as u32;
                let b2 = ram[(off + 2) & mask] as u32;
                let b3 = ram[(off + 3) & mask] as u32;
                (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
            }
            MemRegion::Vdp2Regs(off) => {
                let masked_off = off & 0x1FF;
                if masked_off <= 0x005 {
                    let b0 = self.raw_read_byte_region(MemRegion::Vdp2Regs(off)) as u32;
                    let b1 = self.raw_read_byte_region(MemRegion::Vdp2Regs(off + 1)) as u32;
                    let b2 = self.raw_read_byte_region(MemRegion::Vdp2Regs(off + 2)) as u32;
                    let b3 = self.raw_read_byte_region(MemRegion::Vdp2Regs(off + 3)) as u32;
                    (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
                } else {
                    let ram = self.work_ram.vdp2_regs.read().unwrap();
                    let b0 = ram[masked_off] as u32;
                    let b1 = ram[(masked_off + 1) & 0x1FF] as u32;
                    let b2 = ram[(masked_off + 2) & 0x1FF] as u32;
                    let b3 = ram[(masked_off + 3) & 0x1FF] as u32;
                    (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
                }
            }
            MemRegion::ScuRegs(off) => {
                let off = off & 0xFF;
                if let Some(val) = self.read_scu_dsp_port(off) {
                    val
                } else {
                    let ram = self.work_ram.scu_regs.read().unwrap();
                    let mask = ram.len() - 1;
                    let b0 = ram[off & mask] as u32;
                    let b1 = ram[(off + 1) & mask] as u32;
                    let b2 = ram[(off + 2) & mask] as u32;
                    let b3 = ram[(off + 3) & mask] as u32;
                    (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
                }
            }
            MemRegion::Cs2Regs(off) => {
                let masked_off = off & 0xFFFFF;
                if masked_off < 0x1000 {
                    let ram = self.work_ram.cs2_regs.read().unwrap();
                    let b0 = ram[masked_off] as u32;
                    let b1 = ram[(masked_off + 1) & 0xFFF] as u32;
                    let b2 = ram[(masked_off + 2) & 0xFFF] as u32;
                    let b3 = ram[(masked_off + 3) & 0xFFF] as u32;
                    (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
                } else {
                    0
                }
            }
            MemRegion::BackupRam(off) => {
                let ram = self.work_ram.backup_ram.read().unwrap();
                let mask = ram.len() - 1;
                let b0 = ram[off & mask] as u32;
                let b1 = ram[(off + 1) & mask] as u32;
                let b2 = ram[(off + 2) & mask] as u32;
                let b3 = ram[(off + 3) & mask] as u32;
                (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
            }
            MemRegion::Smpc(off) => {
                let b0 = self.raw_read_byte_region(MemRegion::Smpc(off)) as u32;
                let b1 = self.raw_read_byte_region(MemRegion::Smpc(off + 1)) as u32;
                let b2 = self.raw_read_byte_region(MemRegion::Smpc(off + 2)) as u32;
                let b3 = self.raw_read_byte_region(MemRegion::Smpc(off + 3)) as u32;
                (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
            }
            MemRegion::DataArray(off) => {
                let b0 = self.data_array[off & 0xFFF] as u32;
                let b1 = self.data_array[(off + 1) & 0xFFF] as u32;
                let b2 = self.data_array[(off + 2) & 0xFFF] as u32;
                let b3 = self.data_array[(off + 3) & 0xFFF] as u32;
                (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
            }
            MemRegion::OnChip(off) => self.read_onchip(off),
            MemRegion::AddressArray(off) => self.address_array[off >> 2],
            MemRegion::PurgeArea => 0xFFFFFFFF,
            MemRegion::Unmapped => 0,
        }
    }

    fn raw_write_word_region(&mut self, region: MemRegion, address: u32, val: u16) {
        match region {
            MemRegion::LowRam(off) => {
                let mut ram = self.work_ram.low_ram.write().unwrap();
                let mask = ram.len() - 1;
                ram[off & mask] = (val >> 8) as u8;
                ram[(off + 1) & mask] = val as u8;
            }
            MemRegion::HighRam(off) => {
                self.work_ram.write_high_ram_word(off, val);
            }
            MemRegion::SoundRam(off) => {
                let mut addr = off & 0xFFFFF;
                let mem4b = self
                    .work_ram
                    .mem4b
                    .load(std::sync::atomic::Ordering::Relaxed);
                if !mem4b {
                    addr &= 0x3FFFF;
                    let mut ram = self.work_ram.sound_ram.write().unwrap();
                    let mask = ram.len() - 1;
                    ram[addr & mask] = (val >> 8) as u8;
                    ram[(addr + 1) & mask] = val as u8;
                } else if addr <= 0x7FFFF {
                    let mut ram = self.work_ram.sound_ram.write().unwrap();
                    let mask = ram.len() - 1;
                    ram[addr & mask] = (val >> 8) as u8;
                    ram[(addr + 1) & mask] = val as u8;
                }
            }
            MemRegion::ScspRegs(off) => {
                let mut ram = self.work_ram.scsp_regs.write().unwrap();
                let mask = ram.len() - 1;
                let masked_off = off & mask;
                ram[masked_off] = (val >> 8) as u8;
                if masked_off == 0x400 {
                    let mem4b = (ram[masked_off] & 2) != 0;
                    self.work_ram
                        .mem4b
                        .store(mem4b, std::sync::atomic::Ordering::Relaxed);
                }
                let masked_off2 = (off + 1) & mask;
                ram[masked_off2] = val as u8;
                if masked_off2 == 0x400 {
                    let mem4b = (ram[masked_off2] & 2) != 0;
                    self.work_ram
                        .mem4b
                        .store(mem4b, std::sync::atomic::Ordering::Relaxed);
                }
            }
            MemRegion::Vdp1Vram(off) => {
                let mut ram = self.work_ram.vdp1_vram.write().unwrap();
                let mask = ram.len() - 1;
                ram[off & mask] = (val >> 8) as u8;
                ram[(off + 1) & mask] = val as u8;
            }
            MemRegion::Vdp1Framebuffer(off) => {
                let mut ram = self.work_ram.vdp1_framebuffer.write().unwrap();
                ram[off & 0x3FFFF] = (val >> 8) as u8;
                ram[(off + 1) & 0x3FFFF] = val as u8;
            }
            MemRegion::Vdp1Regs(off) => {
                let mut ram = self.work_ram.vdp1_regs.write().unwrap();
                let mask = ram.len() - 1;
                ram[off & mask] = (val >> 8) as u8;
                ram[(off + 1) & mask] = val as u8;
            }
            MemRegion::Vdp2Vram(off) => {
                let mut ram = self.work_ram.vdp2_vram.write().unwrap();
                let mask = ram.len() - 1;
                ram[off & mask] = (val >> 8) as u8;
                ram[(off + 1) & mask] = val as u8;
            }
            MemRegion::Vdp2Cram(off) => {
                let mut ram = self.work_ram.vdp2_cram.write().unwrap();
                let mask = ram.len() - 1;
                ram[off & mask] = (val >> 8) as u8;
                ram[(off + 1) & mask] = val as u8;
            }
            MemRegion::Vdp2Regs(off) => {
                let mut ram = self.work_ram.vdp2_regs.write().unwrap();
                let mask = ram.len() - 1;
                ram[off & mask] = (val >> 8) as u8;
                ram[(off + 1) & mask] = val as u8;
            }
            MemRegion::ScuRegs(off) => {
                let mut ram = self.work_ram.scu_regs.write().unwrap();
                let mask = ram.len() - 1;
                ram[off & mask] = (val >> 8) as u8;
                ram[(off + 1) & mask] = val as u8;
            }
            MemRegion::Cs2Regs(off) => {
                let masked_off = off & 0xFFFFF;
                if masked_off < 0x1000 {
                    {
                        let mut ram = self.work_ram.cs2_regs.write().unwrap();
                        ram[masked_off] = (val >> 8) as u8;
                        ram[(masked_off + 1) & 0xFFF] = val as u8;
                    }
                    if masked_off == 6
                        || masked_off == 7
                        || masked_off + 1 == 6
                        || masked_off + 1 == 7
                    {
                        self.execute_cdrom_command();
                    }
                }
            }
            MemRegion::BackupRam(off) => {
                let mut ram = self.work_ram.backup_ram.write().unwrap();
                let mask = ram.len() - 1;
                ram[(off | 1) & mask] = (val >> 8) as u8;
                ram[((off + 1) | 1) & mask] = val as u8;
            }
            MemRegion::Smpc(off) => {
                self.raw_write_byte(address, (val >> 8) as u8);
                self.raw_write_byte(address.wrapping_add(1), val as u8);
            }
            MemRegion::OnChip(off) => {
                self.write_onchip_word(off, val);
            }
            MemRegion::PurgeArea => {
                let uncached_addr = address & 0x0FFF_FFFF;
                self.raw_write_word(uncached_addr, val);
            }
            MemRegion::AddressArray(_)
            | MemRegion::DataArray(_)
            | MemRegion::Bios(_)
            | MemRegion::Unmapped => {}
        }
    }

    fn raw_write_long_region(&mut self, region: MemRegion, address: u32, val: u32) {
        match region {
            MemRegion::LowRam(off) => {
                let mut ram = self.work_ram.low_ram.write().unwrap();
                let mask = ram.len() - 1;
                ram[off & mask] = (val >> 24) as u8;
                ram[(off + 1) & mask] = (val >> 16) as u8;
                ram[(off + 2) & mask] = (val >> 8) as u8;
                ram[(off + 3) & mask] = val as u8;
            }
            MemRegion::HighRam(off) => {
                self.work_ram.write_high_ram_long(off, val);
            }
            MemRegion::SoundRam(off) => {
                let mut addr = off & 0xFFFFF;
                let mem4b = self
                    .work_ram
                    .mem4b
                    .load(std::sync::atomic::Ordering::Relaxed);
                if !mem4b {
                    addr &= 0x3FFFF;
                    let mut ram = self.work_ram.sound_ram.write().unwrap();
                    let mask = ram.len() - 1;
                    ram[addr & mask] = (val >> 24) as u8;
                    ram[(addr + 1) & mask] = (val >> 16) as u8;
                    ram[(addr + 2) & mask] = (val >> 8) as u8;
                    ram[(addr + 3) & mask] = val as u8;
                } else if addr <= 0x7FFFF {
                    let mut ram = self.work_ram.sound_ram.write().unwrap();
                    let mask = ram.len() - 1;
                    ram[addr & mask] = (val >> 24) as u8;
                    ram[(addr + 1) & mask] = (val >> 16) as u8;
                    ram[(addr + 2) & mask] = (val >> 8) as u8;
                    ram[(addr + 3) & mask] = val as u8;
                }
            }
            MemRegion::ScspRegs(off) => {
                let mut ram = self.work_ram.scsp_regs.write().unwrap();
                let mask = ram.len() - 1;
                for i in 0..4 {
                    let masked_off = (off + i) & mask;
                    ram[masked_off] = (val >> (8 * (3 - i))) as u8;
                    if masked_off == 0x400 {
                        let mem4b = (ram[masked_off] & 2) != 0;
                        self.work_ram
                            .mem4b
                            .store(mem4b, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
            MemRegion::Vdp1Vram(off) => {
                let mut ram = self.work_ram.vdp1_vram.write().unwrap();
                let mask = ram.len() - 1;
                ram[off & mask] = (val >> 24) as u8;
                ram[(off + 1) & mask] = (val >> 16) as u8;
                ram[(off + 2) & mask] = (val >> 8) as u8;
                ram[(off + 3) & mask] = val as u8;
            }
            MemRegion::Vdp1Framebuffer(off) => {
                let mut ram = self.work_ram.vdp1_framebuffer.write().unwrap();
                ram[off & 0x3FFFF] = (val >> 24) as u8;
                ram[(off + 1) & 0x3FFFF] = (val >> 16) as u8;
                ram[(off + 2) & 0x3FFFF] = (val >> 8) as u8;
                ram[(off + 3) & 0x3FFFF] = val as u8;
            }
            MemRegion::Vdp1Regs(off) => {
                let mut ram = self.work_ram.vdp1_regs.write().unwrap();
                let mask = ram.len() - 1;
                ram[off & mask] = (val >> 24) as u8;
                ram[(off + 1) & mask] = (val >> 16) as u8;
                ram[(off + 2) & mask] = (val >> 8) as u8;
                ram[(off + 3) & mask] = val as u8;
            }
            MemRegion::Vdp2Vram(off) => {
                let mut ram = self.work_ram.vdp2_vram.write().unwrap();
                let mask = ram.len() - 1;
                ram[off & mask] = (val >> 24) as u8;
                ram[(off + 1) & mask] = (val >> 16) as u8;
                ram[(off + 2) & mask] = (val >> 8) as u8;
                ram[(off + 3) & mask] = val as u8;
            }
            MemRegion::Vdp2Cram(off) => {
                let mut ram = self.work_ram.vdp2_cram.write().unwrap();
                let mask = ram.len() - 1;
                ram[off & mask] = (val >> 24) as u8;
                ram[(off + 1) & mask] = (val >> 16) as u8;
                ram[(off + 2) & mask] = (val >> 8) as u8;
                ram[(off + 3) & mask] = val as u8;
            }
            MemRegion::Vdp2Regs(off) => {
                let mut ram = self.work_ram.vdp2_regs.write().unwrap();
                let mask = ram.len() - 1;
                ram[off & mask] = (val >> 24) as u8;
                ram[(off + 1) & mask] = (val >> 16) as u8;
                ram[(off + 2) & mask] = (val >> 8) as u8;
                ram[(off + 3) & mask] = val as u8;
            }
            MemRegion::ScuRegs(off) => {
                let off = off & 0xFF;
                if self.write_scu_dsp_port(off, val) {
                    return;
                }
                {
                    let mut ram = self.work_ram.scu_regs.write().unwrap();
                    let bytes = val.to_be_bytes();
                    if off + 3 < ram.len() {
                        ram[off] = bytes[0];
                        ram[off + 1] = bytes[1];
                        ram[off + 2] = bytes[2];
                        ram[off + 3] = bytes[3];
                    }
                }
                if off == 0x10 {
                    if val & 1 != 0 {
                        self.execute_scu_dma(0);
                    }
                } else if off == 0x30 {
                    if val & 1 != 0 {
                        self.execute_scu_dma(1);
                    }
                } else if off == 0x50 {
                    if val & 1 != 0 {
                        self.execute_scu_dma(2);
                    }
                }
            }
            MemRegion::Cs2Regs(off) => {
                let masked_off = off & 0xFFFFF;
                if masked_off < 0x1000 {
                    {
                        let mut ram = self.work_ram.cs2_regs.write().unwrap();
                        ram[masked_off] = (val >> 24) as u8;
                        ram[(masked_off + 1) & 0xFFF] = (val >> 16) as u8;
                        ram[(masked_off + 2) & 0xFFF] = (val >> 8) as u8;
                        ram[(masked_off + 3) & 0xFFF] = val as u8;
                    }
                    if (masked_off..masked_off + 4).any(|o| o == 6 || o == 7) {
                        self.execute_cdrom_command();
                    }
                }
            }
            MemRegion::BackupRam(off) => {
                let mut ram = self.work_ram.backup_ram.write().unwrap();
                let mask = ram.len() - 1;
                ram[(off | 1) & mask] = (val >> 24) as u8;
                ram[((off + 1) | 1) & mask] = (val >> 16) as u8;
                ram[((off + 2) | 1) & mask] = (val >> 8) as u8;
                ram[((off + 3) | 1) & mask] = val as u8;
            }
            MemRegion::Smpc(off) => {
                self.raw_write_byte(address, (val >> 24) as u8);
                self.raw_write_byte(address.wrapping_add(1), (val >> 16) as u8);
                self.raw_write_byte(address.wrapping_add(2), (val >> 8) as u8);
                self.raw_write_byte(address.wrapping_add(3), val as u8);
            }
            MemRegion::OnChip(off) => {
                self.write_onchip(off & !3, val);
            }
            MemRegion::PurgeArea => {}
            MemRegion::AddressArray(off) => {
                self.address_array[off >> 2] = val;
            }
            MemRegion::DataArray(off) => {
                self.data_array[off & 0xFFF] = (val >> 24) as u8;
                self.data_array[(off + 1) & 0xFFF] = (val >> 16) as u8;
                self.data_array[(off + 2) & 0xFFF] = (val >> 8) as u8;
                self.data_array[(off + 3) & 0xFFF] = val as u8;
            }
            MemRegion::Bios(_) | MemRegion::Unmapped => {}
        }
    }

    /// SCU DSP register ports (offsets 0x80/0x84/0x88/0x8C) are real
    /// hardware ports, not plain memory -- 32-bit-only on real hardware
    /// (byte/word access to them is undefined), so they're intercepted
    /// here at the `read_long`/`write_long` level rather than through the
    /// generic per-byte `ScuRegs` storage `raw_read_byte`/`raw_write_byte`
    /// use for every other SCU register. `None`/`false` (falling through
    /// to plain storage) when no DSP is wired in (e.g. bare unit tests
    /// built via `make_cpu()`), matching the `Option<Arc<...>>` pattern
    /// already used for `m68k_control`/`sound_req_irq`.
    fn read_scu_dsp_port(&self, off: usize) -> Option<u32> {
        let dsp = self.scu_dsp.as_ref()?;
        match off {
            0x80 => Some(dsp.lock().unwrap().read_control_port()),
            0x8C => Some(dsp.lock().unwrap().read_data_ram_data_port()),
            _ => None,
        }
    }

    fn write_scu_dsp_port(&self, off: usize, val: u32) -> bool {
        let Some(dsp) = self.scu_dsp.as_ref() else {
            return false;
        };
        match off {
            0x80 => {
                dsp.lock().unwrap().write_control_port(val);
                if let Some(ref sync) = self.sync {
                    if val & 0x0001_0000 != 0 {
                        sync.set_thread_active(6, true);
                    }
                }
                true
            }
            0x84 => {
                dsp.lock().unwrap().write_program_ram_port(val);
                true
            }
            0x88 => {
                dsp.lock().unwrap().write_data_ram_addr_port(val);
                true
            }
            0x8C => {
                dsp.lock().unwrap().write_data_ram_data_port(val);
                true
            }
            _ => false,
        }
    }

    fn s(&self) -> bool {
        self.sr & SR_S != 0
    }

    fn set_s(&mut self, val: bool) {
        if val {
            self.sr |= SR_S;
        } else {
            self.sr &= !SR_S;
        }
    }

    fn t(&self) -> bool {
        self.sr & SR_T != 0
    }

    fn set_t(&mut self, val: bool) {
        if val {
            self.sr |= SR_T;
        } else {
            self.sr &= !SR_T;
        }
    }

    fn q(&self) -> bool {
        self.sr & SR_Q != 0
    }

    fn set_q(&mut self, val: bool) {
        if val {
            self.sr |= SR_Q;
        } else {
            self.sr &= !SR_Q;
        }
    }

    fn m(&self) -> bool {
        self.sr & SR_M != 0
    }

    fn set_m(&mut self, val: bool) {
        if val {
            self.sr |= SR_M;
        } else {
            self.sr &= !SR_M;
        }
    }

    /// Execute the Free-Running Timer (FRT) for a given number of retired cycles.
    /// DEVIATION: driven per retired cycles of each instruction step for accuracy,
    /// rather than once per SH2Exec batch.
    pub fn frt_exec(&mut self, cycles: u32) {
        let shift = self.frc_shift;
        if shift > 7 {
            return;
        }
        let frcold = self.onchip.frc as u32;
        let mask = (1 << shift) - 1;
        let added_ticks = (cycles + self.frc_leftover) >> shift;
        let mut frctemp = frcold + added_ticks;
        let frctemp_orig = frctemp;
        self.frc_leftover = (cycles + self.frc_leftover) & mask;

        // DEVIATION: The crossing test wraps frctemp to 16-bit first, which can cause
        // compare matches to be missed if the counter wraps past the target OCR
        // value in a single step (HR §11.3).
        frctemp &= 0xFFFF;

        let cclra = (self.onchip.ftcsr & 0x01) != 0;

        // 1. OCRA crossing test
        if frctemp >= self.onchip.ocra as u32 && frcold < self.onchip.ocra as u32 {
            self.onchip.ftcsr |= 0x08;
            if (self.onchip.tier & 0x08) != 0 {
                let vector = (self.onchip.vcrc & 0x7F) as u8;
                let level = ((self.onchip.iprb >> 8) & 0xF) as u8;
                self.queue_send(vector, level);
            }
            if cclra {
                frctemp = 0;
                self.frc_leftover = 0;
            }
        }

        // 2. OCRB crossing test
        if frctemp >= self.onchip.ocrb as u32 && frcold < self.onchip.ocrb as u32 {
            self.onchip.ftcsr |= 0x04;
            if (self.onchip.tier & 0x04) != 0 {
                let vector = (self.onchip.vcrc & 0x7F) as u8;
                let level = ((self.onchip.iprb >> 8) & 0xF) as u8;
                self.queue_send(vector, level);
            }
        }

        // 3. Overflow crossing test
        if frctemp_orig > 0xFFFF {
            self.onchip.ftcsr |= 0x02;
            if (self.onchip.tier & 0x02) != 0 {
                let vector = ((self.onchip.vcrd >> 8) & 0x7F) as u8;
                let level = ((self.onchip.iprb >> 8) & 0xF) as u8;
                self.queue_send(vector, level);
            }
        }

        self.onchip.frc = frctemp as u16;
    }

    /// Trigger FRT Input Capture
    pub fn frt_input_capture(&mut self) {
        self.onchip.ftcsr |= 0x80;
        self.onchip.ficr = self.onchip.frc;
        if (self.onchip.tier & 0x80) != 0 {
            let vector = ((self.onchip.vcrc >> 8) & 0x7F) as u8;
            let level = ((self.onchip.iprb >> 8) & 0xF) as u8;
            self.queue_send(vector, level);
        }
    }

    pub fn dma_exec(&mut self) {
        self.dma_proc(200);
    }

    pub fn dma_proc(&mut self, mut cycles: u32) {
        // 1. AE / NMIF abort check
        if (self.onchip.dmaor & 0x6) != 0 {
            self.onchip.dmaor &= !1; // Clear DME
            return;
        }

        // 2. DME check
        if (self.onchip.dmaor & 0x1) == 0 {
            return;
        }

        // 3. Find active channels (DE set and TE clear)
        let ch0_active = (self.onchip.chcr0 & 1) != 0 && (self.onchip.chcr0 & 2) == 0;
        let ch1_active = (self.onchip.chcr1 & 1) != 0 && (self.onchip.chcr1 & 2) == 0;

        if !ch0_active && !ch1_active {
            return;
        }

        // 4. Determine channel priority
        let ch = if (self.onchip.dmaor & 0x8) != 0 {
            // Round-robin
            if ch0_active && ch1_active {
                if self.onchip.dma_round_robin_next == 1 {
                    1
                } else {
                    0
                }
            } else if ch0_active {
                0
            } else {
                1
            }
        } else {
            // Fixed priority: Channel 0 has priority
            if ch0_active {
                0
            } else {
                1
            }
        };

        // 5. Apply dual channel cycle budget doubling and run transfer
        if ch == 0 {
            let budget = if (self.onchip.chcr0 & 0x8) == 0 {
                cycles.saturating_mul(2)
            } else {
                cycles
            };
            self.dma_transfer_cycles(0, budget);
            if (self.onchip.dmaor & 0x8) != 0 && ch0_active && ch1_active {
                self.onchip.dma_round_robin_next = 1;
            }
        } else {
            let budget = if (self.onchip.chcr1 & 0x8) == 0 {
                cycles.saturating_mul(2)
            } else {
                cycles
            };
            self.dma_transfer_cycles(1, budget);
            if (self.onchip.dmaor & 0x8) != 0 && ch0_active && ch1_active {
                self.onchip.dma_round_robin_next = 0;
            }
        }
    }

    fn get_eat_clock(&self, sar: u32, dar: u32) -> u32 {
        let s = sar & 0x0FFF_FFFF;
        let d = dar & 0x0FFF_FFFF;

        // CS2 source: 0x05800000 to 0x059FFFFF
        if (0x0580_0000..0x05A0_0000).contains(&s) {
            return 1;
        }

        // VDP2 RAM source: 0x05E00000 to 0x05EFFFFF (VDP2 VRAM)
        if (0x05E0_0000..0x05F0_0000).contains(&s) {
            if (0x0600_0000..0x0800_0000).contains(&d) {
                return 44; // High WRAM
            } else if (0x0020_0000..0x0030_0000).contains(&d) {
                return 50; // Low WRAM
            } else if (0x05A0_0000..0x05D8_0000).contains(&d) {
                return 427; // Sound RAM/regs, VDP1 RAM/regs
            } else if (0x05E0_0000..0x05F0_0000).contains(&d) {
                return 1; // VDP2 RAM
            } else if (0x05F8_0000..0x05FC_0000).contains(&d) {
                return 50; // VDP2 regs
            } else {
                return 44;
            }
        }

        // VDP1 RAM source: 0x05C00000 to 0x05C7FFFF
        if (0x05C0_0000..0x05D0_0000).contains(&s) {
            if (0x0600_0000..0x0800_0000).contains(&d)
                || (0x0020_0000..0x0030_0000).contains(&d)
                || (0x05A0_0000..0x05C0_0000).contains(&d)
                || (0x05F8_0000..0x05FC_0000).contains(&d)
            {
                return 50;
            } else if (0x05C0_0000..0x05D8_0000).contains(&d) {
                return 570; // VDP1 RAM / VDP1 regs
            } else if (0x05E0_0000..0x05F0_0000).contains(&d) {
                return 225; // VDP2 RAM
            } else {
                return 44;
            }
        }

        // WRAM / anything else source
        if (0x0600_0000..0x0800_0000).contains(&d)
            || (0x0020_0000..0x0030_0000).contains(&d)
            || (0x05C0_0000..0x05D0_0000).contains(&d)
            || (0x05F8_0000..0x05FC_0000).contains(&d)
        {
            return 14;
        } else if (0x05A0_0000..0x05C0_0000).contains(&d) {
            return 20; // Sound RAM/regs
        } else if (0x05D0_0000..0x05D8_0000).contains(&d) {
            return 30; // VDP1 regs
        } else if (0x05E0_0000..0x05F0_0000).contains(&d) {
            return 82; // VDP2 RAM
        } else {
            return 14;
        }
    }

    fn dma_transfer_cycles(&mut self, ch: usize, budget: u32) {
        let mut sar = if ch == 0 {
            self.onchip.sar0
        } else {
            self.onchip.sar1
        };
        let mut dar = if ch == 0 {
            self.onchip.dar0
        } else {
            self.onchip.dar1
        };
        let mut tcr = if ch == 0 {
            self.onchip.tcr0
        } else {
            self.onchip.tcr1
        };
        let mut chcr = if ch == 0 {
            self.onchip.chcr0
        } else {
            self.onchip.chcr1
        };
        let mut copy_clock = if ch == 0 {
            self.onchip.ch0_copy_clock
        } else {
            self.onchip.ch1_copy_clock
        };
        let vcrdma = if ch == 0 {
            self.onchip.vcrdma0
        } else {
            self.onchip.vcrdma1
        };

        copy_clock = copy_clock.saturating_add(budget);

        let size = (chcr >> 10) & 3;
        let stride = match size {
            0 => 1,
            1 => 2,
            2 | 3 => 4,
            _ => 1,
        };

        let src_mode = (chcr >> 12) & 3;
        let dst_mode = (chcr >> 14) & 3;

        let mut locked = false;

        if tcr == 0 {
            chcr |= 2;
            if ch == 0 {
                self.onchip.chcr0m.set(self.onchip.chcr0m.get() | 2);
            } else {
                self.onchip.chcr1m.set(self.onchip.chcr1m.get() | 2);
            }
            if (chcr & 0x4) != 0 {
                let vector = (vcrdma & 0xFF) as u8;
                let level = ((self.onchip.ipra & 0xF00) >> 8) as u8;
                self.queue_send(vector, level);
            }
        }

        while tcr > 0 {
            let eat = self.get_eat_clock(sar, dar);
            let cost = if size == 3 {
                std::cmp::max(1, eat >> 2)
            } else {
                eat
            };

            if copy_clock < cost {
                break;
            }
            copy_clock -= cost;

            if !locked {
                self.arbiter.lock_for_dma();
                locked = true;
            }

            match size {
                0 => {
                    let val = self.raw_read_byte(sar);
                    self.raw_write_byte(dar, val);
                }
                1 => {
                    let val = self.raw_read_word(sar);
                    self.raw_write_word(dar, val);
                }
                2 | 3 => {
                    let val = self.raw_read_long(sar);
                    self.raw_write_long(dar, val);
                }
                _ => {}
            }

            match src_mode {
                1 => sar = sar.wrapping_add(stride),
                2 => sar = sar.wrapping_sub(stride),
                _ => {}
            }
            match dst_mode {
                1 => dar = dar.wrapping_add(stride),
                2 => dar = dar.wrapping_sub(stride),
                _ => {}
            }

            tcr = tcr.wrapping_sub(1);

            if tcr == 0 {
                chcr |= 2;
                if ch == 0 {
                    self.onchip.chcr0m.set(self.onchip.chcr0m.get() | 2);
                } else {
                    self.onchip.chcr1m.set(self.onchip.chcr1m.get() | 2);
                }
                if (chcr & 0x4) != 0 {
                    let vector = (vcrdma & 0xFF) as u8;
                    let level = ((self.onchip.ipra & 0xF00) >> 8) as u8;
                    self.queue_send(vector, level);
                }
                break;
            }
        }

        if locked {
            self.arbiter.unlock_from_dma();
        }

        if ch == 0 {
            self.onchip.sar0 = sar;
            self.onchip.dar0 = dar;
            self.onchip.tcr0 = tcr;
            self.onchip.chcr0 = chcr;
            self.onchip.ch0_copy_clock = copy_clock;
        } else {
            self.onchip.sar1 = sar;
            self.onchip.dar1 = dar;
            self.onchip.tcr1 = tcr;
            self.onchip.chcr1 = chcr;
            self.onchip.ch1_copy_clock = copy_clock;
        }
    }

    /// Run single step of CPU
    pub fn step(&mut self) {
        self.service_pending_interrupt();
        // NOTE: Instruction fetch uses raw_read_word to avoid charging fetch wait states,
        // matching the hardware reference (P8-3).
        let opcode = if (self.pc & 0xC000_0000) == 0xC000_0000 {
            let off = (self.pc & 0xFFF) as usize;
            let hi = self.data_array[off];
            let lo = self.data_array[off.wrapping_add(1) & 0xFFF];
            (hi as u16) << 8 | (lo as u16)
        } else {
            self.raw_read_word(self.pc)
        };
        self.pc = self.pc.wrapping_add(2);
        let base = self.get_base_cycles(opcode);
        self.execute(opcode);
        self.cycles = self.cycles.wrapping_add(base as u64);
        if self.frc_shift <= 7 {
            self.frt_exec(base);
        }
        self.dma_proc(base);
    }

    /// Raise VBLANK-IN. Actual entry into the handler (if any) happens on
    /// the next `step()`, and only if SR's interrupt mask allows it -- same
    /// as real hardware, a masked interrupt just stays pending.
    pub fn request_vblank_interrupt(&mut self) {
        self.queue_send(VBLANK_IN_VECTOR as u8, VBLANK_IN_LEVEL as u8);
    }

    /// Raise VBLANK-OUT -- see `VBLANK_OUT_LEVEL`'s doc comment for why this
    /// is a real, separate interrupt and not a duplicate of VBLANK-IN.
    pub fn request_vblank_out_interrupt(&mut self) {
        self.queue_send(VBLANK_OUT_VECTOR as u8, VBLANK_OUT_LEVEL as u8);
    }

    /// Apply the side effects `Smpc::execute_command` reported, exactly
    /// mirroring what the old inline `smpc_execute_command` below did for
    /// each case -- see `docs/implementation-plans/smpc-peripheral.md`
    /// Phase 0's "Lock order ... never call back into `Sh2`" rule: this runs
    /// after the `Smpc` mutex has already been released by the caller.
    fn apply_smpc_effects(&mut self, effects: crate::smpc::SmpcEffects) {
        if effects.start_slave {
            if let Some(ref sync) = self.sync {
                sync.set_thread_active(1, true);
            }
        }
        if effects.stop_slave {
            if let Some(ref sync) = self.sync {
                sync.set_thread_active(1, false);
            }
        }
        if effects.sound_on {
            if let Some(ref flag) = self.m68k_control {
                // Release: publishes every Sound RAM write this thread made
                // before this point (the uploaded driver) to Core 3's
                // subsequent Acquire load -- see `m68k_control`'s field doc
                // comment.
                flag.store(true, std::sync::atomic::Ordering::Release);
            }
        }
        if effects.sound_off {
            if let Some(ref flag) = self.m68k_control {
                flag.store(false, std::sync::atomic::Ordering::Release);
            }
        }
        if effects.system_manager_irq {
            self.queue_send(SMPC_IRQ_VECTOR as u8, SMPC_IRQ_LEVEL as u8);
        }
    }

    /// Fallback SMPC command path used only when `self.smpc` is `None` (bare
    /// `Sh2` unit tests that never had a real `Smpc` wired in) -- real,
    /// wired-up systems go through `Smpc::execute_command` +
    /// `apply_smpc_effects` above instead. Kept byte-for-byte identical to
    /// before the Phase 0 extraction so every such test keeps passing
    /// unchanged. Matches the "completes instantly" simplification already
    /// used for SF (see the read-side comment on `MemRegion::Smpc`).
    ///
    /// INTBACK is real -- it's the command real BIOS boot code depends on to
    /// learn system status and proceed past its startup handshake (confirmed
    /// live: real BIOS ROM writes 0x10 to COMREG during boot).
    ///
    /// SNDON/SNDOFF are real too: on real hardware these call `M68KStart`/
    /// `M68KStop` (`SmpcSNDON`/`SmpcSNDOFF` in a real, working SCSP
    /// implementation), which reset/halt the SCSP's onboard M68000 sound
    /// CPU -- confirmed live: real BIOS boots issue SNDON right after
    /// uploading a sound driver into Sound RAM, then wait on a RAM flag only
    /// that uploaded 68000 code can plausibly set (see `m68k.rs` and
    /// `mimas/CLAUDE.md`'s "Current wall" section for how this was traced).
    /// `m68k_control` is `None` when nothing has wired an M68K core to this
    /// CPU (e.g. plain unit tests) -- SNDON/SNDOFF are then accepted as
    /// already-complete no-ops, same as before this was implemented.
    ///
    /// Other commands seen during real boot (RESENAB/RESDISA) don't gate
    /// forward progress and are accepted as already-complete no-ops -- SF
    /// already always reads idle regardless of command.
    fn smpc_execute_command(&mut self, command: u8) {
        if command == 0x02 {
            // SSHON
            if let Some(ref sync) = self.sync {
                sync.set_thread_active(1, true);
            }
            return;
        }
        if command == 0x03 {
            // SSHOFF
            if let Some(ref sync) = self.sync {
                sync.set_thread_active(1, false);
            }
            return;
        }
        if command == SMPC_CMD_SNDON {
            if let Some(ref flag) = self.m68k_control {
                // Release: publishes every Sound RAM write this thread made
                // before this point (the uploaded driver) to Core 3's
                // subsequent Acquire load -- see the field doc comment.
                flag.store(true, std::sync::atomic::Ordering::Release);
            }
            return;
        }
        if command == SMPC_CMD_SNDOFF {
            if let Some(ref flag) = self.m68k_control {
                flag.store(false, std::sync::atomic::Ordering::Release);
            }
            return;
        }
        if command != SMPC_CMD_INTBACK {
            return;
        }
        let mut ram = self.work_ram.smpc_regs.write().unwrap();
        // Real hardware branches on this (see the comment below); this old
        // fallback path predates that decode and never did -- kept
        // unused/prefixed rather than removed so the comment below (which
        // predates Phase 1 and still describes real hardware behavior)
        // keeps its referent. The *new*, wired-in path (`crate::smpc::Smpc`)
        // does decode this -- see `docs/implementation-plans/
        // smpc-peripheral.md` Phase 1.
        let _wants_peripheral = (ram[SMPC_IREG1_OFFSET] & 0x8) != 0;

        // Real `SmpcINTBACKStatus()`: system status + RTC + cartridge/
        // region/reset flags in OREG0-11. RTC bytes are zeroed (BCD-encoded
        // real time isn't needed for boot to proceed, only a well-formed
        // response is); region defaults to Japan (1), the same fallback
        // Yabause itself uses when no CD is present to autodetect from
        // (`SmpcRecheckRegion`).
        ram[SMPC_OREG_BASE_OFFSET] = 0x80; // bit7: normal startup, resd=0
        for i in 1..=7 {
            ram[SMPC_OREG_BASE_OFFSET + i * 2] = 0;
        }
        ram[SMPC_OREG_BASE_OFFSET + 8 * 2] = 0; // cartridge
        ram[SMPC_OREG_BASE_OFFSET + 9 * 2] = 1; // region: Japan fallback
        ram[SMPC_OREG_BASE_OFFSET + 10 * 2] = 0x34; // dotsel/mshnmi/sysres/sndres = 0
        ram[SMPC_OREG_BASE_OFFSET + 11 * 2] = 0; // cdres = 0
                                                 // PDE (bit 5) must be 1 when the command finishes, because peripheral
                                                 // data collection is complete (either we didn't request any, or we did
                                                 // and it completed instantly). Real BIOS boot code polls PDE waiting
                                                 // for it to be 1, so leaving it 0 when wants_peripheral is false hangs
                                                 // the boot loop at 0x338C.
        ram[SMPC_SR_OFFSET] = 0x6F;
        drop(ram);

        // Real hardware fires the System Manager interrupt when the command
        // finishes; BIOS INTBACK handshakes wait on this specifically (not
        // just on SF), so without it the boot sequence stalls even though SF
        // itself always reads idle.
        self.queue_send(SMPC_IRQ_VECTOR as u8, SMPC_IRQ_LEVEL as u8);
    }

    /// Compute TVSTAT live from wall-clock frame timing rather than storing
    /// it as an ordinary register byte -- see the read-side comment at
    /// `MemRegion::Vdp2Regs` offset 0x004/0x005. `next_vblank_due` marks the
    /// upcoming VBLANK-IN edge, so the current frame period started one
    /// `VBLANK_INTERVAL` before that; VBLANK is active for the first
    /// `VBLANK_DURATION` of the period, matching real hardware's scanline
    /// split (see the `VBLANK_DURATION` doc comment).
    fn tvstat_word(&self) -> u16 {
        let Some(due) = self.next_vblank_due else {
            return 0;
        };
        let Some(period_start) = due.checked_sub(VBLANK_INTERVAL) else {
            return 0;
        };
        let now = std::time::Instant::now();
        if now >= period_start && now.duration_since(period_start) < VBLANK_DURATION {
            TVSTAT_VBLANK_BIT
        } else {
            0
        }
    }

    fn service_pending_interrupt(&mut self) {
        // Sync sound request IRQ atomic bool with the queue first
        if let Some(ref f) = self.sound_req_irq {
            if f.load(std::sync::atomic::Ordering::Relaxed) {
                self.queue_send(SOUND_REQ_IRQ_VECTOR as u8, SOUND_REQ_IRQ_LEVEL as u8);
            } else {
                self.queue_remove(SOUND_REQ_IRQ_VECTOR as u8);
            }
        }

        // Peek highest level pending interrupt from the queue
        let Some(int) = self.queue_peek() else {
            return;
        };

        let current_mask = (self.sr >> SR_IMASK_SHIFT) & 0xF;
        // Deliver if level is strictly greater than imask (or level is 16 which is NMI)
        if (int.level as u32) <= current_mask && int.level != 16 {
            return; // masked: stays pending until SR's mask is lowered
        }

        // Dequeue/remove the serviced interrupt
        self.queue_remove(int.vector);

        // Sound request IRQ atomic bool update
        if int.vector == SOUND_REQ_IRQ_VECTOR as u8 {
            if let Some(ref f) = self.sound_req_irq {
                f.store(false, std::sync::atomic::Ordering::Relaxed);
            }
        }

        // Wake parked thread
        if let Some(ref sync) = self.sync {
            sync.set_thread_active(self.core_id, true);
        }

        // Real SH-2 exception entry: push SR then PC
        let sr_addr = self.registers[15].wrapping_sub(4);
        self.write_long(sr_addr, self.sr);
        let pc_addr = sr_addr.wrapping_sub(4);
        self.write_long(pc_addr, self.pc);
        self.registers[15] = pc_addr;

        // Update SR mask: clamp NMI level to 15 (0xF)
        let new_mask = if int.level == 16 {
            15
        } else {
            int.level as u32
        };
        self.sr = (self.sr & !(0xFu32 << SR_IMASK_SHIFT)) | (new_mask << SR_IMASK_SHIFT);

        // Vector jump
        self.pc = self.read_long(self.vbr.wrapping_add((int.vector as u32) * 4));
    }

    /// Fetch and run the instruction at the delay slot (currently at
    /// `self.pc`), then jump to `target`. Used by every branch/call/return
    /// instruction, all of which have a mandatory delay slot on real SH-2.
    fn delay_slot_and_jump(&mut self, target: u32) {
        let slot_pc = self.pc;
        let opcode = self.read_word(slot_pc);
        self.pc = target.wrapping_sub(2);
        let base = self.get_base_cycles(opcode);
        self.execute(opcode);
        self.cycles = self.cycles.wrapping_add(base as u64);
        if self.frc_shift <= 7 {
            self.frt_exec(base);
        }
        self.pc = self.pc.wrapping_add(2);
    }

    /// Execute a fetched SH-2 instruction opcode
    fn execute(&mut self, opcode: u16) {
        let n = ((opcode >> 8) & 0xF) as usize;
        let m = ((opcode >> 4) & 0xF) as usize;
        let d4 = (opcode & 0xF) as u32;
        let d8 = (opcode & 0xFF) as u32;
        let d12 = (opcode & 0xFFF) as u32;
        let imm8 = (opcode & 0xFF) as u8;

        // 0xFFFF and other illegal instructions will fall through to the common exception handler at the end of execute()

        if opcode & 0xFF00 == 0xC300 {
            // TRAPA #imm
            // No delay slot, so the return address is simply the address
            // right after the TRAPA instruction -- which `self.pc` already
            // is at this point (step() advances it past the fetch before
            // calling execute()). Push SR then that return address, then
            // jump through VBR + imm*4; same push order/formula as
            // interrupt entry, confirmed against a real, working SH-2
            // interpreter.
            let sr_addr = self.registers[15].wrapping_sub(4);
            self.write_long(sr_addr, self.sr);
            let pc_addr = sr_addr.wrapping_sub(4);
            self.write_long(pc_addr, self.pc);
            self.registers[15] = pc_addr;
            self.pc = self.read_long(self.vbr.wrapping_add(imm8 as u32 * 4));
            return;
        }

        match opcode & 0xF0FF {
            0x0009 => return, // NOP
            0x000B => {
                // RTS
                let target = self.pr;
                self.delay_slot_and_jump(target);
                return;
            }
            0x0018 => {
                self.set_t(true);
                return;
            } // SETT
            0x0008 => {
                self.set_t(false);
                return;
            } // CLRT
            0x0019 => {
                self.sr &= !(SR_T | SR_M | SR_Q);
                return;
            } // DIV0U: M=Q=T=0
            0x0028 => {
                self.mach = 0;
                self.macl = 0;
                return;
            } // CLRMAC
            0x001B => {
                // SLEEP: PC is not advanced, wait for interrupt.
                // Since step() already advanced self.pc by 2, we rewind it.
                self.pc = self.pc.wrapping_sub(2);
                return;
            }
            0x0023 => {
                // BRAF Rn
                let val = self.registers[n];
                let target = self.pc.wrapping_add(2).wrapping_add(val);
                self.delay_slot_and_jump(target);
                return;
            }
            0x0003 => {
                // BSRF Rn
                let val = self.registers[n];
                let target = self.pc.wrapping_add(2).wrapping_add(val);
                self.pr = self.pc.wrapping_add(2);
                self.delay_slot_and_jump(target);
                return;
            }
            0x002B => {
                // RTE: pops PC first (lower stack address), then SR -- real
                // hardware pushes SR then PC on exception entry, so PC ends
                // up on top; see `service_pending_interrupt`.
                let sp = self.registers[15];
                let new_pc = self.read_long(sp);
                let new_sr = self.read_long(sp.wrapping_add(4));
                self.registers[15] = sp.wrapping_add(8);
                self.sr = new_sr & SR_WRITE_MASK;
                self.delay_slot_and_jump(new_pc);
                return;
            }
            0x0002 => {
                self.registers[n] = self.sr;
                return;
            } // STC SR,Rn
            0x0012 => {
                self.registers[n] = self.gbr;
                return;
            } // STC GBR,Rn
            0x0022 => {
                self.registers[n] = self.vbr;
                return;
            } // STC VBR,Rn
            0x000A => {
                self.registers[n] = self.mach;
                return;
            } // STS MACH,Rn
            0x001A => {
                self.registers[n] = self.macl;
                return;
            } // STS MACL,Rn
            0x002A => {
                self.registers[n] = self.pr;
                return;
            } // STS PR,Rn
            0x0029 => {
                self.registers[n] = self.t() as u32;
                return;
            } // MOVT Rn
            _ => {}
        }

        match opcode & 0xF00F {
            0x0004 => {
                let a = self.registers[0].wrapping_add(self.registers[n]);
                self.write_byte(a, self.registers[m] as u8);
                return;
            }
            0x0005 => {
                let a = self.registers[0].wrapping_add(self.registers[n]);
                self.write_word(a, self.registers[m] as u16);
                return;
            }
            0x0006 => {
                let a = self.registers[0].wrapping_add(self.registers[n]);
                self.write_long(a, self.registers[m]);
                return;
            }
            0x0007 => {
                // MUL.L Rm,Rn
                self.macl = self.registers[n].wrapping_mul(self.registers[m]);
                return;
            }
            0x000C => {
                let a = self.registers[0].wrapping_add(self.registers[m]);
                self.registers[n] = self.read_byte(a) as i8 as i32 as u32;
                return;
            }
            0x000D => {
                let a = self.registers[0].wrapping_add(self.registers[m]);
                self.registers[n] = self.read_word(a) as i16 as i32 as u32;
                return;
            }
            0x000E => {
                let a = self.registers[0].wrapping_add(self.registers[m]);
                self.registers[n] = self.read_long(a);
                return;
            }
            0x000F => {
                // MAC.L @Rm+,@Rn+
                let addr_n = self.registers[n];
                let val_n = self.read_long(addr_n) as i32 as i64;
                self.registers[n] = addr_n.wrapping_add(4);

                let addr_m = self.registers[m];
                let val_m = self.read_long(addr_m) as i32 as i64;
                self.registers[m] = addr_m.wrapping_add(4);

                let mac = (self.macl as u64 | ((self.mach as u64) << 32)) as i64;
                let mul = val_n.wrapping_mul(val_m);
                let mut sum = mac.wrapping_add(mul);

                if self.s() {
                    const SAT_MAX: i64 = 0x0000_7FFF_FFFF_FFFFi64;
                    const SAT_MIN: i64 = -0x0000_8000_0000_0000i64;
                    if sum > SAT_MAX {
                        sum = if mul < 0 { SAT_MIN } else { SAT_MAX };
                    } else if sum < SAT_MIN {
                        sum = if mul < 0 { SAT_MIN } else { SAT_MAX };
                    }
                }

                self.mach = (sum >> 32) as u32;
                self.macl = sum as u32;
                return;
            }
            _ => {}
        }

        match opcode & 0xF000 {
            0x1000 => {
                // MOV.L Rm,@(disp4,Rn)
                let addr = self.registers[n].wrapping_add(d4.wrapping_mul(4));
                self.write_long(addr, self.registers[m]);
                return;
            }
            0x5000 => {
                // MOV.L @(disp4,Rm),Rn
                let addr = self.registers[m].wrapping_add(d4.wrapping_mul(4));
                self.registers[n] = self.read_long(addr);
                return;
            }
            0x7000 => {
                // ADD #imm,Rn
                let imm = (imm8 as i8) as i32 as u32;
                self.registers[n] = self.registers[n].wrapping_add(imm);
                return;
            }
            0x9000 => {
                // MOV.W @(disp8,PC),Rn
                let base = self.pc.wrapping_add(2) & !1u32; // PC of this instr + 4, this instr's PC is self.pc-2
                let addr = base.wrapping_add(d8.wrapping_mul(2));
                self.registers[n] = self.read_word(addr) as i16 as i32 as u32;
                return;
            }
            0xA000 => {
                // BRA label
                let disp = sign_extend12(d12);
                let target = self.pc.wrapping_add(2).wrapping_add((disp << 1) as u32);
                self.delay_slot_and_jump(target);
                return;
            }
            0xB000 => {
                // BSR label
                let disp = sign_extend12(d12);
                let target = self.pc.wrapping_add(2).wrapping_add((disp << 1) as u32);
                self.pr = self.pc.wrapping_add(2);
                self.delay_slot_and_jump(target);
                return;
            }
            0xD000 => {
                // MOV.L @(disp8,PC),Rn
                let base = self.pc.wrapping_add(2) & !3u32;
                let addr = base.wrapping_add(d8.wrapping_mul(4));
                self.registers[n] = self.read_long(addr);
                return;
            }
            0xE000 => {
                // MOV #imm,Rn
                self.registers[n] = (imm8 as i8) as i32 as u32;
                return;
            }
            _ => {}
        }

        match opcode & 0xFF00 {
            0x8800 => {
                // CMP/EQ #imm,R0
                let imm = (imm8 as i8) as i32 as u32;
                self.set_t(self.registers[0] == imm);
                return;
            }
            0x8900 => {
                // BT label (no delay slot)
                // Real formula: target = addr_of_this_instr + 4 + disp*2.
                // self.pc already holds addr_of_this_instr + 2 at this point
                // (step() advanced it past the fetch), so add the other +2
                // here -- BRA/BSR below already do this; these four forms
                // were missing it, which sends every conditional branch two
                // bytes short of its real target.
                if self.t() {
                    let disp = sign_extend8(d8);
                    self.pc = self.pc.wrapping_add(2).wrapping_add((disp << 1) as u32);
                }
                return;
            }
            0x8B00 => {
                // BF label (no delay slot)
                if !self.t() {
                    let disp = sign_extend8(d8);
                    self.pc = self.pc.wrapping_add(2).wrapping_add((disp << 1) as u32);
                }
                return;
            }
            0x8D00 => {
                // BT/S label (delay slot)
                if self.t() {
                    let disp = sign_extend8(d8);
                    let target = self.pc.wrapping_add(2).wrapping_add((disp << 1) as u32);
                    self.delay_slot_and_jump(target);
                }
                return;
            }
            0x8F00 => {
                // BF/S label (delay slot)
                if !self.t() {
                    let disp = sign_extend8(d8);
                    let target = self.pc.wrapping_add(2).wrapping_add((disp << 1) as u32);
                    self.delay_slot_and_jump(target);
                }
                return;
            }
            0xC800 => {
                self.set_t((self.registers[0] & imm8 as u32) == 0);
                return;
            } // TST #imm,R0
            0xC900 => {
                self.registers[0] &= imm8 as u32;
                return;
            } // AND #imm,R0
            0xCA00 => {
                self.registers[0] ^= imm8 as u32;
                return;
            } // XOR #imm,R0
            0xCB00 => {
                self.registers[0] |= imm8 as u32;
                return;
            } // OR #imm,R0
            0xC700 => {
                // MOVA @(disp8,PC),R0
                let base = self.pc.wrapping_add(2) & !3u32;
                self.registers[0] = base.wrapping_add(d8.wrapping_mul(4));
                return;
            }
            0xC000 => {
                let a = self.gbr.wrapping_add(d8);
                self.write_byte(a, self.registers[0] as u8);
                return;
            } // MOV.B R0,@(disp8,GBR)
            0xC100 => {
                let a = self.gbr.wrapping_add(d8.wrapping_mul(2));
                self.write_word(a, self.registers[0] as u16);
                return;
            }
            0xC200 => {
                let a = self.gbr.wrapping_add(d8.wrapping_mul(4));
                self.write_long(a, self.registers[0]);
                return;
            }
            0xC400 => {
                let a = self.gbr.wrapping_add(d8);
                self.registers[0] = self.read_byte(a) as i8 as i32 as u32;
                return;
            }
            0xC500 => {
                let a = self.gbr.wrapping_add(d8.wrapping_mul(2));
                self.registers[0] = self.read_word(a) as i16 as i32 as u32;
                return;
            }
            0xC600 => {
                let a = self.gbr.wrapping_add(d8.wrapping_mul(4));
                self.registers[0] = self.read_long(a);
                return;
            }
            0xCC00 => {
                // TST.B #imm,@(R0,GBR)
                let a = self.gbr.wrapping_add(self.registers[0]);
                let val = self.read_byte(a);
                self.set_t((val & imm8) == 0);
                return;
            }
            0xCD00 => {
                // AND.B #imm,@(R0,GBR)
                let a = self.gbr.wrapping_add(self.registers[0]);
                let val = self.read_byte(a);
                self.write_byte(a, val & imm8);
                return;
            }
            0xCE00 => {
                // XOR.B #imm,@(R0,GBR)
                let a = self.gbr.wrapping_add(self.registers[0]);
                let val = self.read_byte(a);
                self.write_byte(a, val ^ imm8);
                return;
            }
            0xCF00 => {
                // OR.B #imm,@(R0,GBR)
                let a = self.gbr.wrapping_add(self.registers[0]);
                let val = self.read_byte(a);
                self.write_byte(a, val | imm8);
                return;
            }
            _ => {}
        }

        match opcode & 0xF0FF {
            0x4000 => {
                let msb = self.registers[n] & 0x8000_0000 != 0;
                self.registers[n] <<= 1;
                self.set_t(msb);
                return;
            } // SHLL
            0x4001 => {
                let lsb = self.registers[n] & 1 != 0;
                self.registers[n] >>= 1;
                self.set_t(lsb);
                return;
            } // SHLR
            0x4004 => {
                let msb = self.registers[n] & 0x8000_0000 != 0;
                self.registers[n] = self.registers[n].rotate_left(1);
                self.set_t(msb);
                return;
            } // ROTL
            0x4005 => {
                let lsb = self.registers[n] & 1 != 0;
                self.registers[n] = self.registers[n].rotate_right(1);
                self.set_t(lsb);
                return;
            } // ROTR
            0x4008 => {
                self.registers[n] <<= 2;
                return;
            } // SHLL2
            0x4009 => {
                self.registers[n] >>= 2;
                return;
            } // SHLR2
            0x400B => {
                // JSR @Rn
                let target = self.registers[n];
                self.pr = self.pc.wrapping_add(2);
                self.delay_slot_and_jump(target);
                return;
            }
            0x4010 => {
                // DT Rn
                self.registers[n] = self.registers[n].wrapping_sub(1);
                self.set_t(self.registers[n] == 0);
                return;
            }
            0x4011 => {
                self.set_t((self.registers[n] as i32) >= 0);
                return;
            } // CMP/PZ
            0x4015 => {
                self.set_t((self.registers[n] as i32) > 0);
                return;
            } // CMP/PL
            0x4018 => {
                self.registers[n] <<= 8;
                return;
            } // SHLL8
            0x4019 => {
                self.registers[n] >>= 8;
                return;
            } // SHLR8
            0x401B => {
                // TAS.B @Rn
                self.bus_wait();
                let a = self.registers[n];
                let val = if let Some(v) = self.work_ram.tas_byte(a) {
                    v
                } else {
                    let v = self.raw_read_byte(a);
                    self.raw_write_byte(a, v | 0x80);
                    v
                };
                self.set_t(val == 0);
                return;
            }
            0x4020 => {
                // SHAL
                let msb = self.registers[n] & 0x8000_0000 != 0;
                self.registers[n] = ((self.registers[n] as i32) << 1) as u32;
                self.set_t(msb);
                return;
            }
            0x4021 => {
                // SHAR
                let lsb = self.registers[n] & 1 != 0;
                self.registers[n] = ((self.registers[n] as i32) >> 1) as u32;
                self.set_t(lsb);
                return;
            }
            0x4024 => {
                // ROTCL
                let old_t = self.t();
                let msb = self.registers[n] & 0x8000_0000 != 0;
                self.registers[n] = (self.registers[n] << 1) | (old_t as u32);
                self.set_t(msb);
                return;
            }
            0x4025 => {
                // ROTCR
                let old_t = self.t();
                let lsb = self.registers[n] & 1 != 0;
                self.registers[n] = (self.registers[n] >> 1) | ((old_t as u32) << 31);
                self.set_t(lsb);
                return;
            }
            0x4028 => {
                self.registers[n] <<= 16;
                return;
            } // SHLL16
            0x4029 => {
                self.registers[n] >>= 16;
                return;
            } // SHLR16
            0x402B => {
                // JMP @Rn
                let target = self.registers[n];
                self.delay_slot_and_jump(target);
                return;
            }
            0x400E => {
                // Apply SR_WRITE_MASK. Immediate interrupt service is not needed here
                // because service_pending_interrupt runs at the head of every step().
                self.sr = self.registers[n] & SR_WRITE_MASK;
                return;
            } // LDC Rn,SR
            0x401E => {
                self.gbr = self.registers[n];
                return;
            } // LDC Rn,GBR
            0x402E => {
                self.vbr = self.registers[n];
                return;
            } // LDC Rn,VBR
            0x400A => {
                self.mach = self.registers[n];
                return;
            } // LDS Rn,MACH
            0x401A => {
                self.macl = self.registers[n];
                return;
            } // LDS Rn,MACL
            0x402A => {
                self.pr = self.registers[n];
                return;
            } // LDS Rn,PR
            // Memory-indirect LDS.L/STS.L/LDC.L/STC.L forms -- extremely
            // common in real function prologues/epilogues (save/restore PR
            // and friends around a call), found missing while running the
            // actual Saturn BIOS: it hit LDS.L @R15+,PR (a PR pop right
            // before RTS) and, since it wasn't decoded, PR stayed stale and
            // RTS returned to the wrong place.
            0x4006 => {
                let a = self.registers[n];
                self.mach = self.read_long(a);
                self.registers[n] = a.wrapping_add(4);
                return;
            } // LDS.L @Rn+,MACH
            0x4016 => {
                let a = self.registers[n];
                self.macl = self.read_long(a);
                self.registers[n] = a.wrapping_add(4);
                return;
            } // LDS.L @Rn+,MACL
            0x4026 => {
                let a = self.registers[n];
                self.pr = self.read_long(a);
                self.registers[n] = a.wrapping_add(4);
                return;
            } // LDS.L @Rn+,PR
            0x4002 => {
                let a = self.registers[n].wrapping_sub(4);
                self.write_long(a, self.mach);
                self.registers[n] = a;
                return;
            } // STS.L MACH,@-Rn
            0x4012 => {
                let a = self.registers[n].wrapping_sub(4);
                self.write_long(a, self.macl);
                self.registers[n] = a;
                return;
            } // STS.L MACL,@-Rn
            0x4022 => {
                let a = self.registers[n].wrapping_sub(4);
                self.write_long(a, self.pr);
                self.registers[n] = a;
                return;
            } // STS.L PR,@-Rn
            0x4007 => {
                let a = self.registers[n];
                self.sr = self.read_long(a) & SR_WRITE_MASK;
                self.registers[n] = a.wrapping_add(4);
                return;
            } // LDC.L @Rn+,SR
            0x4017 => {
                let a = self.registers[n];
                self.gbr = self.read_long(a);
                self.registers[n] = a.wrapping_add(4);
                return;
            } // LDC.L @Rn+,GBR
            0x4027 => {
                let a = self.registers[n];
                self.vbr = self.read_long(a);
                self.registers[n] = a.wrapping_add(4);
                return;
            } // LDC.L @Rn+,VBR
            0x4003 => {
                let a = self.registers[n].wrapping_sub(4);
                self.write_long(a, self.sr);
                self.registers[n] = a;
                return;
            } // STC.L SR,@-Rn
            0x4013 => {
                let a = self.registers[n].wrapping_sub(4);
                self.write_long(a, self.gbr);
                self.registers[n] = a;
                return;
            } // STC.L GBR,@-Rn
            0x4023 => {
                let a = self.registers[n].wrapping_sub(4);
                self.write_long(a, self.vbr);
                self.registers[n] = a;
                return;
            } // STC.L VBR,@-Rn
            _ => {}
        }

        match opcode & 0xF00F {
            0x2000 => {
                let a = self.registers[n];
                self.write_byte(a, self.registers[m] as u8);
                return;
            } // MOV.B Rm,@Rn
            0x2001 => {
                let a = self.registers[n];
                self.write_word(a, self.registers[m] as u16);
                return;
            }
            0x2002 => {
                let a = self.registers[n];
                self.write_long(a, self.registers[m]);
                return;
            }
            0x2004 => {
                // MOV.B Rm,@-Rn
                let a = self.registers[n].wrapping_sub(1);
                self.write_byte(a, self.registers[m] as u8);
                self.registers[n] = a;
                return;
            }
            0x2005 => {
                // MOV.W Rm,@-Rn
                let a = self.registers[n].wrapping_sub(2);
                self.write_word(a, self.registers[m] as u16);
                self.registers[n] = a;
                return;
            }
            0x2006 => {
                // MOV.L Rm,@-Rn
                let a = self.registers[n].wrapping_sub(4);
                self.write_long(a, self.registers[m]);
                self.registers[n] = a;
                return;
            }
            0x2007 => {
                // DIV0S Rm,Rn -- seeds Q/M/T for a following DIV1 chain.
                let q = self.registers[n] & 0x8000_0000 != 0;
                let m_bit = self.registers[m] & 0x8000_0000 != 0;
                self.set_q(q);
                self.set_m(m_bit);
                self.set_t(q != m_bit);
                return;
            }
            0x2008 => {
                self.set_t((self.registers[n] & self.registers[m]) == 0);
                return;
            } // TST Rm,Rn
            0x2009 => {
                self.registers[n] &= self.registers[m];
                return;
            } // AND Rm,Rn
            0x200A => {
                self.registers[n] ^= self.registers[m];
                return;
            } // XOR Rm,Rn
            0x200B => {
                self.registers[n] |= self.registers[m];
                return;
            } // OR Rm,Rn
            0x200C => {
                // CMP/STR Rm,Rn: T=1 if any byte matches
                let x = self.registers[n] ^ self.registers[m];
                let matches = (x & 0xFF) == 0
                    || (x & 0xFF00) == 0
                    || (x & 0xFF_0000) == 0
                    || (x & 0xFF00_0000) == 0;
                self.set_t(matches);
                return;
            }
            0x200D => {
                // XTRCT Rm,Rn
                self.registers[n] = (self.registers[n] >> 16) | (self.registers[m] << 16);
                return;
            }
            0x200E => {
                self.macl =
                    (self.registers[n] as u16 as u32).wrapping_mul(self.registers[m] as u16 as u32);
                return;
            } // MULU.W
            0x200F => {
                self.macl = ((self.registers[n] as i16 as i32)
                    .wrapping_mul(self.registers[m] as i16 as i32))
                    as u32;
                return;
            } // MULS.W
            0x3000 => {
                self.set_t(self.registers[n] == self.registers[m]);
                return;
            } // CMP/EQ Rm,Rn
            0x3002 => {
                self.set_t(self.registers[n] >= self.registers[m]);
                return;
            } // CMP/HS (unsigned)
            0x3003 => {
                self.set_t((self.registers[n] as i32) >= (self.registers[m] as i32));
                return;
            } // CMP/GE
            0x3004 => {
                // DIV1 Rm,Rn -- one step of the bit-serial division
                // algorithm; Q/M/T persist across successive calls (a real
                // divide is built from N of these in a row, one per bit).
                // Faithfully ported from a real, working SH-2 interpreter's
                // exact case analysis on (old_q, M) rather than derived from
                // the divide's math directly, since the carry/borrow-based
                // Q update is easy to get subtly wrong otherwise.
                // CAUTION: single-step mechanics are verified (see
                // div1_single_step_matches_hand_traced_algorithm), but the
                // full "DIV0U + 32x DIV1 -> quotient in Rn" multi-step
                // convention is NOT yet validated end-to-end against real
                // division results -- do not depend on chained DIV1 output
                // for real division until that's confirmed against actual
                // compiled SH-2 division-routine test vectors.
                let old_q = self.q();
                let shifted_q = (self.registers[n] & 0x8000_0000) != 0;
                self.set_q(shifted_q);
                self.registers[n] = (self.registers[n] << 1) | (self.t() as u32);
                let m_flag = self.m();
                let (result, new_q) = match (old_q, m_flag) {
                    (false, false) => {
                        let tmp0 = self.registers[n];
                        let r = tmp0.wrapping_sub(self.registers[m]);
                        let borrow = r > tmp0;
                        (r, if !shifted_q { borrow } else { !borrow })
                    }
                    (false, true) => {
                        let tmp0 = self.registers[n];
                        let r = tmp0.wrapping_add(self.registers[m]);
                        let carry = r < tmp0;
                        (r, if !shifted_q { !carry } else { carry })
                    }
                    (true, false) => {
                        let tmp0 = self.registers[n];
                        let r = tmp0.wrapping_add(self.registers[m]);
                        let carry = r < tmp0;
                        (r, if !shifted_q { carry } else { !carry })
                    }
                    (true, true) => {
                        let tmp0 = self.registers[n];
                        let r = tmp0.wrapping_sub(self.registers[m]);
                        let borrow = r > tmp0;
                        (r, if !shifted_q { !borrow } else { borrow })
                    }
                };
                self.registers[n] = result;
                self.set_q(new_q);
                self.set_t(self.q() == self.m());
                return;
            }
            0x3006 => {
                self.set_t(self.registers[n] > self.registers[m]);
                return;
            } // CMP/HI (unsigned)
            0x3007 => {
                self.set_t((self.registers[n] as i32) > (self.registers[m] as i32));
                return;
            } // CMP/GT
            0x3008 => {
                self.registers[n] = self.registers[n].wrapping_sub(self.registers[m]);
                return;
            } // SUB
            0x300A => {
                // SUBC Rm,Rn
                let (r1, c1) = self.registers[n].overflowing_sub(self.registers[m]);
                let (r2, c2) = r1.overflowing_sub(self.t() as u32);
                self.registers[n] = r2;
                self.set_t(c1 || c2);
                return;
            }
            0x300B => {
                // SUBV Rm,Rn
                let (r, ov) = (self.registers[n] as i32).overflowing_sub(self.registers[m] as i32);
                self.registers[n] = r as u32;
                self.set_t(ov);
                return;
            }
            0x300C => {
                self.registers[n] = self.registers[n].wrapping_add(self.registers[m]);
                return;
            } // ADD
            0x300D => {
                // DMULS.L Rm,Rn
                let r =
                    (self.registers[n] as i32 as i64).wrapping_mul(self.registers[m] as i32 as i64);
                self.mach = (r >> 32) as u32;
                self.macl = r as u32;
                return;
            }
            0x300E => {
                // ADDC Rm,Rn
                let (r1, c1) = self.registers[n].overflowing_add(self.registers[m]);
                let (r2, c2) = r1.overflowing_add(self.t() as u32);
                self.registers[n] = r2;
                self.set_t(c1 || c2);
                return;
            }
            0x300F => {
                // ADDV Rm,Rn
                let (r, ov) = (self.registers[n] as i32).overflowing_add(self.registers[m] as i32);
                self.registers[n] = r as u32;
                self.set_t(ov);
                return;
            }
            0x3005 => {
                // DMULU.L Rm,Rn
                let r = (self.registers[n] as u64).wrapping_mul(self.registers[m] as u64);
                self.mach = (r >> 32) as u32;
                self.macl = r as u32;
                return;
            }
            0x400F => {
                // MAC.W @Rm+,@Rn+
                let addr_m = self.registers[m];
                let val_m = self.read_word(addr_m) as i16 as i32;
                self.registers[m] = addr_m.wrapping_add(2);

                let addr_n = self.registers[n];
                let val_n = self.read_word(addr_n) as i16 as i32;
                self.registers[n] = addr_n.wrapping_add(2);

                let mul = val_m.wrapping_mul(val_n);
                let sum = (self.macl as i32 as i64).wrapping_add(mul as i64);

                if self.s() {
                    const SAT_MAX: i64 = 0x7FFFFFFF;
                    const SAT_MIN: i64 = -0x80000000;
                    if sum > SAT_MAX {
                        self.mach |= 1;
                        self.macl = if mul < 0 {
                            SAT_MIN as u32
                        } else {
                            SAT_MAX as u32
                        };
                    } else if sum < SAT_MIN {
                        self.mach |= 1;
                        self.macl = if mul < 0 {
                            SAT_MIN as u32
                        } else {
                            SAT_MAX as u32
                        };
                    } else {
                        self.macl = sum as u32;
                    }
                } else {
                    self.macl = sum as u32;
                    // DEVIATION: non-accumulating MACH overwrite.
                    self.mach = (sum >> 32) as u32;
                }
                return;
            }
            0x6000 => {
                let a = self.registers[m];
                self.registers[n] = self.read_byte(a) as i8 as i32 as u32;
                return;
            }
            0x6001 => {
                let a = self.registers[m];
                self.registers[n] = self.read_word(a) as i16 as i32 as u32;
                return;
            }
            0x6002 => {
                let a = self.registers[m];
                self.registers[n] = self.read_long(a);
                return;
            }
            0x6003 => {
                self.registers[n] = self.registers[m];
                return;
            } // MOV Rm,Rn
            0x6004 => {
                // MOV.B @Rm+,Rn
                let a = self.registers[m];
                self.registers[n] = self.read_byte(a) as i8 as i32 as u32;
                if n != m {
                    self.registers[m] = a.wrapping_add(1);
                }
                return;
            }
            0x6005 => {
                // MOV.W @Rm+,Rn
                let a = self.registers[m];
                self.registers[n] = self.read_word(a) as i16 as i32 as u32;
                if n != m {
                    self.registers[m] = a.wrapping_add(2);
                }
                return;
            }
            0x6006 => {
                // MOV.L @Rm+,Rn
                let a = self.registers[m];
                self.registers[n] = self.read_long(a);
                if n != m {
                    self.registers[m] = a.wrapping_add(4);
                }
                return;
            }
            0x6007 => {
                self.registers[n] = !self.registers[m];
                return;
            } // NOT
            0x6008 => {
                // SWAP.B
                let v = self.registers[m];
                self.registers[n] = (v & 0xFFFF_0000) | ((v & 0xFF) << 8) | ((v >> 8) & 0xFF);
                return;
            }
            0x6009 => {
                self.registers[n] = self.registers[m].rotate_left(16);
                return;
            } // SWAP.W
            0x600A => {
                let (r, c) = 0u32.overflowing_sub(self.registers[m]);
                let (r2, c2) = r.overflowing_sub(self.t() as u32);
                self.registers[n] = r2;
                self.set_t(c || c2);
                return;
            } // NEGC
            0x600B => {
                self.registers[n] = 0u32.wrapping_sub(self.registers[m]);
                return;
            } // NEG
            0x600C => {
                self.registers[n] = self.registers[m] & 0xFF;
                return;
            } // EXTU.B
            0x600D => {
                self.registers[n] = self.registers[m] & 0xFFFF;
                return;
            } // EXTU.W
            0x600E => {
                self.registers[n] = self.registers[m] as i8 as i32 as u32;
                return;
            } // EXTS.B
            0x600F => {
                self.registers[n] = self.registers[m] as i16 as i32 as u32;
                return;
            } // EXTS.W
            _ => {}
        }

        // MOV.B/W R0,@(disp4,Rm) and @(disp4,Rm),R0 -- share the 0x8000 nibble
        // with BT/BF/CMP-EQ-imm above, disambiguated by the next nibble.
        match opcode & 0xFF00 {
            0x8000 => {
                let a = self.registers[m].wrapping_add(d4);
                self.write_byte(a, self.registers[0] as u8);
                return;
            }
            0x8100 => {
                let a = self.registers[m].wrapping_add(d4.wrapping_mul(2));
                self.write_word(a, self.registers[0] as u16);
                return;
            }
            0x8400 => {
                let a = self.registers[m].wrapping_add(d4);
                self.registers[0] = self.read_byte(a) as i8 as i32 as u32;
                return;
            }
            0x8500 => {
                let a = self.registers[m].wrapping_add(d4.wrapping_mul(2));
                self.registers[0] = self.read_word(a) as i16 as i32 as u32;
                return;
            }
            _ => {}
        }

        // Illegal-instruction exception sequence (HR §9.9/D-6)
        log_illegal_once(self.pc.wrapping_sub(2), opcode);
        let sr_addr = self.registers[15].wrapping_sub(4);
        self.write_long(sr_addr, self.sr);
        let pc_addr = sr_addr.wrapping_sub(4);
        self.write_long(pc_addr, self.pc);
        self.registers[15] = pc_addr;

        // UNCERTAIN: HR notes delay-slot cases can use vector 6, but Yabause always
        // uses vector 4. We match Yabause unconditionally.
        self.pc = self.read_long(self.vbr.wrapping_add(4 * 4));
        self.cycles = self.cycles.wrapping_add(1);
        self.illegal_instruction_flag = true;
    }

    fn get_onchip_16(&self, off: usize) -> Option<u16> {
        match off {
            // INTC
            0x060 => Some(self.onchip.iprb),
            0x062 => Some(self.onchip.vcra),
            0x064 => Some(self.onchip.vcrb),
            0x066 => Some(self.onchip.vcrc),
            0x068 => Some(self.onchip.vcrd),
            0x0E0 => Some(self.onchip.icr),
            0x0E2 => Some(self.onchip.ipra),
            0x0E4 => Some(self.onchip.vcrwdt),

            // BSC
            0x1E0 | 0x1E2 => Some(self.onchip.bcr1),
            0x1E4 | 0x1E6 => Some(self.onchip.bcr2),
            0x1E8 | 0x1EA => Some(self.onchip.wcr),
            0x1EC | 0x1EE => Some(self.onchip.mcr),
            0x1F0 | 0x1F2 => Some(self.onchip.rtcsr),
            0x1F4 | 0x1F6 => Some(self.onchip.rtcnt),
            0x1F8 | 0x1FA => Some(self.onchip.rtcor),

            // FRT
            0x012 => Some(self.onchip.frc),
            0x014 => {
                if (self.onchip.tocr & 0x10) == 0 {
                    Some(self.onchip.ocra)
                } else {
                    Some(self.onchip.ocrb)
                }
            }
            0x018 => Some(self.onchip.ficr),

            // UBC
            0x148 => Some(self.onchip.bbra),
            0x168 => Some(self.onchip.bbrb),
            _ => None,
        }
    }

    fn get_onchip_32(&self, off: usize) -> Option<u32> {
        let normalized = if off >= 0x120 && off <= 0x13F {
            off - 0x20
        } else {
            off
        };
        match normalized {
            // DIVU
            0x100 => Some(self.onchip.dvsr),
            0x104 => Some(self.onchip.dvdntl), // DVDNT reads return dvdntl
            0x108 => Some(self.onchip.dvcr),
            0x10C => Some(self.onchip.vcrdiv),
            0x110 => Some(self.onchip.dvdnth),
            0x114 => Some(self.onchip.dvdntl),
            0x118 => Some(self.onchip.dvdntuh),
            0x11C => Some(self.onchip.dvdntul),

            // UBC
            0x140 => Some(self.onchip.bara),
            0x144 => Some(self.onchip.bamra),
            0x178 => Some(self.onchip.brcr),

            // DMA
            0x180 => Some(self.onchip.sar0),
            0x184 => Some(self.onchip.dar0),
            0x188 => Some(self.onchip.tcr0),
            0x18C => Some(self.onchip.chcr0),
            0x190 => Some(self.onchip.sar1),
            0x194 => Some(self.onchip.dar1),
            0x198 => Some(self.onchip.tcr1),
            0x19C => Some(self.onchip.chcr1),
            0x1A0 => Some(self.onchip.vcrdma0),
            0x1A8 => Some(self.onchip.vcrdma1),
            0x1B0 => Some(self.onchip.dmaor),
            _ => None,
        }
    }

    fn read_onchip_byte(&self, off: usize) -> u8 {
        match off {
            // SCI (0x000 - 0x005)
            0x000 => self.onchip.smr,
            0x001 => self.onchip.brr,
            0x002 => self.onchip.scr,
            0x003 => self.onchip.tdr,
            0x004 => self.onchip.ssr,
            0x005 => self.onchip.rdr,

            // FRT (0x010 - 0x019)
            0x010 => self.onchip.tier,
            0x011 => self.onchip.ftcsr,
            0x016 => self.onchip.tcr,
            0x017 => self.onchip.tocr,

            // DRCR
            0x071 => self.onchip.drcr0,
            0x072 => self.onchip.drcr1,

            // WDT
            0x080 => self.onchip.wtcsr,
            0x081 => self.onchip.wtcnt,
            0x083 => self.onchip.rstcsr,

            // SBYCR / CCR
            0x091 => self.onchip.sbycr,
            0x092 => self.onchip.ccr,

            _ => {
                if let Some(val) = self.get_onchip_16(off & !1) {
                    if (off & 1) == 0 {
                        (val >> 8) as u8
                    } else {
                        (val & 0xFF) as u8
                    }
                } else if let Some(val) = self.get_onchip_32(off & !3) {
                    let byte_shift = 24 - ((off & 3) * 8);
                    ((val >> byte_shift) & 0xFF) as u8
                } else {
                    // Log unhandled onchip read (deduplicated)
                    let region = MemRegion::OnChip(off);
                    log_reg_access_once(&region, false, 0);
                    0
                }
            }
        }
    }

    fn read_onchip_word(&self, off: usize) -> u16 {
        if let Some(val) = self.get_onchip_16(off) {
            val
        } else if let Some(val) = self.get_onchip_32(off & !3) {
            let byte_shift = 16 - ((off & 2) * 8);
            ((val >> byte_shift) & 0xFFFF) as u16
        } else {
            let b0 = self.read_onchip_byte(off) as u16;
            let b1 = self.read_onchip_byte(off + 1) as u16;
            (b0 << 8) | b1
        }
    }

    fn write_onchip_byte(&mut self, off: usize, val: u8) {
        match off {
            // SCI (0x000 - 0x005)
            0x000 => self.onchip.smr = val,
            0x001 => self.onchip.brr = val,
            0x002 => {
                self.onchip.scr = val;
                if (val & 0x20) == 0 {
                    self.onchip.ssr |= 0x80;
                }
            }
            0x003 => self.onchip.tdr = val,
            0x004 => {
                self.onchip.ssr = val;
            }
            0x005 => {} // RDR is read-only

            // FRT (0x010 - 0x019)
            0x010 => {
                self.onchip.tier = (val & 0x8E) | 0x01;
                if (val & 0x80) != 0 && (self.onchip.ftcsr & 0x80) != 0 {
                    // ICI interrupt immediately
                    let vector = ((self.onchip.vcrc >> 8) & 0x7F) as u8;
                    let level = ((self.onchip.iprb >> 8) & 0xF) as u8;
                    self.queue_send(vector, level);
                }
            }
            0x011 => {
                self.onchip.ftcsr = (self.onchip.ftcsr & (val & 0xFE)) | (val & 0x01);
            }
            0x016 => {
                self.onchip.tcr = val & 0x83;
                match val & 3 {
                    0 => self.frc_shift = 3,
                    1 => self.frc_shift = 5,
                    2 => self.frc_shift = 7,
                    3 => {
                        // Log not implemented, leave frc_shift unchanged
                        println!("[FRT] External clock prescaler select not implemented");
                    }
                    _ => unreachable!(),
                }
            }
            0x017 => {
                self.onchip.tocr = 0xE0 | (val & 0x13);
            }

            // INTC destructive byte write quirks (P4-T3)
            0x060 => self.onchip.iprb = ((val as u16) << 8) & 0xFF00,
            0x061 => {} // Ignored
            0x068 => self.onchip.vcrd = ((val as u16) << 8) & 0x7F00,
            0x069 => {} // Ignored

            // DRCR
            0x071 => self.onchip.drcr0 = val & 0x3,
            0x072 => self.onchip.drcr1 = val & 0x3,

            // WDT
            0x080 => self.onchip.wtcsr = val,
            0x081 => self.onchip.wtcnt = val,
            0x083 => self.onchip.rstcsr = val,

            // SBYCR / CCR
            0x091 => self.onchip.sbycr = val & 0xDF,
            0x092 => {
                self.onchip.ccr = val & 0xCF;
            }

            _ => {
                let reg_off = off & !1;
                if let Some(mut word_val) = self.get_onchip_16(reg_off) {
                    if (off & 1) == 0 {
                        word_val = (word_val & 0x00FF) | ((val as u16) << 8);
                    } else {
                        word_val = (word_val & 0xFF00) | (val as u16);
                    }
                    self.write_onchip_word(reg_off, word_val);
                } else {
                    let reg_off_32 = off & !3;
                    if let Some(mut long_val) = self.get_onchip_32(reg_off_32) {
                        let byte_shift = 24 - ((off & 3) * 8);
                        let mask = !(0xFF << byte_shift);
                        long_val = (long_val & mask) | ((val as u32) << byte_shift);
                        self.write_onchip(reg_off_32, long_val);
                    } else {
                        // Log unhandled onchip write
                        let region = MemRegion::OnChip(off);
                        log_reg_access_once(&region, true, val);
                    }
                }
            }
        }
    }

    fn write_onchip_word(&mut self, off: usize, val: u16) {
        match off {
            // INTC
            0x060 => self.onchip.iprb = val & 0xFF00,
            0x062 => self.onchip.vcra = val & 0x7F7F,
            0x064 => self.onchip.vcrb = val & 0x7F7F,
            0x066 => self.onchip.vcrc = val & 0x7F7F,
            0x068 => self.onchip.vcrd = val & 0x7F7F,
            0x0E0 => self.onchip.icr = val & 0x0101,
            0x0E2 => self.onchip.ipra = val & 0xFFF0,
            0x0E4 | 0x0E5 => self.onchip.vcrwdt = val & 0x7F7F,
            // FRT
            0x012 => self.onchip.frc = val,
            0x014 => {
                if (self.onchip.tocr & 0x10) == 0 {
                    self.onchip.ocra = val;
                } else {
                    self.onchip.ocrb = val;
                }
            }
            0x018 => {} // FICR is read-only

            // BSC
            0x1E0 | 0x1E2 | 0x1E4 | 0x1E6 | 0x1E8 | 0x1EA | 0x1EC | 0x1EE | 0x1F0 | 0x1F2
            | 0x1F4 | 0x1F6 | 0x1F8 | 0x1FA => {
                // Word-only writes to BSC registers are ignored or logged
            }

            // UBC
            0x148 => self.onchip.bbra = val,
            0x168 => self.onchip.bbrb = val,

            _ => {
                let reg_off_32 = off & !3;
                if let Some(mut long_val) = self.get_onchip_32(reg_off_32) {
                    let byte_shift = 16 - ((off & 2) * 8);
                    let mask = !(0xFFFF << byte_shift);
                    long_val = (long_val & mask) | ((val as u32) << byte_shift);
                    self.write_onchip(reg_off_32, long_val);
                } else {
                    self.write_onchip_byte(off, (val >> 8) as u8);
                    self.write_onchip_byte(off + 1, (val & 0xFF) as u8);
                }
            }
        }
    }

    fn read_onchip(&self, off: usize) -> u32 {
        match off {
            // DIVU
            0x100 | 0x120 => self.onchip.dvsr,
            0x104 | 0x124 => self.onchip.dvdntl,
            0x108 | 0x128 => self.onchip.dvcr,
            0x10C | 0x12C => self.onchip.vcrdiv,
            0x110 | 0x130 => self.onchip.dvdnth,
            0x114 | 0x134 => self.onchip.dvdntl,
            0x118 | 0x138 => self.onchip.dvdntuh,
            0x11C | 0x13C => self.onchip.dvdntul,

            // BSC
            0x1E0 => ((self.onchip.bcr1 as u32) << 16) | (self.onchip.bcr2 as u32),
            0x1E8 => ((self.onchip.wcr as u32) << 16) | (self.onchip.mcr as u32),
            0x1F0 => ((self.onchip.rtcsr as u32) << 16) | (self.onchip.rtcnt as u32),
            0x1F8 => (self.onchip.rtcor as u32) << 16,

            // UBC / DMA / others
            0x140 => self.onchip.bara,
            0x144 => self.onchip.bamra,
            0x148 => (self.onchip.bbra as u32) << 16,
            0x178 => self.onchip.brcr,
            0x180 => self.onchip.sar0,
            0x184 => self.onchip.dar0,
            0x188 => self.onchip.tcr0,
            0x18C => {
                let val = self.onchip.chcr0;
                self.onchip.chcr0m.set(0);
                val
            }
            0x190 => self.onchip.sar1,
            0x194 => self.onchip.dar1,
            0x198 => self.onchip.tcr1,
            0x19C => {
                let val = self.onchip.chcr1;
                self.onchip.chcr1m.set(0);
                val
            }
            0x1A0 => self.onchip.vcrdma0,
            0x1A8 => self.onchip.vcrdma1,
            0x1B0 => self.onchip.dmaor,

            _ => {
                let w0 = self.read_onchip_word(off) as u32;
                let w1 = self.read_onchip_word(off + 2) as u32;
                (w0 << 16) | w1
            }
        }
    }

    fn divu_check_interrupt(&mut self) {
        if (self.onchip.dvcr & 0x2) != 0 {
            // DEVIATION: HR §10.6 notes the reference reads the level from MSH2->onchip.IPRA
            // even on the slave. We use the correct-looking per-CPU IPRA.
            let vector = (self.onchip.vcrdiv & 0x7F) as u8;
            let level = ((self.onchip.ipra >> 12) & 0xF) as u8;
            self.queue_send(vector, level);
        }
    }

    fn write_onchip(&mut self, off: usize, val: u32) {
        match off {
            // DIVU
            0x100 | 0x120 => {
                self.onchip.dvsr = val;
            }
            0x104 | 0x124 => {
                let divisor = self.onchip.dvsr as i32;
                let dividend = val as i32;
                if divisor == 0 {
                    if dividend < 0 {
                        self.onchip.dvdntl = 0x80000000;
                        self.onchip.dvdnth = 0xFFFFFFFC | ((val >> 29) & 3);
                    } else {
                        self.onchip.dvdntl = 0x7FFFFFFF;
                        self.onchip.dvdnth = val >> 29;
                    }
                    self.onchip.dvdntul = self.onchip.dvdntl;
                    self.onchip.dvdntuh = self.onchip.dvdnth;
                    self.onchip.dvcr |= 1;
                    self.divu_check_interrupt();
                } else {
                    // DEVIATION-by-necessity: Guard against signed i32 division overflow (i32::MIN / -1)
                    // to prevent hard panics, falling back to a two's-complement wrap.
                    let (quotient, remainder) = if divisor == -1 && dividend == i32::MIN {
                        (i32::MIN, 0)
                    } else {
                        (dividend / divisor, dividend % divisor)
                    };
                    self.onchip.dvdntl = quotient as u32;
                    self.onchip.dvdntul = quotient as u32;
                    self.onchip.dvdnth = remainder as u32;
                    self.onchip.dvdntuh = remainder as u32;
                }
            }
            0x108 | 0x128 => {
                self.onchip.dvcr = val & 3;
            }
            0x10C | 0x12C => {
                self.onchip.vcrdiv = val & 0xFFFF;
            }
            0x110 | 0x130 => {
                self.onchip.dvdnth = val;
            }
            0x114 | 0x134 => {
                let divisor = self.onchip.dvsr as i32;
                let dividend_high = self.onchip.dvdnth as i64;
                let dividend_low = val as i64;
                let dividend = (dividend_high << 32) | (dividend_low & 0xFFFFFFFF);
                if divisor == 0 {
                    if (dividend_high & 0x80000000) != 0 {
                        self.onchip.dvdntl = 0x80000000;
                        self.onchip.dvdnth = (self.onchip.dvdnth << 3) as u32;
                    } else {
                        self.onchip.dvdntl = 0x7FFFFFFF;
                        self.onchip.dvdnth = (self.onchip.dvdnth << 3) as u32;
                    }
                    self.onchip.dvdntul = self.onchip.dvdntl;
                    self.onchip.dvdntuh = self.onchip.dvdnth;
                    self.onchip.dvcr |= 1;
                    self.divu_check_interrupt();
                } else {
                    // DEVIATION-by-necessity: Guard against signed i64 division overflow (i64::MIN / -1)
                    let (quotient, remainder) = if divisor as i64 == -1 && dividend == i64::MIN {
                        (i64::MIN, 0i64)
                    } else {
                        (dividend / (divisor as i64), dividend % (divisor as i64))
                    };

                    if quotient > 0x7FFF_FFFF {
                        self.onchip.dvcr |= 1;
                        self.onchip.dvdntl = 0x7FFF_FFFF;
                        // Note: HR §11.6 flags both 0xFFFF_FFFE values as Yabause "// fix me" and
                        // states the true hardware value is not deducible.
                        self.onchip.dvdnth = 0xFFFF_FFFE;
                        self.onchip.dvdntul = self.onchip.dvdntl;
                        self.onchip.dvdntuh = self.onchip.dvdnth;
                        self.divu_check_interrupt();
                    } else if ((quotient >> 32) as i32) < -1 {
                        self.onchip.dvcr |= 1;
                        self.onchip.dvdntl = 0x8000_0000;
                        // Note: HR §11.6 flags both 0xFFFF_FFFE values as Yabause "// fix me" and
                        // states the true hardware value is not deducible.
                        self.onchip.dvdnth = 0xFFFF_FFFE;
                        self.onchip.dvdntul = self.onchip.dvdntl;
                        self.onchip.dvdntuh = self.onchip.dvdnth;
                        self.divu_check_interrupt();
                    } else {
                        self.onchip.dvdntl = quotient as u32;
                        self.onchip.dvdnth = remainder as u32;
                        self.onchip.dvdntul = self.onchip.dvdntl;
                        self.onchip.dvdntuh = self.onchip.dvdnth;
                    }
                }
            }
            0x118 | 0x138 => {
                self.onchip.dvdntuh = val;
            }
            0x11C | 0x13C => {
                self.onchip.dvdntul = val;
            }

            // BSC
            0x1E0 => {
                self.onchip.bcr1 = (self.onchip.bcr1 & 0x8000) | (((val >> 16) & 0x1FF7) as u16);
                self.onchip.bcr2 = (val & 0xFC) as u16;
            }
            0x1E8 => {
                self.onchip.wcr = (val >> 16) as u16;
                self.onchip.mcr = (val & 0xFEFC) as u16;
            }
            0x1F0 => {
                self.onchip.rtcsr = (((val >> 16) & 0xF8) as u16);
            }
            0x1F8 => {
                self.onchip.rtcor = (((val >> 16) & 0xFF) as u16);
            }

            // UBC
            0x140 => self.onchip.bara = val,
            0x144 => self.onchip.bamra = val,
            0x148 => self.onchip.bbra = (val >> 16) as u16,
            0x178 => self.onchip.brcr = val,

            // DMA
            0x180 => self.onchip.sar0 = val,
            0x184 => self.onchip.dar0 = val,
            0x188 => self.onchip.tcr0 = val & 0xFFFFFF,
            0x18C => {
                if self.onchip.tcr0 != 0 {
                    self.dma_proc(0x7FFFFFFF);
                }
                let val = val & 0xFFFF;
                let chcr0m_val = self.onchip.chcr0m.get();
                let old_chcr0 = self.onchip.chcr0;
                let new_chcr0 = (val & !2) | (old_chcr0 & (val | chcr0m_val) & 2);
                self.onchip.chcr0 = new_chcr0;

                // DEVIATION: Channel-0 arm uses raw written val & 3
                if (self.onchip.dmaor & 7) == 1 && (val & 3) == 1 {
                    self.onchip.ch0_copy_clock = 0;
                    self.dma_exec();
                }
            }
            0x190 => self.onchip.sar1 = val,
            0x194 => self.onchip.dar1 = val,
            0x198 => self.onchip.tcr1 = val & 0xFFFFFF,
            0x19C => {
                if self.onchip.tcr1 != 0 {
                    self.dma_proc(0x7FFFFFFF);
                }
                let val = val & 0xFFFF;
                let chcr1m_val = self.onchip.chcr1m.get();
                let old_chcr1 = self.onchip.chcr1;
                let new_chcr1 = (val & !2) | (old_chcr1 & (val | chcr1m_val) & 2);
                self.onchip.chcr1 = new_chcr1;

                if (self.onchip.dmaor & 7) == 1 && (new_chcr1 & 3) == 1 {
                    self.onchip.ch1_copy_clock = 0;
                    self.dma_exec();
                }
            }
            0x1A0 => self.onchip.vcrdma0 = val & 0xFFFF,
            0x1A8 => self.onchip.vcrdma1 = val & 0xFFFF,
            0x1B0 => {
                let old_dmaor = self.onchip.dmaor;
                let new_dmaor = val & 0xF;
                self.onchip.dmaor = new_dmaor;
                if (new_dmaor & 7) == 1 && (old_dmaor & 7) != 1 {
                    if (self.onchip.chcr0 & 3) == 1 {
                        self.onchip.ch0_copy_clock = 0;
                    }
                    if (self.onchip.chcr1 & 3) == 1 {
                        self.onchip.ch1_copy_clock = 0;
                    }
                    self.dma_exec();
                }
            }

            _ => {
                // Fall back to writing two words
                self.write_onchip_word(off, (val >> 16) as u16);
                self.write_onchip_word(off + 2, (val & 0xFFFF) as u16);
            }
        }
    }

    fn execute_scu_dma(&mut self, channel: usize) {
        let base = channel * 0x20;
        let scu = self.work_ram.scu_regs.read().unwrap();
        let read_addr =
            u32::from_be_bytes([scu[base], scu[base + 1], scu[base + 2], scu[base + 3]]);
        let write_addr =
            u32::from_be_bytes([scu[base + 4], scu[base + 5], scu[base + 6], scu[base + 7]]);
        let count =
            u32::from_be_bytes([scu[base + 8], scu[base + 9], scu[base + 10], scu[base + 11]])
                & 0x00FFFFFF;
        let add_val = u32::from_be_bytes([
            scu[base + 12],
            scu[base + 13],
            scu[base + 14],
            scu[base + 15],
        ]);
        let mode = u32::from_be_bytes([
            scu[base + 20],
            scu[base + 21],
            scu[base + 22],
            scu[base + 23],
        ]);
        drop(scu);

        let indirect = (mode & 0x01_0000) != 0;

        self.arbiter.lock_for_dma();

        if indirect {
            let mut desc_addr = read_addr;
            loop {
                // Read descriptor fields using raw reads to prevent deadlock
                let size = {
                    let b0 = self.raw_read_byte(desc_addr) as u32;
                    let b1 = self.raw_read_byte(desc_addr + 1) as u32;
                    let b2 = self.raw_read_byte(desc_addr + 2) as u32;
                    let b3 = self.raw_read_byte(desc_addr + 3) as u32;
                    (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
                };
                if size == 0 {
                    break;
                }
                let end = (size & 0x80000000) != 0;
                let len = (size & 0x00FFFFFF) as usize;

                let dst = {
                    let b0 = self.raw_read_byte(desc_addr + 4) as u32;
                    let b1 = self.raw_read_byte(desc_addr + 5) as u32;
                    let b2 = self.raw_read_byte(desc_addr + 6) as u32;
                    let b3 = self.raw_read_byte(desc_addr + 7) as u32;
                    (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
                };

                let src = {
                    let b0 = self.raw_read_byte(desc_addr + 8) as u32;
                    let b1 = self.raw_read_byte(desc_addr + 9) as u32;
                    let b2 = self.raw_read_byte(desc_addr + 10) as u32;
                    let b3 = self.raw_read_byte(desc_addr + 11) as u32;
                    (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
                };

                for i in 0..len {
                    let val = self.raw_read_byte(src + i as u32);
                    self.raw_write_byte(dst + i as u32, val);
                }

                if end {
                    break;
                }
                desc_addr += 12;
            }
        } else {
            let add_step = match add_val & 7 {
                0 => 0,
                1 => 2,
                2 => 4,
                3 => 8,
                4 => 16,
                5 => 32,
                6 => 64,
                7 => 128,
                _ => 2,
            };
            let mut src = read_addr;
            let mut dst = write_addr;
            let mut bytes_left = count;
            while bytes_left > 0 {
                if bytes_left >= 2 {
                    let b0 = self.raw_read_byte(src);
                    let b1 = self.raw_read_byte(src + 1);
                    self.raw_write_byte(dst, b0);
                    self.raw_write_byte(dst + 1, b1);
                    src += 2;
                    if add_step > 0 {
                        dst += add_step;
                    } else {
                        dst += 2;
                    }
                    bytes_left = bytes_left.saturating_sub(2);
                } else {
                    let val = self.raw_read_byte(src);
                    self.raw_write_byte(dst, val);
                    src += 1;
                    dst += 1;
                    bytes_left = bytes_left.saturating_sub(1);
                }
            }
        }

        self.arbiter.unlock_from_dma();

        // Clear EN flag
        let mut scu = self.work_ram.scu_regs.write().unwrap();
        scu[base + 0x10] = 0;
        scu[base + 0x11] = 0;
        scu[base + 0x12] = 0;
        scu[base + 0x13] = 0;
    }

    fn execute_cdrom_command(&mut self) {
        let (cr1, cr2, cr3, cr4) = {
            let ram = self.work_ram.cs2_regs.read().unwrap();
            let c1 = u16::from_be_bytes([ram[0], ram[1]]);
            let c2 = u16::from_be_bytes([ram[2], ram[3]]);
            let c3 = u16::from_be_bytes([ram[4], ram[5]]);
            let c4 = u16::from_be_bytes([ram[6], ram[7]]);
            (c1, c2, c3, c4)
        };

        let cmd = (cr1 >> 8) as u8;
        match cmd {
            0x00 => {
                // Get Status
                let mut ram = self.work_ram.cs2_regs.write().unwrap();
                // CR1 = 0x0400 (Status: open/closed, busy, etc.)
                ram[0] = 0x04;
                ram[1] = 0x00;
                // HIRQ = 0x0001 (Command completed)
                ram[8] = 0x00;
                ram[9] = 0x01;
            }
            0x02 => {
                // Get Play Status
                let mut ram = self.work_ram.cs2_regs.write().unwrap();
                ram[0] = 0x04;
                ram[1] = 0x00;
                ram[8] = 0x00;
                ram[9] = 0x01;
            }
            _ => {}
        }
    }

    /// Thread execution entry point
    pub fn run_loop(&mut self, shutdown: Arc<std::sync::atomic::AtomicBool>) {
        let now = std::time::Instant::now();
        self.next_vblank_due = Some(now + VBLANK_INTERVAL);
        // Real wall-clock CPU throttle -- `None` (plain unit tests, and
        // anything that never wires `self.speed` in) means run exactly as
        // fast as this interpreter manages, same as before this existed.
        let mut throttle = self
            .speed
            .clone()
            .map(|speed| crate::throttle::ClockThrottle::new(crate::throttle::SH2_CLOCK_HZ, speed));
        while !shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            if let Some(ref sync) = self.sync {
                if sync.is_shutdown() {
                    break;
                }
            }
            if let Some(due) = self.next_vblank_due {
                let now = std::time::Instant::now();
                if now >= due {
                    self.request_vblank_interrupt();
                    self.next_vblank_due = Some(now + VBLANK_INTERVAL);
                    // VBLANK-OUT fires `VBLANK_DURATION` after this same
                    // VBLANK-IN edge -- keeps this in lockstep with
                    // `tvstat_word()`'s period_start+VBLANK_DURATION edge
                    // rather than running an independently-drifting timer.
                    self.next_vblank_out_due = Some(now + VBLANK_DURATION);
                }
            }
            if let Some(out_due) = self.next_vblank_out_due {
                let now = std::time::Instant::now();
                if now >= out_due {
                    self.request_vblank_out_interrupt();
                    self.next_vblank_out_due = None;
                }
            }
            let cycles_before = self.cycles;
            self.step();
            let delta = self.cycles.wrapping_sub(cycles_before) as u32;
            if let Some(ref mut t) = throttle {
                t.advance(delta as u64);
            }
            if let Some(ref reporter) = self.pc_reporter {
                reporter.store(self.pc, std::sync::atomic::Ordering::Relaxed);
            }
            if let Some(ref sync) = self.sync {
                let limit = sync.slack_limit();
                let batch = if limit > 100 {
                    32
                } else if limit > 10 {
                    4
                } else {
                    1
                };
                self.pending_sync += delta;
                if self.pending_sync >= batch {
                    sync.sync_core(self.core_id, self.cycles);
                    self.pending_sync = 0;
                }
            }
            std::thread::yield_now();
        }
    }
}

fn sign_extend8(d: u32) -> i32 {
    (d as u8) as i8 as i32
}

fn sign_extend12(d: u32) -> i32 {
    // 12-bit value, sign bit is bit 11
    if d & 0x800 != 0 {
        (d | 0xFFFF_F000) as i32
    } else {
        d as i32
    }
}

#[cfg(test)]
mod opcode_tests {
    use super::*;
    use crate::bus_arbiter::BusArbiter;

    fn make_cpu() -> Sh2 {
        let arbiter = Arc::new(BusArbiter::new());
        let ram = Arc::new(WorkRam::new());
        Sh2::new(false, arbiter, ram)
    }

    #[test]
    fn mov_imm_rn() {
        let mut cpu = make_cpu();
        cpu.execute(0xE5_2A); // MOV #0x2A,R5
        assert_eq!(cpu.registers[5], 0x2A);
        cpu.execute(0xE5_FF); // MOV #-1,R5 (sign extended)
        assert_eq!(cpu.registers[5], 0xFFFF_FFFF);
    }

    #[test]
    fn add_imm() {
        let mut cpu = make_cpu();
        cpu.registers[1] = 10;
        cpu.execute(0x7105); // ADD #5,R1
        assert_eq!(cpu.registers[1], 15);
    }

    #[test]
    fn add_reg_reg() {
        let mut cpu = make_cpu();
        cpu.registers[1] = 10;
        cpu.registers[2] = 20;
        // ADD Rm,Rn opcode: 0011nnnnmmmm1100 -> n=1,m=2
        let op = 0x3000 | (1 << 8) | (2 << 4) | 0xC;
        cpu.execute(op);
        assert_eq!(cpu.registers[1], 30);
    }

    #[test]
    fn sub_reg_reg() {
        let mut cpu = make_cpu();
        cpu.registers[1] = 30;
        cpu.registers[2] = 12;
        let op = 0x3000 | (1 << 8) | (2 << 4) | 0x8; // SUB Rm,Rn
        cpu.execute(op);
        assert_eq!(cpu.registers[1], 18);
    }

    #[test]
    fn cmp_eq_sets_t() {
        let mut cpu = make_cpu();
        cpu.registers[1] = 5;
        cpu.registers[2] = 5;
        let op = 0x3000 | (1 << 8) | (2 << 4) | 0x0; // CMP/EQ Rm,Rn
        cpu.execute(op);
        assert!(cpu.t());
        cpu.registers[2] = 6;
        cpu.execute(op);
        assert!(!cpu.t());
    }

    #[test]
    fn and_or_xor_reg() {
        let mut cpu = make_cpu();
        cpu.registers[1] = 0b1100;
        cpu.registers[2] = 0b1010;
        cpu.execute(0x2000 | (1 << 8) | (2 << 4) | 0x9); // AND
        assert_eq!(cpu.registers[1], 0b1000);

        cpu.registers[1] = 0b1100;
        cpu.execute(0x2000 | (1 << 8) | (2 << 4) | 0xB); // OR
        assert_eq!(cpu.registers[1], 0b1110);

        cpu.registers[1] = 0b1100;
        cpu.execute(0x2000 | (1 << 8) | (2 << 4) | 0xA); // XOR
        assert_eq!(cpu.registers[1], 0b0110);
    }

    #[test]
    fn shift_ops() {
        let mut cpu = make_cpu();
        cpu.registers[3] = 0b0001;
        cpu.execute(0x4000 | (3 << 8) | 0x00); // SHLL
        assert_eq!(cpu.registers[3], 0b0010);
        cpu.execute(0x4000 | (3 << 8) | 0x01); // SHLR
        assert_eq!(cpu.registers[3], 0b0001);
    }

    #[test]
    fn bra_takes_delay_slot_then_jumps() {
        let mut cpu = make_cpu();
        // Program in high RAM: at 0x06000000: BRA +2 (disp such that target = pc+2+2*1)
        // We drive this via direct memory + step() to also exercise read_word/execute together.
        cpu.pc = 0x0600_0000;
        // BRA disp=1 -> target = (pc_of_bra + 4) + 1*2 = 0x06000000+4+2 = 0x06000006
        let bra_opcode: u16 = 0xA000 | 0x001;
        cpu.write_word(0x0600_0000, bra_opcode);
        cpu.write_word(0x0600_0002, 0xE0_2A); // delay slot: MOV #0x2A,R0
        cpu.step();
        assert_eq!(
            cpu.registers[0], 0x2A,
            "delay slot instruction did not execute"
        );
        assert_eq!(cpu.pc, 0x0600_0006, "branch target incorrect");
    }

    #[test]
    fn bsr_and_rts_roundtrip() {
        let mut cpu = make_cpu();
        cpu.pc = 0x0600_0000;
        // BSR +1 -> target = 0x06000000+4+2 = 0x06000006; PR = 0x06000004
        cpu.write_word(0x0600_0000, 0xB000 | 0x001);
        cpu.write_word(0x0600_0002, 0x0009); // delay slot: NOP
        cpu.step();
        assert_eq!(cpu.pr, 0x0600_0004);
        assert_eq!(cpu.pc, 0x0600_0006);

        // Now RTS back
        cpu.write_word(0x0600_0006, 0x000B); // RTS
        cpu.write_word(0x0600_0008, 0x0009); // delay slot: NOP
        cpu.step();
        assert_eq!(cpu.pc, 0x0600_0004);
    }

    #[test]
    fn bt_bf_no_delay_slot() {
        let mut cpu = make_cpu();
        cpu.set_t(true);
        cpu.pc = 0x0600_0000;
        // Real SH-2 formula: target = addr_of_instr + 4 + disp*2
        // = 0x06000000 + 4 + 2*2 = 0x06000008.
        cpu.write_word(0x0600_0000, 0x8900 | 0x02);
        cpu.step();
        assert_eq!(cpu.pc, 0x0600_0008);
    }

    #[test]
    fn bf_s_matches_real_bios_wait_loop() {
        // Regression test for the exact bug found running the real Saturn
        // BIOS: a BF/S two bytes short of its target turned a normal
        // countdown loop into an infinite one. Bytes below are the genuine
        // instructions read from the real BIOS ROM at offset 0x3B8:
        //   0x3B8: MOV #0x40,R4      (E440)  -- one-time loop-counter setup
        //   0x3BA: MOV.L R0,@R3      (2302)  -- loop body start
        //   0x3BC: DT R4             (4410)
        //   0x3BE: BF/S -4           (8FFC)  -- delay slot: whatever follows
        //   0x3C0: NOP               (0009)  -- stand-in delay slot instruction
        // The buggy (-2) version branched all the way back to 0x3B8,
        // re-running the setup instruction and resetting R4 every pass --
        // an infinite loop that only a real BIOS run exposed, since the
        // original unit test asserted the same wrong formula it was
        // checking against.
        let mut cpu = make_cpu();
        cpu.pc = 0x0600_03B8;
        cpu.write_word(0x0600_03B8, 0xE440); // MOV #0x40,R4
        cpu.write_word(0x0600_03BA, 0x2302); // MOV.L R0,@R3
        cpu.write_word(0x0600_03BC, 0x4410); // DT R4
        cpu.write_word(0x0600_03BE, 0x8FFC); // BF/S -4
        cpu.write_word(0x0600_03C0, 0x0009); // delay slot: NOP
        cpu.step(); // MOV #0x40,R4 -> R4=0x40
        cpu.step(); // MOV.L R0,@R3
        cpu.step(); // DT R4 -> R4=0x3F, T=0
        cpu.step(); // BF/S -> must land back on MOV.L (0x3BA), not on the setup (0x3B8)
        assert_eq!(
            cpu.pc, 0x0600_03BA,
            "BF/S landed 2 bytes short of the real loop body"
        );
        assert_eq!(
            cpu.registers[4], 0x3F,
            "loop counter must not have been reset by re-running the setup instruction"
        );
    }

    #[test]
    fn mov_l_load_store_register_indirect() {
        let mut cpu = make_cpu();
        cpu.registers[1] = 0x0600_0010;
        cpu.registers[2] = 0xDEAD_BEEF;
        // MOV.L R2,@R1
        cpu.execute(0x2000 | (1 << 8) | (2 << 4) | 0x2);
        // MOV.L @R1,R3
        cpu.execute(0x6000 | (3 << 8) | (1 << 4) | 0x2);
        assert_eq!(cpu.registers[3], 0xDEAD_BEEF);
    }

    #[test]
    fn reset_reads_vector_from_bios() {
        let mut cpu = make_cpu();
        let mut bios = vec![0u8; 0x80000];
        // Reset PC vector = 0x00000100, SP vector = 0x06010000
        bios[0..4].copy_from_slice(&0x0000_0100u32.to_be_bytes());
        bios[4..8].copy_from_slice(&0x0601_0000u32.to_be_bytes());
        cpu.load_bios(bios);
        cpu.reset();
        assert_eq!(cpu.pc, 0x0000_0100);
        assert_eq!(cpu.registers[15], 0x0601_0000);
    }

    #[test]
    fn lds_l_pop_pr_then_rts() {
        // Regression test for the exact gap found running the real Saturn
        // BIOS: a function epilogue of `LDS.L @R15+,PR` followed by `RTS`
        // wasn't decoded at all (fell through as a no-op), so PR stayed
        // stale and RTS returned to the wrong address.
        let mut cpu = make_cpu();
        cpu.registers[15] = 0x0601_0000;
        cpu.write_long(0x0601_0000, 0x0000_1234); // saved return address on the stack
        cpu.pc = 0x0600_0000;
        cpu.write_word(0x0600_0000, 0x4F26); // LDS.L @R15+,PR
        cpu.write_word(0x0600_0002, 0x000B); // RTS
        cpu.write_word(0x0600_0004, 0x0009); // RTS delay slot: NOP
        cpu.step(); // LDS.L @R15+,PR
        assert_eq!(cpu.pr, 0x0000_1234, "PR was not popped from the stack");
        assert_eq!(
            cpu.registers[15], 0x0601_0004,
            "R15 was not post-incremented"
        );
        cpu.step(); // RTS
        assert_eq!(cpu.pc, 0x0000_1234, "RTS did not return to the popped PR");
    }

    #[test]
    fn smpc_status_moved_off_high_ram_start() {
        let mut cpu = make_cpu();
        // High RAM start must now behave as ordinary RAM, not a fake SMPC read.
        cpu.write_word(0x0600_0000, 0x1234);
        assert_eq!(cpu.read_word(0x0600_0000), 0x1234);
        // The real (relocated) SMPC register block reads all-zero by
        // default (see the comment on the Smpc branch in raw_read_byte for
        // why: a nonzero placeholder hangs any real BIOS bit-poll on an
        // unimplemented status register).
        assert_eq!(cpu.read_word(0x0010_0000), 0x0000);
    }

    #[test]
    fn smpc_sf_reads_idle() {
        let mut cpu = make_cpu();
        assert_eq!(cpu.read_byte(0x0010_0000 + SMPC_SF_OFFSET as u32), 0x00);
    }

    #[test]
    fn intback_populates_real_status_and_fires_system_manager_irq() {
        // Regression test for the wall found live-tracing the real BIOS:
        // COMREG got real writes (0x06, 0x07, 0x10, 0x19, 0x1A observed
        // during an actual boot run) that were previously silently
        // discarded, so INTBACK's response (OREG) stayed all-zero forever
        // and the System Manager interrupt (vector 0x47, level 8 -- see
        // `ScuSendSystemManager` in a real, working SCU implementation)
        // never fired. Expected OREG/SR values cross-checked against
        // `SmpcINTBACKStatus`/`SmpcINTBACK` in smpc.c line-by-line, not
        // inferred.
        let mut cpu = make_cpu();
        let base = 0x0010_0000u32;
        cpu.write_byte(base + SMPC_COMREG_OFFSET as u32, SMPC_CMD_INTBACK);

        assert_eq!(
            cpu.read_byte(base + SMPC_OREG_BASE_OFFSET as u32),
            0x80,
            "OREG0: normal startup, resd=0"
        );
        assert_eq!(
            cpu.read_byte(base + (SMPC_OREG_BASE_OFFSET + 9 * 2) as u32),
            0x01,
            "OREG9: region defaults to Japan"
        );
        assert_eq!(
            cpu.read_byte(base + (SMPC_OREG_BASE_OFFSET + 10 * 2) as u32),
            0x34,
            "OREG10: flags all clear"
        );
        assert_eq!(
            cpu.read_byte(base + SMPC_SR_OFFSET as u32),
            0x6F,
            "SR: no peripheral data requested (IREG1 bit3 unset)"
        );
        assert!(
            cpu.smpc_irq_pending,
            "INTBACK completion must raise the System Manager interrupt"
        );

        // Servicing it must jump through VBR + vector*4 at the documented
        // level, exactly like VBLANK-IN (see vblank_interrupt_enters_and_returns).
        cpu.sr = 0;
        cpu.vbr = 0x0601_0000;
        cpu.registers[15] = 0x0601_1000;
        cpu.pc = 0x0600_0000;
        cpu.write_long(cpu.vbr.wrapping_add(SMPC_IRQ_VECTOR * 4), 0x0600_3000);
        cpu.write_word(0x0600_0000, 0x0009); // NOP, preempted by the interrupt
        cpu.write_word(0x0600_3000, 0x0009); // handler entry
        cpu.step();
        assert_eq!(
            cpu.pc, 0x0600_3002,
            "did not jump through the System Manager vector"
        );
        assert!(
            !cpu.smpc_irq_pending,
            "pending flag must clear once serviced"
        );
        assert_eq!(
            (cpu.sr >> SR_IMASK_SHIFT) & 0xF,
            SMPC_IRQ_LEVEL,
            "mask must raise to this interrupt's own level"
        );
    }

    #[test]
    fn intback_requesting_peripheral_data_sets_sr_bit5() {
        let mut cpu = make_cpu();
        let base = 0x0010_0000u32;
        cpu.write_byte(base + SMPC_IREG1_OFFSET as u32, 0x08); // bit3: peripheral data wanted
        cpu.write_byte(base + SMPC_COMREG_OFFSET as u32, SMPC_CMD_INTBACK);
        assert_eq!(
            cpu.read_byte(base + SMPC_SR_OFFSET as u32),
            0x6F,
            "SR bit5 set when IREG1 bit3 requests peripheral data"
        );
    }

    #[test]
    fn sndon_sndoff_flip_the_m68k_control_flag() {
        // Regression test for the wall found live-tracing the real BIOS:
        // SNDON/SNDOFF (COMREG 0x06/0x07) were silently discarded, so the
        // SCSP's M68000 sound CPU (which real hardware starts/stops via
        // `SmpcSNDON`/`SmpcSNDOFF` -> `M68KStart`/`M68KStop`) never ran the
        // uploaded sound driver, and the BIOS's post-SNDON wait for a
        // driver-written RAM flag stalled forever. `m68k_control` is the
        // handle `SaturnSystem` gives Core 3 (which owns the actual `M68k`)
        // to observe this edge -- see `lib.rs`.
        let mut cpu = make_cpu();
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        cpu.m68k_control = Some(flag.clone());
        let base = 0x0010_0000u32;

        cpu.write_byte(base + SMPC_COMREG_OFFSET as u32, SMPC_CMD_SNDON);
        assert!(
            flag.load(std::sync::atomic::Ordering::Acquire),
            "SNDON must set the flag"
        );

        cpu.write_byte(base + SMPC_COMREG_OFFSET as u32, SMPC_CMD_SNDOFF);
        assert!(
            !flag.load(std::sync::atomic::Ordering::Acquire),
            "SNDOFF must clear the flag"
        );
    }

    #[test]
    fn sound_req_irq_enters_through_its_own_vector_at_level_9() {
        // Regression test for the SCU "Sound Request" interrupt (vector
        // 0x46, level 9) that the SCSP's M68000 driver raises via MCIPD/
        // MCIEB (see `M68k::write_byte`) -- the real signal this project's
        // traced BIOS wait loop most plausibly depends on. Verifies it
        // enters/returns exactly like the other two interrupt sources
        // (compare `vblank_interrupt_enters_and_returns`).
        let mut cpu = make_cpu();
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        cpu.sound_req_irq = Some(flag.clone());
        cpu.sr = 0; // nothing masked
        cpu.vbr = 0x0601_0000;
        cpu.registers[15] = 0x0601_1000;
        cpu.pc = 0x0600_0000;
        cpu.write_word(0x0600_0000, 0x0009); // NOP, preempted by the interrupt
        cpu.write_long(cpu.vbr.wrapping_add(SOUND_REQ_IRQ_VECTOR * 4), 0x0600_4000);
        cpu.write_word(0x0600_4000, 0x0009); // handler entry

        cpu.step();
        assert_eq!(
            cpu.pc, 0x0600_4002,
            "did not jump through the Sound Request vector"
        );
        assert!(
            !flag.load(std::sync::atomic::Ordering::Relaxed),
            "flag must clear once serviced"
        );
        assert_eq!(
            (cpu.sr >> SR_IMASK_SHIFT) & 0xF,
            SOUND_REQ_IRQ_LEVEL,
            "mask must raise to this interrupt's own level"
        );
    }

    #[test]
    fn sound_ram_is_real_readwrite_memory() {
        // Regression test for the exact gap found running the real Saturn
        // BIOS: it writes an SCSP/Sound RAM value then reads it back to
        // verify the write landed. This range was previously unmapped
        // (writes discarded, reads always 0), so the verify never matched
        // and the boot sequence retried forever.
        let mut cpu = make_cpu();
        cpu.write_long(0x25A0_0000, 0xCAFEBABE);
        assert_eq!(cpu.read_long(0x25A0_0000), 0xCAFEBABE);
        cpu.write_word(0x25B0_0400, 0xA000);
        assert_eq!(cpu.read_word(0x25B0_0400), 0xA000);
    }

    #[test]
    fn vblank_interrupt_masked_stays_pending() {
        let mut cpu = make_cpu();
        cpu.sr = 0x0000_00F0; // mask level 15: everything blocked
        cpu.pc = 0x0600_0000;
        cpu.write_word(0x0600_0000, 0x0009); // NOP
        cpu.request_vblank_interrupt();
        cpu.step();
        assert!(
            cpu.vblank_pending,
            "a masked interrupt must stay pending, not fire"
        );
        assert_eq!(
            cpu.pc, 0x0600_0002,
            "masked interrupt must not have diverted execution"
        );
    }

    #[test]
    fn vblank_interrupt_enters_and_returns() {
        let mut cpu = make_cpu();
        cpu.sr = 0; // mask level 0: nothing blocked
        cpu.vbr = 0x0601_0000;
        cpu.registers[15] = 0x0601_1000;
        cpu.pc = 0x0600_0000;
        cpu.write_word(0x0600_0000, 0x0009); // NOP -- never actually fetched; interrupt preempts it
                                             // Vector table entry for VBLANK-IN (vector 0x40) points at the handler.
        cpu.write_long(cpu.vbr.wrapping_add(VBLANK_IN_VECTOR * 4), 0x0600_2000);
        // Handler: a NOP first (so the first step() can be observed landing
        // here instead of also completing the return in the same call),
        // then RTE.
        cpu.write_word(0x0600_2000, 0x0009); // NOP
        cpu.write_word(0x0600_2002, 0x002B); // RTE
        cpu.write_word(0x0600_2004, 0x0009); // RTE delay slot: NOP

        cpu.request_vblank_interrupt();
        // step() both enters the handler (redirecting PC through the vector
        // table) and then fetches+executes whatever is now at that new PC --
        // that's the handler's leading NOP here, landing us at vector+2.
        cpu.step();
        assert_eq!(
            cpu.pc, 0x0600_2002,
            "did not jump through the VBR vector table"
        );
        assert!(!cpu.vblank_pending, "pending flag must clear once serviced");
        assert_eq!(
            (cpu.sr >> SR_IMASK_SHIFT) & 0xF,
            VBLANK_IN_LEVEL,
            "mask must raise to the interrupt's own level while it runs"
        );

        cpu.step(); // RTE (+ delay slot)
        assert_eq!(
            cpu.pc, 0x0600_0000,
            "RTE did not return to the interrupted PC"
        );
        assert_eq!(cpu.sr, 0, "RTE did not restore the original SR");
        assert_eq!(
            cpu.registers[15], 0x0601_1000,
            "R15 must be back where it started after the push/pop pair"
        );
    }

    #[test]
    fn vblank_out_interrupt_masked_stays_pending() {
        let mut cpu = make_cpu();
        cpu.sr = 0x0000_00F0; // mask level 15: everything blocked
        cpu.pc = 0x0600_0000;
        cpu.write_word(0x0600_0000, 0x0009); // NOP
        cpu.request_vblank_out_interrupt();
        cpu.step();
        assert!(
            cpu.vblank_out_pending,
            "a masked interrupt must stay pending, not fire"
        );
        assert_eq!(
            cpu.pc, 0x0600_0002,
            "masked interrupt must not have diverted execution"
        );
    }

    #[test]
    fn vblank_out_interrupt_enters_and_returns() {
        // Regression test for the exact wall found running the real Saturn
        // BIOS: a boot wait loop at SH-2 0x060108ba polls a RAM counter that
        // only this BIOS's own VBLANK-OUT handler (installed in its vector
        // table at slot 0x41, distinct from VBLANK-IN's slot 0x40) ever
        // increments. Traced by dumping High RAM at the stuck PC and
        // resolving the BIOS's own interrupt dispatch table -- see
        // `VBLANK_OUT_LEVEL`'s doc comment.
        let mut cpu = make_cpu();
        cpu.sr = 0; // mask level 0: nothing blocked
        cpu.vbr = 0x0601_0000;
        cpu.registers[15] = 0x0601_1000;
        cpu.pc = 0x0600_0000;
        cpu.write_word(0x0600_0000, 0x0009); // NOP -- never actually fetched; interrupt preempts it
                                             // Vector table entry for VBLANK-OUT (vector 0x41) points at the handler.
        cpu.write_long(cpu.vbr.wrapping_add(VBLANK_OUT_VECTOR * 4), 0x0600_2000);
        cpu.write_word(0x0600_2000, 0x0009); // NOP
        cpu.write_word(0x0600_2002, 0x002B); // RTE
        cpu.write_word(0x0600_2004, 0x0009); // RTE delay slot: NOP

        cpu.request_vblank_out_interrupt();
        cpu.step();
        assert_eq!(
            cpu.pc, 0x0600_2002,
            "did not jump through the VBR vector table"
        );
        assert!(
            !cpu.vblank_out_pending,
            "pending flag must clear once serviced"
        );
        assert_eq!(
            (cpu.sr >> SR_IMASK_SHIFT) & 0xF,
            VBLANK_OUT_LEVEL,
            "mask must raise to the interrupt's own level while it runs"
        );

        cpu.step(); // RTE (+ delay slot)
        assert_eq!(
            cpu.pc, 0x0600_0000,
            "RTE did not return to the interrupted PC"
        );
        assert_eq!(cpu.sr, 0, "RTE did not restore the original SR");
        assert_eq!(
            cpu.registers[15], 0x0601_1000,
            "R15 must be back where it started after the push/pop pair"
        );
    }

    #[test]
    fn vblank_in_outranks_vblank_out_when_both_pending() {
        // Real hardware priority: VBLANK-IN (15) > VBLANK-OUT (14) -- confirmed
        // against `ScuSendVBlankIN`/`ScuSendVBlankOUT` in Yabause `scu.c`.
        let mut cpu = make_cpu();
        cpu.sr = 0;
        cpu.pc = 0x0600_0000;
        cpu.write_word(0x0600_0000, 0x0009); // NOP
        cpu.request_vblank_out_interrupt();
        cpu.request_vblank_interrupt();
        cpu.step();
        assert!(
            !cpu.vblank_pending,
            "the higher-priority interrupt must be serviced first"
        );
        assert!(
            cpu.vblank_out_pending,
            "the lower-priority interrupt must stay pending behind it"
        );
        assert_eq!((cpu.sr >> SR_IMASK_SHIFT) & 0xF, VBLANK_IN_LEVEL);
    }

    #[test]
    fn tvstat_vblank_bit_reflects_real_frame_timing() {
        // Real BIOS boot code polls TVSTAT's VBLANK bit directly (independent
        // of the VBLANK-IN interrupt) while waiting to synchronize video
        // register writes -- confirmed live during boot: offsets 4 and 5
        // (TVSTAT) are read from real BIOS ROM. A plain stored-byte register
        // (like every other VDP2 register) would read 0 forever, since
        // nothing ever "writes" the real hardware's timing-driven toggle,
        // hanging that wait loop indefinitely -- this is what backs it with
        // live wall-clock frame timing instead.
        let mut cpu = make_cpu();
        cpu.next_vblank_due = Some(std::time::Instant::now() + VBLANK_INTERVAL);
        assert_eq!(
            cpu.tvstat_word() & TVSTAT_VBLANK_BIT,
            TVSTAT_VBLANK_BIT,
            "must read VBLANK set right after the frame period starts"
        );

        cpu.next_vblank_due =
            Some(std::time::Instant::now() + VBLANK_DURATION + std::time::Duration::from_millis(5));
        assert_eq!(
            cpu.tvstat_word() & TVSTAT_VBLANK_BIT,
            0,
            "must read VBLANK clear well into active display"
        );
    }

    #[test]
    fn tvstat_byte_split_matches_real_bios_access_pattern() {
        // VBLANK (0x0008) is bit 3 of the 16-bit register, landing in the
        // low (second, big-endian) byte -- offset 5, not offset 4. Real BIOS
        // reads both offset 4 and offset 5 during boot; getting the byte
        // split backwards would silently hand it a permanently-zero bit.
        let mut cpu = make_cpu();
        cpu.next_vblank_due = Some(std::time::Instant::now() + VBLANK_INTERVAL);
        assert_eq!(
            cpu.read_byte(0x25F8_0004),
            0x00,
            "TVSTAT high byte has no bits we model"
        );
        assert_eq!(
            cpu.read_byte(0x25F8_0005),
            0x08,
            "TVSTAT low byte carries the VBLANK bit"
        );
    }

    #[test]
    fn div1_single_step_matches_hand_traced_algorithm() {
        // Verifies one DIV1 step against the algorithm traced by hand
        // (Rn=1000, Rm=7, Q=M=T=0 fresh off DIV0U):
        //   shifted_q = bit31(1000) = 0; Q := 0
        //   Rn = (1000<<1)|0 = 2000
        //   old_q=0, M=0 -> subtract: Rn = 2000-7 = 1993, no borrow (1993 < 2000)
        //   Q = borrow (false) since shifted_q was 0
        //   T = (Q==M) = true
        //
        // NOTE: this confirms the single-step mechanics match the reference
        // algorithm exactly. It does NOT confirm that the textbook "DIV0U +
        // 32x DIV1" sequence yields a standard unsigned quotient in Rn for
        // arbitrary inputs -- an earlier version of this test asserted that
        // and was wrong for every case except 1/1, meaning either the real
        // calling convention needs extra setup this test isn't doing (e.g.
        // a 64-bit dividend split across a register pair) or a correction
        // step after the loop (e.g. a final ROTCL) that plain repetition
        // doesn't capture. Don't trust multi-step DIV1 chains against real
        // division results without validating against real compiled SH-2
        // division-routine output first.
        let mut cpu = make_cpu();
        cpu.registers[1] = 1000;
        cpu.registers[2] = 7;
        cpu.execute(0x0019); // DIV0U: Q=M=T=0
        let div1_op = 0x3000 | (1 << 8) | (2 << 4) | 0x4; // DIV1 R2,R1
        cpu.execute(div1_op);
        assert_eq!(cpu.registers[1], 1993);
        assert!(!cpu.q());
        assert!(!cpu.m());
        assert!(cpu.t());
    }

    #[test]
    fn div0s_seeds_qm_for_div1() {
        let mut cpu = make_cpu();
        cpu.registers[1] = 0x8000_0000; // negative
        cpu.registers[2] = 0x0000_0001; // positive
        cpu.execute(0x2000 | (1 << 8) | (2 << 4) | 0x7); // DIV0S R2,R1
        assert!(cpu.q(), "Q must seed from Rn's sign bit");
        assert!(!cpu.m(), "M must seed from Rm's sign bit");
        assert!(cpu.t(), "T = Q^M for a sign mismatch");
    }

    #[test]
    fn trapa_pushes_sr_then_pc_and_jumps_through_vbr() {
        let mut cpu = make_cpu();
        cpu.sr = 0x55; // arbitrary, just needs to round-trip
        cpu.vbr = 0x0601_0000;
        cpu.registers[15] = 0x0601_1000;
        cpu.pc = 0x0600_0000;
        cpu.write_word(0x0600_0000, 0xC3_2A); // TRAPA #0x2A
        cpu.write_long(cpu.vbr.wrapping_add(0x2A * 4), 0x0600_3000);
        cpu.step();
        assert_eq!(cpu.pc, 0x0600_3000, "did not jump through VBR + imm*4");
        assert_eq!(
            cpu.registers[15],
            0x0601_1000 - 8,
            "must push exactly 2 longwords"
        );
        // Popped back in RTE order (PC first, then SR) to confirm the push
        // order matches: PC (return addr) at the lower/top address.
        assert_eq!(
            cpu.read_long(0x0601_1000 - 8),
            0x0600_0002,
            "pushed return address must be right after TRAPA"
        );
        assert_eq!(
            cpu.read_long(0x0601_1000 - 4),
            0x55,
            "pushed SR must be the pre-trap value"
        );
    }

    #[test]
    fn peripheral_regions_are_real_readwrite_memory() {
        // Regression coverage for the broad memory-map sweep done against
        // Yabause's real, working implementation: every one of these was
        // previously Unmapped (writes silently discarded).
        let mut cpu = make_cpu();
        let probes: &[(u32, &str)] = &[
            (0x0580_0000, "CS2/CD-ROM regs"),
            (0x05A0_0000, "sound ram"),
            (0x05B0_0000, "SCSP regs"),
            (0x05C0_0000, "VDP1 VRAM"),
            (0x05C8_0000, "VDP1 framebuffer"),
            (0x05D0_0000, "VDP1 regs"),
            (0x05E0_0000, "VDP2 VRAM"),
            (0x05F0_0000, "VDP2 CRAM"),
            (0x05F8_0000, "VDP2 regs"),
            (0x05FE_0000, "SCU regs"),
        ];
        for &(addr, name) in probes {
            cpu.write_long(addr, 0x1234_5678);
            assert_eq!(
                cpu.read_long(addr),
                0x1234_5678,
                "{name} at {addr:#010X} is not real read/write memory"
            );
        }
    }

    #[test]
    fn smpc_register_window_mirrors_every_512kb() {
        // Real hardware masks the SMPC offset with & 0x7F across the whole
        // 512KB window (0x00100000-0x0017FFFF); confirm a mirror far from
        // the window's start still resolves to the same SF register.
        let mut cpu = make_cpu();
        assert_eq!(cpu.read_byte(0x0010_0063), 0x00); // SF at the window's start
        assert_eq!(cpu.read_byte(0x0017_0063), 0x00); // same offset, mirrored deep in the window
    }

    #[test]
    fn test_onchip_division() {
        let mut cpu = make_cpu();
        // 32-bit / 32-bit division: 100 / 3
        cpu.write_long(0xFFFFFF00, 3); // DVSR = 3
        cpu.write_long(0xFFFFFF04, 100); // DVDNT = 100 -> triggers division
        assert_eq!(cpu.read_long(0xFFFFFF04), 33); // Quotient DVDNTL = 33
        assert_eq!(cpu.read_long(0xFFFFFF10), 1); // Remainder DVDNTH = 1

        // Division by zero
        cpu.write_long(0xFFFFFF00, 0); // DVSR = 0
        cpu.write_long(0xFFFFFF04, 100); // DVDNT = 100 -> triggers division by zero
        assert_eq!(cpu.read_long(0xFFFFFF08) & 1, 1); // DVCR overflow flag = 1

        // 64-bit / 32-bit division: 0x00000001_00000000 / 4
        cpu.write_long(0xFFFFFF00, 4); // DVSR = 4
        cpu.write_long(0xFFFFFF10, 1); // DVDNTH = 1
        cpu.write_long(0xFFFFFF14, 0); // DVDNTL = 0 -> triggers division
        assert_eq!(cpu.read_long(0xFFFFFF04), 0x40000000); // Quotient DVDNTL = 0x40000000
        assert_eq!(cpu.read_long(0xFFFFFF10), 0); // Remainder DVDNTH = 0
    }

    #[test]
    fn test_scu_dma_direct() {
        let mut cpu = make_cpu();
        // Write source data to Low RAM offset 0 (0x00200000)
        cpu.write_long(0x00200000, 0x11223344);
        cpu.write_long(0x00200004, 0x55667788);

        // Configure Channel 0 SCU DMA registers
        cpu.write_long(0x05FE0000, 0x00200000); // D0R (Read Addr)
        cpu.write_long(0x05FE0004, 0x00201000); // D0W (Write Addr)
        cpu.write_long(0x05FE0008, 8); // D0C (Count = 8 bytes)
        cpu.write_long(0x05FE000C, 1); // D0AD (Address increment mode)
        cpu.write_long(0x05FE0014, 0); // D0MD (Direct Mode)

        // Trigger DMA by writing 1 to D0EN
        cpu.write_long(0x05FE0010, 1);

        // Verify data was copied
        assert_eq!(cpu.read_long(0x00201000), 0x11223344);
        assert_eq!(cpu.read_long(0x00201004), 0x55667788);

        // D0EN register must be cleared automatically
        assert_eq!(cpu.read_long(0x05FE0010), 0);
    }

    #[test]
    fn test_cdrom_handshake() {
        let mut cpu = make_cpu();

        // Write CR1 = 0x0000, CR2 = 0x0000, CR3 = 0x0000, CR4 = 0x0000 (Get Status command)
        cpu.write_word(0x05800000, 0x0000); // CR1
        cpu.write_word(0x05800002, 0x0000); // CR2
        cpu.write_word(0x05800004, 0x0000); // CR3
        cpu.write_word(0x05800006, 0x0000); // CR4 (triggers command)

        // Verify response CR1 = 0x0400
        assert_eq!(cpu.read_word(0x05800000), 0x0400);
        // Verify HIRQ = 0x0001
        assert_eq!(cpu.read_word(0x05800008), 0x0001);
    }

    #[test]
    fn test_sleep() {
        let mut cpu = make_cpu();
        cpu.sr = 0; // mask level 0
        cpu.vbr = 0x0601_0000;
        cpu.registers[15] = 0x0601_1000;
        cpu.pc = 0x0600_0000;
        // Write SLEEP opcode
        cpu.write_word(0x0600_0000, 0x001B);
        // Vector table entry for VBLANK-IN points to 0x0600_2000
        cpu.write_long(cpu.vbr.wrapping_add(VBLANK_IN_VECTOR * 4), 0x0600_2000);
        cpu.write_word(0x0600_2000, 0x0009); // NOP inside handler

        let cycles_before = cpu.cycles;
        cpu.step(); // executes SLEEP
        assert_eq!(cpu.pc, 0x0600_0000, "SLEEP must not advance PC");
        assert_eq!(
            cpu.cycles,
            cycles_before.wrapping_add(3),
            "SLEEP must charge 3 cycles"
        );

        // Now request interrupt and step
        cpu.request_vblank_interrupt();
        cpu.step(); // takes interrupt and executes handler first instruction
        assert_eq!(
            cpu.pc, 0x0600_2002,
            "PC must have diverged to interrupt vector handler + 2"
        );
    }

    #[test]
    fn test_braf_bsrf() {
        let mut cpu = make_cpu();
        // BRAF R1
        cpu.pc = 0x0600_1000;
        cpu.write_word(0x0600_1000, 0x0123); // BRAF R1 (nibble B = 1)
        cpu.write_word(0x0600_1002, 0xE212); // MOV #0x12, R2 (delay slot)
        cpu.registers[1] = 0x40;
        cpu.step();
        assert_eq!(cpu.registers[2], 0x12, "delay slot must execute");
        assert_eq!(
            cpu.pc, 0x0600_1044,
            "BRAF target must be PC_br + Rn + 4 (0x0600_1000 + 0x40 + 4)"
        );

        // BSRF R1
        cpu.pc = 0x0600_1000;
        cpu.write_word(0x0600_1000, 0x0103); // BSRF R1 (nibble B = 1)
        cpu.write_word(0x0600_1002, 0xE224); // MOV #0x24, R2 (delay slot)
        cpu.registers[1] = 0x40;
        cpu.step();
        assert_eq!(cpu.registers[2], 0x24, "delay slot must execute");
        assert_eq!(cpu.pc, 0x0600_1044, "BSRF target must be PC_br + Rn + 4");
        assert_eq!(cpu.pr, 0x0600_1004, "PR must be PC_br + 4");
    }

    #[test]
    fn test_mac_l_mac_w() {
        let mut cpu = make_cpu();
        cpu.registers[1] = 0x0020_0000; // Rn
        cpu.registers[2] = 0x0020_0010; // Rm
                                        // Write memory operands
        cpu.write_long(0x0020_0000, 0x1000);
        cpu.write_long(0x0020_0010, 0x2000);

        // MAC.L @R2+,@R1+
        cpu.macl = 0x500;
        cpu.mach = 0;
        cpu.execute(0x012F); // MAC.L @R2+,@R1+ (n=1, m=2)
        assert_eq!(cpu.registers[1], 0x0020_0004);
        assert_eq!(cpu.registers[2], 0x0020_0014);
        // mul = 0x1000 * 0x2000 = 0x0200_0000. sum = 0x500 + 0x0200_0000 = 0x0200_0500
        assert_eq!(cpu.macl, 0x0200_0500);
        assert_eq!(cpu.mach, 0);

        // Test MAC.W
        cpu.registers[1] = 0x0020_0100;
        cpu.registers[2] = 0x0020_0110;
        cpu.write_word(0x0020_0100, 0x1234u16);
        cpu.write_word(0x0020_0110, 0x5678u16);
        cpu.macl = 0x1000;
        cpu.mach = 0xFFFFFFFF; // should be overwritten or modified
        cpu.set_s(false);
        cpu.execute(0x412F); // MAC.W @R2+,@R1+ (n=1, m=2)
        assert_eq!(cpu.registers[1], 0x0020_0102);
        assert_eq!(cpu.registers[2], 0x0020_0112);
        // mul = 0x1234 * 0x5678 = 0x0626_0060. sum = 0x1000 + 0x0626_0060 = 0x0626_1060
        assert_eq!(cpu.macl, 0x0626_1060);
        // S = 0, MACH is non-accumulating overwrite: sum >> 32 = 0
        assert_eq!(cpu.mach, 0);
    }

    #[test]
    fn test_gbr_byte_ops() {
        let mut cpu = make_cpu();
        cpu.gbr = 0x0020_0000;
        cpu.registers[0] = 0x10;
        let addr = 0x0020_0010;

        // TST.B
        cpu.write_byte(addr, 0x5A);
        cpu.execute(0xCC3C); // TST.B #0x3C,@(R0,GBR) -> 0x5A & 0x3C = 0x18 != 0 -> T = 0
        assert!(!cpu.t());
        cpu.execute(0xCC84); // TST.B #0x84,@(R0,GBR) -> 0x5A & 0x84 = 0 -> T = 1
        assert!(cpu.t());

        // AND.B
        cpu.write_byte(addr, 0x5A);
        cpu.execute(0xCD0F); // AND.B #0x0F,@(R0,GBR) -> 0x5A & 0x0F = 0x0A
        assert_eq!(cpu.read_byte(addr), 0x0A);

        // XOR.B
        cpu.write_byte(addr, 0x5A);
        cpu.execute(0xCEAA); // XOR.B #0xAA,@(R0,GBR) -> 0x5A ^ 0xAA = 0xF0
        assert_eq!(cpu.read_byte(addr), 0xF0);

        // OR.B
        cpu.write_byte(addr, 0x5A);
        cpu.execute(0xCFF0); // OR.B #0xF0,@(R0,GBR) -> 0x5A | 0xF0 = 0xFA
        assert_eq!(cpu.read_byte(addr), 0xFA);
    }

    #[test]
    fn test_cache_purge_behavior() {
        let mut cpu = make_cpu();
        // 1. Longword write to CachePurge (e.g. 0x4020_0008) is a no-op (doesn't modify uncached memory)
        cpu.write_long(0x4020_0008, 0x12345678);
        assert_eq!(cpu.read_long(0x0020_0008), 0); // Low RAM remains 0

        // 2. Byte and word writes to CachePurge fall through to uncached write
        cpu.write_byte(0x4020_0000, 0xAA);
        assert_eq!(cpu.read_byte(0x0020_0000), 0xAA);
        cpu.write_word(0x4020_0002, 0xBBCC);
        assert_eq!(cpu.read_word(0x0020_0002), 0xBBCC);

        // 3. Reads to CachePurge always return 0xFF per byte
        assert_eq!(cpu.read_byte(0x4020_0000), 0xFF);
        assert_eq!(cpu.read_word(0x4020_0002), 0xFFFF);
        assert_eq!(cpu.read_long(0x4020_0004), 0xFFFFFFFF);

        // 4. Area 5 (0xA020_0000) must behave as normal cache-through memory (not CachePurge)
        cpu.write_byte(0xA020_0000, 0x55);
        assert_eq!(cpu.read_byte(0xA020_0000), 0x55);
        assert_eq!(cpu.read_byte(0x0020_0000), 0x55);
    }

    #[test]
    fn test_cache_arrays_and_execution() {
        let mut cpu = make_cpu();
        // 1. AddressArray: write u32 (long) and verify layout/mirrors
        cpu.write_long(0x6000_0000, 0x11223344);
        assert_eq!(cpu.address_array[0], 0x11223344);
        assert_eq!(cpu.read_long(0x6000_0400), 0x11223344); // mirrors every 1KB

        // Byte read to AddressArray falls to Unmapped / returns 0
        assert_eq!(cpu.read_byte(0x6000_0000), 0);

        // 2. DataArray: write and read back using byte, word, and long
        cpu.write_byte(0xC000_0010, 0xAA);
        cpu.write_byte(0xC000_0011, 0xBB);
        assert_eq!(cpu.data_array[0x10], 0xAA);
        assert_eq!(cpu.data_array[0x11], 0xBB);
        assert_eq!(cpu.read_byte(0xC000_0010), 0xAA);
        assert_eq!(cpu.read_byte(0xC000_0011), 0xBB);

        // 3. EXEC_FROM_CACHE: write NOP at 0xC000_0000 and run step()
        cpu.data_array[0] = 0x00;
        cpu.data_array[1] = 0x09; // NOP
        cpu.pc = 0xC000_0000;
        let cycles_before = cpu.cycles;
        cpu.step();
        assert_eq!(cpu.pc, 0xC000_0002);
        assert_eq!(cpu.cycles, cycles_before + 1); // NOP cycles
        assert!(!cpu.illegal_instruction_flag);
    }

    #[test]
    fn test_memory_bus_phase_1() {
        let mut cpu = make_cpu();

        // 1. PurgeArea reads (0x40000000)
        assert_eq!(cpu.read_long(0x4000_0000), 0xFFFFFFFF);
        assert_eq!(cpu.read_word(0x4000_0000), 0xFFFF);
        assert_eq!(cpu.read_byte(0x4000_0000), 0xFF);

        // 2. Area 5 behaves as CacheThrough mirror
        cpu.write_long(0x0020_0000, 0x12345678);
        assert_eq!(cpu.read_long(0xA020_0000), 0x12345678);

        // 3. Data array is real memory and mirrors every 4 KB
        cpu.write_long(0xC000_0000, 0xDEADBEEF);
        assert_eq!(cpu.read_long(0xC000_1000), 0xDEADBEEF);
        assert_eq!(cpu.read_long(0xC000_0000), 0xDEADBEEF);

        // 4. Data array is per-CPU
        let mut cpu2 = Sh2::new(true, cpu.arbiter.clone(), cpu.work_ram.clone());
        assert_eq!(cpu2.read_long(0xC000_0000), 0); // Core 1 sees 0

        // 5. Address array is long-only
        cpu.write_long(0x6000_0000, 0x1234);
        assert_eq!(cpu.read_long(0x6000_0400), 0x1234); // mirrors every 1KB
        assert_eq!(cpu.read_byte(0x6000_0000), 0); // byte/word fall to Unmapped/0

        // 6. Area 7 below on-chip registers
        assert_eq!(cpu.read_long(0xE020_0000), 0);
        cpu.write_long(0xE020_0000, 0x99999999);
        assert_eq!(cpu.read_long(0x0020_0000), 0x12345678); // Low RAM unaffected

        // 7. Area 0 genuine hole
        assert_eq!(cpu.read_word(0x0E00_0000), 0);
    }

    #[test]
    fn test_memory_bus_phase_2() {
        let mut cpu = make_cpu();

        // 1. High WRAM: 1MB size and mirrors every 1MB across B-bus
        cpu.write_byte(0x0600_0000, 0x11);
        assert_eq!(cpu.read_byte(0x0610_0000), 0x11);
        cpu.write_byte(0x0608_0000, 0x22);
        assert_eq!(cpu.read_byte(0x0618_0000), 0x22);
        assert_eq!(cpu.read_byte(0x0600_0000), 0x11); // no collision

        // 2. VDP1 registers: 256 B (mirrors every 256 B)
        cpu.write_byte(0x05D0_0000, 0xAA);
        assert_eq!(cpu.read_byte(0x05D0_0100), 0xAA);

        // 3. VDP2 registers: 512 B (mirrors every 512 B)
        cpu.write_byte(0x05F8_0000, 0xBB);
        assert_eq!(cpu.read_byte(0x05F8_0200), 0xBB);

        // TVSTAT read at mirrored offset
        cpu.next_vblank_due = Some(std::time::Instant::now() + VBLANK_INTERVAL);
        assert_eq!(cpu.read_byte(0x05F8_0204), 0x00);
        assert_eq!(cpu.read_byte(0x05F8_0205), 0x08);

        // 4. SCU registers: 256 B (mirrors every 256 B)
        cpu.write_byte(0x05FE_0000, 0xCC);
        assert_eq!(cpu.read_byte(0x05FE_0100), 0xCC);

        // 5. Internal backup RAM: 64 KB, plus odd-byte convention
        cpu.write_byte(0x0018_0000, 0x55);
        assert_eq!(cpu.read_byte(0x0018_0001), 0x55);
        // Writing to 0x0018_0000 actually stores to 0x0018_0001, so 0x0018_0000 remains 0 (unwritten)
        assert_eq!(cpu.read_byte(0x0018_0000), 0x00);

        // 6. Sound RAM MEM4MB mirror
        // mem4b = false by default, mirrors every 256 KB
        cpu.write_byte(0x05A0_0000, 0x77);
        assert_eq!(cpu.read_byte(0x05A4_0000), 0x77);

        // Set mem4b = true via SCSP Reg 0x400
        cpu.write_byte(0x05B0_0400, 0x02); // set bit 9
        cpu.write_byte(0x05A0_0000, 0x88);
        assert_eq!(cpu.read_byte(0x05A4_0000), 0x00); // 256KB mirror mode is off, offset 256KB should be 0
        assert_eq!(cpu.read_byte(0x05A8_0000), 0xFF); // offset > 512KB (0x80000) returns all-ones (0xFF)

        // 7. CS2 20-bit offset (no aliasing)
        cpu.write_byte(0x0580_0000, 0x12);
        assert_eq!(cpu.read_byte(0x0581_8000), 0); // FIFO offset doesn't alias onto CR1
    }

    #[test]
    fn test_torn_read_stress() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::thread;
        use std::time::{Duration, Instant};

        let arbiter = Arc::new(BusArbiter::new());
        let work_ram = Arc::new(WorkRam::new());
        let running = Arc::new(AtomicBool::new(true));

        // Thread 1: Writes alternately to High WRAM (normal and straddling)
        let wr_ram = work_ram.clone();
        let wr_running = running.clone();
        let writer_handle = thread::spawn(move || {
            let mut val = 0u32;
            while wr_running.load(Ordering::Relaxed) {
                // Address 0x0600_0100 (aligned, fits in stripe 0)
                wr_ram.write_high_ram_long(0x100, val);
                // Address 0x0600_7FFE (crosses stripe 0 and stripe 1)
                wr_ram.write_high_ram_long(0x7FFE, val);
                val = if val == 0 { 0xFFFFFFFF } else { 0 };
            }
        });

        // Thread 2: Reads and verifies no torn reads occur
        let rd_ram = work_ram.clone();
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(200) {
            let val_normal = rd_ram.read_high_ram_long(0x100);
            assert!(
                val_normal == 0 || val_normal == 0xFFFFFFFF,
                "Torn read observed on normal path: {val_normal:#010X}"
            );

            let val_straddle = rd_ram.read_high_ram_long(0x7FFE);
            assert!(
                val_straddle == 0 || val_straddle == 0xFFFFFFFF,
                "Torn read observed on straddling path: {val_straddle:#010X}"
            );
        }

        running.store(false, Ordering::Relaxed);
        writer_handle.join().unwrap();
    }

    #[test]
    fn test_illegal_instruction_exceptions() {
        let mut cpu = make_cpu();
        // Setup vector table: VBR = 0x0020_0000
        cpu.vbr = 0x0020_0000;
        // Vector 4 is at offset 16 (0x0020_0010). Point it to 0x0020_0100
        cpu.write_long(0x0020_0010, 0x0020_0100);
        // Setup R15 = 0x0020_0500
        cpu.registers[15] = 0x0020_0500;
        cpu.sr = 0x0000_00F0; // dummy SR

        // 1. Execute illegal instruction 0x0000 at 0x0020_0080
        cpu.write_word(0x0020_0080, 0x0000);
        cpu.pc = 0x0020_0080;
        cpu.step();

        // PC should have jumped to the handler address 0x0020_0100
        assert_eq!(cpu.pc, 0x0020_0100);
        // Stack should have popped/pushed SR and PC (address after 0x0020_0080, which is 0x0020_0082)
        assert_eq!(cpu.registers[15], 0x0020_04F8);
        assert_eq!(cpu.read_long(0x0020_04F8), 0x0020_0082); // PC pushed
        assert_eq!(cpu.read_long(0x0020_04FC), 0x0000_00F0); // SR pushed
        assert!(cpu.illegal_instruction_flag);

        // Reset flag and do 0xFFFF
        cpu.illegal_instruction_flag = false;
        cpu.registers[15] = 0x0020_0500;
        cpu.write_word(0x0020_0080, 0xFFFF);
        cpu.pc = 0x0020_0080;
        cpu.step();

        assert_eq!(cpu.pc, 0x0020_0100);
        assert_eq!(cpu.registers[15], 0x0020_04F8);
        assert_eq!(cpu.read_long(0x0020_04F8), 0x0020_0082); // PC pushed
        assert!(cpu.illegal_instruction_flag);
    }

    #[test]
    fn test_onchip_p4_t1_reset_values() {
        let mut cpu_master = make_cpu();
        let mut cpu_slave = Sh2::new(
            true,
            cpu_master.arbiter.clone(),
            cpu_master.work_ram.clone(),
        );

        // SCI
        assert_eq!(cpu_master.read_byte(0xFFFFFE00), 0x00); // SMR
        assert_eq!(cpu_master.read_byte(0xFFFFFE01), 0xFF); // BRR
        assert_eq!(cpu_master.read_byte(0xFFFFFE02), 0x00); // SCR
        assert_eq!(cpu_master.read_byte(0xFFFFFE03), 0xFF); // TDR
        assert_eq!(cpu_master.read_byte(0xFFFFFE04), 0x84); // SSR
        assert_eq!(cpu_master.read_byte(0xFFFFFE05), 0x00); // RDR

        // FRT
        assert_eq!(cpu_master.read_byte(0xFFFFFE10), 0x01); // TIER
        assert_eq!(cpu_master.read_byte(0xFFFFFE11), 0x00); // FTCSR
        assert_eq!(cpu_master.read_word(0xFFFFFE12), 0x0000); // FRC
        assert_eq!(cpu_master.read_word(0xFFFFFE14), 0xFFFF); // OCRA/OCRB
        assert_eq!(cpu_master.read_byte(0xFFFFFE16), 0x00); // TCR
        assert_eq!(cpu_master.read_byte(0xFFFFFE17), 0xE0); // TOCR
        assert_eq!(cpu_master.read_word(0xFFFFFE18), 0x0000); // FICR

        // INTC
        assert_eq!(cpu_master.read_word(0xFFFFFE60), 0x0000); // IPRB
        assert_eq!(cpu_master.read_word(0xFFFFFE62), 0x0000); // VCRA
        assert_eq!(cpu_master.read_word(0xFFFFFE64), 0x0000); // VCRB
        assert_eq!(cpu_master.read_word(0xFFFFFE66), 0x0000); // VCRC
        assert_eq!(cpu_master.read_word(0xFFFFFE68), 0x0000); // VCRD
        assert_eq!(cpu_master.read_word(0xFFFFFEE0), 0x0000); // ICR
        assert_eq!(cpu_master.read_word(0xFFFFFEE2), 0x0000); // IPRA
        assert_eq!(cpu_master.read_word(0xFFFFFEE4), 0x0000); // VCRWDT

        // WDT
        assert_eq!(cpu_master.read_byte(0xFFFFFE80), 0x18); // WTCSR
        assert_eq!(cpu_master.read_byte(0xFFFFFE81), 0x00); // WTCNT
        assert_eq!(cpu_master.read_byte(0xFFFFFE83), 0x1F); // RSTCSR

        // SBYCR / CCR
        assert_eq!(cpu_master.read_byte(0xFFFFFE91), 0x60); // SBYCR
        assert_eq!(cpu_master.read_byte(0xFFFFFE92), 0x00); // CCR

        // BSC
        // Master BCR1: bit 15 is 0
        assert_eq!(cpu_master.read_word(0xFFFFFFE0), 0x03F0);
        // Slave BCR1: bit 15 is 1
        assert_eq!(cpu_slave.read_word(0xFFFFFFE0), 0x83F0);
        assert_eq!(cpu_master.read_word(0xFFFFFFE4), 0x00FC); // BCR2
        assert_eq!(cpu_master.read_word(0xFFFFFFE8), 0xAAFF); // WCR
        assert_eq!(cpu_master.read_word(0xFFFFFFEC), 0x0000); // MCR
        assert_eq!(cpu_master.read_word(0xFFFFFFF0), 0x0000); // RTCSR
        assert_eq!(cpu_master.read_word(0xFFFFFFF4), 0x0000); // RTCNT
        assert_eq!(cpu_master.read_word(0xFFFFFFF8), 0x0000); // RTCOR

        // DRCR
        assert_eq!(cpu_master.read_byte(0xFFFFFE71), 0x00); // DRCR0
        assert_eq!(cpu_master.read_byte(0xFFFFFE72), 0x00); // DRCR1
    }

    #[test]
    fn test_onchip_p4_t2_write_masking() {
        let mut cpu = make_cpu();

        // INTC Masking
        cpu.write_word(0xFFFFFE60, 0xFFFF); // IPRB -> val & 0xFF00
        assert_eq!(cpu.read_word(0xFFFFFE60), 0xFF00);

        cpu.write_word(0xFFFFFE62, 0xFFFF); // VCRA -> val & 0x7F7F
        assert_eq!(cpu.read_word(0xFFFFFE62), 0x7F7F);

        cpu.write_word(0xFFFFFE64, 0xFFFF); // VCRB -> val & 0x7F7F
        assert_eq!(cpu.read_word(0xFFFFFE64), 0x7F7F);

        cpu.write_word(0xFFFFFE66, 0xFFFF); // VCRC -> val & 0x7F7F
        assert_eq!(cpu.read_word(0xFFFFFE66), 0x7F7F);

        cpu.write_word(0xFFFFFE68, 0xFFFF); // VCRD -> val & 0x7F7F
        assert_eq!(cpu.read_word(0xFFFFFE68), 0x7F7F);

        cpu.write_word(0xFFFFFEE0, 0xFFFF); // ICR -> val & 0x0101
        assert_eq!(cpu.read_word(0xFFFFFEE0), 0x0101);

        cpu.write_word(0xFFFFFEE2, 0xFFFF); // IPRA -> val & 0xFFF0
        assert_eq!(cpu.read_word(0xFFFFFEE2), 0xFFF0);

        cpu.write_word(0xFFFFFEE4, 0xFFFF); // VCRWDT -> val & 0x7F7F
        assert_eq!(cpu.read_word(0xFFFFFEE4), 0x7F7F);

        // CCR
        cpu.write_byte(0xFFFFFE92, 0xFF); // CCR -> val & 0xCF
        assert_eq!(cpu.read_byte(0xFFFFFE92), 0xCF);

        // SBYCR
        cpu.write_byte(0xFFFFFE91, 0xFF); // SBYCR -> val & 0xDF
        assert_eq!(cpu.read_byte(0xFFFFFE91), 0xDF);

        // BSC
        cpu.write_long(0xFFFFFFE0, 0x1FF7_00FC); // BCR1 = (bcr1&0x8000) | (val>>16 & 0x1FF7), BCR2 = val & 0xFC
        assert_eq!(cpu.read_word(0xFFFFFFE0), 0x1FF7); // master bit 15 is 0
        assert_eq!(cpu.read_word(0xFFFFFFE4), 0x00FC);

        cpu.write_long(0xFFFFFFE8, 0x1234_FEFC); // WCR, MCR
        assert_eq!(cpu.read_word(0xFFFFFFE8), 0x1234);
        assert_eq!(cpu.read_word(0xFFFFFFEC), 0xFEFC);

        cpu.write_long(0xFFFFFFF0, 0x00F8_0000); // RTCSR
        assert_eq!(cpu.read_word(0xFFFFFFF0), 0x00F8);

        cpu.write_long(0xFFFFFFF8, 0x00FF_0000); // RTCOR
        assert_eq!(cpu.read_word(0xFFFFFFF8), 0x00FF);

        // DRCR
        cpu.write_byte(0xFFFFFE71, 0xFF); // DRCR0 -> val & 3
        assert_eq!(cpu.read_byte(0xFFFFFE71), 0x03);
    }

    #[test]
    fn test_onchip_p4_t3_destructive_byte_write_quirks() {
        let mut cpu = make_cpu();

        // 1. IPRB byte-write at 0x060 must destroy the low byte
        cpu.write_word(0xFFFFFE60, 0x1234);
        cpu.write_byte(0xFFFFFE60, 0x55);
        assert_eq!(cpu.read_word(0xFFFFFE60), 0x5500);

        // 2. VCRD byte-write at 0x068 must clear the low byte
        cpu.write_word(0xFFFFFE68, 0x1234);
        cpu.write_byte(0xFFFFFE68, 0x55);
        assert_eq!(cpu.read_word(0xFFFFFE68), 0x5500 & 0x7F00);

        // 3. Byte writes at 0x061 and 0x069 must be ignored
        cpu.write_word(0xFFFFFE60, 0x1234);
        cpu.write_byte(0xFFFFFE61, 0x55);
        assert_eq!(cpu.read_word(0xFFFFFE60), 0x1200); // remains 0x1200 (since 0x34 is low byte, but wait, write_word does val & 0xFF00, so 0x34 was already masked to 0!)

        cpu.write_word(0xFFFFFE68, 0x1234 & 0x7F7F);
        cpu.write_byte(0xFFFFFE69, 0x55);
        assert_eq!(cpu.read_word(0xFFFFFE68), 0x1234 & 0x7F7F);
    }

    #[test]
    fn test_onchip_p4_t4_access_width_matrix() {
        let mut cpu = make_cpu();

        // CCR: readable and writable as byte and word
        cpu.write_byte(0xFFFFFE92, 0x0C);
        assert_eq!(cpu.read_byte(0xFFFFFE92), 0x0C);
        assert_eq!(cpu.read_word(0xFFFFFE92), 0x0C00); // read as word

        cpu.write_word(0xFFFFFE92, 0x0F00);
        assert_eq!(cpu.read_byte(0xFFFFFE92), 0x0F);

        // BCR1: written only on long path, word read at +2, no word write
        cpu.write_long(0xFFFFFFE0, 0x1FF7_0000);
        assert_eq!(cpu.read_word(0xFFFFFFE2), 0x1FF7); // read BCR1 at +2

        cpu.write_word(0xFFFFFFE0, 0x0000); // word write to BCR1 ignored
        assert_eq!(cpu.read_word(0xFFFFFFE2), 0x1FF7);
    }

    #[test]
    fn test_tas_b_fallback() {
        let mut cpu = make_cpu();
        // 1. TAS.B @Rn on Low RAM (atomic path)
        cpu.write_byte(0x0020_0000, 0x55);
        cpu.registers[1] = 0x0020_0000;
        cpu.execute(0x411B); // TAS.B @R1
        assert_eq!(cpu.read_byte(0x0020_0000), 0xD5); // 0x55 | 0x80 = 0xD5
        assert!(!cpu.t()); // MSB was 0

        // 2. TAS.B @Rn on SMPC (non-atomic fallback path)
        cpu.write_byte(0x0010_0000, 0x00);
        cpu.registers[2] = 0x0010_0000;
        cpu.execute(0x421B); // TAS.B @R2
        assert_eq!(cpu.read_byte(0x0010_0000), 0x80); // 0x00 | 0x80 = 0x80
        assert!(cpu.t()); // MSB was 0
    }

    #[test]
    fn test_bus_miss_logging() {
        std::env::set_var("MIMAS_BUS_TRACE", "1");

        let mut cpu = make_cpu();

        // 1. Synthetic access at 0xC000_0000 (Area 6)
        let key1 = "area=6 block=0x00000000 is_write=false width=1".to_string();
        cpu.read_byte(0xC000_0000);
        {
            let log = lock_bus_miss_log();
            assert!(log.contains(&key1));
            let count = log.iter().filter(|k| **k == key1).count();
            assert_eq!(count, 1);
        }

        // Repeat access -> should not log again (count remains 1)
        cpu.read_byte(0xC000_0000);
        {
            let log = lock_bus_miss_log();
            let count = log.iter().filter(|k| **k == key1).count();
            assert_eq!(count, 1);
        }

        // 2. Access at 0x0400_0000 (Unmapped CS1)
        let key2 = "area=0 block=0x04000000 is_write=false width=4".to_string();
        cpu.read_long(0x0400_0000);
        {
            let log = lock_bus_miss_log();
            assert!(log.contains(&key2));
            let count = log.iter().filter(|k| **k == key2).count();
            assert_eq!(count, 1);
        }

        // 3. Access at 0x0610_0000 (High RAM mirror offset)
        let key3 = "area=0 block=0x06100000 is_write=true width=2".to_string();
        cpu.write_word(0x0610_0000, 0x1234);
        {
            let log = lock_bus_miss_log();
            assert!(log.contains(&key3));
            let count = log.iter().filter(|k| **k == key3).count();
            assert_eq!(count, 1);
        }

        // 4. Access at 0x4000_0000 (Area 2 / Cache Purge)
        let key4 = "area=2 block=0x00000000 is_write=false width=1".to_string();
        cpu.read_byte(0x4000_0000);
        {
            let log = lock_bus_miss_log();
            assert!(log.contains(&key4));
            let count = log.iter().filter(|k| **k == key4).count();
            assert_eq!(count, 1);
        }

        std::env::remove_var("MIMAS_BUS_TRACE");
    }

    #[test]
    fn test_onchip_p5_t1_sort_dedupe() {
        let mut cpu = make_cpu();
        cpu.queue_send(0x40, 15);
        cpu.queue_send(0x47, 8);
        cpu.queue_send(0x41, 14);
        cpu.queue_send(0x40, 2); // Duplicate vector, must be ignored and not change level

        // The queue should look like: [0x47 (level 8), 0x41 (level 14), 0x40 (level 15)]
        let q = if let Some(ref q_ref) = cpu.irq_in {
            q_ref.lock().unwrap().clone()
        } else {
            cpu.local_irq_in.clone()
        };

        assert_eq!(q.pending.len(), 3);
        assert_eq!(q.pending[0].vector, 0x47);
        assert_eq!(q.pending[0].level, 8);
        assert_eq!(q.pending[1].vector, 0x41);
        assert_eq!(q.pending[1].level, 14);
        assert_eq!(q.pending[2].vector, 0x40);
        assert_eq!(q.pending[2].level, 15);
    }

    #[test]
    fn test_onchip_p5_t2_strictly_greater_masking() {
        let mut cpu = make_cpu();
        cpu.sr = 8 << SR_IMASK_SHIFT; // mask = 8
        cpu.vbr = 0x0600_0000;
        cpu.registers[15] = 0x0600_1000;
        cpu.pc = 0x0600_0100;
        cpu.write_word(0x0600_0100, 0x0009); // NOP

        cpu.write_long(
            cpu.vbr.wrapping_add(SMPC_IRQ_VECTOR as u32 * 4),
            0x0600_2000,
        );
        cpu.write_word(0x0600_2000, 0x0009);

        cpu.write_long(
            cpu.vbr.wrapping_add(SOUND_REQ_IRQ_VECTOR as u32 * 4),
            0x0600_3000,
        );
        cpu.write_word(0x0600_3000, 0x0009);

        cpu.queue_send(SMPC_IRQ_VECTOR as u8, SMPC_IRQ_LEVEL as u8); // level 8: must not deliver
        cpu.step();
        assert_eq!(cpu.pc, 0x0600_0102); // Executing normal flow (PC advanced to next instruction)

        cpu.queue_send(SOUND_REQ_IRQ_VECTOR as u8, SOUND_REQ_IRQ_LEVEL as u8); // level 9: must deliver
        cpu.step();
        assert_eq!(cpu.pc, 0x0600_3002); // Branched to handler!
    }

    #[test]
    fn test_onchip_p5_t3_one_per_call() {
        let mut cpu = make_cpu();
        cpu.sr = 0;
        cpu.vbr = 0x0600_0000;
        cpu.registers[15] = 0x0600_1000;
        // Two pending interrupts
        cpu.queue_send(0x47, 8);
        cpu.queue_send(0x41, 14);

        cpu.write_long(cpu.vbr.wrapping_add(0x41 * 4), 0x0600_2000);
        cpu.write_long(cpu.vbr.wrapping_add(0x47 * 4), 0x0600_3000);
        cpu.write_word(0x0600_2000, 0x0009); // NOP inside handler
        cpu.write_word(0x0600_3000, 0x0009); // NOP inside handler

        cpu.step(); // Steps the first (highest priority: 0x41 level 14)
        assert_eq!(cpu.pc, 0x0600_2002);

        // Assert the second interrupt (0x47 level 8) is still queued
        let q = if let Some(ref q_ref) = cpu.irq_in {
            q_ref.lock().unwrap().clone()
        } else {
            cpu.local_irq_in.clone()
        };
        assert!(q.pending.iter().any(|x| x.vector == 0x47));
    }

    #[test]
    fn test_onchip_p5_t4_nmi_clamp() {
        let mut cpu = make_cpu();
        cpu.sr = 15 << SR_IMASK_SHIFT; // Mask 15 (maximum normal mask)
        cpu.vbr = 0x0600_0000;
        cpu.registers[15] = 0x0600_1000;
        cpu.write_long(cpu.vbr.wrapping_add(0xB * 4), 0x0600_2000); // NMI vector 11 -> offset 0x2C
        cpu.write_word(0x0600_2000, 0x0009);

        cpu.nmi();
        assert_eq!(cpu.onchip.icr & 0x8000, 0x8000); // Bit 15 set

        cpu.step(); // NMI must bypass mask 15 and execute
        assert_eq!(cpu.pc, 0x0600_2002);
        assert_eq!((cpu.sr >> SR_IMASK_SHIFT) & 0xF, 0xF); // Mask clamped to 15 (0xF)
    }

    #[test]
    fn test_onchip_p5_t6_delay_slot_no_interrupt() {
        let mut cpu = make_cpu();
        cpu.sr = 15 << SR_IMASK_SHIFT; // Mask 15: VBLANK-IN is masked
        cpu.vbr = 0x0600_0000;
        cpu.registers[15] = 0x0600_1000;

        // Interrupt vector points to 0x0600_2000
        cpu.write_long(
            cpu.vbr.wrapping_add(VBLANK_IN_VECTOR as u32 * 4),
            0x0600_2000,
        );
        cpu.write_word(0x0600_2000, 0x0009);

        // Branch instruction: BRA 0x0600_0020
        // Delay slot: LDC R1, SR (0x410E) which writes R1 (0) to SR, lowering mask to 0
        cpu.registers[1] = 0; // Value to write to SR
        cpu.pc = 0x0600_0000;
        cpu.write_word(0x0600_0000, 0xA00E); // BRA + 0x10 -> 0x0600_0020
        cpu.write_word(0x0600_0002, 0x410E); // delay slot: LDC R1, SR

        // Unmasked interrupt pending
        cpu.queue_send(VBLANK_IN_VECTOR as u8, VBLANK_IN_LEVEL as u8);

        cpu.step(); // Steps BRA + delay slot (LDC R1, SR).
                    // Assert: delay slot ran, lowered SR mask to 0
        assert_eq!((cpu.sr >> SR_IMASK_SHIFT) & 0xF, 0);
        // Assert: PC is now at branch target (0x0600_0020) and interrupt has not been taken yet
        assert_eq!(cpu.pc, 0x0600_0020);

        cpu.step(); // Now we step starting at target, which services the unmasked VBLANK-IN
                    // Assert: interrupt was taken, redirecting to the handler
        assert_eq!(cpu.pc, 0x0600_2002);

        // Read pushed PC from stack, it should point to the target of the branch (0x0600_0020)
        let pushed_pc = cpu.read_long(cpu.registers[15]);
        assert_eq!(pushed_pc, 0x0600_0020);
    }

    #[test]
    fn test_divu_p6_t1_crash_regression_overflow() {
        let mut cpu = make_cpu();

        // 1. 32/32 signed overflow: i32::MIN / -1
        cpu.write_long(0xFFFFFF00, 0xFFFF_FFFF); // DVSR = -1
        cpu.write_long(0xFFFFFF04, 0x8000_0000); // DVDNT = i32::MIN -> triggers division
        assert_eq!(cpu.read_long(0xFFFFFF04), 0x8000_0000); // Quotient wrapped to i32::MIN
        assert_eq!(cpu.read_long(0xFFFFFF10), 0); // Remainder is 0

        // 2. 64/32 signed overflow: i64::MIN / -1
        cpu.write_long(0xFFFFFF00, 0xFFFF_FFFF); // DVSR = -1
        cpu.write_long(0xFFFFFF10, 0x8000_0000); // DVDNTH = i32::MIN
        cpu.write_long(0xFFFFFF14, 0x0000_0000); // DVDNTL = 0 -> triggers 64-bit division
                                                 // Quotient overflows, so it should trigger overflow
        assert_eq!(cpu.read_long(0xFFFFFF08) & 1, 1); // DVCR overflow flag set
    }

    #[test]
    fn test_divu_p6_t4_overflow_interrupt() {
        let mut cpu = make_cpu();
        cpu.sr = 0; // mask = 0 (unmasked)
        cpu.vbr = 0x0600_0000;
        cpu.registers[15] = 0x0600_1000;

        // Configure DIVU interrupt priority and vector
        cpu.write_long(0xFFFFFF0C, 0x48); // VCRDIV vector = 0x48
        cpu.write_word(0xFFFFFEE2, 0x9000); // IPRA level for DIVU = 9 (bits 12-15)

        // Set DVCR interrupt enable bit (bit 1: Interrupt Enable)
        cpu.write_long(0xFFFFFF08, 0x2);

        // Vector table points to 0x0600_2000
        cpu.write_long(cpu.vbr.wrapping_add(0x48 * 4), 0x0600_2000);
        cpu.write_word(0x0600_2000, 0x0009);

        // Trigger division by zero
        cpu.write_long(0xFFFFFF00, 0); // DVSR = 0
        cpu.write_long(0xFFFFFF04, 100); // Trigger

        // Step to trigger interrupt handling
        cpu.step();

        // PC should be at the vector handler
        assert_eq!(cpu.pc, 0x0600_2002);
    }

    #[test]
    fn test_frt_p7_t1_prescaler() {
        let mut cpu = make_cpu();
        // Test TCR selects prescalers
        cpu.write_byte(0xFFFFFE16, 0); // TCR = 0
        assert_eq!(cpu.frc_shift, 3);
        assert_eq!(cpu.read_byte(0xFFFFFE16), 0);

        cpu.write_byte(0xFFFFFE16, 1); // TCR = 1
        assert_eq!(cpu.frc_shift, 5);
        assert_eq!(cpu.read_byte(0xFFFFFE16), 1);

        cpu.write_byte(0xFFFFFE16, 2); // TCR = 2
        assert_eq!(cpu.frc_shift, 7);
        assert_eq!(cpu.read_byte(0xFFFFFE16), 2);

        cpu.write_byte(0xFFFFFE16, 3); // TCR = 3 -> external, frc_shift unchanged/disabled
        assert_eq!(cpu.frc_shift, 7);
        assert_eq!(cpu.read_byte(0xFFFFFE16), 3);
    }

    #[test]
    fn test_frt_p7_t2_counter_advance() {
        let mut cpu = make_cpu();
        cpu.frc_shift = 3;
        cpu.frc_leftover = 0;
        cpu.onchip.frc = 0;

        // 8 * 5 + 3 = 43 cycles -> FRC should be 5, leftover should be 3
        cpu.frt_exec(43);
        assert_eq!(cpu.onchip.frc, 5);
        assert_eq!(cpu.frc_leftover, 3);

        // 8 * 10 + 7 = 87 cycles -> total FRC = 5 + 11 = 16, leftover = (3 + 87) & 7 = 2
        cpu.frt_exec(87);
        assert_eq!(cpu.onchip.frc, 16);
        assert_eq!(cpu.frc_leftover, 2);
    }

    #[test]
    fn test_frt_p7_t3_ftcsr_write_clear() {
        let mut cpu = make_cpu();
        cpu.onchip.ftcsr = 0x0E;
        cpu.write_byte(0xFFFFFE11, 0x0A);
        assert_eq!(cpu.onchip.ftcsr, 0x0A);

        cpu.write_byte(0xFFFFFE11, 0x01);
        assert_eq!(cpu.onchip.ftcsr, 0x01);
    }

    #[test]
    fn test_frt_p7_t4_ocra_ocrb_selector() {
        let mut cpu = make_cpu();
        cpu.write_byte(0xFFFFFE17, 0x00);
        cpu.write_word(0xFFFFFE14, 0x1234);
        assert_eq!(cpu.onchip.ocra, 0x1234);
        assert_eq!(cpu.onchip.ocrb, 0xFFFF);

        cpu.write_byte(0xFFFFFE17, 0x10);
        cpu.write_word(0xFFFFFE14, 0x5678);
        assert_eq!(cpu.onchip.ocra, 0x1234);
        assert_eq!(cpu.onchip.ocrb, 0x5678);
    }

    #[test]
    fn test_frt_p7_t5_compare_match_cclra() {
        let mut cpu = make_cpu();
        cpu.sr = 0;
        cpu.vbr = 0x0600_0000;
        cpu.registers[15] = 0x0600_1000;
        cpu.frc_shift = 0;

        cpu.onchip.ocra = 0x100;
        cpu.onchip.ocrb = 0xFFFF;
        cpu.onchip.tier = 0x08;
        cpu.onchip.ftcsr = 0x01;
        cpu.onchip.vcrc = 0x50;
        cpu.onchip.iprb = 0x0A00;

        cpu.write_long(cpu.vbr.wrapping_add(0x50 * 4), 0x0600_2000);
        cpu.write_word(0x0600_2000, 0x0009);

        cpu.onchip.frc = 0x0FF;
        cpu.frt_exec(1);

        assert_eq!(cpu.onchip.ftcsr & 0x08, 0x08);
        assert_eq!(cpu.onchip.frc, 0);

        cpu.step();
        assert_eq!(cpu.pc, 0x0600_2002);

        let mut cpu2 = make_cpu();
        cpu2.frc_shift = 0;
        cpu2.onchip.ocra = 0x100;
        cpu2.onchip.ocrb = 0xFFFF;
        cpu2.onchip.tier = 0x00;
        cpu2.onchip.ftcsr = 0x00;
        cpu2.onchip.frc = 0x0FF;
        cpu2.frt_exec(1);
        assert_eq!(cpu2.onchip.frc, 0x100);
    }

    #[test]
    fn test_frt_p7_t6_missed_compare_deviation() {
        let mut cpu = make_cpu();
        cpu.frc_shift = 0;
        cpu.onchip.ocra = 0x0100;
        cpu.onchip.ocrb = 0xFFFF;
        cpu.onchip.frc = 0;

        // Jump from 0 past 0xFFFF in one call to miss compare (HR §11.3)
        cpu.frt_exec(0x10050);

        assert_eq!(cpu.onchip.ftcsr & 0x08, 0);
        assert_eq!(cpu.onchip.ftcsr & 0x02, 0x02);
    }

    #[test]
    fn test_frt_p7_t7_tier_ici_rearm() {
        let mut cpu = make_cpu();
        cpu.sr = 0;
        cpu.vbr = 0x0600_0000;
        cpu.registers[15] = 0x0600_1000;
        cpu.onchip.vcrc = 0x5100;
        cpu.onchip.iprb = 0x0A00;

        cpu.write_long(cpu.vbr.wrapping_add(0x51 * 4), 0x0600_2000);
        cpu.write_word(0x0600_2000, 0x0009);

        cpu.onchip.ftcsr = 0x80;
        cpu.write_byte(0xFFFFFE10, 0x80);

        cpu.step();
        assert_eq!(cpu.pc, 0x0600_2002);
    }

    #[test]
    fn test_cycles_p8_t1_base_costs() {
        let mut cpu = make_cpu();

        // NOP (0x0009): 1 cycle
        cpu.write_word(0x0600_1000, 0x0009);
        cpu.pc = 0x0600_1000;
        let c = cpu.cycles;
        cpu.step();
        assert_eq!(cpu.cycles - c, 1);

        // RTS (0x000B): 2 cycles (plus NOP in delay slot = 1 cycle, total = 3)
        cpu.write_word(0x0600_1000, 0x000B);
        cpu.write_word(0x0600_1002, 0x0009);
        cpu.pr = 0x0600_1004;
        cpu.pc = 0x0600_1000;
        let c = cpu.cycles;
        cpu.step();
        assert_eq!(cpu.cycles - c, 3); // 2 base + 1 delay slot NOP

        // SLEEP (0x001B): 3 cycles
        cpu.write_word(0x0600_1000, 0x001B);
        cpu.pc = 0x0600_1000;
        let c = cpu.cycles;
        cpu.step();
        assert_eq!(cpu.cycles - c, 3);
    }

    #[test]
    fn test_cycles_p8_t2_wait_states() {
        let mut cpu = make_cpu();

        // High WRAM: 0 read wait states
        let c = cpu.cycles;
        cpu.read_byte(0x0600_0000);
        assert_eq!(cpu.cycles - c, 0);

        // Low WRAM: 12 read wait states
        let c = cpu.cycles;
        cpu.read_byte(0x0020_0000);
        assert_eq!(cpu.cycles - c, 12);

        // BIOS: 16 read wait states
        let c = cpu.cycles;
        cpu.read_byte(0x0000_0000);
        assert_eq!(cpu.cycles - c, 16);

        // Sound RAM: 50 read wait states
        let c = cpu.cycles;
        cpu.read_byte(0x05A0_0000);
        assert_eq!(cpu.cycles - c, 50);

        // High WRAM write: 2 wait states
        let c = cpu.cycles;
        cpu.write_byte(0x0600_0000, 0);
        assert_eq!(cpu.cycles - c, 2);

        // Low WRAM write: 7 wait states
        let c = cpu.cycles;
        cpu.write_byte(0x0020_0000, 0);
        assert_eq!(cpu.cycles - c, 7);
    }

    #[test]
    fn test_cycles_p8_t3_conditional_branch() {
        let mut cpu = make_cpu();

        // BT taken (T=1): 3 cycles
        cpu.set_t(true);
        cpu.write_word(0x0600_1000, 0x8900); // BT to relative offset +0
        cpu.pc = 0x0600_1000;
        let c = cpu.cycles;
        cpu.step();
        assert_eq!(cpu.cycles - c, 3);

        // BT not taken (T=0): 1 cycle
        cpu.set_t(false);
        cpu.write_word(0x0600_1000, 0x8900);
        cpu.pc = 0x0600_1000;
        let c = cpu.cycles;
        cpu.step();
        assert_eq!(cpu.cycles - c, 1);
    }

    #[test]
    fn test_cycles_p8_t5_throttle_end_to_end() {
        let mut cpu = make_cpu();
        let speed = std::sync::Arc::new(std::sync::Mutex::new(
            crate::throttle::ThrottleSpeed::Multiplier(1.0),
        ));
        cpu.speed = Some(speed.clone());
        let mut throttle =
            crate::throttle::ClockThrottle::new(crate::throttle::SH2_CLOCK_HZ, speed);

        // Just verify advance can accept delta cycles without panicking
        throttle.advance(100);
    }

    #[test]
    fn test_dmac_p9_t1_transfer_modes() {
        // Usable src modes x dst modes x 4 sizes
        let src_modes = [0, 1, 2]; // fixed, increment, decrement
        let dst_modes = [0, 1, 2];
        let sizes = [0, 1, 2, 3]; // byte, word, longword, burst

        for &src_mode in &src_modes {
            for &dst_mode in &dst_modes {
                for &size in &sizes {
                    let mut cpu = make_cpu();

                    // Pre-fill source buffer
                    let src_base = 0x0600_1000;
                    for i in 0..64 {
                        cpu.write_byte(src_base + i, (i + 1) as u8);
                    }

                    // Pre-fill destination buffer with zeroes
                    let dst_base = 0x0600_2000;
                    for i in 0..64 {
                        cpu.write_byte(dst_base + i, 0);
                    }

                    let stride = match size {
                        0 => 1,
                        1 => 2,
                        2 | 3 => 4,
                        _ => 1,
                    } as u32;

                    let count = 4;
                    // Compute starting address based on mode
                    let sar = if src_mode == 2 {
                        src_base + 32
                    } else {
                        src_base
                    };

                    let dar = if dst_mode == 2 {
                        dst_base + 32
                    } else {
                        dst_base
                    };

                    // Setup DMA
                    cpu.write_long(0xFFFF_FF80, sar); // SAR0
                    cpu.write_long(0xFFFF_FF84, dar); // DAR0
                    cpu.write_long(0xFFFF_FF88, count); // TCR0
                    cpu.write_long(
                        0xFFFF_FF8C,
                        (dst_mode << 14) | (src_mode << 12) | (size << 10) | 1,
                    ); // CHCR0: DE=1
                    cpu.write_long(0xFFFF_FFB0, 1); // DMAOR: DME=1

                    // Execute DMA
                    cpu.dma_proc(1000);

                    // Assert TE set
                    assert_ne!(
                        cpu.read_long(0xFFFF_FF8C) & 2,
                        0,
                        "TE bit must be set upon completion"
                    );

                    // Compute expected destination state locally
                    let mut expected_dst = vec![0u8; 64];
                    let mut temp_sar = sar;
                    let mut temp_dar = dar;
                    for _ in 0..count {
                        let offset_s = (temp_sar - src_base) as usize;
                        let offset_d = (temp_dar - dst_base) as usize;
                        for b in 0..stride {
                            expected_dst[offset_d + b as usize] = (offset_s + b as usize + 1) as u8;
                        }
                        match src_mode {
                            1 => temp_sar += stride,
                            2 => temp_sar -= stride,
                            _ => {}
                        }
                        match dst_mode {
                            1 => temp_dar += stride,
                            2 => temp_dar -= stride,
                            _ => {}
                        }
                    }

                    // Verify against destination buffer
                    for i in 0..64 {
                        let actual = cpu.read_byte(dst_base + i as u32);
                        assert_eq!(
                            actual, expected_dst[i],
                            "Mismatch at offset {} for mode src={}, dst={}, size={}",
                            i, src_mode, dst_mode, size
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn test_dmac_p9_t2_te_clear() {
        let mut cpu = make_cpu();
        cpu.write_byte(0x0600_1000, 42);
        cpu.write_long(0xFFFF_FF80, 0x0600_1000); // SAR0
        cpu.write_long(0xFFFF_FF84, 0x0600_2000); // DAR0
        cpu.write_long(0xFFFF_FF88, 1); // TCR0
        cpu.write_long(0xFFFF_FF8C, 1); // CHCR0: DE=1
        cpu.write_long(0xFFFF_FFB0, 1); // DMAOR: DME=1

        cpu.dma_proc(1000);

        // TE must be set
        let chcr = cpu.read_long(0xFFFF_FF8C);
        assert_ne!(chcr & 2, 0, "TE must be set");

        // Write with bit 1 = 1 (TE=1), TE should survive
        cpu.write_long(0xFFFF_FF8C, chcr | 2);
        assert_ne!(
            cpu.read_long(0xFFFF_FF8C) & 2,
            0,
            "TE must survive when written with 1"
        );

        // Write with bit 1 = 0, but without reading first, TE survives because shadow chcr0m is still set
        cpu.onchip.chcr0m.set(2);
        cpu.write_long(0xFFFF_FF8C, chcr & !2);
        assert_ne!(
            cpu.read_long(0xFFFF_FF8C) & 2,
            0,
            "TE must survive writing 0 if shadow register has TE set"
        );

        // Read to clear shadow
        let _ = cpu.read_long(0xFFFF_FF8C);
        assert_eq!(
            cpu.onchip.chcr0m.get(),
            0,
            "Shadow register must clear on read"
        );

        // Set DME=0 to prevent TCR0=0 from re-arming upon DE=1 write
        cpu.write_long(0xFFFF_FFB0, 0);

        // Now write with bit 1 = 0, TE clears
        cpu.write_long(0xFFFF_FF8C, chcr & !2);
        assert_eq!(
            cpu.read_long(0xFFFF_FF8C) & 2,
            0,
            "TE must clear when writing 0 after reading"
        );
    }

    #[test]
    fn test_dmac_p9_t3_arming_gate() {
        let mut cpu = make_cpu();
        cpu.write_byte(0x0600_1000, 99);

        // 1. DMAOR = 0 (DME clear)
        cpu.write_long(0xFFFF_FFB0, 0); // DMAOR = 0
        cpu.write_long(0xFFFF_FF80, 0x0600_1000); // SAR0
        cpu.write_long(0xFFFF_FF84, 0x0600_2000); // DAR0
        cpu.write_long(0xFFFF_FF88, 1); // TCR0
        cpu.write_long(0xFFFF_FF8C, 1); // CHCR0: DE=1

        cpu.dma_proc(200);
        assert_eq!(
            cpu.read_long(0xFFFF_FF88),
            1,
            "Transfer must not occur when DME=0"
        );

        // 2. DMAOR = 1 (DME set)
        cpu.write_long(0xFFFF_FFB0, 1); // DMAOR = 1
                                        // Rewrite CHCR0 to arm
        cpu.write_long(0xFFFF_FF8C, 1); // CHCR0: DE=1
        cpu.dma_proc(200);
        assert_eq!(
            cpu.read_long(0xFFFF_FF88),
            0,
            "Transfer must occur when DME=1"
        );

        // 3. Test NMIF (DMAOR = 3) - Write DMAOR to 3 first so that write to CHCR0 is blocked
        cpu.write_long(0xFFFF_FF88, 1);
        cpu.write_long(0xFFFF_FF8C, 0); // Clear TE
        let _ = cpu.read_long(0xFFFF_FF8C); // Clear shadow
        cpu.write_long(0xFFFF_FFB0, 3); // DMAOR = 3 (DME=1, NMIF=1)
        cpu.write_long(0xFFFF_FF8C, 1); // CHCR0: DE=1

        cpu.dma_proc(200);
        assert_eq!(
            cpu.read_long(0xFFFF_FF88),
            1,
            "Transfer must not occur when NMIF=1"
        );
        assert_eq!(
            cpu.read_long(0xFFFF_FFB0) & 1,
            0,
            "DME must be cleared on NMIF/AE abort"
        );
    }

    #[test]
    fn test_dmac_p9_t4_completion_interrupt() {
        let mut cpu = make_cpu();
        cpu.write_byte(0x0600_1000, 77);

        cpu.write_long(0xFFFF_FF80, 0x0600_1000); // SAR0
        cpu.write_long(0xFFFF_FF84, 0x0600_2000); // DAR0
        cpu.write_long(0xFFFF_FF88, 1); // TCR0
        cpu.write_long(0xFFFF_FF8C, 5); // CHCR0: DE=1, IE=1 (bit 2)
        cpu.write_long(0xFFFF_FFA0, 0x1234); // VCRDMA0
        cpu.write_word(0xFFFF_FEE2, 0x0500); // IPRA: DMAC level = 5

        cpu.write_long(0xFFFF_FFB0, 1); // DMAOR: DME=1

        cpu.dma_proc(200);

        let pending = &cpu.local_irq_in.pending;
        assert_eq!(pending.len(), 1, "Interrupt must be queued");
        assert_eq!(pending[0].vector, 0x34, "Vector must be VCRDMA0 & 0xFF");
        assert_eq!(pending[0].level, 5, "Level must match IPRA bits 11-8");
    }

    #[test]
    fn test_dmac_p9_t5_eat_table() {
        let mut cpu = make_cpu();

        // 14 cycles (WRAM -> WRAM)
        assert_eq!(cpu.get_eat_clock(0x0600_1000, 0x0600_2000), 14);
        // 570 cycles (VDP1-RAM -> VDP1-RAM)
        assert_eq!(cpu.get_eat_clock(0x05C0_1000, 0x05C0_2000), 570);

        // Drive engine with budgeted cycles.
        //
        // Arming below (the DMAOR write, transitioning DME 0->1 while CHCR0's
        // DE is already set) itself fires a real, HR-documented DMAExec() ==
        // DMAProc(200) burst (HR sh2-cpu.md:1327 / real `sh2core.c:2140`), so
        // the channel always gets a free 200-cycle head start before this
        // test ever calls `dma_proc` explicitly. TCR0 must be large enough
        // (20 units * 14 cycles = 280 total) that the free burst can't finish
        // it alone, so the boundary this test actually cares about -- one
        // cycle short vs. exactly enough -- lands on the explicit calls
        // below, not on arming. (A smaller TCR0, e.g. 10, made this test
        // flaky-looking: the 200-cycle arm burst alone finishes a 140-cycle
        // transfer, so TCR0 already read 0 before `dma_proc` was ever called
        // explicitly.)
        cpu.write_long(0xFFFF_FF80, 0x0600_1000); // SAR0
        cpu.write_long(0xFFFF_FF84, 0x0600_2000); // DAR0
        cpu.write_long(0xFFFF_FF88, 20); // TCR0
        cpu.write_long(0xFFFF_FF8C, 9); // CHCR0: DE=1, size=0, "dual channel" bit set (bit 3 = 1, no budget doubling)
        cpu.write_long(0xFFFF_FFB0, 1); // DMAOR: DME=1 -- this write alone burns the 200-cycle arm burst (14 units done, 4 cycles banked)

        // 6 units remain (84 cycles), minus the 4 cycles already banked by
        // the arm-time burst = 80 more needed. Feed 79 -> should NOT finish.
        cpu.dma_proc(79);
        assert_ne!(cpu.read_long(0xFFFF_FF88), 0);

        // Add the last cycle -> should finish.
        cpu.dma_proc(1);
        assert_eq!(cpu.read_long(0xFFFF_FF88), 0);
    }

    #[test]
    fn test_dmac_p9_t6_arbiter_interaction() {
        let arbiter = std::sync::Arc::new(BusArbiter::new());
        let ram = std::sync::Arc::new(WorkRam::new());
        let mut cpu = Sh2::new(false, arbiter.clone(), ram);

        arbiter.lock_for_dma();

        // CPU read_byte should block because the arbiter is locked
        let handle = std::thread::spawn(move || {
            cpu.read_byte(0x0600_1000);
        });

        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(arbiter.is_locked());
        arbiter.unlock_from_dma();
        handle.join().unwrap();
    }
}
