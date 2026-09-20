//! Barramento externo do sistema: registro de dispositivos por faixa de endereços físicos
//! (29 bits). Novos chips entram só com `map(...)` — nenhum código existente muda (OCP).

use std::collections::BTreeMap;

use super::device::MemoryDevice;

const PAGE_SHIFT: u32 = 16;
const PAGES: usize = 1 << (29 - PAGE_SHIFT);
const NONE: u16 = u16::MAX;
/// Granularidade do rastreio de código compilado.
const CODE_SHIFT: u32 = 8;

struct Slot {
    name: &'static str,
    base: u32,
    /// Tamanho do armazenamento subjacente; a faixa mapeada se repete a cada `mirror` bytes.
    mirror: u32,
    dev: Box<dyn MemoryDevice>,
    code_bits: Option<Vec<u8>>,
}

pub struct SystemBus {
    page_slot: Vec<u16>,
    slots: Vec<Slot>,
    dirty: Vec<(u32, u32)>,
    /// Acessos a endereços sem dispositivo: (página de 64 KiB, é escrita) -> contagem.
    unmapped: BTreeMap<(u32, bool), u64>,
}

impl Default for SystemBus {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemBus {
    pub fn new() -> Self {
        Self {
            page_slot: vec![NONE; PAGES],
            slots: Vec::new(),
            dirty: Vec::new(),
            unmapped: BTreeMap::new(),
        }
    }

    /// Mapeia `dev` em `[base, base+len)`. `mirror` é o tamanho real do dispositivo
    /// (a faixa se repete a cada `mirror` bytes). `track_code` habilita a invalidação de
    /// blocos JIT quando esta memória for sobrescrita.
    pub fn map(
        &mut self,
        name: &'static str,
        base: u32,
        len: u32,
        mirror: u32,
        dev: Box<dyn MemoryDevice>,
        track_code: bool,
    ) {
        assert!(
            mirror > 0 && base % (1 << PAGE_SHIFT) == 0 && len % (1 << PAGE_SHIFT) == 0,
            "faixa {name} desalinhada"
        );
        let idx = self.slots.len() as u16;
        let code_bits = track_code.then(|| vec![0u8; (mirror >> CODE_SHIFT) as usize]);
        self.slots.push(Slot {
            name,
            base,
            mirror,
            dev,
            code_bits,
        });
        for p in (base >> PAGE_SHIFT)..((base + len) >> PAGE_SHIFT) {
            assert_eq!(
                self.page_slot[p as usize],
                NONE,
                "faixa {name} sobrepõe outra em {:08X}",
                p << PAGE_SHIFT
            );
            self.page_slot[p as usize] = idx;
        }
    }

    fn lookup(&mut self, addr: u32) -> Option<(usize, u32)> {
        let a = addr & 0x1FFF_FFFF;
        let s = self.page_slot[(a >> PAGE_SHIFT) as usize];
        if s == NONE {
            *self.unmapped.entry((a >> PAGE_SHIFT, false)).or_insert(0) += 1;
            return None;
        }
        let slot = &self.slots[s as usize];
        Some((s as usize, (a - slot.base) % slot.mirror.max(1)))
    }

    fn lookup_w(&mut self, addr: u32) -> Option<(usize, u32)> {
        let a = addr & 0x1FFF_FFFF;
        let s = self.page_slot[(a >> PAGE_SHIFT) as usize];
        if s == NONE {
            *self.unmapped.entry((a >> PAGE_SHIFT, true)).or_insert(0) += 1;
            return None;
        }
        let slot = &self.slots[s as usize];
        Some((s as usize, (a - slot.base) % slot.mirror.max(1)))
    }

    fn note_write(&mut self, s: usize, off: u32, size: u32) {
        let slot = &mut self.slots[s];
        if let Some(bits) = slot.code_bits.as_mut() {
            for o in [off, off + size - 1] {
                let chunk = (o >> CODE_SHIFT) as usize;
                if bits[chunk] != 0 {
                    bits[chunk] = 0;
                    let start = slot.base + ((chunk as u32) << CODE_SHIFT);
                    self.dirty.push((start, start + (1 << CODE_SHIFT)));
                }
            }
        }
    }

    pub fn read8(&mut self, addr: u32) -> u8 {
        match self.lookup(addr) {
            Some((s, off)) => self.slots[s].dev.read_byte(off),
            None => 0,
        }
    }
    pub fn read16(&mut self, addr: u32) -> u16 {
        match self.lookup(addr) {
            Some((s, off)) => self.slots[s].dev.read_word(off),
            None => 0,
        }
    }
    pub fn read32(&mut self, addr: u32) -> u32 {
        match self.lookup(addr) {
            Some((s, off)) => self.slots[s].dev.read_long(off),
            None => 0,
        }
    }
    pub fn write8(&mut self, addr: u32, v: u8) {
        if let Some((s, off)) = self.lookup_w(addr) {
            self.note_write(s, off, 1);
            self.slots[s].dev.write_byte(off, v);
        }
    }
    pub fn write16(&mut self, addr: u32, v: u16) {
        if let Some((s, off)) = self.lookup_w(addr) {
            self.note_write(s, off, 2);
            self.slots[s].dev.write_word(off, v);
        }
    }
    pub fn write32(&mut self, addr: u32, v: u32) {
        if let Some((s, off)) = self.lookup_w(addr) {
            self.note_write(s, off, 4);
            self.slots[s].dev.write_long(off, v);
        }
    }

    /// Endereço sem espelhos: base do dispositivo + deslocamento dentro do armazenamento.
    pub fn canonical(&self, addr: u32) -> u32 {
        let a = addr & 0x1FFF_FFFF;
        let s = self.page_slot[(a >> PAGE_SHIFT) as usize];
        if s == NONE {
            return a;
        }
        let slot = &self.slots[s as usize];
        slot.base + (a - slot.base) % slot.mirror.max(1)
    }

    pub fn mark_code(&mut self, canon_start: u32, len: u32) {
        let s = self.page_slot[(canon_start >> PAGE_SHIFT) as usize];
        if s == NONE {
            return;
        }
        let slot = &mut self.slots[s as usize];
        if let Some(bits) = slot.code_bits.as_mut() {
            let off = canon_start - slot.base;
            let first = (off >> CODE_SHIFT) as usize;
            let last = (((off + len.max(1) - 1) >> CODE_SHIFT) as usize).min(bits.len() - 1);
            for b in &mut bits[first..=last] {
                *b = 1;
            }
        }
    }

    pub fn drain_dirty_code(&mut self, out: &mut Vec<(u32, u32)>) {
        out.append(&mut self.dirty);
    }

    /// Relatório dos acessos a endereços sem dispositivo (útil ao escrever stubs).
    pub fn unmapped_report(&self) -> Vec<(u32, bool, u64)> {
        self.unmapped
            .iter()
            .map(|(&(p, w), &n)| (p << PAGE_SHIFT, w, n))
            .collect()
    }

    pub fn slot_name_at(&self, addr: u32) -> &'static str {
        let a = addr & 0x1FFF_FFFF;
        match self.page_slot[(a >> PAGE_SHIFT) as usize] {
            NONE => "(sem dispositivo)",
            s => self.slots[s as usize].name,
        }
    }
}
