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
//! surface), including all 8 of real hardware's DMA addressing-mode
//! variants (`dsp_dma01`-`dsp_dma08` in Yabause's naming, `scu.c:674-946`).

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
    /// Deferred `CT[n]` post-increment flags (real hardware's `incFlg`,
    /// `scu.c:62`/`:498` -- a set of flags raised by a `readgensrc`/
    /// `writed1busdest`/`writeloadimdest` call with post-increment
    /// semantics, applied *after* the whole instruction body runs
    /// (`scu.c:1949-1952`), not immediately at the point of use. This
    /// matters when one instruction reads the same `MCn` bus source twice
    /// (X and Y in the same cycle): both reads see the *same*, not-yet-
    /// incremented `CT[n]`.
    inc_flg: [bool; 4],
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
            inc_flg: [false; 4],
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

    /// Apply every deferred `CT[n]` post-increment now and clear the flags
    /// (`scu.c:1949-1952`'s post-instruction block, and the one early-apply
    /// call site inside `MOV SImm,[d]`, `scu.c:1691-1694`).
    fn apply_inc_flg(&mut self) {
        for n in 0..4 {
            if self.inc_flg[n] {
                self.ct[n] = self.ct[n].wrapping_add(1) & 0x3F;
                self.inc_flg[n] = false;
            }
        }
    }

    /// Force-complete a pending DSP DMA right now, matching `scu.c`'s
    /// `readgensrc`/`writed1busdest`/`writeloadimdest` preamble
    /// (`:500-503`, `:555-558`, `:620-623`): any DSP RAM/register access
    /// while a DMA is in flight makes it finish immediately rather than on
    /// its own two-step schedule.
    fn force_complete_dma(&mut self, work_ram: &WorkRam) {
        if self.dsp_dma_wait > 0 {
            self.dsp_dma_wait = 0;
            self.step_dma(work_ram);
        }
    }

    /// `scu.c`'s `readgensrc` -- general-purpose ALU/bus source read.
    fn read_gen_src(&mut self, num: u32, work_ram: &WorkRam) -> u32 {
        if num <= 7 {
            let bank = (num & 0x3) as usize;
            if (num >> 2) & 1 != 0 {
                self.inc_flg[bank] = true;
            }
            self.force_complete_dma(work_ram);
            self.md[bank][(self.ct[bank] & 0x3F) as usize]
        } else if num == 0x9 {
            Self::low32(self.alu) as u32 // ALL
        } else if num == 0xA {
            (self.alu >> 16) as u32 // ALH
        } else {
            0xFFFF_FFFF
        }
    }

    /// `scu.c`'s `writed1busdest` -- D1-bus store destinations.
    fn write_d1_bus_dest(&mut self, num: u32, val: u32, work_ram: &WorkRam) {
        self.force_complete_dma(work_ram);
        match num {
            0x0 => {
                self.md[0][(self.ct[0] & 0x3F) as usize] = val;
                self.inc_flg[0] = true;
            }
            0x1 => {
                self.md[1][(self.ct[1] & 0x3F) as usize] = val;
                self.inc_flg[1] = true;
            }
            0x2 => {
                self.md[2][(self.ct[2] & 0x3F) as usize] = val;
                self.inc_flg[2] = true;
            }
            0x3 => {
                self.md[3][(self.ct[3] & 0x3F) as usize] = val;
                self.inc_flg[3] = true;
            }
            0x4 => self.rx = val as i32,
            0x5 => self.p = val as i32 as i64,
            0x6 => self.ra0 = val,
            0x7 => self.wa0 = val,
            0xA => self.lop = val as u16,
            0xB => self.top = val as u8,
            0xC => {
                self.ct[0] = val as u8;
                self.inc_flg[0] = false;
            }
            0xD => {
                self.ct[1] = val as u8;
                self.inc_flg[1] = false;
            }
            0xE => {
                self.ct[2] = val as u8;
                self.inc_flg[2] = false;
            }
            0xF => {
                self.ct[3] = val as u8;
                self.inc_flg[3] = false;
            }
            _ => {}
        }
    }

    /// `scu.c`'s `writeloadimdest` -- MVI (load-immediate) destinations.
    /// Destination `0xC` is the DSP's "call" form (`scu.h`/`scu.c`: sets
    /// `TOP` as a return address, then jumps).
    fn write_load_im_dest(&mut self, num: u32, val: u32, work_ram: &WorkRam) {
        self.force_complete_dma(work_ram);
        match num {
            0x0 => {
                self.md[0][(self.ct[0] & 0x3F) as usize] = val;
                self.inc_flg[0] = true;
            }
            0x1 => {
                self.md[1][(self.ct[1] & 0x3F) as usize] = val;
                self.inc_flg[1] = true;
            }
            0x2 => {
                self.md[2][(self.ct[2] & 0x3F) as usize] = val;
                self.inc_flg[2] = true;
            }
            0x3 => {
                self.md[3][(self.ct[3] & 0x3F) as usize] = val;
                self.inc_flg[3] = true;
            }
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
    /// Returns `true` exactly on the instruction where an `ENDI` (END with
    /// interrupt-request set) just executed -- the caller (Core 6,
    /// `lib.rs`) uses that to call `Scu::dsp_end()` *after* this returns,
    /// once the `dsp` lock this method runs under is released, matching
    /// the same "signal while locked, act after releasing" shape
    /// `Scu::tick_timer0_and_arm_timer1` already uses for its own
    /// same-module callback (`docs/implementation-plans/scu.md` Phase 6).
    pub fn step(&mut self, work_ram: &WorkRam) -> bool {
        if !self.is_executing() {
            return false;
        }

        if self.prog_control & PCP_T0 != 0 {
            self.step_dma(work_ram);
        }

        let instruction = self.program_ram[self.pc as usize];

        // Deferred CT-increment flags are cleared once per instruction,
        // right after fetch (`scu.c:1384-1387`) -- before anything in this
        // instruction gets a chance to set one.
        self.inc_flg = [false; 4];

        // ALU op always computes (real hardware: a VLIW slot that runs
        // every cycle regardless of whether anything captures its
        // result) -- `scu.c`'s unconditional `ScuDsp->ALU.all = ScuDsp->AC.all;`
        // followed by the ALU switch.
        self.alu = self.ac;
        self.execute_alu(instruction);

        let top2 = (instruction >> 30) & 0x3;
        let dsp_end = match top2 {
            0x0 => {
                self.execute_operation(instruction, work_ram);
                false
            }
            0x2 => {
                self.execute_load_immediate(instruction, work_ram);
                false
            }
            0x3 => self.execute_other(instruction, work_ram),
            _ => false,
        };

        // Pending CT increments apply after the whole instruction body, but
        // before PC advances (`scu.c:1949-1954`).
        self.apply_inc_flg();

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

        dsp_end
    }

    /// ALU op group, cross-checked against `scu.c`'s switch on
    /// `instruction >> 26` (**not** masked to 4 bits there -- the switch is
    /// unguarded by instruction class, but classes `01`/`10`/`11` push the
    /// unmasked selector to `>= 0x10`, which hits no case and falls to
    /// `default` (no ALU effect). Masking to `& 0xF` before matching, as
    /// this used to do, throws away the class bits and lets a JMP/LPS/BTM/
    /// MVI's own encoding bits accidentally land on a real ALU opcode --
    /// this was D-DSP-1, a phantom-ALU-op bug on every non-Operation-Command
    /// instruction).
    fn execute_alu(&mut self, instruction: u32) {
        let op = instruction >> 26;
        let ac_l = Self::low32(self.ac);
        let p_l = Self::low32(self.p);
        match op {
            0x0 => {} // NOP -- ALU already holds AC passthrough
            0x1 => {
                // AND
                let r = (ac_l as u32) & (p_l as u32);
                Self::set_low32(&mut self.alu, r as i32);
                self.set_zsc(r == 0, (r as i32) < 0, false);
            }
            0x2 => {
                // OR
                let r = (ac_l as u32) | (p_l as u32);
                Self::set_low32(&mut self.alu, r as i32);
                self.set_zsc(r == 0, (r as i32) < 0, false);
            }
            0x3 => {
                // XOR
                let r = (ac_l as u32) ^ (p_l as u32);
                Self::set_low32(&mut self.alu, r as i32);
                self.set_zsc(r == 0, (r as i32) < 0, false);
            }
            0x4 => {
                // ADD
                let r = ac_l.wrapping_add(p_l);
                Self::set_low32(&mut self.alu, r);
                let carry = ((ac_l as u32 as u64) + (p_l as u32 as u64)) & 0x1_0000_0000 != 0;
                self.set_zsc(r == 0, r < 0, carry);
            }
            0x5 => {
                // SUB
                let r = ac_l.wrapping_sub(p_l);
                Self::set_low32(&mut self.alu, r);
                let carry =
                    ((ac_l as u32 as u64).wrapping_sub(p_l as u32 as u64)) & 0x1_0000_0000 != 0;
                self.set_zsc(r == 0, r < 0, carry);
            }
            0x6 => {
                // AD2 -- full 48-bit add, uses .all directly
                let r = self.ac.wrapping_add(self.p);
                self.alu = r;
                let carry = (((self.ac & 0xFFFF_FFFF_FFFF) as u64)
                    + ((self.p & 0xFFFF_FFFF_FFFF) as u64))
                    & 0x1_0000_0000_0000
                    != 0;
                self.set_zsc(r == 0, r & 0x8000_0000_0000 != 0, carry);
            }
            0x8 => {
                // SR
                let carry = ac_l & 1 != 0;
                let r = ((ac_l as u32 & 0x8000_0000) | ((ac_l as u32) >> 1)) as i32;
                Self::set_low32(&mut self.alu, r);
                self.set_zsc(r == 0, r < 0, carry);
            }
            0x9 => {
                // RR
                let carry_in = ac_l & 1 != 0;
                let r = (((carry_in as u32) << 31) | ((ac_l as u32) >> 1)) as i32;
                Self::set_low32(&mut self.alu, r);
                self.set_zsc(r == 0, r < 0, carry_in);
            }
            0xA => {
                // SL
                let carry = (ac_l as u32) & 0x8000_0000 != 0;
                let r = ((ac_l as u32) << 1) as i32;
                Self::set_low32(&mut self.alu, r);
                self.set_zsc(r == 0, r < 0, carry);
            }
            0xB => {
                // RL
                let carry = (ac_l as u32) & 0x8000_0000 != 0;
                let r = (((ac_l as u32) << 1) | (carry as u32)) as i32;
                Self::set_low32(&mut self.alu, r);
                self.set_zsc(r == 0, r < 0, carry);
            }
            0xF => {
                // RL8
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
            3 => self.p = self.read_gen_src((instruction >> 20) & 0x7, work_ram) as i32 as i64, // MOV [s],P
            _ => {}
        }
        if (instruction >> 23) & 0x4 != 0 {
            self.rx = self.read_gen_src((instruction >> 20) & 0x7, work_ram) as i32;
            // MOV [s],X
        }
        if (instruction >> 17) & 0x4 != 0 {
            self.ry = self.read_gen_src((instruction >> 14) & 0x7, work_ram) as i32;
            // MOV [s],Y
        }
        match (instruction >> 17) & 0x3 {
            1 => self.ac = 0,        // CLR A
            2 => self.ac = self.alu, // MOV ALU,A
            3 => self.ac = self.read_gen_src((instruction >> 14) & 0x7, work_ram) as i32 as i64, // MOV [s],A
            _ => {}
        }
        match (instruction >> 12) & 0x3 {
            1 => {
                // MOV SImm,[d]
                // Early-apply: any pending CT increment from earlier in
                // this same instruction (X/Y-bus reads above) lands before
                // this store, not after the whole instruction like normal
                // (`scu.c:1691-1694`) -- matters when the D1 dest is the
                // same bank an earlier MCn read this instruction.
                self.apply_inc_flg();
                let imm = (instruction & 0xFF) as i8 as i32 as u32;
                self.write_d1_bus_dest((instruction >> 8) & 0xF, imm, work_ram);
            }
            3 => {
                // MOV [s],[d]
                let src = self.read_gen_src(instruction & 0xF, work_ram);
                self.write_d1_bus_dest((instruction >> 8) & 0xF, src, work_ram);
            }
            _ => {}
        }
    }

    /// Load Immediate Commands (top2 == 0b10), cross-checked against
    /// `scu.c`'s `case 0x02` block (conditional MVI variants + plain MVI).
    fn execute_load_immediate(&mut self, instruction: u32, work_ram: &WorkRam) {
        let dest = (instruction >> 26) & 0xF;
        if (instruction >> 25) & 1 != 0 {
            let cond = (instruction >> 19) & 0x3F;
            let imm = (instruction & 0x7_FFFF)
                | if instruction & 0x4_0000 != 0 {
                    0xFFF8_0000
                } else {
                    0
                };
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
                self.write_load_im_dest(dest, imm, work_ram);
            }
        } else {
            let raw = instruction & 0x01FF_FFFF;
            let imm = if raw & 0x0100_0000 != 0 {
                raw | 0xFE00_0000
            } else {
                raw
            };
            self.write_load_im_dest(dest, imm, work_ram);
        }
    }

    /// "Other" group (top2 == 0b11): DMA/JMP/Loop/End, cross-checked
    /// against `scu.c`'s `case 0x03` block. Returns `true` exactly for an
    /// `ENDI` (End Commands, interrupt-request bit set) -- see `step`'s own
    /// doc comment for how that propagates to `Scu::dsp_end()`.
    fn execute_other(&mut self, instruction: u32, work_ram: &WorkRam) -> bool {
        match (instruction >> 28) & 0xF {
            0xC => {
                self.start_dma(instruction, work_ram); // DMA Commands
                false
            }
            0xD => {
                self.execute_jump(instruction); // Jump Commands
                false
            }
            0xE => {
                // Loop bottom Commands
                if instruction & 0x0800_0000 != 0 {
                    // LPS
                    if self.lop != 0 {
                        self.jmpaddr = Some(self.pc);
                        self.delayed = false;
                        self.lop -= 1;
                    }
                } else if self.lop != 0 {
                    // BTM
                    self.jmpaddr = Some(self.top);
                    self.delayed = false;
                    self.lop -= 1;
                }
                false
            }
            0xF => {
                // End Commands
                self.prog_control &= !PCP_EX;
                let is_endi = instruction & 0x0800_0000 != 0;
                if is_endi {
                    self.prog_control |= PCP_E;
                    // Real hardware also raises the SCU DSP-End interrupt
                    // here (`ScuSendDSPEnd`, vector 0x45, level 10, mask
                    // 0x0020) -- `docs/implementation-plans/scu.md` Phase 6
                    // wires this: the caller raises it via `Scu::dsp_end()`
                    // once this method's `dsp` lock is released, not from
                    // in here (would violate this crate's `regs`/`irq`
                    // before `dsp` lock-ordering rule).
                }
                // D-DSP-5: real hardware writes PC+1 here (`scu.c`:
                // `ProgControlPort.part.P = ScuDsp->PC+1;`), anticipating
                // the unconditional `PC++` every instruction still gets
                // after this switch -- not the pre-increment `PC` a naive
                // read would use.
                self.prog_control = (self.prog_control & !0xFF) | (self.pc.wrapping_add(1) as u32);
                is_endi
            }
            _ => false,
        }
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

    // ---- DMA (all 8 real addressing-mode variants) ----

    fn start_dma(&mut self, instruction: u32, work_ram: &WorkRam) {
        // Finish a still-in-flight previous DMA first, matching real
        // hardware's "each DMA instruction stalls behind the last"
        // (`scu.c:1765-1768`: this force-completes it, doesn't just drop
        // it on the floor).
        self.force_complete_dma(work_ram);
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

    /// Called once per `step()` while `T0` is set (and, force-completing,
    /// from `read_gen_src`/`write_d1_bus_dest`/`write_load_im_dest`/
    /// `start_dma`) -- mirrors `scu.c`'s `step_dsp_dma`'s countdown-then-
    /// fire shape (real DMA takes a few real cycles, it isn't instantaneous
    /// even though this interpreter doesn't model exact per-word timing).
    fn step_dma(&mut self, work_ram: &WorkRam) {
        if self.prog_control & PCP_T0 == 0 {
            return;
        }
        self.dsp_dma_wait -= 1;
        if self.dsp_dma_wait > 0 {
            return;
        }
        // Reproduce `step_dsp_dma`'s exact if-else chain order
        // (`scu.c:960-989`) -- the eight tests are mutually exclusive by
        // construction, but keep the reference's own order rather than
        // relying on that.
        let instruction = self.dsp_dma_instruction;
        let d1 = (instruction >> 10) & 0x1F;
        let d2 = (instruction >> 11) & 0xF;
        if d1 == 0x00 {
            self.dsp_dma01(instruction, work_ram);
        } else if d1 == 0x04 {
            self.dsp_dma02(instruction, work_ram);
        } else if d2 == 0x04 {
            self.dsp_dma03(instruction, work_ram);
        } else if d1 == 0x0C {
            self.dsp_dma04(instruction, work_ram);
        } else if d2 == 0x08 {
            self.dsp_dma05(instruction, work_ram);
        } else if d1 == 0x14 {
            self.dsp_dma06(instruction, work_ram);
        } else if d2 == 0x0C {
            self.dsp_dma07(instruction, work_ram);
        } else if d1 == 0x1C {
            self.dsp_dma08(instruction, work_ram);
        }
        // [QUIRK] Encodings matching none of the eight (bit 11 set, or bit
        // 10 set on the immediate-count/write variants) are silently
        // dropped -- T0/instruction/wait still clear below as though a
        // transfer had run (§3.8.2).
        self.prog_control &= !PCP_T0;
        self.dsp_dma_instruction = 0;
        self.dsp_dma_wait = 0;
    }

    /// Read-side address step shared by `dsp_dma01`/`dsp_dma03` (and their
    /// hold wrappers): instruction bit 16 selects 1 long-word advance or
    /// none. Already `>> 2`'d into long-word units (`scu.c:681-682`,
    /// `:823-824`).
    fn dma_read_add(instruction: u32) -> u32 {
        let mode = (instruction >> 15) & 0x7;
        ((1u32 << (mode & 0x2)) & !1) >> 2
    }

    /// Write-side address step shared by `dsp_dma02`/`dsp_dma04` (and their
    /// hold wrappers): full 3-bit table in long-word units (`scu.c:798-808`,
    /// deliberately a *different* decode of the same instruction field from
    /// the read-side rule above -- keep them separate, don't unify).
    fn dma_write_add(instruction: u32) -> u32 {
        match (instruction >> 15) & 0x7 {
            0 => 0,
            1 => 1,
            2 => 2,
            3 => 4,
            4 => 8,
            5 => 16,
            6 => 32,
            7 => 64,
            _ => 0,
        }
    }

    /// Yabause's `dsp_dma01`: non-hold, immediate count, D0-bus ->
    /// `MD[sel]`. `sel` is 2 bits here (unlike `dsp_dma03`'s 3) -- the
    /// immediate-count read variants never target Program RAM.
    fn dsp_dma01(&mut self, instruction: u32, work_ram: &WorkRam) {
        let sel = ((instruction >> 8) & 0x3) as usize;
        let count = instruction & 0xFF;
        let add = Self::dma_read_add(instruction);
        for _ in 0..count {
            let val = read_long(work_ram, self.ra0m << 2);
            self.md[sel][(self.ct[sel] & 0x3F) as usize] = val;
            self.ct[sel] = self.ct[sel].wrapping_add(1) & 0x3F;
            self.ra0m = self.ra0m.wrapping_add(add);
        }
        self.ra0 = self.ra0m;
    }

    /// Yabause's `dsp_dma02`: non-hold, immediate count, `MD[sel]` ->
    /// D0-bus, through the shared `dsp_dma_write_d0bus` path.
    fn dsp_dma02(&mut self, instruction: u32, work_ram: &WorkRam) {
        let sel = ((instruction >> 8) & 0x3) as usize;
        let count = instruction & 0xFF;
        let add = Self::dma_write_add(instruction);
        self.dsp_dma_write_d0bus(sel, add, count, work_ram);
    }

    /// Yabause's `dsp_dma03`: Main RAM -> Data RAM / Program RAM, reading
    /// `dsp_dma_size` longwords starting at `ra0m<<2`. `sel == 0x4` selects
    /// Program RAM, written from a local index starting at 0 (ignoring
    /// `PC`), instead of a Data RAM bank.
    fn dsp_dma03(&mut self, instruction: u32, work_ram: &WorkRam) {
        let sel = ((instruction >> 8) & 0x7) as usize;
        let add = Self::dma_read_add(instruction);
        // [QUIRK] `RA0` is only written back on the non-A-Bus path
        // (`scu.c:864`, D-DSP-7) -- an A-Bus source read via this variant
        // leaves `RA0` at its pre-transfer value even without the hold bit.
        let abus_check = (self.ra0m << 2) & 0x0FF0_0000;
        let is_abus = (0x0200_0000..0x0590_0000).contains(&abus_check);
        let mut index = 0usize;
        for _ in 0..self.dsp_dma_size {
            let val = read_long(work_ram, self.ra0m << 2);
            if sel == 0x4 {
                if index < self.program_ram.len() {
                    self.program_ram[index] = val;
                }
                index += 1;
            } else {
                self.md[sel][(self.ct[sel] & 0x3F) as usize] = val;
                self.ct[sel] = self.ct[sel].wrapping_add(1) & 0x3F;
            }
            self.ra0m = self.ra0m.wrapping_add(add);
        }
        if !is_abus {
            self.ra0 = self.ra0m;
        }
    }

    /// Yabause's `dsp_dma04`: Data RAM -> Main RAM, writing
    /// `dsp_dma_size` longwords through the shared `dsp_dma_write_d0bus`
    /// path.
    fn dsp_dma04(&mut self, instruction: u32, work_ram: &WorkRam) {
        let sel = ((instruction >> 8) & 0x3) as usize;
        let add = Self::dma_write_add(instruction);
        let count = self.dsp_dma_size;
        self.dsp_dma_write_d0bus(sel, add, count, work_ram);
    }

    /// Yabause's `dsp_dma05`: `dsp_dma01` wrapped, restoring `RA0`
    /// afterward (the hold bit -- the transfer itself still walks `RA0M`
    /// forward, only the CPU-visible `RA0` is rewound). [QUIRK] this
    /// variant's own encoding accepts `RAMsel == 4` (Program RAM) but
    /// forwards into `dsp_dma01`, whose `sel` is masked to 2 bits -- `PRG`
    /// degrades silently to `MD0`. `dsp_dma07` (below) does not have this
    /// problem: it wraps `dsp_dma03`, whose `sel` is the full 3 bits.
    fn dsp_dma05(&mut self, instruction: u32, work_ram: &WorkRam) {
        let save_ra0 = self.ra0m;
        self.dsp_dma01(instruction, work_ram);
        self.ra0 = save_ra0;
    }

    /// Yabause's `dsp_dma06`: `dsp_dma02` wrapped, restoring `WA0`.
    fn dsp_dma06(&mut self, instruction: u32, work_ram: &WorkRam) {
        let save_wa0 = self.wa0m;
        self.dsp_dma02(instruction, work_ram);
        self.wa0 = save_wa0;
    }

    /// Yabause's `dsp_dma07`: `dsp_dma03` wrapped, restoring `RA0`.
    fn dsp_dma07(&mut self, instruction: u32, work_ram: &WorkRam) {
        let save_ra0 = self.ra0m;
        self.dsp_dma03(instruction, work_ram);
        self.ra0 = save_ra0;
    }

    /// Yabause's `dsp_dma08`: `dsp_dma04` wrapped, restoring `WA0`.
    fn dsp_dma08(&mut self, instruction: u32, work_ram: &WorkRam) {
        let save_wa0 = self.wa0m;
        self.dsp_dma04(instruction, work_ram);
        self.wa0 = save_wa0;
    }

    /// Yabause's `dsp_dma_write_d0bus` (`scu.c:715-788`) -- shared by
    /// `dsp_dma02` and `dsp_dma04` (and, through them, `dsp_dma06`/
    /// `dsp_dma08`). Three destination classes, each with its own `add`
    /// fixup: A-Bus writes as longwords with `add` clamped to at most 1;
    /// B-Bus writes as two 16-bit halves (real SCSP/Sound RAM 16-bit port
    /// width), `WA0M` advancing once at the end by `add * count`; CPU bus
    /// [QUIRK] redirects into High WRAM (`0xFFFFC` mask) regardless of the
    /// nominal destination, with its own halved stride when `add != 1`.
    fn dsp_dma_write_d0bus(&mut self, sel: usize, add: u32, count: u32, work_ram: &WorkRam) {
        let addr = (self.wa0m << 2) & 0x0FFF_FFFF;
        if (0x0200_0000..0x05A0_0000).contains(&addr) {
            let add = if add > 1 { 1 } else { add };
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

/// Which `WorkRam` region a DSP-DMA address decodes to (D-DSP-6).
///
/// `pub(crate)`: also reused by `scu.rs`'s Phase 4 DMA engine
/// (`docs/implementation-plans/scu.md` Phase 4) for its own main-RAM/
/// peripheral reads and writes -- both engines target the exact same
/// address space (Low WRAM, Sound RAM, SCSP regs, VDP1/VDP2 RAM/regs, CS2
/// regs, High WRAM), so sharing this one decode table avoids a third
/// hand-copied boundary table alongside `Sh2::translate`'s (the doc comment
/// on `decode` below already notes it's a *second* copy of that one).
pub(crate) enum DspRegion {
    LowRam,
    SoundRam,
    ScspRegs,
    Vdp1Vram,
    Vdp1Framebuffer,
    Vdp1Regs,
    Vdp2Vram,
    Vdp2Cram,
    Vdp2Regs,
    Cs2Regs,
    HighRam,
}

/// Shared main-RAM address decode for DSP DMA -- mirrors `Sh2::translate`'s
/// region boundaries (`sh2.rs:594-611`) for every region real SCU DSP DMA
/// can plausibly target: Low WRAM, CS2, Sound RAM, SCSP regs, VDP1 VRAM/
/// framebuffer/regs, VDP2 VRAM/CRAM/regs, High WRAM. Duplicated rather than
/// shared because `Sh2`'s `MemRegion`/`translate` are private to that
/// module and this is a small, stable set of ranges; if the two ever drift,
/// `sh2.rs`'s `translate` remains the source of truth (cross-checked
/// against Yabause's `memory.c`). Unmapped A-Bus/cartridge space and
/// anything else reads `0` / discards writes -- `None`.
pub(crate) fn decode(address: u32) -> Option<(DspRegion, usize)> {
    let a = address & 0x0FFF_FFFF;
    if (0x0020_0000..0x0030_0000).contains(&a) {
        Some((DspRegion::LowRam, (a - 0x0020_0000) as usize))
    } else if (0x0580_0000..0x0590_0000).contains(&a) {
        Some((DspRegion::Cs2Regs, (a - 0x0580_0000) as usize))
    } else if (0x05A0_0000..0x05B0_0000).contains(&a) {
        Some((DspRegion::SoundRam, (a - 0x05A0_0000) as usize))
    } else if (0x05B0_0000..0x05C0_0000).contains(&a) {
        Some((DspRegion::ScspRegs, (a - 0x05B0_0000) as usize))
    } else if (0x05C0_0000..0x05C8_0000).contains(&a) {
        Some((DspRegion::Vdp1Vram, (a - 0x05C0_0000) as usize))
    } else if (0x05C8_0000..0x05D0_0000).contains(&a) {
        Some((DspRegion::Vdp1Framebuffer, (a - 0x05C8_0000) as usize))
    } else if (0x05D0_0000..0x05D8_0000).contains(&a) {
        Some((DspRegion::Vdp1Regs, (a - 0x05D0_0000) as usize))
    } else if (0x05E0_0000..0x05F0_0000).contains(&a) {
        Some((DspRegion::Vdp2Vram, (a - 0x05E0_0000) as usize))
    } else if (0x05F0_0000..0x05F8_0000).contains(&a) {
        Some((DspRegion::Vdp2Cram, (a - 0x05F0_0000) as usize))
    } else if (0x05F8_0000..0x05FC_0000).contains(&a) {
        Some((DspRegion::Vdp2Regs, (a - 0x05F8_0000) as usize))
    } else if (0x0600_0000..0x0700_0000).contains(&a) {
        Some((DspRegion::HighRam, (a - 0x0600_0000) as usize))
    } else {
        None
    }
}

pub(crate) fn read_long(work_ram: &WorkRam, address: u32) -> u32 {
    match decode(address) {
        Some((DspRegion::LowRam, off)) => {
            read_long_from(&work_ram.low_ram.read().unwrap()[..], off)
        }
        Some((DspRegion::SoundRam, off)) => {
            read_long_from(&work_ram.sound_ram.read().unwrap()[..], off)
        }
        Some((DspRegion::ScspRegs, off)) => {
            read_long_from(&work_ram.scsp_regs.read().unwrap()[..], off)
        }
        Some((DspRegion::Vdp1Vram, off)) => {
            read_long_from(&work_ram.vdp1_vram.read().unwrap()[..], off)
        }
        Some((DspRegion::Vdp1Framebuffer, off)) => {
            let back = work_ram
                .vdp1_framebuffers
                .back
                .load(std::sync::atomic::Ordering::Relaxed);
            read_long_from(
                &work_ram.vdp1_framebuffers.banks[back].read().unwrap()[..],
                off,
            )
        }
        Some((DspRegion::Vdp1Regs, off)) => {
            read_long_from(&work_ram.vdp1_regs.read().unwrap()[..], off)
        }
        Some((DspRegion::Vdp2Vram, off)) => {
            read_long_from(&work_ram.vdp2_vram.read().unwrap()[..], off)
        }
        Some((DspRegion::Vdp2Cram, off)) => {
            read_long_from(&work_ram.vdp2_cram.read().unwrap()[..], off)
        }
        Some((DspRegion::Vdp2Regs, off)) => {
            read_long_from(&work_ram.vdp2_regs.read().unwrap()[..], off)
        }
        Some((DspRegion::Cs2Regs, _off)) => 0,
        Some((DspRegion::HighRam, off)) => work_ram.read_high_ram_long(off),
        None => 0,
    }
}

pub(crate) fn write_long(work_ram: &WorkRam, address: u32, val: u32) {
    match decode(address) {
        Some((DspRegion::LowRam, off)) => {
            write_long_to(&mut work_ram.low_ram.write().unwrap()[..], off, val)
        }
        Some((DspRegion::SoundRam, off)) => {
            write_long_to(&mut work_ram.sound_ram.write().unwrap()[..], off, val)
        }
        Some((DspRegion::ScspRegs, off)) => {
            write_long_to(&mut work_ram.scsp_regs.write().unwrap()[..], off, val)
        }
        Some((DspRegion::Vdp1Vram, off)) => {
            write_long_to(&mut work_ram.vdp1_vram.write().unwrap()[..], off, val)
        }
        Some((DspRegion::Vdp1Framebuffer, off)) => {
            let back = work_ram
                .vdp1_framebuffers
                .back
                .load(std::sync::atomic::Ordering::Relaxed);
            write_long_to(
                &mut work_ram.vdp1_framebuffers.banks[back].write().unwrap()[..],
                off,
                val,
            )
        }
        Some((DspRegion::Vdp1Regs, off)) => {
            write_long_to(&mut work_ram.vdp1_regs.write().unwrap()[..], off, val)
        }
        Some((DspRegion::Vdp2Vram, off)) => {
            write_long_to(&mut work_ram.vdp2_vram.write().unwrap()[..], off, val)
        }
        Some((DspRegion::Vdp2Cram, off)) => {
            write_long_to(&mut work_ram.vdp2_cram.write().unwrap()[..], off, val)
        }
        Some((DspRegion::Vdp2Regs, off)) => {
            write_long_to(&mut work_ram.vdp2_regs.write().unwrap()[..], off, val)
        }
        Some((DspRegion::Cs2Regs, _off)) => {}
        Some((DspRegion::HighRam, off)) => work_ram.write_high_ram_long(off, val),
        None => {}
    }
}

pub(crate) fn write_word(work_ram: &WorkRam, address: u32, val: u16) {
    match decode(address) {
        Some((DspRegion::LowRam, off)) => {
            write_word_to(&mut work_ram.low_ram.write().unwrap()[..], off, val)
        }
        Some((DspRegion::SoundRam, off)) => {
            write_word_to(&mut work_ram.sound_ram.write().unwrap()[..], off, val)
        }
        Some((DspRegion::ScspRegs, off)) => {
            write_word_to(&mut work_ram.scsp_regs.write().unwrap()[..], off, val)
        }
        Some((DspRegion::Vdp1Vram, off)) => {
            write_word_to(&mut work_ram.vdp1_vram.write().unwrap()[..], off, val)
        }
        Some((DspRegion::Vdp1Framebuffer, off)) => {
            let back = work_ram
                .vdp1_framebuffers
                .back
                .load(std::sync::atomic::Ordering::Relaxed);
            write_word_to(
                &mut work_ram.vdp1_framebuffers.banks[back].write().unwrap()[..],
                off,
                val,
            )
        }
        Some((DspRegion::Vdp1Regs, off)) => {
            write_word_to(&mut work_ram.vdp1_regs.write().unwrap()[..], off, val)
        }
        Some((DspRegion::Vdp2Vram, off)) => {
            write_word_to(&mut work_ram.vdp2_vram.write().unwrap()[..], off, val)
        }
        Some((DspRegion::Vdp2Cram, off)) => {
            write_word_to(&mut work_ram.vdp2_cram.write().unwrap()[..], off, val)
        }
        Some((DspRegion::Vdp2Regs, off)) => {
            write_word_to(&mut work_ram.vdp2_regs.write().unwrap()[..], off, val)
        }
        Some((DspRegion::Cs2Regs, _off)) => {}
        Some((DspRegion::HighRam, off)) => work_ram.write_high_ram_word(off, val),
        None => {}
    }
}

/// `docs/hardware-reference/scu.md` §2.4's 16-bit copy-mode unit -- not
/// needed by DSP DMA itself (which never reads the D1 bus, only writes to
/// it), added here alongside `write_word` purely so `scu.rs`'s Phase 4 DMA
/// engine can reuse this module's one decode table rather than adding its
/// own read-only counterpart.
pub(crate) fn read_word(work_ram: &WorkRam, address: u32) -> u16 {
    match decode(address) {
        Some((DspRegion::LowRam, off)) => {
            read_word_from(&work_ram.low_ram.read().unwrap()[..], off)
        }
        Some((DspRegion::SoundRam, off)) => {
            read_word_from(&work_ram.sound_ram.read().unwrap()[..], off)
        }
        Some((DspRegion::ScspRegs, off)) => {
            read_word_from(&work_ram.scsp_regs.read().unwrap()[..], off)
        }
        Some((DspRegion::Vdp1Vram, off)) => {
            read_word_from(&work_ram.vdp1_vram.read().unwrap()[..], off)
        }
        Some((DspRegion::Vdp1Framebuffer, off)) => {
            let back = work_ram
                .vdp1_framebuffers
                .back
                .load(std::sync::atomic::Ordering::Relaxed);
            read_word_from(
                &work_ram.vdp1_framebuffers.banks[back].read().unwrap()[..],
                off,
            )
        }
        Some((DspRegion::Vdp1Regs, off)) => {
            read_word_from(&work_ram.vdp1_regs.read().unwrap()[..], off)
        }
        Some((DspRegion::Vdp2Vram, off)) => {
            read_word_from(&work_ram.vdp2_vram.read().unwrap()[..], off)
        }
        Some((DspRegion::Vdp2Cram, off)) => {
            read_word_from(&work_ram.vdp2_cram.read().unwrap()[..], off)
        }
        Some((DspRegion::Vdp2Regs, off)) => {
            read_word_from(&work_ram.vdp2_regs.read().unwrap()[..], off)
        }
        Some((DspRegion::Cs2Regs, _off)) => 0,
        Some((DspRegion::HighRam, off)) => work_ram.read_high_ram_word(off),
        None => 0,
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

fn read_word_from(buf: &[u8], off: usize) -> u16 {
    let mask = buf.len() - 1;
    let b0 = buf[off & mask] as u16;
    let b1 = buf[(off + 1) & mask] as u16;
    (b0 << 8) | b1
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
        assert_eq!(
            readback & PCP_READABLE_MASK,
            readback,
            "read must never expose bits outside 0x00FD00FF"
        );
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
        assert_eq!(
            dsp.pc, 3,
            "each write must auto-increment PC, matching real hardware's upload port"
        );
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
        assert!(
            dsp.is_executing(),
            "an empty program must never clear EX by itself"
        );
    }

    #[test]
    fn end_instruction_clears_ex() {
        let mut dsp = ScuDsp::new();
        let work_ram = WorkRam::new();
        dsp.program_ram[0] = 0xF000_0000; // End, no interrupt requested
        dsp.write_control_port(0x0001_8000); // EX+LE, PC=0
        dsp.step(&work_ram);
        assert!(!dsp.is_executing(), "an End instruction must clear EX");
        assert_eq!(
            dsp.prog_control & PCP_E,
            0,
            "plain End (bit27 clear) must not raise the interrupt flag"
        );
    }

    #[test]
    fn end_with_interrupt_sets_e_flag() {
        let mut dsp = ScuDsp::new();
        let work_ram = WorkRam::new();
        dsp.program_ram[0] = 0xF800_0000; // End with interrupt (bit 27 set)
        dsp.write_control_port(0x0001_8000);
        dsp.step(&work_ram);
        assert!(!dsp.is_executing());
        assert_ne!(
            dsp.prog_control & PCP_E,
            0,
            "End-with-interrupt must set the E status bit"
        );
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
        assert_ne!(
            dsp.prog_control & PCP_C,
            0,
            "5 - 8 must set the borrow/carry flag"
        );
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
            0x00001c00, 0x00003604, 0x00003704, 0x00001c02, 0x00001d00, 0x00861540, 0x14003109,
            0x00003100, 0x00001d00, 0x00003005, 0x00001c03, 0x00003005, 0x00001c02, 0x83100000,
            0x00001c03, 0x82100040, 0x00001c03, 0x00823500, 0x10000000, 0xd308001f, 0x00000000,
            0xd3400015, 0x00001f00, 0xc0012300, 0xd3400018, 0x00001f00, 0xc000b300, 0xd340001b,
            0x00000000, 0xd0000003, 0x00000000, 0xf0000000,
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
        assert!(
            !dsp.is_executing(),
            "the real BIOS DSP program must reach its End instruction, not loop forever"
        );
    }

    // ---- Phase 1 (scu.md): interpreter defects (D-DSP-1..7) ----

    #[test]
    fn ddsp1_p9_jmp_does_not_corrupt_alu_flags() {
        // D-DSP-1 regression: before the fix, `execute_alu`'s op selector
        // was masked to 4 bits (`(instruction >> 26) & 0xF`), discarding the
        // instruction-class bits, so a non-Operation-Command instruction's
        // own encoding could accidentally match a real ALU opcode. 0xD3400015
        // is a JMP word (class 11, `(instr>>28)&0xF == 0xD`) whose bits
        // 29:26 happen to equal 0x4 (ADD) -- under the old masked code this
        // ran a phantom ADD on AC=5,P=8 (5+8=13 -> Z=0,S=0,C=0), clobbering
        // the preceding SUB's real Z=0,S=1,C=1.
        let mut dsp = ScuDsp::new();
        let work_ram = WorkRam::new();
        dsp.ac = 5;
        dsp.p = 8;
        dsp.program_ram[0] = 0x1400_0000; // Operation Command, ALU op SUB
        dsp.program_ram[1] = 0xD340_0015; // JMP -- must not touch flags
        dsp.write_control_port(0x0001_8000); // EX+LE, PC=0
        dsp.step(&work_ram); // SUB: ALU = 5-8 = -3
        assert_eq!(dsp.prog_control & PCP_Z, 0);
        assert_ne!(dsp.prog_control & PCP_S, 0);
        assert_ne!(dsp.prog_control & PCP_C, 0);
        dsp.step(&work_ram); // JMP
        assert_eq!(
            dsp.prog_control & PCP_Z,
            0,
            "JMP must not clear Z spuriously"
        );
        assert_ne!(
            dsp.prog_control & PCP_S,
            0,
            "JMP must not clobber S -- this is the D-DSP-1 regression"
        );
        assert_ne!(
            dsp.prog_control & PCP_C,
            0,
            "JMP must not clobber C -- this is the D-DSP-1 regression"
        );
    }

    #[test]
    fn ddsp2_3_p9_deferred_increment_same_instruction_dual_read() {
        // D-DSP-2/3 regression: reading MC0 on both the X-bus and Y-bus in
        // the SAME instruction must return the *same* Data RAM word --
        // CT[0] hasn't advanced yet mid-instruction, since increments are
        // deferred via `inc_flg` and applied once after the whole
        // instruction body runs. CT[0] must advance by exactly 1 total for
        // the instruction, not once per read.
        let mut dsp = ScuDsp::new();
        let work_ram = WorkRam::new();
        dsp.md[0][0] = 0x1234_5678;
        // Operation Command: XL=1,Xsrc=MC0(4),YL=1,Ysrc=MC0(4), nothing else.
        dsp.program_ram[0] = 0x0249_0000;
        dsp.write_control_port(0x0001_8000);
        dsp.step(&work_ram);
        assert_eq!(dsp.rx, 0x1234_5678, "X-bus read of MC0");
        assert_eq!(
            dsp.ry, 0x1234_5678,
            "Y-bus read of MC0 in the same instruction must see the SAME pre-increment word"
        );
        assert_eq!(
            dsp.ct[0], 1,
            "CT[0] must advance by exactly 1 for the whole instruction, not once per read"
        );
    }

    #[test]
    fn ddsp3_p9_mvi_mc0_sequence_lands_at_incrementing_offsets() {
        let mut dsp = ScuDsp::new();
        let work_ram = WorkRam::new();
        dsp.program_ram[0] = 0x8000_000A; // MVI 10, MC0
        dsp.program_ram[1] = 0x8000_0014; // MVI 20, MC0
        dsp.program_ram[2] = 0x8000_001E; // MVI 30, MC0
        dsp.write_control_port(0x0001_8000);
        dsp.step(&work_ram);
        dsp.step(&work_ram);
        dsp.step(&work_ram);
        assert_eq!(dsp.md[0][0], 10);
        assert_eq!(dsp.md[0][1], 20);
        assert_eq!(dsp.md[0][2], 30);
        assert_eq!(
            dsp.ct[0], 3,
            "each MVI ...,MC0 must set incFlg[0], applied once per instruction"
        );
    }

    #[test]
    fn ddsp5_p9_end_writes_pc_plus_one_into_control_port() {
        let mut dsp = ScuDsp::new();
        let work_ram = WorkRam::new();
        dsp.program_ram[5] = 0xF000_0000; // plain End
        dsp.write_control_port(0x0001_8005); // EX+LE, PC=5
        dsp.step(&work_ram);
        assert!(!dsp.is_executing());
        assert_eq!(
            dsp.read_control_port() & 0xFF,
            6,
            "End must write PC+1 (6), not the pre-increment PC (5)"
        );
    }

    #[test]
    fn ddsp6_p9_dma_write_reaches_vdp2_vram() {
        // D-DSP-6 regression: before widening read_long/write_long/
        // write_word's region coverage, VDP2 VRAM wasn't decoded by this
        // module at all -- this DMA silently wrote nothing.
        let mut dsp = ScuDsp::new();
        let work_ram = WorkRam::new();
        // dsp_dma02 (H=0 CS=0 DIR=1, d1==0x04): mode=1 (add=1 longword),
        // sel=1, immediate count=1.
        dsp.program_ram[0] = 0xC000_9101;
        dsp.wa0 = 0x0178_0000; // WA0<<2 == 0x05E00000 (VDP2 VRAM start -- inside the DMA engine's B-Bus range, 0x05A00000..0x06000000)
        dsp.md[1][0] = 0xCAFE_BABE;
        dsp.write_control_port(0x0001_8000);
        for _ in 0..3 {
            dsp.step(&work_ram);
        }
        let vram = work_ram.vdp2_vram.read().unwrap();
        assert_eq!(
            &vram[0..4],
            &[0xCA, 0xFE, 0xBA, 0xBE],
            "B-Bus path writes two 16-bit halves, big-endian"
        );
    }

    // ---- Phase 1 (scu.md): the 6 previously-missing DMA addressing-mode
    // variants. Instruction words and expected transfer results derived
    // independently via a throwaway Python model of HR sec 3.8's bit
    // layout (`dsp_dma_model.py`, not checked in) -- not hand-typed to
    // match whatever this file's own implementation happens to compute. ----

    #[test]
    fn dsp_dma01_p9_read_immediate_nonhold() {
        // 0xC0010103: DMA read, H=0 CS=0 DIR=0 (dispatch d1=0x00), mode
        // bits17:15=010 (bit16 set -> read add=1 longword), sel=1 (MD1),
        // immediate count=3.
        let mut dsp = ScuDsp::new();
        let work_ram = WorkRam::new();
        dsp.program_ram[0] = 0xC001_0103;
        dsp.ra0 = 0x0008_0000; // RA0<<2 == 0x00200000 (Low WRAM)
        {
            let mut low = work_ram.low_ram.write().unwrap();
            write_long_to(&mut low[..], 0, 0xA5A5_0000);
            write_long_to(&mut low[..], 4, 0xA5A5_0001);
            write_long_to(&mut low[..], 8, 0xA5A5_0002);
        }
        dsp.write_control_port(0x0001_8000);
        for _ in 0..3 {
            dsp.step(&work_ram);
        }
        assert_eq!(dsp.md[1][0], 0xA5A5_0000);
        assert_eq!(dsp.md[1][1], 0xA5A5_0001);
        assert_eq!(dsp.md[1][2], 0xA5A5_0002);
        assert_eq!(
            dsp.ra0, 0x0008_0003,
            "RA0 must advance by 3 longwords (add=1 each)"
        );
    }

    #[test]
    fn dsp_dma02_p9_write_immediate_nonhold() {
        // 0xC0009203: DMA write, H=0 CS=0 DIR=1 (dispatch d1=0x04), mode=1
        // (add=1 longword), sel=2 (MD2), immediate count=3.
        let mut dsp = ScuDsp::new();
        let work_ram = WorkRam::new();
        dsp.program_ram[0] = 0xC000_9203;
        dsp.wa0 = 0x0180_0400; // WA0<<2 == 0x06001000 (High WRAM, CPU-bus path)
        dsp.md[2][0] = 0x1111_1111;
        dsp.md[2][1] = 0x2222_2222;
        dsp.md[2][2] = 0x3333_3333;
        dsp.write_control_port(0x0001_8000);
        for _ in 0..3 {
            dsp.step(&work_ram);
        }
        assert_eq!(work_ram.read_high_ram_long(0x1000), 0x1111_1111);
        assert_eq!(work_ram.read_high_ram_long(0x1004), 0x2222_2222);
        assert_eq!(work_ram.read_high_ram_long(0x1008), 0x3333_3333);
        assert_eq!(dsp.wa0, 0x0180_0403, "WA0 must advance by 3 longwords");
    }

    #[test]
    fn dsp_dma05_p9_read_immediate_hold() {
        // 0xC0014103: same as dsp_dma01's test but with H=1 (hold, dispatch
        // d2=0x08).
        let mut dsp = ScuDsp::new();
        let work_ram = WorkRam::new();
        dsp.program_ram[0] = 0xC001_4103;
        dsp.ra0 = 0x0008_0000;
        {
            let mut low = work_ram.low_ram.write().unwrap();
            write_long_to(&mut low[..], 0, 0xA5A5_0000);
            write_long_to(&mut low[..], 4, 0xA5A5_0001);
            write_long_to(&mut low[..], 8, 0xA5A5_0002);
        }
        dsp.write_control_port(0x0001_8000);
        for _ in 0..3 {
            dsp.step(&work_ram);
        }
        assert_eq!(
            dsp.md[1][0], 0xA5A5_0000,
            "hold variant must still move data, same as the non-hold one"
        );
        assert_eq!(dsp.md[1][1], 0xA5A5_0001);
        assert_eq!(dsp.md[1][2], 0xA5A5_0002);
        assert_eq!(
            dsp.ra0, 0x0008_0000,
            "H (hold) bit must restore RA0 to its pre-transfer value"
        );
    }

    #[test]
    fn dsp_dma06_p9_write_immediate_hold() {
        // 0xC000D203: same as dsp_dma02's test but with H=1 (hold,
        // dispatch d1=0x14).
        let mut dsp = ScuDsp::new();
        let work_ram = WorkRam::new();
        dsp.program_ram[0] = 0xC000_D203;
        dsp.wa0 = 0x0180_0400;
        dsp.md[2][0] = 0x1111_1111;
        dsp.md[2][1] = 0x2222_2222;
        dsp.md[2][2] = 0x3333_3333;
        dsp.write_control_port(0x0001_8000);
        for _ in 0..3 {
            dsp.step(&work_ram);
        }
        assert_eq!(work_ram.read_high_ram_long(0x1000), 0x1111_1111);
        assert_eq!(work_ram.read_high_ram_long(0x1004), 0x2222_2222);
        assert_eq!(work_ram.read_high_ram_long(0x1008), 0x3333_3333);
        assert_eq!(
            dsp.wa0, 0x0180_0400,
            "H (hold) bit must restore WA0 to its pre-transfer value"
        );
    }

    #[test]
    fn dsp_dma07_p9_read_count_from_ram_hold_program_ram() {
        // 0xC0016404: H=1 CS=1 DIR=0 (dispatch d2=0x0C), RAMsel=4 (Program
        // RAM), count source bits2:0=4 -> MD[0][CT0] with post-increment.
        // Count reduced to 2 (not the encoding's ceiling) so the transfer
        // doesn't overwrite `program_ram[2]`, which is still the current PC
        // by the time the transfer runs -- real hardware would genuinely
        // execute whatever DMA just wrote there next (self-modifying-code
        // edge case matching `step_dsp_dma` running before fetch each
        // step), which this test isn't trying to exercise.
        let mut dsp = ScuDsp::new();
        let work_ram = WorkRam::new();
        dsp.program_ram[0] = 0xC001_6404;
        dsp.ra0 = 0x0008_0800; // RA0<<2 == 0x00202000
        dsp.ct[0] = 5;
        dsp.md[0][5] = 2; // count, read with post-increment (bits2:0 == 4)
        {
            let mut low = work_ram.low_ram.write().unwrap();
            write_long_to(&mut low[..], 0x2000, 0xB0B0_0000);
            write_long_to(&mut low[..], 0x2004, 0xB0B0_0001);
        }
        dsp.write_control_port(0x0001_8000);
        for _ in 0..3 {
            dsp.step(&work_ram);
        }
        assert_eq!(
            dsp.program_ram[0], 0xB0B0_0000,
            "RAMsel==4 must load Program RAM from index 0, ignoring PC"
        );
        assert_eq!(dsp.program_ram[1], 0xB0B0_0001);
        assert_eq!(dsp.ra0, 0x0008_0800, "H (hold) bit must restore RA0");
        assert_eq!(
            dsp.ct[0], 6,
            "count-from-RAM with bits2:0==4 must post-increment CT0"
        );
    }

    #[test]
    fn dsp_dma08_p9_write_count_from_ram_hold() {
        // 0xC000F204: H=1 CS=1 DIR=1 (dispatch d1=0x1C), sel=2, count
        // source bits2:0=4 -> MD[0][CT0] with post-increment.
        let mut dsp = ScuDsp::new();
        let work_ram = WorkRam::new();
        dsp.program_ram[0] = 0xC000_F204;
        dsp.wa0 = 0x0180_0800; // WA0<<2 == 0x06002000
        dsp.ct[0] = 5;
        dsp.md[0][5] = 4; // count, post-increment
        dsp.md[2][0] = 0x4444_4444;
        dsp.md[2][1] = 0x5555_5555;
        dsp.md[2][2] = 0x6666_6666;
        dsp.md[2][3] = 0x7777_7777;
        dsp.write_control_port(0x0001_8000);
        for _ in 0..3 {
            dsp.step(&work_ram);
        }
        assert_eq!(work_ram.read_high_ram_long(0x2000), 0x4444_4444);
        assert_eq!(work_ram.read_high_ram_long(0x2004), 0x5555_5555);
        assert_eq!(work_ram.read_high_ram_long(0x2008), 0x6666_6666);
        assert_eq!(work_ram.read_high_ram_long(0x200C), 0x7777_7777);
        assert_eq!(dsp.wa0, 0x0180_0800, "H (hold) bit must restore WA0");
        assert_eq!(
            dsp.ct[0], 6,
            "count-from-RAM with bits2:0==4 must post-increment CT0"
        );
    }
}
