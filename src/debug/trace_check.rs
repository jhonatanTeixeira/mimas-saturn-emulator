//! Validação contra os traces de referência. Eles são amostras "primeira execução de cada
//! PC" (sem o delay slot de desvios tomados, começando no meio da execução), então a checagem
//! é de cobertura + opcode no PC + drift de quadro — nunca um diff linha a linha.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::cpu::sh2_bus::{Sh2Bus, Tracer};
use crate::cpu::state::Sh2State;

/// Chave de deduplicação da referência: PC sem os bits de espelho de cache.
pub fn key(pc: u32) -> u32 {
    pc & 0x0FFF_FFFE
}

#[derive(Clone, Debug)]
pub struct RefEntry {
    pub frame: u32,
    pub pc: u32,
    pub opcode: u16,
}

pub fn load_reference(path: &str) -> std::io::Result<Vec<RefEntry>> {
    let text = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in text.lines() {
        let mut frame = None;
        let mut pc = None;
        let mut op = None;
        for f in line.split(" | ").take(4) {
            if let Some(v) = f.strip_prefix("Frame: ") {
                frame = v.trim().parse().ok();
            } else if let Some(v) = f.strip_prefix("PC: ") {
                pc = u32::from_str_radix(v.trim(), 16).ok();
            } else if let Some(v) = f.strip_prefix("Opcode: ") {
                op = u16::from_str_radix(v.trim(), 16).ok();
            }
        }
        if let (Some(frame), Some(pc), Some(opcode)) = (frame, pc, op) {
            out.push(RefEntry { frame, pc, opcode });
        }
    }
    Ok(out)
}

pub struct Coverage {
    seen: Vec<u64>,
    /// chave -> (PC como executado, opcode, quadro na primeira visita, ordem de visita)
    pub first: HashMap<u32, (u32, u16, u32, usize)>,
    frame: Rc<Cell<u32>>,
}

impl Coverage {
    pub fn new(frame: Rc<Cell<u32>>) -> Self {
        Self {
            seen: vec![0; (1 << 27) / 64],
            first: HashMap::new(),
            frame,
        }
    }
}

impl Tracer for Coverage {
    fn on_instruction(&mut self, _st: &Sh2State, pc: u32, bus: &mut dyn Sh2Bus) {
        let idx = (key(pc) >> 1) as usize;
        let (w, b) = (idx / 64, idx % 64);
        if self.seen[w] & (1 << b) == 0 {
            self.seen[w] |= 1 << b;
            let op = bus.read16(pc);
            let n = self.first.len();
            self.first.insert(key(pc), (pc, op, self.frame.get(), n));
        }
    }
}

pub struct Report {
    pub total: usize,
    pub visited: usize,
    pub opcode_mismatch: Vec<(u32, u16, u16)>,
    pub missing: Vec<usize>,
    pub median_frame_drift: i64,
    pub extra: usize,
}

pub fn compare(reference: &[RefEntry], cov: &Coverage) -> Report {
    let mut visited = 0;
    let mut mism = Vec::new();
    let mut missing = Vec::new();
    let mut drifts = Vec::new();
    let mut refkeys = std::collections::HashSet::new();
    for (i, r) in reference.iter().enumerate() {
        refkeys.insert(key(r.pc));
        match cov.first.get(&key(r.pc)) {
            Some(&(_, op, frame, _)) => {
                visited += 1;
                if op != r.opcode {
                    mism.push((r.pc, r.opcode, op));
                }
                drifts.push(frame as i64 - r.frame as i64);
            }
            None => missing.push(i),
        }
    }
    drifts.sort_unstable();
    let median = drifts.get(drifts.len() / 2).copied().unwrap_or(0);
    let extra = cov.first.keys().filter(|k| !refkeys.contains(k)).count();
    Report {
        total: reference.len(),
        visited,
        opcode_mismatch: mism,
        missing,
        median_frame_drift: median,
        extra,
    }
}

impl Report {
    pub fn print(&self, reference: &[RefEntry]) {
        println!(
            "TRACE: {}/{} PCs da referência visitados ({:.1}%), {} PCs nossos fora da referência, drift mediano de quadro = {:+}",
            self.visited,
            self.total,
            100.0 * self.visited as f64 / self.total.max(1) as f64,
            self.extra,
            self.median_frame_drift
        );
        if !self.opcode_mismatch.is_empty() {
            println!(
                "TRACE: {} opcodes divergentes (pc, ref, nosso), primeiros:",
                self.opcode_mismatch.len()
            );
            for (pc, a, b) in self.opcode_mismatch.iter().take(8) {
                println!("   {:08X}: ref={:04X} nosso={:04X}", pc, a, b);
            }
        }
        if let Some(&first) = self.missing.first() {
            let prev = first.checked_sub(1).map(|i| reference[i].pc);
            println!(
                "TRACE: primeiro PC não visitado (índice {}): {:08X} (quadro ref {}), anterior na referência: {:08X?}",
                first, reference[first].pc, reference[first].frame, prev
            );
        }
    }
}

impl Report {
    /// Faixas contíguas (na ordem da referência) de PCs não visitados: (índice inicial, tamanho, quadro).
    pub fn missing_runs(&self, reference: &[RefEntry]) -> Vec<(usize, usize, u32, u32)> {
        let mut runs: Vec<(usize, usize, u32, u32)> = Vec::new();
        for &i in &self.missing {
            match runs.last_mut() {
                Some(r) if r.0 + r.1 == i => r.1 += 1,
                _ => runs.push((i, 1, reference[i].frame, reference[i].pc)),
            }
        }
        runs
    }
}
