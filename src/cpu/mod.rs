//! CPU SH-2: estado + JIT + entrada de exceções. Não há interpretador.

pub mod address_space;
pub mod decode;
pub mod dmac;
pub mod jit;
pub mod onchip;
pub mod sh2_bus;
pub mod state;

use jit::Sh2Jit;
use sh2_bus::{Sh2Bus, Tracer};
use state::*;

#[derive(Debug)]
pub enum Fault {
    IllegalInstruction { pc: u32, op: u16 },
}

pub struct Sh2Cpu {
    pub st: Sh2State,
    pub jit: Sh2Jit,
    pub sleeping: bool,
}

impl Sh2Cpu {
    /// Reset de energia: PC/SP dos vetores 0 e 4, SR com máscara de interrupção 0xF.
    pub fn power_on(bus: &mut dyn Sh2Bus) -> Self {
        let mut st = Sh2State::default();
        st.pc = bus.read32(0);
        st.r[15] = bus.read32(4);
        st.sr = 0xF0;
        Self {
            st,
            jit: Sh2Jit::new(),
            sleeping: false,
        }
    }

    /// Executa um bloco; devolve os ciclos consumidos.
    pub fn run_block(
        &mut self,
        bus: &mut dyn Sh2Bus,
        tracer: Option<&mut dyn Tracer>,
    ) -> Result<u64, Fault> {
        if self.sleeping {
            return Ok(64);
        }
        let c0 = self.st.cycles;
        self.jit.run_block(&mut self.st, bus, tracer);
        let reason = std::mem::replace(&mut self.st.exit_reason, EXIT_NONE);
        match reason {
            EXIT_ILLEGAL => {
                let pc = self.st.exit_arg;
                let op = bus.read16(pc);
                return Err(Fault::IllegalInstruction { pc, op });
            }
            EXIT_TRAPA => {
                let vector = self.st.exit_arg;
                self.enter_exception(bus, vector, None);
            }
            EXIT_SLEEP => self.sleeping = true,
            _ => {}
        }
        Ok(self.st.cycles - c0)
    }

    /// Sequência de exceção do SH-2: empilha SR e PC, opcionalmente atualiza I, vai ao vetor.
    pub fn enter_exception(&mut self, bus: &mut dyn Sh2Bus, vector: u32, new_imask: Option<u32>) {
        let sp = self.st.r[15].wrapping_sub(4);
        bus.write32(sp, self.st.sr);
        let sp = sp.wrapping_sub(4);
        bus.write32(sp, self.st.pc);
        self.st.r[15] = sp;
        if let Some(level) = new_imask {
            self.st.sr = (self.st.sr & !0xF0) | ((level & 0xF) << 4);
        }
        self.st.pc = bus.read32(self.st.vbr.wrapping_add(vector * 4));
        self.st.cycles += 8;
    }

    /// Tenta aceitar uma interrupção de `level` com `vector`; devolve se foi aceita.
    pub fn try_interrupt(&mut self, bus: &mut dyn Sh2Bus, level: u32, vector: u32) -> bool {
        if level <= self.st.imask() {
            return false;
        }
        self.sleeping = false;
        self.enter_exception(bus, vector, Some(level));
        true
    }
}
