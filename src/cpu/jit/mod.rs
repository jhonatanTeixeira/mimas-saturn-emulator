//! Cache de blocos compilados e despacho. Um bloco vai do PC de entrada até um desvio (com
//! delay slot) ou até `MAX_BLOCK_INSNS`. Blocos em RAM são invalidados quando o barramento
//! informa que a memória de código foi sobrescrita.
//!
//! Nada aqui conhece a arquitetura de destino: só fala com `backend::Compiler` e trata
//! `CompiledBlock` como opaco. Ver `backend/mod.rs` para a seleção de backend por
//! `cfg(target_arch)` — é o que faz um segundo backend (ARM64, por exemplo) custar zero
//! linhas aqui.

pub mod backend;
#[cfg(test)]
mod tests;

use std::collections::HashMap;

use backend::{CompiledBlock, Compiler};

use crate::cpu::sh2_bus::{Sh2Bus, Sh2Runtime, Tracer};
use crate::cpu::state::Sh2State;

type BlockFn = unsafe extern "C" fn(*mut Sh2State, *mut Sh2Runtime) -> u32;

struct Entry {
    block: CompiledBlock,
    canon_start: u32,
    canon_end: u32,
}

pub struct Sh2Jit {
    blocks: HashMap<u32, Entry>,
    trace: bool,
    dirty: Vec<(u32, u32)>,
    pub compiled_total: u64,
}

impl Default for Sh2Jit {
    fn default() -> Self {
        Self::new()
    }
}

impl Sh2Jit {
    pub fn new() -> Self {
        Self {
            blocks: HashMap::new(),
            trace: false,
            dirty: Vec::new(),
            compiled_total: 0,
        }
    }

    /// No modo trace cada instrução chama o `Tracer` antes de executar.
    pub fn set_trace(&mut self, on: bool) {
        if on != self.trace {
            self.trace = on;
            self.blocks.clear();
        }
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    fn invalidate(&mut self) {
        let dirty = std::mem::take(&mut self.dirty);
        self.blocks.retain(|_, e| {
            !dirty
                .iter()
                .any(|&(s, en)| e.canon_start < en && s < e.canon_end)
        });
    }

    /// Executa exatamente um bloco a partir de `st.pc` e atualiza `st.pc`.
    pub fn run_block(
        &mut self,
        st: &mut Sh2State,
        bus: &mut dyn Sh2Bus,
        tracer: Option<&mut dyn Tracer>,
    ) {
        bus.drain_dirty_code(&mut self.dirty);
        if !self.dirty.is_empty() {
            self.invalidate();
        }
        let pc = st.pc;
        if !self.blocks.contains_key(&pc) {
            let block = {
                let mut fetch = |a: u32| bus.read16(a);
                Compiler::compile(pc, &mut fetch, self.trace)
            };
            let canon_start = bus.canonical(block.start);
            let len = block.end.wrapping_sub(block.start);
            bus.mark_code(canon_start, len);
            self.compiled_total += 1;
            self.blocks.insert(
                pc,
                Entry {
                    block,
                    canon_start,
                    canon_end: canon_start + len,
                },
            );
        }
        let e = &self.blocks[&pc];
        let f: BlockFn = unsafe { std::mem::transmute(e.block.buf.ptr(e.block.entry)) };
        let mut rt = Sh2Runtime::new(bus, tracer);
        st.pc = unsafe { f(st as *mut Sh2State, &mut rt as *mut Sh2Runtime) };
    }
}
