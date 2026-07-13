//! Motorola 68000 interpreter for the SCSP's onboard sound CPU.
//!
//! Real Saturn hardware: the SCSP (sound chip) has its own M68000, whose
//! *entire* address space is a direct window onto Sound RAM starting at
//! M68K address 0 (confirmed against Yabause's `scsp.c`:
//! `M68K->SetFetch(0x000000, 0x080000, SoundRam)`). The SH-2 side uploads a
//! sound driver into Sound RAM, then issues SMPC's SNDON command
//! (`Sh2::smpc_execute_command`), which on real hardware just resets this
//! CPU (`M68KStart` -> `M68K->Reset()`) -- standard 68000 reset semantics:
//! read the initial supervisor stack pointer from address 0, the initial PC
//! from address 4, and start executing.
//!
//! This is a from-scratch, standard-68000 interpreter (the ISA itself is a
//! long-public, thoroughly documented standard -- not Saturn-specific, so
//! opcode semantics come from the 68000 Programmer's Reference Manual, not
//! from Yabause's Musashi core transliterated). Coverage grows the same way
//! `sh2.rs` did: implement the common subset real driver code uses, hit a
//! wall, decode the exact missing opcode, add it, keep going -- see
//! `mimas/CLAUDE.md`.
use std::sync::Arc;
use crate::shared_buffers::WorkRam;

// Status Register bits (standard 68000 layout).
const SR_C: u16 = 1 << 0;
const SR_V: u16 = 1 << 1;
const SR_Z: u16 = 1 << 2;
const SR_N: u16 = 1 << 3;
const SR_X: u16 = 1 << 4;
const SR_S: u16 = 1 << 13;
const SR_IMASK_SHIFT: u16 = 8;

static UNIMPL_LOG_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static TRACE_RING: std::sync::Mutex<Vec<(u32, u16, u32, u32, u32)>> = std::sync::Mutex::new(Vec::new());
static LOOP_ENTRY_LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// SCSP register offsets for the real main-CPU interrupt handshake
/// (cross-checked against Yabause `scsp.c`'s register `switch` and its
/// doc comments: "$42a ... MCIEB allow main cpu interrupt", "$42c ...
/// MCIPD request main cpu interrupt"). Real hardware: the sound driver
/// requests an interrupt to the SH-2 by writing a bit into MCIPD; it only
/// actually reaches the SH-2 if that same bit is set in MCIEB (the SH-2's
/// own earlier "which sound events do I want to hear about" mask).
const SCSP_MCIEB_OFFSET: usize = 0x2A;
const SCSP_MCIPD_OFFSET: usize = 0x2C;

pub struct M68k {
    pub d: [u32; 8],
    pub a: [u32; 8],
    pub sr: u16,
    pub pc: u32,
    pub running: bool,
    work_ram: Arc<WorkRam>,
    /// Set when a real MCIPD write both requests and is enabled (via
    /// MCIEB) for a given interrupt source -- see `check_main_interrupt`.
    /// `None` when nothing has wired the SH-2 side up to observe this
    /// (e.g. plain unit tests).
    pub sound_req_irq: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl M68k {
    pub fn new(work_ram: Arc<WorkRam>) -> Self {
        Self { d: [0; 8], a: [0; 8], sr: 0, pc: 0, running: false, work_ram, sound_req_irq: None }
    }

    /// Real 68000 RESET exception: fetch initial SSP from address 0,
    /// initial PC from address 4 (both big-endian longs -- 68000 is
    /// big-endian throughout), enter supervisor mode with interrupts fully
    /// masked. Matches `M68KStart`'s `M68K->Reset()` in real, working SCSP
    /// emulation.
    pub fn reset(&mut self) {
        self.a[7] = self.read_long(0);
        self.pc = self.read_long(4);
        self.sr = SR_S | (7 << SR_IMASK_SHIFT);
        self.d = [0; 8];
        for i in 0..7 {
            self.a[i] = 0;
        }
        self.running = true;
    }

    pub fn stop(&mut self) {
        self.running = false;
    }

    /// Real M68000 address decode on the SCSP (cross-checked against
    /// Yabause's `c68k_byte_read`/`c68k_byte_write` in `scsp.c`): addresses
    /// below 0x080000 are Sound RAM (dual-ported -- the same physical chip
    /// the SH-2 sees at 0x05A00000/0x25A00000), addresses at/above 0x100000
    /// are the SCSP's own register block (also dual-ported -- the same
    /// registers the SH-2 sees at 0x05B00000/0x25B00000; comment in real
    /// source: "model2 scsp is mapped to 0x100000~0x100ee4"). This is the
    /// real SH-2<->M68K communication path: a boot-sound-driver handshake
    /// writes/polls shared bytes here, not in SH-2-exclusive Work RAM (the
    /// M68K has no access to that at all on real hardware).
    fn read_byte(&self, addr: u32) -> u8 {
        if addr < 0x0008_0000 {
            let ram = self.work_ram.sound_ram.read().unwrap();
            ram[(addr as usize) & (ram.len() - 1)]
        } else if addr >= 0x0010_0000 {
            let off = (addr - 0x0010_0000) as usize;
            let ram = self.work_ram.scsp_regs.read().unwrap();
            ram[off & (ram.len() - 1)]
        } else {
            0
        }
    }

    fn write_byte(&mut self, addr: u32, val: u8) {
        if addr < 0x0008_0000 {
            let mut ram = self.work_ram.sound_ram.write().unwrap();
            let mask = ram.len() - 1;
            ram[(addr as usize) & mask] = val;
        } else if addr >= 0x0010_0000 {
            let off = (addr - 0x0010_0000) as usize;
            // One held write-guard spans the register store AND the
            // MCIEB/MCIPD read-back below -- both must observe the exact
            // same instant, or a concurrent SH-2 write to MCIEB in the gap
            // between them could change whether *this* write should have
            // fired the interrupt (see `WorkRam`'s per-field-lock doc
            // comment: this is exactly the "acquire once, hold across
            // related operations" pattern the split is safe to keep here).
            let mut ram = self.work_ram.scsp_regs.write().unwrap();
            let mask = ram.len() - 1;
            let off = off & mask;
            ram[off] = val;
            // MCIPD write: real hardware ORs the written bits into the
            // pending-interrupt register, then fires the real Sound
            // Request interrupt (SCU vector 0x46, level 9) only for bits
            // also enabled in MCIEB -- see `scsp_main_interrupt` in a
            // real, working SCSP implementation (`scsp.c`).
            if off == SCSP_MCIPD_OFFSET || off == SCSP_MCIPD_OFFSET + 1 {
                let mcieb = u16::from_be_bytes([
                    ram[SCSP_MCIEB_OFFSET],
                    ram[SCSP_MCIEB_OFFSET + 1],
                ]);
                let mcipd = u16::from_be_bytes([
                    ram[SCSP_MCIPD_OFFSET],
                    ram[SCSP_MCIPD_OFFSET + 1],
                ]);
                if mcieb & mcipd != 0 {
                    if let Some(ref flag) = self.sound_req_irq {
                        flag.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        }
    }

    fn read_word(&self, addr: u32) -> u16 {
        ((self.read_byte(addr) as u16) << 8) | self.read_byte(addr.wrapping_add(1)) as u16
    }

    fn write_word(&mut self, addr: u32, val: u16) {
        self.write_byte(addr, (val >> 8) as u8);
        self.write_byte(addr.wrapping_add(1), val as u8);
    }

    fn read_long(&self, addr: u32) -> u32 {
        ((self.read_word(addr) as u32) << 16) | self.read_word(addr.wrapping_add(2)) as u32
    }

    fn write_long(&mut self, addr: u32, val: u32) {
        self.write_word(addr, (val >> 16) as u16);
        self.write_word(addr.wrapping_add(2), val as u16);
    }

    fn set_nz16(&mut self, val: u16) {
        self.sr &= !(SR_N | SR_Z);
        if val == 0 { self.sr |= SR_Z; }
        if val & 0x8000 != 0 { self.sr |= SR_N; }
    }
    fn set_nz32(&mut self, val: u32) {
        self.sr &= !(SR_N | SR_Z);
        if val == 0 { self.sr |= SR_Z; }
        if val & 0x8000_0000 != 0 { self.sr |= SR_N; }
    }
    fn set_nz8(&mut self, val: u8) {
        self.sr &= !(SR_N | SR_Z);
        if val == 0 { self.sr |= SR_Z; }
        if val & 0x80 != 0 { self.sr |= SR_N; }
    }

    fn fetch_word(&mut self) -> u16 {
        let w = self.read_word(self.pc);
        self.pc = self.pc.wrapping_add(2);
        w
    }

    /// Effective-address computation for the common addressing modes real
    /// sound-driver code uses. Returns the resolved memory address for
    /// memory-based modes; register-direct modes are handled by the caller
    /// (no address to compute). `size` in bytes (1/2/4) matters for the
    /// post-increment/pre-decrement step amount and for A7's word-alignment
    /// rule (68000 keeps the stack pointer even).
    fn ea_addr(&mut self, mode: u8, reg: usize, size: u32) -> u32 {
        match mode {
            2 => self.a[reg], // (An)
            3 => { // (An)+
                let addr = self.a[reg];
                let step = if reg == 7 && size == 1 { 2 } else { size };
                self.a[reg] = self.a[reg].wrapping_add(step);
                addr
            }
            4 => { // -(An)
                let step = if reg == 7 && size == 1 { 2 } else { size };
                self.a[reg] = self.a[reg].wrapping_sub(step);
                self.a[reg]
            }
            5 => { // (d16,An)
                let disp = self.fetch_word() as i16 as i32 as u32;
                self.a[reg].wrapping_add(disp)
            }
            6 => { // (d8,An,Xn)
                let ext = self.fetch_word();
                self.indexed_addr(self.a[reg], ext)
            }
            7 => match reg {
                0 => self.fetch_word() as i16 as i32 as u32, // abs.W (sign-extended)
                1 => { let hi = self.fetch_word(); let lo = self.fetch_word(); ((hi as u32) << 16) | lo as u32 } // abs.L
                2 => { // (d16,PC)
                    let base = self.pc;
                    let disp = self.fetch_word() as i16 as i32 as u32;
                    base.wrapping_add(disp)
                }
                3 => { // (d8,PC,Xn)
                    let base = self.pc;
                    let ext = self.fetch_word();
                    self.indexed_addr(base, ext)
                }
                _ => 0,
            },
            _ => 0,
        }
    }

    fn indexed_addr(&self, base: u32, ext: u16) -> u32 {
        let xn = ((ext >> 12) & 7) as usize;
        let is_addr_reg = ext & 0x8000 != 0;
        let long = ext & 0x0800 != 0;
        let raw = if is_addr_reg { self.a[xn] } else { self.d[xn] };
        let xn_val = if long { raw } else { raw as i16 as i32 as u32 };
        let disp = ext as i8 as i32 as u32;
        base.wrapping_add(xn_val).wrapping_add(disp)
    }

    /// Read the value an effective address operand refers to, for register
    /// modes (0/1) or by reading memory for the rest. `size` in bytes.
    fn read_ea(&mut self, mode: u8, reg: usize, size: u32) -> u32 {
        match mode {
            0 => self.d[reg],
            1 => self.a[reg],
            _ => {
                let addr = self.ea_addr(mode, reg, size);
                match size {
                    1 => self.read_byte(addr) as u32,
                    2 => self.read_word(addr) as u32,
                    _ => self.read_long(addr),
                }
            }
        }
    }

    fn write_ea(&mut self, mode: u8, reg: usize, size: u32, val: u32) {
        match mode {
            0 => {
                match size {
                    1 => self.d[reg] = (self.d[reg] & 0xFFFF_FF00) | (val & 0xFF),
                    2 => self.d[reg] = (self.d[reg] & 0xFFFF_0000) | (val & 0xFFFF),
                    _ => self.d[reg] = val,
                }
            }
            1 => self.a[reg] = if size == 2 { val as i16 as i32 as u32 } else { val },
            _ => {
                let addr = self.ea_addr(mode, reg, size);
                match size {
                    1 => self.write_byte(addr, val as u8),
                    2 => self.write_word(addr, val as u16),
                    _ => self.write_long(addr, val),
                }
            }
        }
    }

    fn condition(&self, cc: u8) -> bool {
        let c = self.sr & SR_C != 0;
        let v = self.sr & SR_V != 0;
        let z = self.sr & SR_Z != 0;
        let n = self.sr & SR_N != 0;
        match cc {
            0x0 => true,             // T
            0x1 => false,            // F
            0x2 => !c && !z,         // HI
            0x3 => c || z,           // LS
            0x4 => !c,               // CC/HS
            0x5 => c,                // CS/LO
            0x6 => !z,               // NE
            0x7 => z,                // EQ
            0x8 => true,             // VC (not modeled precisely, rare)
            0x9 => false,            // VS
            0xA => !n,               // PL
            0xB => n,                // MI
            0xC => n == v,           // GE
            0xD => n != v,           // LT
            0xE => !z && (n == v),   // GT
            0xF => z || (n != v),    // LE
            _ => false,
        }
    }

    pub fn step(&mut self) {
        if !self.running {
            return;
        }
        let trace_pc = self.pc;
        if std::env::var("MIMAS_DEBUG_M68K").is_ok()
            && trace_pc == 0x0000_322E
            && !LOOP_ENTRY_LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            eprintln!(
                "[M68K] first entry to clear-loop at {:#010X}: d={:08X?} a={:08X?}",
                trace_pc, self.d, self.a
            );
        }
        let opcode = self.fetch_word();
        if std::env::var("MIMAS_DEBUG_M68K").is_ok() {
            let mut ring = TRACE_RING.lock().unwrap();
            if ring.len() >= 32 { ring.remove(0); }
            ring.push((trace_pc, opcode, self.a[0], self.a[1], self.d[7]));
        }
        self.execute(opcode);
    }

    fn execute(&mut self, opcode: u16) {
        // MOVE.b/w/l  (bits 15-12 = 00, bits 13-12 encode size for MOVE
        // proper: 01=byte,11=word,10=long)
        let top4 = (opcode >> 12) & 0xF;

        if top4 == 0 && (opcode >> 12) == 0 {
            // 0000 group: ANDI/ORI/EORI/CMPI to SR or memory, BTST/BSET/BCLR/BCHG, MOVEP
            self.execute_group0(opcode);
            return;
        }
        if top4 == 1 || top4 == 2 || top4 == 3 {
            self.execute_move(opcode, top4);
            return;
        }
        if top4 == 4 {
            self.execute_group4(opcode);
            return;
        }
        if top4 == 5 {
            self.execute_group5(opcode);
            return;
        }
        if top4 == 6 {
            self.execute_bcc(opcode);
            return;
        }
        if top4 == 7 {
            // MOVEQ
            let reg = ((opcode >> 9) & 7) as usize;
            let data = (opcode & 0xFF) as i8 as i32 as u32;
            self.d[reg] = data;
            self.set_nz32(data);
            self.sr &= !SR_V;
            self.sr &= !SR_C;
            return;
        }
        if top4 == 8 {
            self.execute_or_group(opcode);
            return;
        }
        if top4 == 9 {
            self.execute_addsub(opcode, false);
            return;
        }
        if top4 == 0xB {
            self.execute_cmp_eor(opcode);
            return;
        }
        if top4 == 0xC {
            self.execute_and_group(opcode);
            return;
        }
        if top4 == 0xD {
            self.execute_addsub(opcode, true);
            return;
        }
        if top4 == 0xE {
            self.execute_shift(opcode);
            return;
        }
        // Unimplemented major group (0xA, 0xF, or an unrecognized minor
        // pattern within a handled group): leave CPU state unchanged rather
        // than guessing, same policy as `sh2.rs::execute`'s tail comment.
        if std::env::var("MIMAS_DEBUG_M68K").is_ok() {
            let n = UNIMPL_LOG_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if n < 20 {
                let pc = self.pc.wrapping_sub(2);
                eprintln!(
                    "[M68K] unimplemented opcode={:#06X} at pc={:#010X} d={:08X?} a={:08X?}",
                    opcode, pc, self.d, self.a
                );
                if n == 0 {
                    let ram = self.work_ram.sound_ram.read().unwrap();
                    std::fs::write("/tmp/claude-1000/sound_ram_dump.bin", &ram[..])
                        .expect("failed to write sound ram dump");
                    drop(ram);
                    eprintln!("[M68K] dumped full sound_ram (512KB) to /tmp/claude-1000/sound_ram_dump.bin");
                    let ring = TRACE_RING.lock().unwrap();
                    eprintln!("[M68K] last {} executed (pc,opcode,a0,a1,d7) tuples:", ring.len());
                    for (tpc, top, ta0, ta1, td7) in ring.iter() {
                        eprintln!("  pc={:#010X} op={:#06X} a0={:#010X} a1={:#010X} d7={:#010X}", tpc, top, ta0, ta1, td7);
                    }
                }
            }
        }
    }

    fn execute_group0(&mut self, opcode: u16) {
        let mode = ((opcode >> 3) & 7) as u8;
        let reg = (opcode & 7) as usize;

        // BTST/BCHG/BCLR/BSET Dn,<ea> (register bit number form: bits 8-6 = 1xx)
        if opcode & 0x0100 != 0 {
            let bit_reg = ((opcode >> 9) & 7) as usize;
            let op = (opcode >> 6) & 3;
            self.bit_op(op, self.d[bit_reg], mode, reg);
            return;
        }
        // #imm forms: ORI/ANDI/SUBI/ADDI/EORI/CMPI to <ea>, or BTST/BCHG/BCLR/BSET #imm,<ea>
        let sub_op = (opcode >> 9) & 7;
        if opcode & 0x0038 == 0x0008 && sub_op == 4 {
            // BTST/etc #imm,<ea> encoding actually shares opcode&0xFF00==0x0800 pattern; handled below.
        }
        if (opcode & 0xFF00) == 0x0800 {
            let imm = self.fetch_word() & 0x1F;
            let op = (opcode >> 6) & 3;
            self.bit_op(op, imm as u32, mode, reg);
            return;
        }
        let size_bits = (opcode >> 6) & 3;
        let size: u32 = match size_bits { 0 => 1, 1 => 2, _ => 4 };
        match sub_op {
            0 => { // ORI
                if mode == 7 && reg == 4 {
                    let imm = self.fetch_word();
                    if size_bits == 0 { self.sr = (self.sr & 0xFF00) | ((self.sr | (imm & 0xFF)) & 0xFF); }
                    else { self.sr |= imm; }
                    return;
                }
                let imm = self.fetch_imm(size);
                let val = self.read_ea(mode, reg, size) | imm;
                self.write_ea(mode, reg, size, val);
                self.set_nz_sized(val, size);
                self.sr &= !(SR_V | SR_C);
            }
            1 => { // ANDI
                if mode == 7 && reg == 4 {
                    let imm = self.fetch_word();
                    self.sr &= imm;
                    return;
                }
                let imm = self.fetch_imm(size);
                let val = self.read_ea(mode, reg, size) & imm;
                self.write_ea(mode, reg, size, val);
                self.set_nz_sized(val, size);
                self.sr &= !(SR_V | SR_C);
            }
            2 => { // SUBI
                let imm = self.fetch_imm(size);
                let old = self.read_ea(mode, reg, size);
                let val = old.wrapping_sub(imm);
                self.write_ea(mode, reg, size, val);
                self.set_nz_sized(val, size);
                self.sr = if val > old { self.sr | SR_C | SR_X } else { self.sr & !(SR_C | SR_X) };
            }
            3 => { // ADDI
                let imm = self.fetch_imm(size);
                let old = self.read_ea(mode, reg, size);
                let val = old.wrapping_add(imm);
                self.write_ea(mode, reg, size, val);
                self.set_nz_sized(val, size);
                self.sr = if val < old { self.sr | SR_C | SR_X } else { self.sr & !(SR_C | SR_X) };
            }
            5 => { // EORI
                if mode == 7 && reg == 4 {
                    let imm = self.fetch_word();
                    self.sr ^= imm;
                    return;
                }
                let imm = self.fetch_imm(size);
                let val = self.read_ea(mode, reg, size) ^ imm;
                self.write_ea(mode, reg, size, val);
                self.set_nz_sized(val, size);
                self.sr &= !(SR_V | SR_C);
            }
            6 => { // CMPI
                let imm = self.fetch_imm(size);
                let old = self.read_ea(mode, reg, size);
                let val = old.wrapping_sub(imm);
                self.set_nz_sized(val, size);
                self.sr = if old < imm { self.sr | SR_C } else { self.sr & !SR_C };
            }
            _ => {}
        }
    }

    fn bit_op(&mut self, op: u16, bit_val: u32, mode: u8, reg: usize) {
        let size: u32 = if mode == 0 { 4 } else { 1 };
        let bit = if mode == 0 { bit_val & 0x1F } else { bit_val & 0x7 };
        let val = self.read_ea(mode, reg, size);
        let mask = 1u32 << bit;
        if val & mask == 0 { self.sr |= SR_Z; } else { self.sr &= !SR_Z; }
        let new_val = match op {
            1 => val ^ mask,        // BCHG
            2 => val & !mask,       // BCLR
            3 => val | mask,        // BSET
            _ => { return; }        // BTST: read-only
        };
        self.write_ea(mode, reg, size, new_val);
    }

    fn fetch_imm(&mut self, size: u32) -> u32 {
        if size == 1 {
            (self.fetch_word() & 0xFF) as u32
        } else if size == 2 {
            self.fetch_word() as u32
        } else {
            let hi = self.fetch_word();
            let lo = self.fetch_word();
            ((hi as u32) << 16) | lo as u32
        }
    }

    fn set_nz_sized(&mut self, val: u32, size: u32) {
        match size {
            1 => self.set_nz8(val as u8),
            2 => self.set_nz16(val as u16),
            _ => self.set_nz32(val),
        }
    }

    fn execute_move(&mut self, opcode: u16, top4: u16) {
        let size: u32 = match top4 { 1 => 1, 3 => 2, _ => 4 };
        let src_mode = ((opcode >> 3) & 7) as u8;
        let src_reg = (opcode & 7) as usize;
        let dst_reg = ((opcode >> 9) & 7) as usize;
        let dst_mode = ((opcode >> 6) & 7) as u8;
        let val = self.read_ea(src_mode, src_reg, size);
        if dst_mode == 1 {
            // MOVEA: no flags affected
            self.a[dst_reg] = if size == 2 { val as i16 as i32 as u32 } else { val };
            return;
        }
        self.write_ea(dst_mode, dst_reg, size, val);
        self.set_nz_sized(val, size);
        self.sr &= !(SR_V | SR_C);
    }

    fn execute_group4(&mut self, opcode: u16) {
        // NOP
        if opcode == 0x4E71 { return; }
        // RTS
        if opcode == 0x4E75 {
            self.pc = self.pop_long();
            return;
        }
        // RTE (privileged; we run everything as if supervisor, just pop SR then PC like RTS's mirror)
        if opcode == 0x4E73 {
            self.sr = self.pop_word();
            self.pc = self.pop_long();
            return;
        }
        // JSR <ea>
        if (opcode & 0xFFC0) == 0x4E80 {
            let mode = ((opcode >> 3) & 7) as u8;
            let reg = (opcode & 7) as usize;
            let target = self.ea_addr(mode, reg, 4);
            self.push_long(self.pc);
            self.pc = target;
            return;
        }
        // JMP <ea>
        if (opcode & 0xFFC0) == 0x4EC0 {
            let mode = ((opcode >> 3) & 7) as u8;
            let reg = (opcode & 7) as usize;
            self.pc = self.ea_addr(mode, reg, 4);
            return;
        }
        // LEA <ea>,An
        if (opcode & 0xF1C0) == 0x41C0 {
            let mode = ((opcode >> 3) & 7) as u8;
            let reg = (opcode & 7) as usize;
            let areg = ((opcode >> 9) & 7) as usize;
            self.a[areg] = self.ea_addr(mode, reg, 4);
            return;
        }
        // CLR.size <ea>
        if (opcode & 0xFF00) == 0x4200 {
            let size_bits = (opcode >> 6) & 3;
            let size: u32 = match size_bits { 0 => 1, 1 => 2, _ => 4 };
            let mode = ((opcode >> 3) & 7) as u8;
            let reg = (opcode & 7) as usize;
            self.write_ea(mode, reg, size, 0);
            self.sr = (self.sr & !(SR_N | SR_V | SR_C)) | SR_Z;
            return;
        }
        // NOT.size <ea>
        if (opcode & 0xFF00) == 0x4600 {
            let size_bits = (opcode >> 6) & 3;
            let size: u32 = match size_bits { 0 => 1, 1 => 2, _ => 4 };
            let mode = ((opcode >> 3) & 7) as u8;
            let reg = (opcode & 7) as usize;
            let val = (!self.read_ea(mode, reg, size)) & size_mask(size);
            self.write_ea(mode, reg, size, val);
            self.set_nz_sized(val, size);
            self.sr &= !(SR_V | SR_C);
            return;
        }
        // NEG.size <ea>
        if (opcode & 0xFF00) == 0x4400 {
            let size_bits = (opcode >> 6) & 3;
            let size: u32 = match size_bits { 0 => 1, 1 => 2, _ => 4 };
            let mode = ((opcode >> 3) & 7) as u8;
            let reg = (opcode & 7) as usize;
            let old = self.read_ea(mode, reg, size);
            let val = 0u32.wrapping_sub(old) & size_mask(size);
            self.write_ea(mode, reg, size, val);
            self.set_nz_sized(val, size);
            self.sr = if val != 0 { self.sr | SR_C | SR_X } else { self.sr & !(SR_C | SR_X) };
            return;
        }
        // TST.size <ea>
        if (opcode & 0xFF00) == 0x4A00 && (opcode & 0xC0) != 0xC0 {
            let size_bits = (opcode >> 6) & 3;
            let size: u32 = match size_bits { 0 => 1, 1 => 2, _ => 4 };
            let mode = ((opcode >> 3) & 7) as u8;
            let reg = (opcode & 7) as usize;
            let val = self.read_ea(mode, reg, size);
            self.set_nz_sized(val, size);
            self.sr &= !(SR_V | SR_C);
            return;
        }
        // TAS <ea>
        if (opcode & 0xFFC0) == 0x4AC0 {
            let mode = ((opcode >> 3) & 7) as u8;
            let reg = (opcode & 7) as usize;
            let val = self.read_ea(mode, reg, 1);
            self.set_nz8(val as u8);
            self.sr &= !(SR_V | SR_C);
            self.write_ea(mode, reg, 1, (val as u8 | 0x80) as u32);
            return;
        }
        // SWAP Dn
        if (opcode & 0xFFF8) == 0x4840 {
            let reg = (opcode & 7) as usize;
            self.d[reg] = self.d[reg].rotate_left(16);
            self.set_nz32(self.d[reg]);
            self.sr &= !(SR_V | SR_C);
            return;
        }
        // EXT.w / EXT.l Dn
        if (opcode & 0xFFB8) == 0x4880 {
            let reg = (opcode & 7) as usize;
            let long = opcode & 0x0040 != 0;
            if long {
                self.d[reg] = self.d[reg] as i16 as i32 as u32;
                self.set_nz32(self.d[reg]);
            } else {
                let ext = (self.d[reg] as u8) as i8 as i16 as u16;
                self.d[reg] = (self.d[reg] & 0xFFFF_0000) | ext as u32;
                self.set_nz16(ext);
            }
            self.sr &= !(SR_V | SR_C);
            return;
        }
        // PEA <ea>
        if (opcode & 0xFFC0) == 0x4840 {
            let mode = ((opcode >> 3) & 7) as u8;
            let reg = (opcode & 7) as usize;
            let addr = self.ea_addr(mode, reg, 4);
            self.push_long(addr);
            return;
        }
        // LINK An,#disp
        if (opcode & 0xFFF8) == 0x4E50 {
            let reg = (opcode & 7) as usize;
            self.push_long(self.a[reg]);
            self.a[reg] = self.a[7];
            let disp = self.fetch_word() as i16 as i32 as u32;
            self.a[7] = self.a[7].wrapping_add(disp);
            return;
        }
        // UNLK An
        if (opcode & 0xFFF8) == 0x4E58 {
            let reg = (opcode & 7) as usize;
            self.a[7] = self.a[reg];
            self.a[reg] = self.pop_long();
            return;
        }
        // MOVEM regs,<ea> and <ea>,regs
        if (opcode & 0xFB80) == 0x4880 {
            self.execute_movem(opcode);
            return;
        }
        // MOVE An,USP / MOVE USP,An -- privileged, no real USP modeled; no-op read as 0.
        if (opcode & 0xFFF0) == 0x4E60 || (opcode & 0xFFF0) == 0x4E68 {
            return;
        }
        // Trap/illegal/stop/reset -- not needed by ordinary driver code paths; no-op.
    }

    fn execute_movem(&mut self, opcode: u16) {
        let to_mem = opcode & 0x0400 == 0;
        let long = opcode & 0x0040 != 0;
        let size: u32 = if long { 4 } else { 2 };
        let mode = ((opcode >> 3) & 7) as u8;
        let reg = (opcode & 7) as usize;
        let mask = self.fetch_word();
        if !to_mem {
            let addr = self.ea_addr(mode, reg, size);
            let mut a = addr;
            for i in 0..16 {
                if mask & (1 << i) != 0 {
                    let val = if size == 4 { self.read_long(a) } else { self.read_word(a) as i16 as i32 as u32 };
                    if i < 8 { self.d[i] = val; } else { self.a[i - 8] = val; }
                    a = a.wrapping_add(size);
                }
            }
            if mode == 3 {
                self.a[reg] = a;
            }
        } else if mode == 4 {
            // predecrement: register list bit order is reversed (A7..A0,D7..D0)
            let mut a = self.a[reg];
            for i in (0..16).rev() {
                if mask & (1 << (15 - i)) != 0 {
                    let val = if i < 8 { self.d[i] } else { self.a[i - 8] };
                    a = a.wrapping_sub(size);
                    if size == 4 { self.write_long(a, val); } else { self.write_word(a, val as u16); }
                }
            }
            self.a[reg] = a;
        } else {
            let addr = self.ea_addr(mode, reg, size);
            let mut a = addr;
            for i in 0..16 {
                if mask & (1 << i) != 0 {
                    let val = if i < 8 { self.d[i] } else { self.a[i - 8] };
                    if size == 4 { self.write_long(a, val); } else { self.write_word(a, val as u16); }
                    a = a.wrapping_add(size);
                }
            }
        }
    }

    fn execute_group5(&mut self, opcode: u16) {
        let mode = ((opcode >> 3) & 7) as u8;
        let reg = (opcode & 7) as usize;
        // DBcc Dn,disp
        if (opcode & 0xF0F8) == 0x50C8 {
            let cc = ((opcode >> 8) & 0xF) as u8;
            let disp = self.fetch_word() as i16 as i32;
            if !self.condition(cc) {
                let dreg = reg;
                let val = (self.d[dreg] as u16).wrapping_sub(1);
                self.d[dreg] = (self.d[dreg] & 0xFFFF_0000) | val as u32;
                if val != 0xFFFF {
                    self.pc = (self.pc as i32).wrapping_sub(2).wrapping_add(disp) as u32;
                }
            }
            return;
        }
        // Scc <ea>
        if (opcode & 0xF0C0) == 0x50C0 {
            let cc = ((opcode >> 8) & 0xF) as u8;
            let val: u32 = if self.condition(cc) { 0xFF } else { 0x00 };
            self.write_ea(mode, reg, 1, val);
            return;
        }
        // ADDQ/SUBQ #imm,<ea>
        let size_bits = (opcode >> 6) & 3;
        if size_bits != 3 {
            let size: u32 = match size_bits { 0 => 1, 1 => 2, _ => 4 };
            let data = ((opcode >> 9) & 7) as u32;
            let data = if data == 0 { 8 } else { data };
            let is_sub = opcode & 0x0100 != 0;
            let old = self.read_ea(mode, reg, size);
            let val = if is_sub { old.wrapping_sub(data) } else { old.wrapping_add(data) } & size_mask(size);
            self.write_ea(mode, reg, size, val);
            if mode != 1 {
                self.set_nz_sized(val, size);
                let carry = if is_sub { val > old } else { val < old };
                self.sr = if carry { self.sr | SR_C | SR_X } else { self.sr & !(SR_C | SR_X) };
            }
        }
    }

    fn execute_bcc(&mut self, opcode: u16) {
        let cc = ((opcode >> 8) & 0xF) as u8;
        let disp8 = (opcode & 0xFF) as u8;
        let base_pc = self.pc; // PC is already past the opcode word here
        let target = if disp8 == 0 {
            let disp16 = self.fetch_word() as i16 as i32;
            (base_pc as i32).wrapping_add(disp16) as u32
        } else {
            let disp = disp8 as i8 as i32;
            (base_pc as i32).wrapping_add(disp) as u32
        };
        if cc == 1 {
            // BSR
            self.push_long(self.pc.wrapping_add(if disp8 == 0 { 0 } else { 0 })); // return addr already correct: pc after operand fetch
            self.pc = target;
            return;
        }
        if self.condition(cc) {
            self.pc = target;
        }
    }

    fn execute_or_group(&mut self, opcode: u16) {
        let reg = ((opcode >> 9) & 7) as usize;
        let size_bits = (opcode >> 6) & 3;
        let mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as usize;
        if size_bits == 3 {
            // DIVU
            let size = 2;
            let src = self.read_ea(mode, ea_reg, size) as u16;
            if src != 0 {
                let dividend = self.d[reg];
                let quot = dividend / src as u32;
                let rem = dividend % src as u32;
                if quot <= 0xFFFF {
                    self.d[reg] = (rem << 16) | (quot & 0xFFFF);
                    self.set_nz16(quot as u16);
                    self.sr &= !SR_C;
                }
            }
            return;
        }
        if size_bits == 7 {
            // DIVS -- simplified signed divide
            let src = self.read_ea(mode, ea_reg, 2) as u16 as i16 as i32;
            if src != 0 {
                let dividend = self.d[reg] as i32;
                let quot = dividend / src;
                let rem = dividend % src;
                if quot >= -32768 && quot <= 32767 {
                    self.d[reg] = ((rem as u32) << 16) | (quot as u32 & 0xFFFF);
                    self.set_nz16(quot as u16);
                    self.sr &= !SR_C;
                }
            }
            return;
        }
        let size: u32 = match size_bits { 0 => 1, 1 => 2, _ => 4 };
        let to_mem = opcode & 0x0100 != 0;
        if to_mem {
            let val = self.read_ea(mode, ea_reg, size) | (self.d[reg] & size_mask(size));
            self.write_ea(mode, ea_reg, size, val);
            self.set_nz_sized(val, size);
        } else {
            let val = self.read_ea(mode, ea_reg, size) | (self.d[reg] & size_mask(size));
            self.d[reg] = (self.d[reg] & !size_mask(size)) | (val & size_mask(size));
            self.set_nz_sized(val, size);
        }
        self.sr &= !(SR_V | SR_C);
    }

    fn execute_and_group(&mut self, opcode: u16) {
        let reg = ((opcode >> 9) & 7) as usize;
        let size_bits = (opcode >> 6) & 3;
        let mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as usize;
        // MULU/MULS
        if size_bits == 3 {
            let src = self.read_ea(mode, ea_reg, 2) as u16;
            let val = (self.d[reg] as u16 as u32).wrapping_mul(src as u32);
            self.d[reg] = val;
            self.set_nz32(val);
            self.sr &= !(SR_V | SR_C);
            return;
        }
        if size_bits == 7 {
            let src = self.read_ea(mode, ea_reg, 2) as u16 as i16 as i32;
            let val = ((self.d[reg] as u16 as i16 as i32).wrapping_mul(src)) as u32;
            self.d[reg] = val;
            self.set_nz32(val);
            self.sr &= !(SR_V | SR_C);
            return;
        }
        // EXG (register exchange) shares this major opcode: 1100 rrr1 ss01 0 rrr etc.
        if (opcode & 0x01F0) == 0x0140 { // EXG Dx,Dy
            let rx = reg; let ry = ea_reg;
            self.d.swap(rx, ry);
            return;
        }
        if (opcode & 0x01F0) == 0x0148 { // EXG Ax,Ay
            let rx = reg; let ry = ea_reg;
            self.a.swap(rx, ry);
            return;
        }
        if (opcode & 0x01F0) == 0x0188 { // EXG Dx,Ay
            let tmp = self.d[reg];
            self.d[reg] = self.a[ea_reg];
            self.a[ea_reg] = tmp;
            return;
        }
        let size: u32 = match size_bits { 0 => 1, 1 => 2, _ => 4 };
        let to_mem = opcode & 0x0100 != 0;
        if to_mem {
            let val = self.read_ea(mode, ea_reg, size) & (self.d[reg] & size_mask(size));
            self.write_ea(mode, ea_reg, size, val);
            self.set_nz_sized(val, size);
        } else {
            let val = self.read_ea(mode, ea_reg, size) & (self.d[reg] & size_mask(size));
            self.d[reg] = (self.d[reg] & !size_mask(size)) | (val & size_mask(size));
            self.set_nz_sized(val, size);
        }
        self.sr &= !(SR_V | SR_C);
    }

    fn execute_cmp_eor(&mut self, opcode: u16) {
        let reg = ((opcode >> 9) & 7) as usize;
        let size_bits = (opcode >> 6) & 3;
        let mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as usize;
        if size_bits == 3 || size_bits == 7 {
            // CMPA.w / CMPA.l
            let size: u32 = if size_bits == 3 { 2 } else { 4 };
            let src = self.read_ea(mode, ea_reg, size);
            let src = if size == 2 { src as i16 as i32 as u32 } else { src };
            let old = self.a[reg];
            let val = old.wrapping_sub(src);
            self.set_nz32(val);
            self.sr = if old < src { self.sr | SR_C } else { self.sr & !SR_C };
            return;
        }
        let size: u32 = match size_bits { 0 => 1, 1 => 2, _ => 4 };
        let is_eor = opcode & 0x0100 != 0 && mode != 1;
        if is_eor {
            let val = self.read_ea(mode, ea_reg, size) ^ (self.d[reg] & size_mask(size));
            self.write_ea(mode, ea_reg, size, val);
            self.set_nz_sized(val, size);
            self.sr &= !(SR_V | SR_C);
        } else {
            // CMP
            let src = self.read_ea(mode, ea_reg, size);
            let old = self.d[reg] & size_mask(size);
            let val = old.wrapping_sub(src) & size_mask(size);
            self.set_nz_sized(val, size);
            self.sr = if old < src { self.sr | SR_C } else { self.sr & !SR_C };
        }
    }

    fn execute_addsub(&mut self, opcode: u16, is_add: bool) {
        let reg = ((opcode >> 9) & 7) as usize;
        let size_bits = (opcode >> 6) & 3;
        let mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as usize;
        if size_bits == 3 || size_bits == 7 {
            // ADDA/SUBA
            let size: u32 = if size_bits == 3 { 2 } else { 4 };
            let src = self.read_ea(mode, ea_reg, size);
            let src = if size == 2 { src as i16 as i32 as u32 } else { src };
            self.a[reg] = if is_add { self.a[reg].wrapping_add(src) } else { self.a[reg].wrapping_sub(src) };
            return;
        }
        let size: u32 = match size_bits { 0 => 1, 1 => 2, _ => 4 };
        let to_mem = opcode & 0x0100 != 0;
        if to_mem {
            let old = self.read_ea(mode, ea_reg, size);
            let operand = self.d[reg] & size_mask(size);
            let val = (if is_add { old.wrapping_add(operand) } else { old.wrapping_sub(operand) }) & size_mask(size);
            self.write_ea(mode, ea_reg, size, val);
            self.set_nz_sized(val, size);
            let carry = if is_add { val < old } else { val > old };
            self.sr = if carry { self.sr | SR_C | SR_X } else { self.sr & !(SR_C | SR_X) };
        } else {
            let old = self.d[reg] & size_mask(size);
            let operand = self.read_ea(mode, ea_reg, size);
            let val = (if is_add { old.wrapping_add(operand) } else { old.wrapping_sub(operand) }) & size_mask(size);
            self.d[reg] = (self.d[reg] & !size_mask(size)) | val;
            self.set_nz_sized(val, size);
            let carry = if is_add { val < old } else { val > old };
            self.sr = if carry { self.sr | SR_C | SR_X } else { self.sr & !(SR_C | SR_X) };
        }
    }

    fn execute_shift(&mut self, opcode: u16) {
        let size_bits = (opcode >> 6) & 3;
        if size_bits == 3 {
            // memory shift (single bit, LSL/LSR/ASL/ASR/ROL/ROR <ea>) -- rare in driver code, skip.
            return;
        }
        let size: u32 = match size_bits { 0 => 1, 1 => 2, _ => 4 };
        let reg = (opcode & 7) as usize;
        let dir_left = opcode & 0x0100 != 0;
        let kind = (opcode >> 3) & 3; // 0=ASx 1=LSx 2=ROXx 3=ROx
        let count_field = ((opcode >> 9) & 7) as u32;
        let use_reg_count = opcode & 0x0020 != 0;
        let count = if use_reg_count { self.d[count_field as usize] % 64 } else if count_field == 0 { 8 } else { count_field };

        let mut val = self.d[reg] & size_mask(size);
        let bits = size * 8;
        let mut carry = false;
        for _ in 0..count {
            let msb = val & (1 << (bits - 1)) != 0;
            let lsb = val & 1 != 0;
            match (kind, dir_left) {
                (1, true) => { carry = msb; val = (val << 1) & size_mask(size); } // LSL
                (1, false) => { carry = lsb; val >>= 1; } // LSR
                (0, true) => { carry = msb; val = (val << 1) & size_mask(size); } // ASL (same bit motion as LSL)
                (0, false) => { // ASR: replicate sign bit
                    carry = lsb;
                    let sign = val & (1 << (bits - 1));
                    val = (val >> 1) | sign;
                }
                (3, true) => { carry = msb; val = ((val << 1) | (msb as u32)) & size_mask(size); } // ROL
                (3, false) => { carry = lsb; val = (val >> 1) | ((lsb as u32) << (bits - 1)); } // ROR
                _ => {}
            }
        }
        self.d[reg] = (self.d[reg] & !size_mask(size)) | val;
        self.set_nz_sized(val, size);
        self.sr = if carry { self.sr | SR_C } else { self.sr & !SR_C };
        self.sr &= !SR_V;
    }

    fn push_long(&mut self, val: u32) {
        self.a[7] = self.a[7].wrapping_sub(4);
        self.write_long(self.a[7], val);
    }
    fn pop_long(&mut self) -> u32 {
        let val = self.read_long(self.a[7]);
        self.a[7] = self.a[7].wrapping_add(4);
        val
    }
    fn pop_word(&mut self) -> u16 {
        let val = self.read_word(self.a[7]);
        self.a[7] = self.a[7].wrapping_add(2);
        val
    }
}

fn size_mask(size: u32) -> u32 {
    match size {
        1 => 0xFF,
        2 => 0xFFFF,
        _ => 0xFFFF_FFFF,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn make_cpu() -> M68k {
        M68k::new(Arc::new(WorkRam::new()))
    }

    #[test]
    fn reset_reads_ssp_and_pc_from_address_zero() {
        let mut cpu = make_cpu();
        cpu.write_long(0, 0x0004_0000);
        cpu.write_long(4, 0x0000_0100);
        cpu.reset();
        assert_eq!(cpu.a[7], 0x0004_0000);
        assert_eq!(cpu.pc, 0x0000_0100);
        assert!(cpu.sr & SR_S != 0, "reset must enter supervisor mode");
    }

    #[test]
    fn moveq_loads_sign_extended_immediate() {
        let mut cpu = make_cpu();
        cpu.reset();
        cpu.pc = 0x100;
        cpu.write_word(0x100, 0x7EFF); // MOVEQ #-1,D7
        cpu.step();
        assert_eq!(cpu.d[7], 0xFFFF_FFFF);
        assert!(cpu.sr & SR_N != 0);
    }

    #[test]
    fn move_l_dn_to_an_indirect() {
        let mut cpu = make_cpu();
        cpu.reset();
        cpu.d[0] = 0xCAFEBABE;
        cpu.a[1] = 0x1000;
        cpu.pc = 0x100;
        cpu.write_word(0x100, 0x2280); // MOVE.L D0,(A1)
        cpu.step();
        assert_eq!(cpu.read_long(0x1000), 0xCAFEBABE);
    }

    #[test]
    fn bra_short_branches_relative_to_next_instruction() {
        let mut cpu = make_cpu();
        cpu.reset();
        cpu.pc = 0x100;
        cpu.write_word(0x100, 0x6004); // BRA +4
        cpu.step();
        assert_eq!(cpu.pc, 0x106, "BRA target = pc-after-opcode(0x102) + disp(4)");
    }

    #[test]
    fn dbra_loops_until_counter_reaches_minus_one() {
        let mut cpu = make_cpu();
        cpu.reset();
        cpu.d[0] = 2;
        cpu.pc = 0x100;
        cpu.write_word(0x100, 0x51C8); // DBRA D0,<here>
        cpu.write_word(0x102, 0xFFFE); // disp -2 (branch back to 0x100)
        cpu.step(); // d0: 2->1, branches back
        assert_eq!(cpu.d[0] as u16, 1);
        assert_eq!(cpu.pc, 0x100);
        cpu.step(); // d0: 1->0, branches back
        assert_eq!(cpu.d[0] as u16, 0);
        assert_eq!(cpu.pc, 0x100);
        cpu.step(); // d0: 0->0xFFFF, falls through (no branch)
        assert_eq!(cpu.d[0] as u16, 0xFFFF);
        assert_eq!(cpu.pc, 0x104);
    }

    #[test]
    fn jsr_and_rts_roundtrip() {
        let mut cpu = make_cpu();
        cpu.reset();
        cpu.a[7] = 0x2000;
        cpu.pc = 0x100;
        cpu.write_word(0x100, 0x4EB9); // JSR abs.L
        cpu.write_long(0x102, 0x0000_0200);
        cpu.write_word(0x200, 0x4E75); // RTS
        cpu.step(); // JSR
        assert_eq!(cpu.pc, 0x200);
        cpu.step(); // RTS
        assert_eq!(cpu.pc, 0x106, "must return past the JSR's 6-byte instruction");
    }

    #[test]
    fn tas_sets_top_bit_and_flags() {
        let mut cpu = make_cpu();
        cpu.reset();
        cpu.a[0] = 0x1000;
        cpu.write_byte(0x1000, 0x00);
        cpu.pc = 0x100;
        cpu.write_word(0x100, 0x4AD0); // TAS (A0)
        cpu.step();
        assert_eq!(cpu.read_byte(0x1000), 0x80);
        assert!(cpu.sr & SR_Z != 0, "original value was 0, Z must be set from the pre-set-bit read");
    }

    #[test]
    fn scsp_register_window_is_shared_with_the_sh2_side() {
        // Real hardware: M68K addresses >= 0x100000 hit the SCSP's own
        // register block, the same dual-ported registers the SH-2 sees at
        // physical 0x05B00000 -- cross-checked against Yabause's
        // `c68k_byte_read`/`c68k_byte_write` (`scsp.c`). This is the actual
        // SH-2<->M68K communication path a boot sound-driver handshake
        // would use; the M68K has no access to SH-2 Work RAM at all.
        let work_ram = Arc::new(WorkRam::new());
        let mut cpu = M68k::new(work_ram.clone());
        cpu.reset();
        cpu.d[0] = 0x42;
        cpu.a[0] = 0x10_0010; // M68K-side SCSP register offset 0x10
        cpu.pc = 0x100;
        cpu.write_word(0x100, 0x1080); // MOVE.B D0,(A0)
        cpu.step();
        let ram = work_ram.scsp_regs.read().unwrap();
        assert_eq!(ram[0x10], 0x42, "M68K write at 0x100010 must land in the shared scsp_regs[0x10], the same array the SH-2 side reads/writes");
    }

    #[test]
    fn mcipd_write_fires_sound_req_irq_only_when_mcieb_enables_it() {
        // Regression test for the real main-CPU interrupt handshake (see
        // `scsp_main_interrupt` in a real, working SCSP implementation):
        // a bit written to MCIPD (offset 0x2C) only actually raises the
        // SH-2's Sound Request interrupt if that same bit is set in MCIEB
        // (offset 0x2A) -- the SH-2's own "which sound events do I care
        // about" mask, set up before the driver handshake begins.
        let work_ram = Arc::new(WorkRam::new());
        let mut cpu = M68k::new(work_ram.clone());
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        cpu.sound_req_irq = Some(flag.clone());
        cpu.reset();

        // MCIPD bit 0x20 set, but MCIEB never enabled it: must NOT fire.
        cpu.write_byte(0x0010_002D, 0x20); // MCIPD low byte
        assert!(!flag.load(std::sync::atomic::Ordering::Relaxed), "MCIPD alone, without MCIEB, must not raise the interrupt");

        // Now enable it in MCIEB, then request it again via MCIPD: must fire.
        cpu.write_byte(0x0010_002B, 0x20); // MCIEB low byte
        cpu.write_byte(0x0010_002D, 0x20); // MCIPD low byte again
        assert!(flag.load(std::sync::atomic::Ordering::Relaxed), "MCIPD with the matching MCIEB bit enabled must raise the interrupt");
    }
}
