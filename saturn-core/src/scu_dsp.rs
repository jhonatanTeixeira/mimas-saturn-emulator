//! Real SCU DSP interpreter -- the 32-bit VLIW co-processor real Saturn
//! hardware uses for transform/DMA-style number crunching, a distinct
//! component from both SH-2 cores and the SCSP's M68000 (Core 2's slot in
//! `SaturnSystem`, parked until this landed).
//!
//! Traced via a real boot wall: a BIOS wait loop polled the DSP Program
//! Control Port's `EX` (execute) bit, waiting for it to clear -- real
//! hardware clears `EX` when the running DSP program reaches an End
//! instruction. See `.development/current_blocker.md` for the full trace
//! (dumping High RAM at the stuck PC, finding the real, uploaded 32-word
//! program, and decoding its instruction mix).
//!
//! Cross-checked instruction-by-instruction against Yabause's `scu.c`
//! (`ScuDsp` struct in `scu.h`, the DSP exec block inside `ScuExec`,
//! `readgensrc`/`writed1busdest`/`writeloadimdest`, `dsp_dma01`-`dsp_dma08`)
//! -- not transliterated; see each method's doc comment for the exact
//! source it mirrors.
//!
//! Scope: the full ALU/operation/load-immediate/jump/loop/end instruction
//! groups are implemented (Program RAM is only 256 words -- not a large
//! surface). Only 2 of real hardware's 8 DMA addressing-mode variants
//! (Yabause's `dsp_dma03`/`dsp_dma04`) are implemented, matching what the
//! real BIOS DSP program traced during this wall actually uses. The other
//! 6 are a known gap -- add them the same way as everything else in this
//! project: hit the wall, decode, cross-check Yabause, implement, test.

use crate::shared_buffers::WorkRam;

/// Real hardware: `EX` bit (program execute control), bit 16 of the
/// Program Control Port.
const PCP_EX: u32 = 0x0001_0000;
/// `LE` bit (PC load enable), bit 15.
const PCP_LE: u32 = 0x0000_8000;
/// `E` bit (program end interrupt flag), bit 18.
const PCP_E: u32 = 0x0004_0000;
/// `T0` bit (D0-bus/DMA busy flag), bit 23.
const PCP_T0: u32 = 0x0080_0000;
/// `Z`/`S`/`C` flag bits, 21/22/20.
const PCP_Z: u32 = 0x0020_0000;
const PCP_S: u32 = 0x0040_0000;
const PCP_C: u32 = 0x0010_0000;
/// Mask of bits a CPU write to the Program Control Port may actually set
/// (P/LE/EX/ES/EP/PR) -- status bits (E/V/C/Z/S/T0) are read-only from
/// this path, preserved across a write. Cross-checked against Yabause's
/// `scu.c` `case 0x80` write handler: `(val & 0x060380FF)`.
const PCP_WRITABLE_MASK: u32 = 0x0603_80FF;
/// Mask of bits `Vdp1ReadWord`-style status readback exposes (matches
/// Yabause's `case 0x80` read handler: `all & 0x00FD00FF`).
const PCP_READABLE_MASK: u32 = 0x00FD_00FF;

pub struct ScuDsp {
    pub pc: u8,
    pub program_ram: Box<[u32; 256]>,
    pub md: Box<[[u32; 64]; 4]>,
    pub ct: [u8; 4],
    pub rx: i32,
    pub ry: i32,
    pub ra0: u32,
    pub wa0: u32,
    /// Internal working copy of `ra0`/`wa0` a DMA transfer advances through
    /// (real hardware keeps the CPU-visible register and the in-flight DMA
    /// cursor separate -- see Yabause's `RA0M`/`WA0M`).
    ra0m: u32,
    wa0m: u32,
    pub lop: u16,
    pub top: u8,
    /// 48-bit-effective accumulators, stored as `i64` with only the low 48
    /// bits meaningful -- mirrors Yabause's `s64 all` union exactly (see
    /// `scu.h`'s `AC`/`P`/`ALU` bitfield unions: `.part.L` is the low 32
    /// bits, `.part.H` the next 16). ALU ops that only assign `.part.L` in
    /// the reference leave the upper bits untouched here too (`set_low32`).
    pub ac: i64,
    pub p: i64,
    pub alu: i64,
    /// Program Control Port bits, real layout (see this module's doc
    /// comment / the `PCP_*` constants).
    pub prog_control: u32,
    pub data_ram_page: u8,
    pub data_ram_addr: u8,
    /// `None` == no pending delayed jump (real hardware: `jmpaddr == -1`).
    jmpaddr: Option<u8>,
    delayed: bool,
    /// DMA busy countdown and the instruction that triggered it -- mirrors
    /// Yabause's `dsp_dma_wait`/`dsp_dma_instruction`/`dsp_dma_size`, so a
    /// DMA "instruction" and its actual data movement are two separate
    /// steps (real hardware's DMA takes real time; the DSP keeps executing
    /// other instructions while `T0` is set).
    dsp_dma_wait: i32,
    dsp_dma_instruction: u32,
    dsp_dma_size: u32,
}

impl ScuDsp {
    pub fn new() -> Self {
        Self {
            pc: 0,
            program_ram: Box::new([0; 256]),
            md: Box::new([[0; 64]; 4]),
            ct: [0; 4],
            rx: 0,
            ry: 0,
            ra0: 0,
            wa0: 0,
            ra0m: 0,
            wa0m: 0,
            lop: 0,
            top: 0,
            ac: 0,
            p: 0,
            alu: 0,
            prog_control: 0,
            data_ram_page: 0,
            data_ram_addr: 0,
            jmpaddr: None,
            delayed: false,
            dsp_dma_wait: 0,
            dsp_dma_instruction: 0,
            dsp_dma_size: 0,
        }
    }

    pub fn is_executing(&self) -> bool {
        self.prog_control & PCP_EX != 0
    }

    // ---- Register port access (SCU offsets 0x80/0x84/0x88/0x8C) ----

    /// Offset 0x80 read: `scu.c`'s `case 0x80: return (ScuDsp->ProgControlPort.all & 0x00FD00FF);`
    pub fn read_control_port(&self) -> u32 {
        self.prog_control & PCP_READABLE_MASK
    }

    /// Offset 0x80 write: `scu.c`'s `case 0x80` handler -- merges the
    /// writable bits, optionally reloads `PC` from the `P` field if `LE`
    /// is set, and resets any pending delayed jump when execution starts.
    pub fn write_control_port(&mut self, val: u32) {
        self.prog_control = (self.prog_control & !PCP_WRITABLE_MASK) | (val & PCP_WRITABLE_MASK);
        if self.prog_control & PCP_LE != 0 {
            self.pc = (self.prog_control & 0xFF) as u8;
        }
        if val & PCP_EX != 0 {
            self.jmpaddr = None;
        }
    }

    /// Offset 0x84 write: `scu.c`'s `case 0x84` -- writes one Program RAM
    /// word at the current PC, then auto-increments (real hardware lets
    /// the CPU upload a whole program via repeated writes to this same
    /// address).
    pub fn write_program_ram_port(&mut self, val: u32) {
        self.program_ram[self.pc as usize] = val;
        self.pc = self.pc.wrapping_add(1);
        self.prog_control = (self.prog_control & !0xFF) | (self.pc as u32);
    }

    /// Offset 0x88 write: `scu.c`'s `case 0x88`.
    pub fn write_data_ram_addr_port(&mut self, val: u32) {
        self.data_ram_page = ((val >> 6) & 3) as u8;
        self.data_ram_addr = (val & 0x3F) as u8;
    }

    /// Offset 0x8C write: `scu.c`'s `case 0x8C` -- blocked while executing
    /// (real hardware: the CPU can't poke Data RAM while the DSP owns it).
    pub fn write_data_ram_data_port(&mut self, val: u32) {
        if self.is_executing() {
            return;
        }
        self.md[self.data_ram_page as usize][self.data_ram_addr as usize] = val;
        self.data_ram_addr = self.data_ram_addr.wrapping_add(1) & 0x3F;
    }

    /// Offset 0x8C read: `scu.c`'s `case 0x8C` -- also blocked while
    /// executing (reads 0 instead).
    pub fn read_data_ram_data_port(&mut self) -> u32 {
        if self.is_executing() {
            return 0;
        }
        let val = self.md[self.data_ram_page as usize][self.data_ram_addr as usize];
        self.data_ram_addr = self.data_ram_addr.wrapping_add(1) & 0x3F;
        val
    }

    // ---- Execution ----

    fn set_low32(acc: &mut i64, low: i32) {
        *acc = (*acc & !0xFFFF_FFFFi64) | ((low as u32) as i64);
    }

    fn low32(acc: i64) -> i32 {
        acc as i32
    }

    /// `scu.c`'s `readgensrc` -- general-purpose ALU/bus source read.
    fn read_gen_src(&mut self, num: u32) -> u32 {
        if num <= 7 {
            let bank = (num & 0x3) as usize;
            let val = self.md[bank][(self.ct[bank] & 0x3F) as usize];
            if (num >> 2) & 1 != 0 {
                self.ct[bank] = self.ct[bank].wrapping_add(1) & 0x3F;
            }
            val
        } else if num == 0x9 {
            Self::low32(self.alu) as u32 // ALL
        } else if num == 0xA {
            (self.alu >> 16) as u32 // ALH
        } else {
            0xFFFF_FFFF
        }
    }

    /// `scu.c`'s `writed1busdest` -- D1-bus store destinations.
    fn write_d1_bus_dest(&mut self, num: u32, val: u32) {
        match num {
            0x0 => { self.md[0][(self.ct[0] & 0x3F) as usize] = val; self.ct[0] = self.ct[0].wrapping_add(1) & 0x3F; }
            0x1 => { self.md[1][(self.ct[1] & 0x3F) as usize] = val; self.ct[1] = self.ct[1].wrapping_add(1) & 0x3F; }
            0x2 => { self.md[2][(self.ct[2] & 0x3F) as usize] = val; self.ct[2] = self.ct[2].wrapping_add(1) & 0x3F; }
            0x3 => { self.md[3][(self.ct[3] & 0x3F) as usize] = val; self.ct[3] = self.ct[3].wrapping_add(1) & 0x3F; }
            0x4 => self.rx = val as i32,
            0x5 => self.p = val as i32 as i64,
            0x6 => self.ra0 = val,
            0x7 => self.wa0 = val,
            0xA => self.lop = val as u16,
            0xB => self.top = val as u8,
            0xC => self.ct[0] = val as u8,
            0xD => self.ct[1] = val as u8,
            0xE => self.ct[2] = val as u8,
            0xF => self.ct[3] = val as u8,
            _ => {}
        }
    }

    /// `scu.c`'s `writeloadimdest` -- MVI (load-immediate) destinations.
    /// Destination `0xC` is the DSP's "call" form (`scu.h`/`scu.c`: sets
    /// `TOP` as a return address, then jumps).
    fn write_load_im_dest(&mut self, num: u32, val: u32) {
        match num {
            0x0 => self.md[0][(self.ct[0] & 0x3F) as usize] = val,
            0x1 => self.md[1][(self.ct[1] & 0x3F) as usize] = val,
            0x2 => self.md[2][(self.ct[2] & 0x3F) as usize] = val,
            0x3 => self.md[3][(self.ct[3] & 0x3F) as usize] = val,
            0x4 => self.rx = val as i32,
            0x5 => self.p = val as i32 as i64,
            0x6 => self.ra0 = val & 0x01FF_FFFF,
            0x7 => self.wa0 = val & 0x01FF_FFFF,
            0xA => self.lop = (val & 0x0FFF) as u16,
            0xC => {
                self.top = self.pc.wrapping_add(1);
                self.jmpaddr = Some(val as u8);
                self.delayed = false;
            }
            _ => {}
        }
    }

    /// Execute exactly one DSP instruction. Only meaningful while `EX` is
    /// set -- callers (Core 2's thread loop) should check `is_executing()`
    /// first, matching real hardware only clocking the DSP while it's
    /// actually running a program.
    pub fn step(&mut self, work_ram: &WorkRam) {
        if !self.is_executing() {
            return;
        }

        if self.prog_control & PCP_T0 != 0 {
            self.step_dma(work_ram);
        }

        let instruction = self.program_ram[self.pc as usize];

        // ALU op always computes (real hardware: a VLIW slot that runs
        // every cycle regardless of whether anything captures its
        // result) -- `scu.c`'s unconditional `ScuDsp->ALU.all = ScuDsp->AC.all;`
        // followed by the ALU switch.
        self.alu = self.ac;
        self.execute_alu(instruction);

        let top2 = (instruction >> 30) & 0x3;
        match top2 {
            0x0 => self.execute_operation(instruction, work_ram),
            0x2 => self.execute_load_immediate(instruction),
            0x3 => self.execute_other(instruction, work_ram),
            _ => {}
        }

        self.pc = self.pc.wrapping_add(1);

        // Delayed jump slot -- `scu.c`'s exact two-step handling: the
        // instruction right after a jump/loop-back still executes before
        // the jump actually lands.
        if let Some(target) = self.jmpaddr {
            if self.delayed {
                self.pc = target;
                self.jmpaddr = None;
            } else {
                self.delayed = true;
            }
        }
    }

    /// ALU op group (bits 29-26), cross-checked against `scu.c`'s switch
    /// on `instruction >> 26`.
    fn execute_alu(&mut self, instruction: u32) {
        let op = (instruction >> 26) & 0xF;
        let ac_l = Self::low32(self.ac);
        let p_l = Self::low32(self.p);
        match op {
            0x0 => {} // NOP -- ALU already holds AC passthrough
            0x1 => { // AND
                let r = (ac_l as u32) & (p_l as u32);
                Self::set_low32(&mut self.alu, r as i32);
                self.set_zsc(r == 0, (r as i32) < 0, false);
            }
            0x2 => { // OR
                let r = (ac_l as u32) | (p_l as u32);
                Self::set_low32(&mut self.alu, r as i32);
                self.set_zsc(r == 0, (r as i32) < 0, false);
            }
            0x3 => { // XOR
                let r = (ac_l as u32) ^ (p_l as u32);
                Self::set_low32(&mut self.alu, r as i32);
                self.set_zsc(r == 0, (r as i32) < 0, false);
            }
            0x4 => { // ADD
                let r = ac_l.wrapping_add(p_l);
                Self::set_low32(&mut self.alu, r);
                let carry = ((ac_l as u32 as u64) + (p_l as u32 as u64)) & 0x1_0000_0000 != 0;
                self.set_zsc(r == 0, r < 0, carry);
            }
            0x5 => { // SUB
                let r = ac_l.wrapping_sub(p_l);
                Self::set_low32(&mut self.alu, r);
                let carry = ((ac_l as u32 as u64).wrapping_sub(p_l as u32 as u64)) & 0x1_0000_0000 != 0;
                self.set_zsc(r == 0, r < 0, carry);
            }
            0x6 => { // AD2 -- full 48-bit add, uses .all directly
                let r = self.ac.wrapping_add(self.p);
                self.alu = r;
                let carry = (((self.ac & 0xFFFF_FFFF_FFFF) as u64) + ((self.p & 0xFFFF_FFFF_FFFF) as u64)) & 0x1_0000_0000_0000 != 0;
                self.set_zsc(r == 0, r & 0x8000_0000_0000 != 0, carry);
            }
            0x8 => { // SR
                let carry = ac_l & 1 != 0;
                let r = ((ac_l as u32 & 0x8000_0000) | ((ac_l as u32) >> 1)) as i32;
                Self::set_low32(&mut self.alu, r);
                self.set_zsc(r == 0, r < 0, carry);
            }
            0x9 => { // RR
                let carry_in = ac_l & 1 != 0;
                let r = (((carry_in as u32) << 31) | ((ac_l as u32) >> 1)) as i32;
                Self::set_low32(&mut self.alu, r);
                self.set_zsc(r == 0, r < 0, carry_in);
            }
            0xA => { // SL
                let carry = (ac_l as u32) & 0x8000_0000 != 0;
                let r = ((ac_l as u32) << 1) as i32;
                Self::set_low32(&mut self.alu, r);
                self.set_zsc(r == 0, r < 0, carry);
            }
            0xB => { // RL
                let carry = (ac_l as u32) & 0x8000_0000 != 0;
                let r = (((ac_l as u32) << 1) | (carry as u32)) as i32;
                Self::set_low32(&mut self.alu, r);
                self.set_zsc(r == 0, r < 0, carry);
            }
            0xF => { // RL8
                let carry = (ac_l as u32) & 0x0100_0000 != 0;
                let r = (((ac_l as u32) << 8) | (((ac_l as u32) >> 24) & 0xFF)) as i32;
                Self::set_low32(&mut self.alu, r);
                self.set_zsc(r == 0, r < 0, carry);
            }
            _ => {}
        }
    }

    fn set_zsc(&mut self, z: bool, s: bool, c: bool) {
        self.prog_control = (self.prog_control & !(PCP_Z | PCP_S | PCP_C))
            | if z { PCP_Z } else { 0 }
            | if s { PCP_S } else { 0 }
            | if c { PCP_C } else { 0 };
    }

    /// Operation Commands (top2 == 0b00): X/Y/D1-bus micro-ops, cross-
    /// checked against `scu.c`'s `case 0x00` block.
    fn execute_operation(&mut self, instruction: u32, work_ram: &WorkRam) {
        match (instruction >> 23) & 0x3 {
            2 => self.p = (self.rx as i64).wrapping_mul(self.ry as i64), // MOV MUL,P
            3 => self.p = self.read_gen_src((instruction >> 20) & 0x7) as i32 as i64, // MOV [s],P
            _ => {}
        }
        if (instruction >> 23) & 0x4 != 0 {
            self.rx = self.read_gen_src((instruction >> 20) & 0x7) as i32; // MOV [s],X
        }
        if (instruction >> 17) & 0x4 != 0 {
            self.ry = self.read_gen_src((instruction >> 14) & 0x7) as i32; // MOV [s],Y
        }
        match (instruction >> 17) & 0x3 {
            1 => self.ac = 0, // CLR A
            2 => self.ac = self.alu, // MOV ALU,A
            3 => self.ac = self.read_gen_src((instruction >> 14) & 0x7) as i32 as i64, // MOV [s],A
            _ => {}
        }
        match (instruction >> 12) & 0x3 {
            1 => { // MOV SImm,[d]
                let imm = (instruction & 0xFF) as i8 as i32 as u32;
                self.write_d1_bus_dest((instruction >> 8) & 0xF, imm);
            }
            3 => { // MOV [s],[d]
                let src = self.read_gen_src(instruction & 0xF);
                self.write_d1_bus_dest((instruction >> 8) & 0xF, src);
            }
            _ => {}
        }
        let _ = work_ram; // this instruction group never touches main RAM directly
    }

    /// Load Immediate Commands (top2 == 0b10), cross-checked against
    /// `scu.c`'s `case 0x02` block (conditional MVI variants + plain MVI).
    fn execute_load_immediate(&mut self, instruction: u32) {
        let dest = (instruction >> 26) & 0xF;
        if (instruction >> 25) & 1 != 0 {
            let cond = (instruction >> 19) & 0x3F;
            let imm = (instruction & 0x7_FFFF) | if instruction & 0x4_0000 != 0 { 0xFFF8_0000 } else { 0 };
            let z = self.prog_control & PCP_Z != 0;
            let s = self.prog_control & PCP_S != 0;
            let c = self.prog_control & PCP_C != 0;
            let t0 = self.prog_control & PCP_T0 != 0;
            let take = match cond {
                0x01 => !z,
                0x02 => !s,
                0x03 => !z && !s,
                0x04 => !c,
                0x08 => !t0,
                0x21 => z,
                0x22 => s,
                0x23 => z || s,
                0x24 => c,
                0x28 => t0,
                _ => false,
            };
            if take {
                self.write_load_im_dest(dest, imm);
            }
        } else {
            let raw = instruction & 0x01FF_FFFF;
            let imm = if raw & 0x0100_0000 != 0 { raw | 0xFE00_0000 } else { raw };
            self.write_load_im_dest(dest, imm);
        }
    }

    /// "Other" group (top2 == 0b11): DMA/JMP/Loop/End, cross-checked
    /// against `scu.c`'s `case 0x03` block.
    fn execute_other(&mut self, instruction: u32, work_ram: &WorkRam) {
        match (instruction >> 28) & 0xF {
            0xC => self.start_dma(instruction), // DMA Commands
            0xD => self.execute_jump(instruction), // Jump Commands
            0xE => { // Loop bottom Commands
                if instruction & 0x0800_0000 != 0 { // LPS
                    if self.lop != 0 {
                        self.jmpaddr = Some(self.pc);
                        self.delayed = false;
                        self.lop -= 1;
                    }
                } else if self.lop != 0 { // BTM
                    self.jmpaddr = Some(self.top);
                    self.delayed = false;
                    self.lop -= 1;
                }
            }
            0xF => { // End Commands
                self.prog_control &= !PCP_EX;
                if instruction & 0x0800_0000 != 0 {
                    self.prog_control |= PCP_E;
                    // Real hardware also raises the SCU DSP-End interrupt
                    // here (`ScuSendDSPEnd`) -- not wired to an SH-2
                    // interrupt vector yet; the BIOS program traced for
                    // this wall uses plain END (no interrupt requested).
                }
                self.prog_control = (self.prog_control & !0xFF) | (self.pc as u32);
            }
            _ => {}
        }
        let _ = work_ram; // DMA is the only sub-case touching main RAM; handled in start_dma/step_dma
    }

    fn execute_jump(&mut self, instruction: u32) {
        if self.jmpaddr.is_some() {
            return; // a jump is already pending this cycle
        }
        let cond = (instruction >> 19) & 0x7F;
        let target = (instruction & 0xFF) as u8;
        let z = self.prog_control & PCP_Z != 0;
        let s = self.prog_control & PCP_S != 0;
        let c = self.prog_control & PCP_C != 0;
        let t0 = self.prog_control & PCP_T0 != 0;
        let take = match cond {
            0x00 => true,
            0x41 => !z,
            0x42 => !s,
            0x43 => !z && !s,
            0x44 => !c,
            0x48 => !t0,
            0x61 => z,
            0x62 => s,
            0x63 => z || s,
            0x64 => c,
            0x68 => t0,
            _ => false,
        };
        if take {
            self.jmpaddr = Some(target);
            self.delayed = false;
        }
    }

    // ---- DMA (only the 2 real-BIOS-observed variants) ----

    fn start_dma(&mut self, instruction: u32) {
        // Finish a still-in-flight previous DMA first, matching real
        // hardware's "each DMA instruction stalls behind the last".
        if self.dsp_dma_wait > 0 {
            self.dsp_dma_wait = 0;
        }
        self.dsp_dma_instruction = instruction;
        self.prog_control |= PCP_T0;

        let d1 = (instruction >> 10) & 0x1F;
        let d2 = (instruction >> 11) & 0xF;
        let counter = if d1 == 0x00 || d1 == 0x04 || d2 == 0x08 || d1 == 0x14 {
            instruction & 0xFF
        } else if d2 == 0x04 || d1 == 0x0C || d2 == 0x0C || d1 == 0x1C {
            let bank = (instruction & 0x7) as usize;
            match instruction & 0x7 {
                0x00..=0x03 => self.md[bank & 0x3][(self.ct[bank & 0x3] & 0x3F) as usize],
                0x04..=0x07 => {
                    let b = bank & 0x3;
                    let v = self.md[b][(self.ct[b] & 0x3F) as usize];
                    self.ct[b] = self.ct[b].wrapping_add(1) & 0x3F;
                    v
                }
                _ => 0,
            }
        } else {
            0
        };
        self.dsp_dma_size = counter;
        self.dsp_dma_wait = 2;
        self.wa0m = self.wa0;
        self.ra0m = self.ra0;
    }

    /// Called once per `step()` while `T0` is set -- mirrors `scu.c`'s
    /// `step_dsp_dma`'s countdown-then-fire shape (real DMA takes a few
    /// real cycles, it isn't instantaneous even though this interpreter
    /// doesn't model exact per-word timing).
    fn step_dma(&mut self, work_ram: &WorkRam) {
        if self.prog_control & PCP_T0 == 0 {
            return;
        }
        self.dsp_dma_wait -= 1;
        if self.dsp_dma_wait > 0 {
            return;
        }
        let instruction = self.dsp_dma_instruction;
        let d1 = (instruction >> 10) & 0x1F;
        let d2 = (instruction >> 11) & 0xF;
        if d2 == 0x04 {
            self.dma_read_from_main_ram(instruction, work_ram);
        } else if d1 == 0x0C {
            self.dma_write_to_main_ram(instruction, work_ram);
        }
        // Other 6 real addressing-mode variants (dsp_dma01/02/05/06/07/08
        // in Yabause's naming) aren't implemented yet -- not exercised by
        // the real BIOS program that unblocked this wall. Add the same
        // way as everything else here: hit it, decode, cross-check
        // Yabause, implement, test.
        self.prog_control &= !PCP_T0;
        self.dsp_dma_instruction = 0;
        self.dsp_dma_wait = 0;
    }

    /// Yabause's `dsp_dma03`: Main RAM -> Data RAM / Program RAM, reading
    /// `dsp_dma_size` longwords starting at `ra0m<<2`.
    fn dma_read_from_main_ram(&mut self, instruction: u32, work_ram: &WorkRam) {
        let sel = ((instruction >> 8) & 0x7) as usize;
        let mode = (instruction >> 15) & 0x7;
        let add = (1u32 << (mode & 0x2)) & !1;
        let mut index = 0usize;
        for _ in 0..self.dsp_dma_size {
            let val = read_long(work_ram, self.ra0m << 2);
            if sel == 0x4 {
                if index < self.program_ram.len() {
                    self.program_ram[index] = val;
                }
                index += 1;
            } else {
                let bank = sel & 0x3;
                self.md[bank][(self.ct[bank] & 0x3F) as usize] = val;
                self.ct[bank] = self.ct[bank].wrapping_add(1) & 0x3F;
            }
            self.ra0m = self.ra0m.wrapping_add(add >> 2);
        }
        self.ra0 = self.ra0m;
    }

    /// Yabause's `dsp_dma04` + `dsp_dma_write_d0bus`: Data RAM -> Main RAM,
    /// writing `dsp_dma_size` longwords starting at `wa0m<<2`, with the
    /// same A-bus/B-bus/CPU-bus address-range split the reference uses
    /// (B-bus writes as two 16-bit halves, matching real SCSP/Sound RAM
    /// 16-bit port width).
    fn dma_write_to_main_ram(&mut self, instruction: u32, work_ram: &WorkRam) {
        let sel = ((instruction >> 8) & 0x3) as usize;
        let mode = (instruction >> 15) & 0x7;
        let mut add: u32 = match mode {
            0 => 0, 1 => 1, 2 => 2, 3 => 4, 4 => 8, 5 => 16, 6 => 32, 7 => 64, _ => 0,
        };
        let count = self.dsp_dma_size;
        let addr = (self.wa0m << 2) & 0x0FFF_FFFF;
        if (0x0200_0000..0x05A0_0000).contains(&addr) {
            if add > 1 { add = 1; }
            for _ in 0..count {
                let val = self.md[sel][(self.ct[sel] & 0x3F) as usize];
                write_long(work_ram, self.wa0m << 2, val);
                self.ct[sel] = self.ct[sel].wrapping_add(1) & 0x3F;
                self.wa0m = self.wa0m.wrapping_add(add);
            }
        } else if (0x05A0_0000..0x0600_0000).contains(&addr) {
            let add = if add == 0 { 1 } else { add };
            let mut a = addr;
            for _ in 0..count {
                let val = self.md[sel][(self.ct[sel] & 0x3F) as usize];
                write_word(work_ram, a, (val >> 16) as u16);
                write_word(work_ram, a.wrapping_add(2), val as u16);
                self.ct[sel] = self.ct[sel].wrapping_add(1) & 0x3F;
                a = a.wrapping_add(add << 2);
            }
            self.wa0m = self.wa0m.wrapping_add(add * count);
        } else {
            let add = if add == 0 { 1 } else { add };
            for _ in 0..count {
                let val = self.md[sel][(self.ct[sel] & 0x3F) as usize];
                let a = (self.wa0m << 2) & 0x000F_FFFC;
                write_long(work_ram, 0x0600_0000 | a, val);
                self.ct[sel] = self.ct[sel].wrapping_add(1) & 0x3F;
                self.wa0m = self.wa0m.wrapping_add(if add == 1 { 1 } else { add >> 1 });
            }
        }
        self.wa0 = self.wa0m;
    }
}

impl Default for ScuDsp {
    fn default() -> Self {
        Self::new()
    }
}


/// Minimal main-RAM address decode for DSP DMA -- mirrors `Sh2::translate`'s
/// region boundaries (`sh2.rs`) for the regions real SCU DSP DMA can
/// plausibly target. Duplicated rather than shared because `Sh2`'s
/// `MemRegion`/`translate` are private to that module and this is a small,
/// stable set of ranges; if the two ever drift, `sh2.rs`'s `translate` is
/// the source of truth (cross-checked against Yabause's `memory.c`).
fn read_long(work_ram: &WorkRam, address: u32) -> u32 {
    let a = address & 0x0FFF_FFFF;
    if (0x0020_0000..0x0030_0000).contains(&a) {
        read_long_from(&work_ram.low_ram.read().unwrap()[..], (a - 0x0020_0000) as usize)
    } else if (0x05A0_0000..0x05B0_0000).contains(&a) {
        read_long_from(&work_ram.sound_ram.read().unwrap()[..], (a - 0x05A0_0000) as usize)
    } else if (0x05E0_0000..0x05F0_0000).contains(&a) {
        read_long_from(&work_ram.vdp2_vram.read().unwrap()[..], (a - 0x05E0_0000) as usize)
    } else if (0x05F0_0000..0x05F8_0000).contains(&a) {
        read_long_from(&work_ram.vdp2_cram.read().unwrap()[..], (a - 0x05F0_0000) as usize)
    } else if (0x0600_0000..0x0700_0000).contains(&a) {
        read_long_from(&work_ram.high_ram.read().unwrap()[..], (a - 0x0600_0000) as usize)
    } else {
        0
    }
}

fn write_long(work_ram: &WorkRam, address: u32, val: u32) {
    let a = address & 0x0FFF_FFFF;
    if (0x0020_0000..0x0030_0000).contains(&a) {
        write_long_to(&mut work_ram.low_ram.write().unwrap()[..], (a - 0x0020_0000) as usize, val);
    } else if (0x05A0_0000..0x05B0_0000).contains(&a) {
        write_long_to(&mut work_ram.sound_ram.write().unwrap()[..], (a - 0x05A0_0000) as usize, val);
    } else if (0x05E0_0000..0x05F0_0000).contains(&a) {
        write_long_to(&mut work_ram.vdp2_vram.write().unwrap()[..], (a - 0x05E0_0000) as usize, val);
    } else if (0x05F0_0000..0x05F8_0000).contains(&a) {
        write_long_to(&mut work_ram.vdp2_cram.write().unwrap()[..], (a - 0x05F0_0000) as usize, val);
    } else if (0x0600_0000..0x0700_0000).contains(&a) {
        write_long_to(&mut work_ram.high_ram.write().unwrap()[..], (a - 0x0600_0000) as usize, val);
    }
}

fn write_word(work_ram: &WorkRam, address: u32, val: u16) {
    let a = address & 0x0FFF_FFFF;
    if (0x0020_0000..0x0030_0000).contains(&a) {
        write_word_to(&mut work_ram.low_ram.write().unwrap()[..], (a - 0x0020_0000) as usize, val);
    } else if (0x05A0_0000..0x05B0_0000).contains(&a) {
        write_word_to(&mut work_ram.sound_ram.write().unwrap()[..], (a - 0x05A0_0000) as usize, val);
    } else if (0x0600_0000..0x0700_0000).contains(&a) {
        write_word_to(&mut work_ram.high_ram.write().unwrap()[..], (a - 0x0600_0000) as usize, val);
    }
}

fn read_long_from(buf: &[u8], off: usize) -> u32 {
    let mask = buf.len() - 1;
    let b0 = buf[off & mask] as u32;
    let b1 = buf[(off + 1) & mask] as u32;
    let b2 = buf[(off + 2) & mask] as u32;
    let b3 = buf[(off + 3) & mask] as u32;
    (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
}

fn write_long_to(buf: &mut [u8], off: usize, val: u32) {
    let mask = buf.len() - 1;
    buf[off & mask] = (val >> 24) as u8;
    buf[(off + 1) & mask] = (val >> 16) as u8;
    buf[(off + 2) & mask] = (val >> 8) as u8;
    buf[(off + 3) & mask] = val as u8;
}

fn write_word_to(buf: &mut [u8], off: usize, val: u16) {
    let mask = buf.len() - 1;
    buf[off & mask] = (val >> 8) as u8;
    buf[(off + 1) & mask] = val as u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_port_write_sets_ex_and_loads_pc_when_le_set() {
        let mut dsp = ScuDsp::new();
        // EX (bit16) + LE (bit15) + P=0x05 -- "load PC=5, start executing".
        dsp.write_control_port(0x0001_8005);
        assert!(dsp.is_executing());
        assert_eq!(dsp.pc, 5, "LE must reload PC from the P field");
    }

    #[test]
    fn control_port_write_without_le_does_not_move_pc() {
        let mut dsp = ScuDsp::new();
        dsp.pc = 10;
        // EX only, P=0x05, LE clear -- must NOT reload PC.
        dsp.write_control_port(0x0001_0005);
        assert!(dsp.is_executing());
        assert_eq!(dsp.pc, 10, "PC must stay put when LE isn't set");
    }

    #[test]
    fn control_port_read_masks_to_readable_bits_only() {
        let mut dsp = ScuDsp::new();
        dsp.write_control_port(0x0001_8000); // EX+LE
        dsp.prog_control |= PCP_Z | PCP_T0; // simulate status bits the exec loop would set
        let readback = dsp.read_control_port();
        assert_eq!(readback & PCP_READABLE_MASK, readback, "read must never expose bits outside 0x00FD00FF");
        assert_ne!(readback & PCP_EX, 0, "EX must read back set");
        assert_ne!(readback & PCP_Z, 0, "Z must read back set");
    }

    #[test]
    fn program_ram_port_write_auto_increments_pc() {
        let mut dsp = ScuDsp::new();
        dsp.write_program_ram_port(0x1111_1111);
        dsp.write_program_ram_port(0x2222_2222);
        dsp.write_program_ram_port(0x3333_3333);
        assert_eq!(dsp.program_ram[0], 0x1111_1111);
        assert_eq!(dsp.program_ram[1], 0x2222_2222);
        assert_eq!(dsp.program_ram[2], 0x3333_3333);
        assert_eq!(dsp.pc, 3, "each write must auto-increment PC, matching real hardware's upload port");
    }

    #[test]
    fn all_zero_program_never_clears_ex() {
        // Regression test for the exact wall this module fixes: a Program
        // RAM full of zero words decodes as an endless run of harmless
        // NOPs (verified against Yabause's exact bit layout -- top2 bits
        // 00 = Operation Command, ALU op 0 = NOP, no X/Y/D1-bus effect).
        // Without a real End instruction, EX must never clear on its own.
        let mut dsp = ScuDsp::new();
        let work_ram = WorkRam::new();
        dsp.write_control_port(0x0001_8000); // EX+LE, PC=0
        for _ in 0..1000 {
            dsp.step(&work_ram);
        }
        assert!(dsp.is_executing(), "an empty program must never clear EX by itself");
    }

    #[test]
    fn end_instruction_clears_ex() {
        let mut dsp = ScuDsp::new();
        let work_ram = WorkRam::new();
        dsp.program_ram[0] = 0xF000_0000; // End, no interrupt requested
        dsp.write_control_port(0x0001_8000); // EX+LE, PC=0
        dsp.step(&work_ram);
        assert!(!dsp.is_executing(), "an End instruction must clear EX");
        assert_eq!(dsp.prog_control & PCP_E, 0, "plain End (bit27 clear) must not raise the interrupt flag");
    }

    #[test]
    fn end_with_interrupt_sets_e_flag() {
        let mut dsp = ScuDsp::new();
        let work_ram = WorkRam::new();
        dsp.program_ram[0] = 0xF800_0000; // End with interrupt (bit 27 set)
        dsp.write_control_port(0x0001_8000);
        dsp.step(&work_ram);
        assert!(!dsp.is_executing());
        assert_ne!(dsp.prog_control & PCP_E, 0, "End-with-interrupt must set the E status bit");
    }

    #[test]
    fn add_alu_op_hand_traced() {
        // AC=5, P=3 -- hand-traced: ALU=8, Z=0 (nonzero), S=0 (positive),
        // C=0 (no unsigned carry: 5+3 doesn't overflow 32 bits).
        let mut dsp = ScuDsp::new();
        let work_ram = WorkRam::new();
        dsp.ac = 5;
        dsp.p = 3;
        // Operation Command (top2=00), ALU op ADD (bits29-26=0x4), no
        // X/Y/D1-bus side effects (all-zero elsewhere).
        dsp.program_ram[0] = 0x1000_0000;
        dsp.write_control_port(0x0001_8000);
        dsp.step(&work_ram);
        assert_eq!(ScuDsp::low32(dsp.alu), 8, "5 + 3 must land in ALU");
        assert_eq!(dsp.prog_control & PCP_Z, 0);
        assert_eq!(dsp.prog_control & PCP_S, 0);
        assert_eq!(dsp.prog_control & PCP_C, 0);
    }

    #[test]
    fn sub_alu_op_hand_traced() {
        // AC=5, P=8 -- hand-traced: ALU=5-8=-3 (negative), Z=0, S=1,
        // C=1 (unsigned borrow: (u64)5 - (u64)8 wraps, bit32 set).
        let mut dsp = ScuDsp::new();
        let work_ram = WorkRam::new();
        dsp.ac = 5;
        dsp.p = 8;
        // ALU op SUB (bits29-26=0x5).
        dsp.program_ram[0] = 0x1400_0000;
        dsp.write_control_port(0x0001_8000);
        dsp.step(&work_ram);
        assert_eq!(ScuDsp::low32(dsp.alu), -3, "5 - 8 must land in ALU as -3");
        assert_eq!(dsp.prog_control & PCP_Z, 0);
        assert_ne!(dsp.prog_control & PCP_S, 0, "-3 must set the sign flag");
        assert_ne!(dsp.prog_control & PCP_C, 0, "5 - 8 must set the borrow/carry flag");
    }

    #[test]
    fn mvi_immediate_writes_data_ram() {
        // Plain (unconditional) MVI: top2=10, dest=0 (MD[0]), imm=42.
        let mut dsp = ScuDsp::new();
        let work_ram = WorkRam::new();
        dsp.program_ram[0] = 0x8000_002A; // top2=10, dest bits(29-26)=0, imm=42
        dsp.write_control_port(0x0001_8000);
        dsp.step(&work_ram);
        assert_eq!(dsp.md[0][0], 42);
    }

    #[test]
    fn real_bios_dsp_program_runs_to_completion() {
        // Regression test for the actual wall: the real, uploaded 32-word
        // BIOS DSP program (dumped from High RAM at SH-2 address 0x06013280
        // during a real BIOS boot -- see `.development/current_blocker.md`)
        // and the exact 3 Data RAM parameter words Core 0 pre-loaded via
        // the Data RAM Data Port before triggering execution. Before this
        // module existed, EX never cleared (no DSP execution at all);
        // this program must now run to completion in a bounded number of
        // steps.
        const PROGRAM: [u32; 32] = [
            0x00001c00, 0x00003604, 0x00003704, 0x00001c02, 0x00001d00, 0x00861540,
            0x14003109, 0x00003100, 0x00001d00, 0x00003005, 0x00001c03, 0x00003005,
            0x00001c02, 0x83100000, 0x00001c03, 0x82100040, 0x00001c03, 0x00823500,
            0x10000000, 0xd308001f, 0x00000000, 0xd3400015, 0x00001f00, 0xc0012300,
            0xd3400018, 0x00001f00, 0xc000b300, 0xd340001b, 0x00000000, 0xd0000003,
            0x00000000, 0xf0000000,
        ];
        let mut dsp = ScuDsp::new();
        let work_ram = WorkRam::new();
        for (i, &word) in PROGRAM.iter().enumerate() {
            dsp.program_ram[i] = word;
        }
        dsp.md[0][0] = 0x0000_0000;
        dsp.md[0][1] = 0x0969_4000;
        dsp.md[0][2] = 0x0000_02AB;
        dsp.write_control_port(0x0001_8000); // EX+LE, PC=0

        let mut steps = 0;
        while dsp.is_executing() && steps < 100_000 {
            dsp.step(&work_ram);
            steps += 1;
        }
        assert!(!dsp.is_executing(), "the real BIOS DSP program must reach its End instruction, not loop forever");
    }
}
