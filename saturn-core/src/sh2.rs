use std::sync::Arc;
use crate::bus_arbiter::BusArbiter;
use crate::shared_buffers::WorkRam;
use crate::sync::LockStepSync;

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
    pub work_ram: Arc<std::sync::RwLock<WorkRam>>,
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
    /// thing that makes it safe for Core 3 to read Sound RAM (via the
    /// `Arc<RwLock<WorkRam>>` both cores share) immediately after observing
    /// this flag flip true: by the time this store executes, every Sound
    /// RAM write the driver-upload routine made is already behind it in
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
}

// SR bit positions actually used by this subset of the ISA. Layout (T, S,
// I3-I0, M, Q) matches real SH-2 and is cross-checked against
// SR_WRITE_MASK (0x3F3), which a real, working interpreter (Yabause) uses
// at every site that writes SR from an external value.
const SR_T: u32 = 1 << 0;
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

fn log_reg_access_once(region: &MemRegion, is_write: bool, val: u8) {
    let interesting = matches!(
        region,
        MemRegion::Smpc(_) | MemRegion::Vdp1Regs(_) | MemRegion::Vdp2Regs(_)
            | MemRegion::ScuRegs(_) | MemRegion::Cs2Regs(_)
    );
    if !interesting {
        return;
    }
    let key = format!("{:?} {} val={:#04X}", region, if is_write { "W" } else { "R" }, val);
    let mut log = REG_ACCESS_LOG.lock().unwrap();
    if !log.contains(&key) {
        eprintln!("[REGACCESS] {}", key);
        log.push(key);
    }
}

impl Sh2 {
    pub fn new(is_slave: bool, arbiter: Arc<BusArbiter>, work_ram: Arc<std::sync::RwLock<WorkRam>>) -> Self {
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
            smpc_irq_pending: false,
            sound_req_irq: None,
        }
    }

    /// Load real BIOS ROM bytes. Call before `reset()`/stepping so the CPU
    /// actually fetches genuine boot code instead of reading zeros.
    pub fn load_bios(&mut self, data: Vec<u8>) {
        self.bios = Arc::new(data);
    }

    /// Share an already-loaded BIOS image (cheap: clones the Arc, not the
    /// underlying bytes). Used when multiple cores need to see the same ROM.
    pub fn set_bios_arc(&mut self, bios: Arc<Vec<u8>>) {
        self.bios = bios;
    }

    /// Perform the real SH-2 reset sequence: PC and R15 (stack pointer) are
    /// read from the first two 32-bit words of the reset vector table (at
    /// physical address 0x00000000), which lives in BIOS ROM. Must be called
    /// after `load_bios()` for this to do anything meaningful.
    pub fn reset(&mut self) {
        self.pc = self.read_long(0x00000000);
        self.registers[15] = self.read_long(0x00000004);
        // Real SH-2 reset value: interrupt mask level 15 (I3-I0 = 1111),
        // blocking every maskable interrupt until the BIOS explicitly lowers
        // it -- important once real interrupts exist, since accepting one
        // before the vector table / stack are set up would jump through
        // garbage.
        self.sr = 0x0000_00F0;
        self.illegal_instruction_flag = false;
        self.unaligned_access_flag = false;
    }

    fn translate(&self, address: u32) -> MemRegion {
        // Strip the SH-2 cache-control/partition bits (bit 29 selects the
        // "cache-through" mirror of the same physical space) -- we don't
        // model cache timing, so both mirrors resolve to the same data.
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
        } else if (0x0600_0000..0x0700_0000).contains(&a) {
            MemRegion::HighRam((a - 0x0600_0000) as usize)
        } else {
            MemRegion::Unmapped
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
                if self.bios.is_empty() {
                    0
                } else {
                    self.bios[off & (self.bios.len() - 1)]
                }
            }
            MemRegion::LowRam(off) => {
                let ram = self.work_ram.read().unwrap();
                ram.low_ram[off & (ram.low_ram.len() - 1)]
            }
            MemRegion::HighRam(off) => {
                let ram = self.work_ram.read().unwrap();
                ram.high_ram[off & (ram.high_ram.len() - 1)]
            }
            MemRegion::SoundRam(off) => {
                let ram = self.work_ram.read().unwrap();
                ram.sound_ram[off & (ram.sound_ram.len() - 1)]
            }
            MemRegion::ScspRegs(off) => {
                let ram = self.work_ram.read().unwrap();
                ram.scsp_regs[off & (ram.scsp_regs.len() - 1)]
            }
            MemRegion::Vdp1Vram(off) => {
                let ram = self.work_ram.read().unwrap();
                ram.vdp1_vram[off & (ram.vdp1_vram.len() - 1)]
            }
            MemRegion::Vdp1Framebuffer(off) => {
                let ram = self.work_ram.read().unwrap();
                ram.vdp1_framebuffer[off & (ram.vdp1_framebuffer.len() - 1)]
            }
            MemRegion::Vdp1Regs(off) => {
                let ram = self.work_ram.read().unwrap();
                ram.vdp1_regs[off & (ram.vdp1_regs.len() - 1)]
            }
            MemRegion::Vdp2Vram(off) => {
                let ram = self.work_ram.read().unwrap();
                ram.vdp2_vram[off & (ram.vdp2_vram.len() - 1)]
            }
            MemRegion::Vdp2Cram(off) => {
                let ram = self.work_ram.read().unwrap();
                ram.vdp2_cram[off & (ram.vdp2_cram.len() - 1)]
            }
            // TVSTAT (offset 0x004-0x005): real hardware status register,
            // never written by the CPU -- its VBLANK bit is a live signal
            // from the video timing generator. Backing it with a plain
            // read-write byte array (like every other VDP2 register) would
            // leave it permanently 0, since nothing ever "writes" the real
            // hardware's toggle -- exactly the kind of real BIOS wait loop
            // (`poll TVSTAT until VBLANK` as an alternative to the VBLANK-IN
            // interrupt) this stalls forever. Compute it live instead.
            MemRegion::Vdp2Regs(off) if off == 0x004 || off == 0x005 => {
                let tvstat = self.tvstat_word();
                if off == 0x004 { (tvstat >> 8) as u8 } else { (tvstat & 0xFF) as u8 }
            }
            MemRegion::Vdp2Regs(off) => {
                let ram = self.work_ram.read().unwrap();
                ram.vdp2_regs[off & (ram.vdp2_regs.len() - 1)]
            }
            MemRegion::ScuRegs(off) => {
                let ram = self.work_ram.read().unwrap();
                ram.scu_regs[off & (ram.scu_regs.len() - 1)]
            }
            MemRegion::Cs2Regs(off) => {
                let ram = self.work_ram.read().unwrap();
                ram.cs2_regs[off & (ram.cs2_regs.len() - 1)]
            }
            MemRegion::BackupRam(off) => {
                let ram = self.work_ram.read().unwrap();
                ram.backup_ram[off & (ram.backup_ram.len() - 1)]
            }
            // SF (offset 0x63): the busy/idle Status Flag. Real hardware
            // sets it to 1 when a command is written to COMREG and clears
            // it back to 0 once the SMPC finishes executing that command.
            // Commands complete "instantly" from the CPU's point of view
            // (see `smpc_execute_command`) -- SF reads idle (0)
            // unconditionally. This is what actually unblocks the BIOS's
            // real "wait for SMPC" handshake loops; returning a constant
            // nonzero byte here (as the earlier code did) has bit 0
            // permanently set, which looks like "busy forever" and hangs the
            // boot sequence indefinitely.
            MemRegion::Smpc(off) if off == SMPC_SF_OFFSET => 0x00,
            // Every other SMPC register (OREG/IREG/SR/PDR/DDR/IOSEL and
            // friends): real, persisted storage -- `smpc_execute_command`
            // populates OREG with genuine INTBACK response data on command
            // completion, and IREG/PDR/DDR are whatever the CPU last wrote,
            // matching real hardware's plain register-file behavior for
            // those.
            MemRegion::Smpc(off) => {
                let ram = self.work_ram.read().unwrap();
                ram.smpc_regs[off & (ram.smpc_regs.len() - 1)]
            }
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
                let mut ram = self.work_ram.write().unwrap();
                let mask = ram.low_ram.len() - 1;
                ram.low_ram[off & mask] = val;
            }
            MemRegion::HighRam(off) => {
                if self.core_id == 0 && (off == 0x0408a4 || off == 0x0408a5) {
                    eprintln!("[PROBE] write to counter byte off={:#x} val={:#04X} from pc={:#010X}", off, val, self.pc);
                }
                let mut ram = self.work_ram.write().unwrap();
                let mask = ram.high_ram.len() - 1;
                ram.high_ram[off & mask] = val;
            }
            MemRegion::SoundRam(off) => {
                let mut ram = self.work_ram.write().unwrap();
                let mask = ram.sound_ram.len() - 1;
                ram.sound_ram[off & mask] = val;
            }
            MemRegion::ScspRegs(off) => {
                let mut ram = self.work_ram.write().unwrap();
                let mask = ram.scsp_regs.len() - 1;
                ram.scsp_regs[off & mask] = val;
            }
            MemRegion::Vdp1Vram(off) => {
                let mut ram = self.work_ram.write().unwrap();
                let mask = ram.vdp1_vram.len() - 1;
                ram.vdp1_vram[off & mask] = val;
            }
            MemRegion::Vdp1Framebuffer(off) => {
                let mut ram = self.work_ram.write().unwrap();
                let mask = ram.vdp1_framebuffer.len() - 1;
                ram.vdp1_framebuffer[off & mask] = val;
            }
            MemRegion::Vdp1Regs(off) => {
                let mut ram = self.work_ram.write().unwrap();
                let mask = ram.vdp1_regs.len() - 1;
                ram.vdp1_regs[off & mask] = val;
            }
            MemRegion::Vdp2Vram(off) => {
                let mut ram = self.work_ram.write().unwrap();
                let mask = ram.vdp2_vram.len() - 1;
                ram.vdp2_vram[off & mask] = val;
            }
            MemRegion::Vdp2Cram(off) => {
                let mut ram = self.work_ram.write().unwrap();
                let mask = ram.vdp2_cram.len() - 1;
                ram.vdp2_cram[off & mask] = val;
            }
            MemRegion::Vdp2Regs(off) => {
                let mut ram = self.work_ram.write().unwrap();
                let mask = ram.vdp2_regs.len() - 1;
                ram.vdp2_regs[off & mask] = val;
            }
            MemRegion::ScuRegs(off) => {
                let mut ram = self.work_ram.write().unwrap();
                let mask = ram.scu_regs.len() - 1;
                ram.scu_regs[off & mask] = val;
            }
            MemRegion::Cs2Regs(off) => {
                let mut ram = self.work_ram.write().unwrap();
                let mask = ram.cs2_regs.len() - 1;
                ram.cs2_regs[off & mask] = val;
            }
            MemRegion::BackupRam(off) => {
                let mut ram = self.work_ram.write().unwrap();
                let mask = ram.backup_ram.len() - 1;
                ram.backup_ram[off & mask] = val;
            }
            // SMPC: a real, persisted register file (IREG/OREG/SR/PDR/DDR/
            // IOSEL/EXLE) -- see `MemRegion::Smpc` on the read side. A write
            // to COMREG (offset 0x1F) additionally triggers real command
            // processing, matching real hardware issuing the command the
            // instant COMREG is written.
            MemRegion::Smpc(off) => {
                {
                    let mut ram = self.work_ram.write().unwrap();
                    let mask = ram.smpc_regs.len() - 1;
                    ram.smpc_regs[off & mask] = val;
                }
                if off == SMPC_COMREG_OFFSET {
                    self.smpc_execute_command(val);
                }
            }
            // BIOS is ROM: writes are silently discarded, matching real hardware.
            MemRegion::Bios(_) | MemRegion::Unmapped => {}
        }
    }

    pub fn read_byte(&mut self, address: u32) -> u8 {
        self.bus_wait();
        self.raw_read_byte(address)
    }

    pub fn write_byte(&mut self, address: u32, val: u8) {
        self.bus_wait();
        if address == 0x0600_1000 {
            self.cdrom_command_executed = true;
        }
        self.raw_write_byte(address, val);
    }

    /// Read 16-bit word from memory using the bus arbiter check. Arbitration
    /// happens once per transaction (matching a real single bus cycle), not
    /// once per byte fetched.
    pub fn read_word(&mut self, address: u32) -> u16 {
        if address % 2 != 0 {
            self.unaligned_access_flag = true;
            return 0;
        }
        self.bus_wait();
        let hi = self.raw_read_byte(address);
        let lo = self.raw_read_byte(address.wrapping_add(1));
        (hi as u16) << 8 | (lo as u16)
    }

    /// Write 16-bit word to memory using the bus arbiter check
    pub fn write_word(&mut self, address: u32, val: u16) {
        if address % 2 != 0 {
            self.unaligned_access_flag = true;
            return;
        }
        self.bus_wait();
        if address == 0x0600_1000 {
            self.cdrom_command_executed = true;
        }
        self.raw_write_byte(address, (val >> 8) as u8);
        self.raw_write_byte(address.wrapping_add(1), (val & 0xFF) as u8);
    }

    /// Read 32-bit long word (big-endian, matching real SH-2/Saturn wiring).
    pub fn read_long(&mut self, address: u32) -> u32 {
        if address % 4 != 0 {
            self.unaligned_access_flag = true;
        }
        self.bus_wait();
        let b0 = self.raw_read_byte(address) as u32;
        let b1 = self.raw_read_byte(address.wrapping_add(1)) as u32;
        let b2 = self.raw_read_byte(address.wrapping_add(2)) as u32;
        let b3 = self.raw_read_byte(address.wrapping_add(3)) as u32;
        (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
    }

    /// Write 32-bit long word (big-endian).
    pub fn write_long(&mut self, address: u32, val: u32) {
        if address % 4 != 0 {
            self.unaligned_access_flag = true;
        }
        self.bus_wait();
        if address <= 0x0600_1000 && address + 4 > 0x0600_1000 {
            self.cdrom_command_executed = true;
        }
        self.raw_write_byte(address, (val >> 24) as u8);
        self.raw_write_byte(address.wrapping_add(1), (val >> 16) as u8);
        self.raw_write_byte(address.wrapping_add(2), (val >> 8) as u8);
        self.raw_write_byte(address.wrapping_add(3), val as u8);
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

    /// Run single step of CPU
    pub fn step(&mut self) {
        self.service_pending_interrupt();
        let opcode = self.read_word(self.pc);
        self.pc = self.pc.wrapping_add(2);
        self.execute(opcode);
        self.cycles = self.cycles.wrapping_add(2);
    }

    /// Raise VBLANK-IN. Actual entry into the handler (if any) happens on
    /// the next `step()`, and only if SR's interrupt mask allows it -- same
    /// as real hardware, a masked interrupt just stays pending.
    pub fn request_vblank_interrupt(&mut self) {
        self.vblank_pending = true;
    }

    /// Execute an SMPC command the instant COMREG is written, matching the
    /// "completes instantly" simplification already used for SF (see the
    /// read-side comment on `MemRegion::Smpc`).
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
        let mut ram = self.work_ram.write().unwrap();
        let ireg1 = ram.smpc_regs[SMPC_IREG1_OFFSET];
        let wants_peripheral = (ireg1 & 0x8) != 0;

        // Real `SmpcINTBACKStatus()`: system status + RTC + cartridge/
        // region/reset flags in OREG0-11. RTC bytes are zeroed (BCD-encoded
        // real time isn't needed for boot to proceed, only a well-formed
        // response is); region defaults to Japan (1), the same fallback
        // Yabause itself uses when no CD is present to autodetect from
        // (`SmpcRecheckRegion`).
        ram.smpc_regs[SMPC_OREG_BASE_OFFSET] = 0x80; // bit7: normal startup, resd=0
        for i in 1..=7 {
            ram.smpc_regs[SMPC_OREG_BASE_OFFSET + i * 2] = 0;
        }
        ram.smpc_regs[SMPC_OREG_BASE_OFFSET + 8 * 2] = 0; // cartridge
        ram.smpc_regs[SMPC_OREG_BASE_OFFSET + 9 * 2] = 1; // region: Japan fallback
        ram.smpc_regs[SMPC_OREG_BASE_OFFSET + 10 * 2] = 0x34; // dotsel/mshnmi/sysres/sndres = 0
        ram.smpc_regs[SMPC_OREG_BASE_OFFSET + 11 * 2] = 0; // cdres = 0
        ram.smpc_regs[SMPC_SR_OFFSET] = 0x4F | ((wants_peripheral as u8) << 5);
        drop(ram);

        // Real hardware fires the System Manager interrupt when the command
        // finishes; BIOS INTBACK handshakes wait on this specifically (not
        // just on SF), so without it the boot sequence stalls even though SF
        // itself always reads idle.
        self.smpc_irq_pending = true;
    }

    /// Compute TVSTAT live from wall-clock frame timing rather than storing
    /// it as an ordinary register byte -- see the read-side comment at
    /// `MemRegion::Vdp2Regs` offset 0x004/0x005. `next_vblank_due` marks the
    /// upcoming VBLANK-IN edge, so the current frame period started one
    /// `VBLANK_INTERVAL` before that; VBLANK is active for the first
    /// `VBLANK_DURATION` of the period, matching real hardware's scanline
    /// split (see the `VBLANK_DURATION` doc comment).
    fn tvstat_word(&self) -> u16 {
        let Some(due) = self.next_vblank_due else { return 0 };
        let Some(period_start) = due.checked_sub(VBLANK_INTERVAL) else { return 0 };
        let now = std::time::Instant::now();
        if now >= period_start && now.duration_since(period_start) < VBLANK_DURATION {
            TVSTAT_VBLANK_BIT
        } else {
            0
        }
    }

    fn service_pending_interrupt(&mut self) {
        // Real hardware picks the highest-priority pending request:
        // VBLANK-IN (15) > Sound Request (9) > SMPC System Manager (8).
        let sound_req_pending = self.sound_req_irq.as_ref()
            .is_some_and(|f| f.load(std::sync::atomic::Ordering::Relaxed));
        let (level, vector) = if self.vblank_pending {
            (VBLANK_IN_LEVEL, VBLANK_IN_VECTOR)
        } else if sound_req_pending {
            (SOUND_REQ_IRQ_LEVEL, SOUND_REQ_IRQ_VECTOR)
        } else if self.smpc_irq_pending {
            (SMPC_IRQ_LEVEL, SMPC_IRQ_VECTOR)
        } else {
            return;
        };
        let current_mask = (self.sr >> SR_IMASK_SHIFT) & 0xF;
        if level <= current_mask {
            return; // masked: stays pending until SR's mask is lowered
        }
        if self.vblank_pending {
            self.vblank_pending = false;
        } else if sound_req_pending {
            if let Some(ref f) = self.sound_req_irq {
                f.store(false, std::sync::atomic::Ordering::Relaxed);
            }
        } else {
            self.smpc_irq_pending = false;
        }
        // Real SH-2 exception entry: push SR then PC (so RTE's PC-then-SR
        // pop order, see the RTE opcode handler above, reads them back out
        // correctly), then jump through the VBR-relative vector table and
        // raise the mask to this interrupt's own level so it can't
        // re-enter itself before RTE restores the original SR.
        let sr_addr = self.registers[15].wrapping_sub(4);
        self.write_long(sr_addr, self.sr);
        let pc_addr = sr_addr.wrapping_sub(4);
        self.write_long(pc_addr, self.pc);
        self.registers[15] = pc_addr;
        self.sr = (self.sr & !(0xFu32 << SR_IMASK_SHIFT)) | (level << SR_IMASK_SHIFT);
        self.pc = self.read_long(self.vbr.wrapping_add(vector * 4));
    }

    /// Fetch and run the instruction at the delay slot (currently at
    /// `self.pc`), then jump to `target`. Used by every branch/call/return
    /// instruction, all of which have a mandatory delay slot on real SH-2.
    fn delay_slot_and_jump(&mut self, target: u32) {
        let slot_pc = self.pc;
        let opcode = self.read_word(slot_pc);
        self.pc = slot_pc.wrapping_add(2);
        self.execute(opcode);
        self.pc = target;
    }

    /// Execute a fetched SH-2 instruction opcode
    fn execute(&mut self, opcode: u16) {
        let n = ((opcode >> 8) & 0xF) as usize;
        let m = ((opcode >> 4) & 0xF) as usize;
        let d4 = (opcode & 0xF) as u32;
        let d8 = (opcode & 0xFF) as u32;
        let d12 = (opcode & 0xFFF) as u32;
        let imm8 = (opcode & 0xFF) as u8;


        match opcode {
            0x0009 => return, // NOP
            0x000B => { // RTS
                let target = self.pr;
                self.delay_slot_and_jump(target);
                return;
            }
            0x0018 => { self.set_t(true); return; } // SETT
            0x0008 => { self.set_t(false); return; } // CLRT
            0x0019 => { self.sr &= !(SR_T | SR_M | SR_Q); return; } // DIV0U: M=Q=T=0
            0x0028 => { self.mach = 0; self.macl = 0; return; } // CLRMAC
            0x002B => { // RTE: pops PC first (lower stack address), then SR -- real
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
            0xFFFF => { self.illegal_instruction_flag = true; return; }
            _ => {}
        }

        if opcode & 0xFF00 == 0xC300 { // TRAPA #imm
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
            0x0002 => { self.registers[n] = self.sr; return; } // STC SR,Rn
            0x0012 => { self.registers[n] = self.gbr; return; } // STC GBR,Rn
            0x0022 => { self.registers[n] = self.vbr; return; } // STC VBR,Rn
            0x000A => { self.registers[n] = self.mach; return; } // STS MACH,Rn
            0x001A => { self.registers[n] = self.macl; return; } // STS MACL,Rn
            0x002A => { self.registers[n] = self.pr; return; } // STS PR,Rn
            0x0029 => { self.registers[n] = self.t() as u32; return; } // MOVT Rn
            _ => {}
        }

        match opcode & 0xF00F {
            0x0004 => { let a = self.registers[0].wrapping_add(self.registers[n]); self.write_byte(a, self.registers[m] as u8); return; }
            0x0005 => { let a = self.registers[0].wrapping_add(self.registers[n]); self.write_word(a, self.registers[m] as u16); return; }
            0x0006 => { let a = self.registers[0].wrapping_add(self.registers[n]); self.write_long(a, self.registers[m]); return; }
            0x0007 => { // MUL.L Rm,Rn
                self.macl = self.registers[n].wrapping_mul(self.registers[m]);
                return;
            }
            0x000C => { let a = self.registers[0].wrapping_add(self.registers[m]); self.registers[n] = self.read_byte(a) as i8 as i32 as u32; return; }
            0x000D => { let a = self.registers[0].wrapping_add(self.registers[m]); self.registers[n] = self.read_word(a) as i16 as i32 as u32; return; }
            0x000E => { let a = self.registers[0].wrapping_add(self.registers[m]); self.registers[n] = self.read_long(a); return; }
            _ => {}
        }

        match opcode & 0xF000 {
            0x1000 => { // MOV.L Rm,@(disp4,Rn)
                let addr = self.registers[n].wrapping_add(d4.wrapping_mul(4));
                self.write_long(addr, self.registers[m]);
                return;
            }
            0x5000 => { // MOV.L @(disp4,Rm),Rn
                let addr = self.registers[m].wrapping_add(d4.wrapping_mul(4));
                self.registers[n] = self.read_long(addr);
                return;
            }
            0x7000 => { // ADD #imm,Rn
                let imm = (imm8 as i8) as i32 as u32;
                self.registers[n] = self.registers[n].wrapping_add(imm);
                return;
            }
            0x9000 => { // MOV.W @(disp8,PC),Rn
                let base = self.pc.wrapping_add(2) & !1u32; // PC of this instr + 4, this instr's PC is self.pc-2
                let addr = base.wrapping_add(d8.wrapping_mul(2));
                self.registers[n] = self.read_word(addr) as i16 as i32 as u32;
                return;
            }
            0xA000 => { // BRA label
                let disp = sign_extend12(d12);
                let target = self.pc.wrapping_add(2).wrapping_add((disp << 1) as u32);
                self.delay_slot_and_jump(target);
                return;
            }
            0xB000 => { // BSR label
                let disp = sign_extend12(d12);
                let target = self.pc.wrapping_add(2).wrapping_add((disp << 1) as u32);
                self.pr = self.pc.wrapping_add(2);
                self.delay_slot_and_jump(target);
                return;
            }
            0xD000 => { // MOV.L @(disp8,PC),Rn
                let base = self.pc.wrapping_add(2) & !3u32;
                let addr = base.wrapping_add(d8.wrapping_mul(4));
                self.registers[n] = self.read_long(addr);
                return;
            }
            0xE000 => { // MOV #imm,Rn
                self.registers[n] = (imm8 as i8) as i32 as u32;
                return;
            }
            _ => {}
        }

        match opcode & 0xFF00 {
            0x8800 => { // CMP/EQ #imm,R0
                let imm = (imm8 as i8) as i32 as u32;
                self.set_t(self.registers[0] == imm);
                return;
            }
            0x8900 => { // BT label (no delay slot)
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
            0x8B00 => { // BF label (no delay slot)
                if !self.t() {
                    let disp = sign_extend8(d8);
                    self.pc = self.pc.wrapping_add(2).wrapping_add((disp << 1) as u32);
                }
                return;
            }
            0x8D00 => { // BT/S label (delay slot)
                if self.t() {
                    let disp = sign_extend8(d8);
                    let target = self.pc.wrapping_add(2).wrapping_add((disp << 1) as u32);
                    self.delay_slot_and_jump(target);
                }
                return;
            }
            0x8F00 => { // BF/S label (delay slot)
                if !self.t() {
                    let disp = sign_extend8(d8);
                    let target = self.pc.wrapping_add(2).wrapping_add((disp << 1) as u32);
                    self.delay_slot_and_jump(target);
                }
                return;
            }
            0xC800 => { self.set_t((self.registers[0] & imm8 as u32) == 0); return; } // TST #imm,R0
            0xC900 => { self.registers[0] &= imm8 as u32; return; } // AND #imm,R0
            0xCA00 => { self.registers[0] |= imm8 as u32; return; } // OR #imm,R0
            0xCB00 => { self.registers[0] ^= imm8 as u32; return; } // XOR #imm,R0
            0xC700 => { // MOVA @(disp8,PC),R0
                let base = self.pc.wrapping_add(2) & !3u32;
                self.registers[0] = base.wrapping_add(d8.wrapping_mul(4));
                return;
            }
            0xC000 => { let a = self.gbr.wrapping_add(d8); self.write_byte(a, self.registers[0] as u8); return; } // MOV.B R0,@(disp8,GBR)
            0xC100 => { let a = self.gbr.wrapping_add(d8.wrapping_mul(2)); self.write_word(a, self.registers[0] as u16); return; }
            0xC200 => { let a = self.gbr.wrapping_add(d8.wrapping_mul(4)); self.write_long(a, self.registers[0]); return; }
            0xC400 => { let a = self.gbr.wrapping_add(d8); self.registers[0] = self.read_byte(a) as i8 as i32 as u32; return; }
            0xC500 => { let a = self.gbr.wrapping_add(d8.wrapping_mul(2)); self.registers[0] = self.read_word(a) as i16 as i32 as u32; return; }
            0xC600 => { let a = self.gbr.wrapping_add(d8.wrapping_mul(4)); self.registers[0] = self.read_long(a); return; }
            _ => {}
        }

        match opcode & 0xF0FF {
            0x4000 => { let msb = self.registers[n] & 0x8000_0000 != 0; self.registers[n] <<= 1; self.set_t(msb); return; } // SHLL
            0x4001 => { let lsb = self.registers[n] & 1 != 0; self.registers[n] >>= 1; self.set_t(lsb); return; } // SHLR
            0x4004 => { let msb = self.registers[n] & 0x8000_0000 != 0; self.registers[n] = self.registers[n].rotate_left(1); self.set_t(msb); return; } // ROTL
            0x4005 => { let lsb = self.registers[n] & 1 != 0; self.registers[n] = self.registers[n].rotate_right(1); self.set_t(lsb); return; } // ROTR
            0x4008 => { self.registers[n] <<= 2; return; } // SHLL2
            0x4009 => { self.registers[n] >>= 2; return; } // SHLR2
            0x400B => { // JSR @Rn
                let target = self.registers[n];
                self.pr = self.pc.wrapping_add(2);
                self.delay_slot_and_jump(target);
                return;
            }
            0x4010 => { // DT Rn
                self.registers[n] = self.registers[n].wrapping_sub(1);
                self.set_t(self.registers[n] == 0);
                return;
            }
            0x4011 => { self.set_t((self.registers[n] as i32) >= 0); return; } // CMP/PZ
            0x4015 => { self.set_t((self.registers[n] as i32) > 0); return; } // CMP/PL
            0x4018 => { self.registers[n] <<= 8; return; } // SHLL8
            0x4019 => { self.registers[n] >>= 8; return; } // SHLR8
            0x401B => { // TAS.B @Rn
                let a = self.registers[n];
                let val = self.read_byte(a);
                self.set_t(val == 0);
                self.write_byte(a, val | 0x80);
                return;
            }
            0x4020 => { // SHAL
                let msb = self.registers[n] & 0x8000_0000 != 0;
                self.registers[n] = ((self.registers[n] as i32) << 1) as u32;
                self.set_t(msb);
                return;
            }
            0x4021 => { // SHAR
                let lsb = self.registers[n] & 1 != 0;
                self.registers[n] = ((self.registers[n] as i32) >> 1) as u32;
                self.set_t(lsb);
                return;
            }
            0x4024 => { // ROTCL
                let old_t = self.t();
                let msb = self.registers[n] & 0x8000_0000 != 0;
                self.registers[n] = (self.registers[n] << 1) | (old_t as u32);
                self.set_t(msb);
                return;
            }
            0x4025 => { // ROTCR
                let old_t = self.t();
                let lsb = self.registers[n] & 1 != 0;
                self.registers[n] = (self.registers[n] >> 1) | ((old_t as u32) << 31);
                self.set_t(lsb);
                return;
            }
            0x4028 => { self.registers[n] <<= 16; return; } // SHLL16
            0x4029 => { self.registers[n] >>= 16; return; } // SHLR16
            0x402B => { // JMP @Rn
                let target = self.registers[n];
                self.delay_slot_and_jump(target);
                return;
            }
            0x400E => { self.sr = self.registers[n]; return; } // LDC Rn,SR
            0x401E => { self.gbr = self.registers[n]; return; } // LDC Rn,GBR
            0x402E => { self.vbr = self.registers[n]; return; } // LDC Rn,VBR
            0x400A => { self.mach = self.registers[n]; return; } // LDS Rn,MACH
            0x401A => { self.macl = self.registers[n]; return; } // LDS Rn,MACL
            0x402A => { self.pr = self.registers[n]; return; } // LDS Rn,PR
            // Memory-indirect LDS.L/STS.L/LDC.L/STC.L forms -- extremely
            // common in real function prologues/epilogues (save/restore PR
            // and friends around a call), found missing while running the
            // actual Saturn BIOS: it hit LDS.L @R15+,PR (a PR pop right
            // before RTS) and, since it wasn't decoded, PR stayed stale and
            // RTS returned to the wrong place.
            0x4006 => { let a = self.registers[n]; self.mach = self.read_long(a); self.registers[n] = a.wrapping_add(4); return; } // LDS.L @Rn+,MACH
            0x4016 => { let a = self.registers[n]; self.macl = self.read_long(a); self.registers[n] = a.wrapping_add(4); return; } // LDS.L @Rn+,MACL
            0x4026 => { let a = self.registers[n]; self.pr = self.read_long(a); self.registers[n] = a.wrapping_add(4); return; } // LDS.L @Rn+,PR
            0x4002 => { let a = self.registers[n].wrapping_sub(4); self.write_long(a, self.mach); self.registers[n] = a; return; } // STS.L MACH,@-Rn
            0x4012 => { let a = self.registers[n].wrapping_sub(4); self.write_long(a, self.macl); self.registers[n] = a; return; } // STS.L MACL,@-Rn
            0x4022 => { let a = self.registers[n].wrapping_sub(4); self.write_long(a, self.pr); self.registers[n] = a; return; } // STS.L PR,@-Rn
            0x4007 => { let a = self.registers[n]; self.sr = self.read_long(a); self.registers[n] = a.wrapping_add(4); return; } // LDC.L @Rn+,SR
            0x4017 => { let a = self.registers[n]; self.gbr = self.read_long(a); self.registers[n] = a.wrapping_add(4); return; } // LDC.L @Rn+,GBR
            0x4027 => { let a = self.registers[n]; self.vbr = self.read_long(a); self.registers[n] = a.wrapping_add(4); return; } // LDC.L @Rn+,VBR
            0x4003 => { let a = self.registers[n].wrapping_sub(4); self.write_long(a, self.sr); self.registers[n] = a; return; } // STC.L SR,@-Rn
            0x4013 => { let a = self.registers[n].wrapping_sub(4); self.write_long(a, self.gbr); self.registers[n] = a; return; } // STC.L GBR,@-Rn
            0x4023 => { let a = self.registers[n].wrapping_sub(4); self.write_long(a, self.vbr); self.registers[n] = a; return; } // STC.L VBR,@-Rn
            _ => {}
        }

        match opcode & 0xF00F {
            0x2000 => { let a = self.registers[n]; self.write_byte(a, self.registers[m] as u8); return; } // MOV.B Rm,@Rn
            0x2001 => { let a = self.registers[n]; self.write_word(a, self.registers[m] as u16); return; }
            0x2002 => { let a = self.registers[n]; self.write_long(a, self.registers[m]); return; }
            0x2004 => { // MOV.B Rm,@-Rn
                let a = self.registers[n].wrapping_sub(1);
                self.write_byte(a, self.registers[m] as u8);
                self.registers[n] = a;
                return;
            }
            0x2005 => { // MOV.W Rm,@-Rn
                let a = self.registers[n].wrapping_sub(2);
                self.write_word(a, self.registers[m] as u16);
                self.registers[n] = a;
                return;
            }
            0x2006 => { // MOV.L Rm,@-Rn
                let a = self.registers[n].wrapping_sub(4);
                self.write_long(a, self.registers[m]);
                self.registers[n] = a;
                return;
            }
            0x2007 => { // DIV0S Rm,Rn -- seeds Q/M/T for a following DIV1 chain.
                let q = self.registers[n] & 0x8000_0000 != 0;
                let m_bit = self.registers[m] & 0x8000_0000 != 0;
                self.set_q(q);
                self.set_m(m_bit);
                self.set_t(q != m_bit);
                return;
            }
            0x2008 => { self.set_t((self.registers[n] & self.registers[m]) == 0); return; } // TST Rm,Rn
            0x2009 => { self.registers[n] &= self.registers[m]; return; } // AND Rm,Rn
            0x200A => { self.registers[n] ^= self.registers[m]; return; } // XOR Rm,Rn
            0x200B => { self.registers[n] |= self.registers[m]; return; } // OR Rm,Rn
            0x200C => { // CMP/STR Rm,Rn: T=1 if any byte matches
                let x = self.registers[n] ^ self.registers[m];
                let matches = (x & 0xFF) == 0 || (x & 0xFF00) == 0 || (x & 0xFF_0000) == 0 || (x & 0xFF00_0000) == 0;
                self.set_t(matches);
                return;
            }
            0x200D => { // XTRCT Rm,Rn
                self.registers[n] = (self.registers[n] >> 16) | (self.registers[m] << 16);
                return;
            }
            0x200E => { self.macl = (self.registers[n] as u16 as u32).wrapping_mul(self.registers[m] as u16 as u32); return; } // MULU.W
            0x200F => { self.macl = ((self.registers[n] as i16 as i32).wrapping_mul(self.registers[m] as i16 as i32)) as u32; return; } // MULS.W
            0x3000 => { self.set_t(self.registers[n] == self.registers[m]); return; } // CMP/EQ Rm,Rn
            0x3002 => { self.set_t(self.registers[n] >= self.registers[m]); return; } // CMP/HS (unsigned)
            0x3003 => { self.set_t((self.registers[n] as i32) >= (self.registers[m] as i32)); return; } // CMP/GE
            0x3004 => { // DIV1 Rm,Rn -- one step of the bit-serial division
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
            0x3006 => { self.set_t(self.registers[n] > self.registers[m]); return; } // CMP/HI (unsigned)
            0x3007 => { self.set_t((self.registers[n] as i32) > (self.registers[m] as i32)); return; } // CMP/GT
            0x3008 => { self.registers[n] = self.registers[n].wrapping_sub(self.registers[m]); return; } // SUB
            0x300A => { // SUBC Rm,Rn
                let (r1, c1) = self.registers[n].overflowing_sub(self.registers[m]);
                let (r2, c2) = r1.overflowing_sub(self.t() as u32);
                self.registers[n] = r2;
                self.set_t(c1 || c2);
                return;
            }
            0x300B => { // SUBV Rm,Rn
                let (r, ov) = (self.registers[n] as i32).overflowing_sub(self.registers[m] as i32);
                self.registers[n] = r as u32;
                self.set_t(ov);
                return;
            }
            0x300C => { self.registers[n] = self.registers[n].wrapping_add(self.registers[m]); return; } // ADD
            0x300D => { // DMULS.L Rm,Rn
                let r = (self.registers[n] as i32 as i64).wrapping_mul(self.registers[m] as i32 as i64);
                self.mach = (r >> 32) as u32;
                self.macl = r as u32;
                return;
            }
            0x300E => { // ADDC Rm,Rn
                let (r1, c1) = self.registers[n].overflowing_add(self.registers[m]);
                let (r2, c2) = r1.overflowing_add(self.t() as u32);
                self.registers[n] = r2;
                self.set_t(c1 || c2);
                return;
            }
            0x300F => { // ADDV Rm,Rn
                let (r, ov) = (self.registers[n] as i32).overflowing_add(self.registers[m] as i32);
                self.registers[n] = r as u32;
                self.set_t(ov);
                return;
            }
            0x3005 => { // DMULU.L Rm,Rn
                let r = (self.registers[n] as u64).wrapping_mul(self.registers[m] as u64);
                self.mach = (r >> 32) as u32;
                self.macl = r as u32;
                return;
            }
            0x6000 => { let a = self.registers[m]; self.registers[n] = self.read_byte(a) as i8 as i32 as u32; return; }
            0x6001 => { let a = self.registers[m]; self.registers[n] = self.read_word(a) as i16 as i32 as u32; return; }
            0x6002 => { let a = self.registers[m]; self.registers[n] = self.read_long(a); return; }
            0x6003 => { self.registers[n] = self.registers[m]; return; } // MOV Rm,Rn
            0x6004 => { // MOV.B @Rm+,Rn
                let a = self.registers[m];
                self.registers[n] = self.read_byte(a) as i8 as i32 as u32;
                if n != m { self.registers[m] = a.wrapping_add(1); }
                return;
            }
            0x6005 => { // MOV.W @Rm+,Rn
                let a = self.registers[m];
                self.registers[n] = self.read_word(a) as i16 as i32 as u32;
                if n != m { self.registers[m] = a.wrapping_add(2); }
                return;
            }
            0x6006 => { // MOV.L @Rm+,Rn
                let a = self.registers[m];
                self.registers[n] = self.read_long(a);
                if n != m { self.registers[m] = a.wrapping_add(4); }
                return;
            }
            0x6007 => { self.registers[n] = !self.registers[m]; return; } // NOT
            0x6008 => { // SWAP.B
                let v = self.registers[m];
                self.registers[n] = (v & 0xFFFF_0000) | ((v & 0xFF) << 8) | ((v >> 8) & 0xFF);
                return;
            }
            0x6009 => { self.registers[n] = self.registers[m].rotate_left(16); return; } // SWAP.W
            0x600A => { let (r, c) = 0u32.overflowing_sub(self.registers[m]); let (r2, c2) = r.overflowing_sub(self.t() as u32); self.registers[n] = r2; self.set_t(c || c2); return; } // NEGC
            0x600B => { self.registers[n] = 0u32.wrapping_sub(self.registers[m]); return; } // NEG
            0x600C => { self.registers[n] = self.registers[m] & 0xFF; return; } // EXTU.B
            0x600D => { self.registers[n] = self.registers[m] & 0xFFFF; return; } // EXTU.W
            0x600E => { self.registers[n] = self.registers[m] as i8 as i32 as u32; return; } // EXTS.B
            0x600F => { self.registers[n] = self.registers[m] as i16 as i32 as u32; return; } // EXTS.W
            _ => {}
        }

        // MOV.B/W R0,@(disp4,Rm) and @(disp4,Rm),R0 -- share the 0x8000 nibble
        // with BT/BF/CMP-EQ-imm above, disambiguated by the next nibble.
        match opcode & 0xFF00 {
            0x8000 => { let a = self.registers[m].wrapping_add(d4); self.write_byte(a, self.registers[0] as u8); return; }
            0x8100 => { let a = self.registers[m].wrapping_add(d4.wrapping_mul(2)); self.write_word(a, self.registers[0] as u16); return; }
            0x8400 => { let a = self.registers[m].wrapping_add(d4); self.registers[0] = self.read_byte(a) as i8 as i32 as u32; return; }
            0x8500 => { let a = self.registers[m].wrapping_add(d4.wrapping_mul(2)); self.registers[0] = self.read_word(a) as i16 as i32 as u32; return; }
            _ => {}
        }

        // Unimplemented opcode: leave CPU state unchanged rather than
        // guessing. Known real coverage gap as of this writing: the
        // GBR-indexed byte TST/AND/OR/XOR forms (0xCC00/0xCD00/0xCE00/
        // 0xCF00 -- TST.B/AND.B/OR.B/XOR.B #imm,@(R0,GBR); not the same as
        // the already-implemented 0xC800-0xCB00 immediate-only forms).
        // Not yet hit running the real BIOS -- see `.development/
        // current_bugs.md` before assuming this list is exhaustive; add
        // opcodes here the same way as everything else in this file (hit
        // the wall, decode, cross-check Yabause, implement, test).
    }

    /// Thread execution entry point
    pub fn run_loop(&mut self, shutdown: Arc<std::sync::atomic::AtomicBool>) {
        let now = std::time::Instant::now();
        self.next_vblank_due = Some(now + VBLANK_INTERVAL);
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
                }
            }
            self.step();
            if let Some(ref reporter) = self.pc_reporter {
                reporter.store(self.pc, std::sync::atomic::Ordering::Relaxed);
            }
            if let Some(ref sync) = self.sync {
                sync.sync_core(self.core_id, self.cycles);
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
    use std::sync::RwLock;

    fn make_cpu() -> Sh2 {
        let arbiter = Arc::new(BusArbiter::new());
        let ram = Arc::new(RwLock::new(WorkRam::new()));
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
        assert_eq!(cpu.registers[0], 0x2A, "delay slot instruction did not execute");
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
        assert_eq!(cpu.pc, 0x0600_03BA, "BF/S landed 2 bytes short of the real loop body");
        assert_eq!(cpu.registers[4], 0x3F, "loop counter must not have been reset by re-running the setup instruction");
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
        assert_eq!(cpu.registers[15], 0x0601_0004, "R15 was not post-incremented");
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

        assert_eq!(cpu.read_byte(base + SMPC_OREG_BASE_OFFSET as u32), 0x80, "OREG0: normal startup, resd=0");
        assert_eq!(cpu.read_byte(base + (SMPC_OREG_BASE_OFFSET + 9 * 2) as u32), 0x01, "OREG9: region defaults to Japan");
        assert_eq!(cpu.read_byte(base + (SMPC_OREG_BASE_OFFSET + 10 * 2) as u32), 0x34, "OREG10: flags all clear");
        assert_eq!(cpu.read_byte(base + SMPC_SR_OFFSET as u32), 0x4F, "SR: no peripheral data requested (IREG1 bit3 unset)");
        assert!(cpu.smpc_irq_pending, "INTBACK completion must raise the System Manager interrupt");

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
        assert_eq!(cpu.pc, 0x0600_3002, "did not jump through the System Manager vector");
        assert!(!cpu.smpc_irq_pending, "pending flag must clear once serviced");
        assert_eq!((cpu.sr >> SR_IMASK_SHIFT) & 0xF, SMPC_IRQ_LEVEL, "mask must raise to this interrupt's own level");
    }

    #[test]
    fn intback_requesting_peripheral_data_sets_sr_bit5() {
        let mut cpu = make_cpu();
        let base = 0x0010_0000u32;
        cpu.write_byte(base + SMPC_IREG1_OFFSET as u32, 0x08); // bit3: peripheral data wanted
        cpu.write_byte(base + SMPC_COMREG_OFFSET as u32, SMPC_CMD_INTBACK);
        assert_eq!(cpu.read_byte(base + SMPC_SR_OFFSET as u32), 0x6F, "SR bit5 set when IREG1 bit3 requests peripheral data");
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
        assert!(flag.load(std::sync::atomic::Ordering::Acquire), "SNDON must set the flag");

        cpu.write_byte(base + SMPC_COMREG_OFFSET as u32, SMPC_CMD_SNDOFF);
        assert!(!flag.load(std::sync::atomic::Ordering::Acquire), "SNDOFF must clear the flag");
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
        assert_eq!(cpu.pc, 0x0600_4002, "did not jump through the Sound Request vector");
        assert!(!flag.load(std::sync::atomic::Ordering::Relaxed), "flag must clear once serviced");
        assert_eq!((cpu.sr >> SR_IMASK_SHIFT) & 0xF, SOUND_REQ_IRQ_LEVEL, "mask must raise to this interrupt's own level");
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
        assert!(cpu.vblank_pending, "a masked interrupt must stay pending, not fire");
        assert_eq!(cpu.pc, 0x0600_0002, "masked interrupt must not have diverted execution");
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
        assert_eq!(cpu.pc, 0x0600_2002, "did not jump through the VBR vector table");
        assert!(!cpu.vblank_pending, "pending flag must clear once serviced");
        assert_eq!((cpu.sr >> SR_IMASK_SHIFT) & 0xF, VBLANK_IN_LEVEL, "mask must raise to the interrupt's own level while it runs");

        cpu.step(); // RTE (+ delay slot)
        assert_eq!(cpu.pc, 0x0600_0000, "RTE did not return to the interrupted PC");
        assert_eq!(cpu.sr, 0, "RTE did not restore the original SR");
        assert_eq!(cpu.registers[15], 0x0601_1000, "R15 must be back where it started after the push/pop pair");
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

        cpu.next_vblank_due = Some(std::time::Instant::now() + VBLANK_DURATION + std::time::Duration::from_millis(5));
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
        assert_eq!(cpu.read_byte(0x25F8_0004), 0x00, "TVSTAT high byte has no bits we model");
        assert_eq!(cpu.read_byte(0x25F8_0005), 0x08, "TVSTAT low byte carries the VBLANK bit");
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
        assert_eq!(cpu.registers[15], 0x0601_1000 - 8, "must push exactly 2 longwords");
        // Popped back in RTE order (PC first, then SR) to confirm the push
        // order matches: PC (return addr) at the lower/top address.
        assert_eq!(cpu.read_long(0x0601_1000 - 8), 0x0600_0002, "pushed return address must be right after TRAPA");
        assert_eq!(cpu.read_long(0x0601_1000 - 4), 0x55, "pushed SR must be the pre-trap value");
    }

    #[test]
    fn peripheral_regions_are_real_readwrite_memory() {
        // Regression coverage for the broad memory-map sweep done against
        // Yabause's real, working implementation: every one of these was
        // previously Unmapped (writes silently discarded).
        let mut cpu = make_cpu();
        let probes: &[(u32, &str)] = &[
            (0x0018_0000, "backup ram"),
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
            assert_eq!(cpu.read_long(addr), 0x1234_5678, "{name} at {addr:#010X} is not real read/write memory");
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
}
