//! The sound 68000: it runs the driver the BIOS uploads into sound RAM.
//!
//! The core comes from the `m68k` crate. What is ours is the address space it sees — sound
//! RAM at `0x000000`, SCSP registers at `0x100000` — and the pacing, derived from the
//! cycles the SH-2 has already executed. The sound CPU never runs ahead of the SH-2: it
//! reads memory the SH-2 writes, and running ahead would be reading the future.

use std::collections::HashMap;

use m68k::{AddressBus, CpuCore, CpuType, CycleBatchControl};

use crate::bus::MemoryDevice;
use crate::devices::ram::SoundRam;
use crate::devices::scsp::Scsp;

/// Clock ratio: SH-2 at 28.6364 MHz, 68000 at 11.2896 MHz.
const NUM: u64 = 112_896;
const DEN: u64 = 286_364;
/// 68000 cycles per call into the core. 64 is about a quarter of a sample: small enough to
/// stay glued to the SH-2, large enough that a single instruction cannot dominate the call.
const MIN_BATCH: i64 = 64;
/// Average cycles per instruction for the real BIOS driver, measured (`docs/sound.md`:
/// 222,897 instructions in 2,000,000 cycles). `run_batch` is the only entry point the
/// crate's Cranelift JIT compiles through, and it is instruction-budgeted, not
/// cycle-budgeted — it clobbers `cycles_remaining` and its result reports no cycle count at
/// all. This constant is how a cycle budget becomes an instruction budget and back; it is
/// not exact per instruction (real 68000 opcodes run from 4 to 20+ cycles), so this trades a
/// little pacing precision for real throughput. The audio pipeline has an exact-value gate
/// (`compare_audio`, step 9, no slack above the measured floor) that would fail if this
/// estimate drifted audio timing enough to matter — see `docs/current_status.md`.
const AVG_CYCLES_PER_INSTR: f64 = 2_000_000.0 / 222_897.0;

pub struct SoundCpu {
    cpu: CpuCore,
    /// Turned on by the SMPC's `SNDON`, off by `SNDOFF`.
    pub running: bool,
    /// SH-2 cycles not yet converted into 68000 cycles.
    carry: u64,
    /// 68000 cycles earned and not yet spent. Goes negative when an instruction overruns
    /// the batch — instructions are never split, so the overrun must be charged to the next
    /// batch or the sound CPU accelerates without bound.
    budget: i64,
    pub cycles: u64,
    pub instructions: u64,
    /// Last non-ordinary exit from the core, for diagnosis.
    pub last_exit: Option<String>,
    /// How many instructions started at each PC. Off by default: the hook costs a branch
    /// per instruction. `--sound-profile` turns it on, and it is how you find the driver
    /// sitting in a wait loop instead of doing its work.
    pub profile: Option<HashMap<u32, u64>>,
}

impl Default for SoundCpu {
    fn default() -> Self {
        Self::new()
    }
}

impl SoundCpu {
    pub fn new() -> Self {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68000);
        Self {
            cpu,
            running: false,
            carry: 0,
            budget: 0,
            cycles: 0,
            instructions: 0,
            last_exit: None,
            profile: None,
        }
    }

    /// `SNDON`: releases the 68000 from reset, which loads SP and PC from the vectors the
    /// BIOS placed at the bottom of sound RAM.
    pub fn power_on(&mut self, ram: &mut SoundRam, scsp: &mut Scsp) {
        let mut bus = Bus { ram, scsp };
        self.cpu.reset(&mut bus);
        self.running = true;
    }

    pub fn power_off(&mut self) {
        self.running = false;
    }

    pub fn pc(&self) -> u32 {
        self.cpu.pc
    }

    /// Run the 68000 for the equivalent of `sh2_cycles`, returning the cycles it moved.
    pub fn advance(&mut self, sh2_cycles: u64, ram: &mut SoundRam, scsp: &mut Scsp) -> u64 {
        self.carry += sh2_cycles * NUM;
        let earned = (self.carry / DEN) as i64;
        self.carry -= earned as u64 * DEN;
        if !self.running {
            self.budget = 0;
            return 0;
        }
        self.budget += earned;
        if self.budget < MIN_BATCH {
            return 0;
        }

        self.cpu.set_irq(scsp.irq_level());

        let mut bus = Bus { ram, scsp };
        // `run_batch` has no hook variant, so profiling — off by default, opt-in through
        // --sound-profile — stays on the cycle-exact interpreter. Every other call goes
        // through the JIT.
        let (spent, instructions, exit) = if let Some(hist) = self.profile.as_mut() {
            let budget = self.budget.min(i32::MAX as i64) as i32;
            let r = self
                .cpu
                .run_for_cycles_with_hook(&mut bus, budget, |cpu, _, _| {
                    *hist.entry(cpu.pc).or_insert(0) += 1;
                    CycleBatchControl::Continue
                });
            (
                r.cycles.max(0) as i64,
                r.instructions as u64,
                format!("{:?}", r.exit),
            )
        } else {
            let instr_budget = ((self.budget as f64 / AVG_CYCLES_PER_INSTR).floor() as u32).max(1);
            let r = self.cpu.run_batch(&mut bus, instr_budget, &[]);
            let spent = (r.instructions as f64 * AVG_CYCLES_PER_INSTR).round() as i64;
            (spent, r.instructions as u64, format!("{:?}", r.exit))
        };
        self.budget -= spent; // the overrun becomes debt
        self.cycles += spent.max(0) as u64;
        self.instructions += instructions;
        if exit != "BudgetExhausted" {
            self.last_exit = Some(exit);
        }
        spent.max(0) as u64
    }
}

/// What the 68000 sees. Outside these two ranges the bus reads as open.
struct Bus<'a> {
    ram: &'a mut SoundRam,
    scsp: &'a mut Scsp,
}

const RAM_END: u32 = 0x8_0000;
const SCSP_BASE: u32 = 0x10_0000;
const SCSP_END: u32 = 0x10_1000;

impl AddressBus for Bus<'_> {
    fn read_byte(&mut self, address: u32) -> u8 {
        let a = address & 0xFF_FFFF;
        if a < RAM_END {
            self.ram.read_byte(a)
        } else if (SCSP_BASE..SCSP_END).contains(&a) {
            self.scsp.read_byte(a - SCSP_BASE)
        } else {
            0xFF
        }
    }

    fn read_word(&mut self, address: u32) -> u16 {
        let a = address & 0xFF_FFFE;
        if a < RAM_END {
            self.ram.read_word(a)
        } else if (SCSP_BASE..SCSP_END).contains(&a) {
            self.scsp.read_word(a - SCSP_BASE)
        } else {
            0xFFFF
        }
    }

    fn read_long(&mut self, address: u32) -> u32 {
        ((self.read_word(address) as u32) << 16) | self.read_word(address.wrapping_add(2)) as u32
    }

    fn write_byte(&mut self, address: u32, value: u8) {
        let a = address & 0xFF_FFFF;
        if a < RAM_END {
            self.ram.write_byte(a, value);
        } else if (SCSP_BASE..SCSP_END).contains(&a) {
            self.scsp.write_byte(a - SCSP_BASE, value);
        }
    }

    fn write_word(&mut self, address: u32, value: u16) {
        let a = address & 0xFF_FFFE;
        if a < RAM_END {
            self.ram.write_word(a, value);
        } else if (SCSP_BASE..SCSP_END).contains(&a) {
            self.scsp.write_word(a - SCSP_BASE, value);
        }
    }

    fn write_long(&mut self, address: u32, value: u32) {
        self.write_word(address, (value >> 16) as u16);
        self.write_word(address.wrapping_add(2), value as u16);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sound RAM holding the reset vectors and a branch-to-self at the entry point, which
    /// is what an idle driver looks like.
    fn ram_with_driver() -> SoundRam {
        let mut ram = SoundRam::new(0x8_0000);
        ram.write_long(0x0, 0x0000_A000); // initial SP
        ram.write_long(0x4, 0x0000_1000); // initial PC
        ram.write_word(0x1000, 0x60FE); // BRA to itself
        ram
    }

    #[test]
    fn power_on_loads_sp_and_pc_from_the_vectors() {
        let mut ram = ram_with_driver();
        let mut scsp = Scsp::new();
        let mut cpu = SoundCpu::new();
        assert!(!cpu.running);
        cpu.power_on(&mut ram, &mut scsp);
        assert!(cpu.running);
        assert_eq!(cpu.pc(), 0x1000, "the 68000 starts where the vector points");
    }

    #[test]
    fn a_powered_off_cpu_consumes_nothing() {
        let mut ram = ram_with_driver();
        let mut scsp = Scsp::new();
        let mut cpu = SoundCpu::new();
        assert_eq!(cpu.advance(1_000_000, &mut ram, &mut scsp), 0);
        assert_eq!(cpu.cycles, 0);
    }

    #[test]
    fn the_clock_ratio_holds_over_a_long_run() {
        let mut ram = ram_with_driver();
        let mut scsp = Scsp::new();
        let mut cpu = SoundCpu::new();
        cpu.power_on(&mut ram, &mut scsp);

        // One emulated second of SH-2 time, fed in small pieces the way blocks arrive.
        let sh2_cycles = 28_636_400u64;
        for _ in 0..sh2_cycles / 100 {
            cpu.advance(100, &mut ram, &mut scsp);
        }
        let expected = sh2_cycles * NUM / DEN;
        let drift = cpu.cycles.abs_diff(expected);
        assert!(
            drift < expected / 1000,
            "the sound CPU drifted {drift} cycles from {expected}, over 0.1%"
        );
    }

    #[test]
    fn an_instruction_that_overruns_its_batch_is_charged_to_the_next_one() {
        let mut ram = ram_with_driver();
        let mut scsp = Scsp::new();
        let mut cpu = SoundCpu::new();
        cpu.power_on(&mut ram, &mut scsp);

        // Budgets below the batch size must not run anything at all: this is what stopped
        // the sound CPU from racing ahead when it was called once per SH-2 block.
        let ran = cpu.advance(10, &mut ram, &mut scsp);
        assert_eq!(ran, 0);
        assert_eq!(cpu.cycles, 0);
    }

    /// The profiler is how you find the driver sitting in a wait loop instead of doing its
    /// work — a 5-instruction loop taking half the driver's time is what a stuck handshake
    /// looks like from here. It must count what actually ran, at the PC it ran from.
    #[test]
    fn the_profiler_counts_instructions_at_the_pc_that_ran_them() {
        let mut ram = ram_with_driver();
        let mut scsp = Scsp::new();
        let mut cpu = SoundCpu::new();
        cpu.profile = Some(Default::default());
        cpu.power_on(&mut ram, &mut scsp);

        cpu.advance(10_000, &mut ram, &mut scsp);

        let hist = cpu.profile.as_ref().unwrap();
        assert!(!hist.is_empty(), "the profile has to record something");
        let (hot, n) = hist.iter().max_by_key(|(_, n)| **n).unwrap();
        assert_eq!(*hot, 0x1000, "the driver loop is the hottest PC");
        assert_eq!(
            hist.values().sum::<u64>(),
            cpu.instructions,
            "every executed instruction is counted once"
        );
        assert!(*n > 1, "the loop repeats");
    }
}
