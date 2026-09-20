//! Breakpoint com histórico: ao atingir o N-ésimo passe por um PC, imprime as últimas
//! instruções executadas com os registradores. Ferramenta de investigação de divergências.

use std::cell::Cell;
use std::collections::VecDeque;
use std::rc::Rc;

use crate::cpu::decode::decode;
use crate::cpu::sh2_bus::{Sh2Bus, Tracer};
use crate::cpu::state::Sh2State;

struct Entry {
    pc: u32,
    op: u16,
    r: [u32; 16],
    sr: u32,
    pr: u32,
}

pub struct BreakTrace {
    pcs: Vec<u32>,
    occurrence: u64,
    hits: u64,
    history: VecDeque<Entry>,
    depth: usize,
    frame: Rc<Cell<u32>>,
    pub done: bool,
}

impl BreakTrace {
    pub fn new(pcs: Vec<u32>, occurrence: u64, depth: usize, frame: Rc<Cell<u32>>) -> Self {
        Self {
            pcs,
            occurrence,
            hits: 0,
            history: VecDeque::new(),
            depth,
            frame,
            done: false,
        }
    }
}

impl Tracer for BreakTrace {
    fn on_instruction(&mut self, st: &Sh2State, pc: u32, bus: &mut dyn Sh2Bus) {
        if self.done {
            return;
        }
        let op = bus.read16(pc);
        if self.history.len() == self.depth {
            self.history.pop_front();
        }
        self.history.push_back(Entry {
            pc,
            op,
            r: st.r,
            sr: st.sr,
            pr: st.pr,
        });
        if self
            .pcs
            .iter()
            .any(|&p| p & 0x0FFF_FFFE == pc & 0x0FFF_FFFE)
        {
            self.hits += 1;
            if self.hits == self.occurrence {
                self.done = true;
                println!(
                    "=== breakpoint {:08X} (passagem #{}) no quadro {} — últimas {} instruções ===",
                    pc,
                    self.hits,
                    self.frame.get(),
                    self.history.len()
                );
                for e in &self.history {
                    println!(
                        "{:08X}: {:04X} {:<34} sr={:08X} pr={:08X} | {}",
                        e.pc,
                        e.op,
                        format!("{:?}", decode(e.op)),
                        e.sr,
                        e.pr,
                        e.r.iter()
                            .enumerate()
                            .map(|(i, v)| format!("r{i}={v:X}"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    );
                }
            }
        }
    }
}
