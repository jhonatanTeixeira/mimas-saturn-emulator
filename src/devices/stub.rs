//! Stubs para chips fora do caminho de vídeo: devolvem dados simulados só para a sequência
//! da BIOS seguir. Cada stub registra seus acessos (deduplicados) para orientar o ajuste.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::bus::MemoryDevice;

/// Registro compartilhado de acessos a stubs: (nome, offset, escrita) -> (contagem, último valor).
#[derive(Default)]
pub struct AccessLog {
    pub entries: BTreeMap<(&'static str, u32, bool), (u64, u32)>,
}

pub type SharedLog = Rc<RefCell<AccessLog>>;

/// Banco de bytes que guarda o que a BIOS escreve; leituras podem ser sobrepostas por
/// `fixed` (valor constante por offset de byte).
pub struct RegisterStub {
    name: &'static str,
    mem: Vec<u8>,
    fixed: BTreeMap<u32, u8>,
    log: Option<SharedLog>,
}

impl RegisterStub {
    pub fn new(name: &'static str, size: usize) -> Self {
        Self {
            name,
            mem: vec![0; size],
            fixed: BTreeMap::new(),
            log: None,
        }
    }
    pub fn with_log(mut self, log: SharedLog) -> Self {
        self.log = Some(log);
        self
    }
    pub fn fill(mut self, byte: u8) -> Self {
        self.mem.iter_mut().for_each(|b| *b = byte);
        self
    }
    pub fn fixed(mut self, off: u32, byte: u8) -> Self {
        self.fixed.insert(off, byte);
        self
    }
    fn note(&self, off: u32, write: bool, v: u32) {
        if let Some(l) = &self.log {
            let mut log = l.borrow_mut();
            let e = log.entries.entry((self.name, off, write)).or_insert((0, 0));
            e.0 += 1;
            e.1 = v;
        }
    }
}

impl MemoryDevice for RegisterStub {
    fn read_byte(&mut self, off: u32) -> u8 {
        let v = self
            .fixed
            .get(&off)
            .copied()
            .unwrap_or_else(|| self.mem[off as usize % self.mem.len()]);
        self.note(off, false, v as u32);
        v
    }
    fn write_byte(&mut self, off: u32, v: u8) {
        self.note(off, true, v as u32);
        let n = self.mem.len();
        self.mem[off as usize % n] = v;
    }
}

/// Barramento sem nada conectado: leituras devolvem um valor constante, escritas somem.
pub struct OpenBus(pub u8);

impl MemoryDevice for OpenBus {
    fn read_byte(&mut self, _off: u32) -> u8 {
        self.0
    }
    fn write_byte(&mut self, _off: u32, _v: u8) {}
}
