//! Visão do barramento pela CPU (inversão de dependência): o JIT só conhece `Sh2Bus`,
//! nunca um barramento concreto. Endereços aqui são os de 32 bits do SH-2.

use super::state::Sh2State;

pub trait Sh2Bus {
    fn read8(&mut self, addr: u32) -> u8;
    fn read16(&mut self, addr: u32) -> u16;
    fn read32(&mut self, addr: u32) -> u32;
    fn write8(&mut self, addr: u32, v: u8);
    fn write16(&mut self, addr: u32, v: u16);
    fn write32(&mut self, addr: u32, v: u32);

    /// Endereço canônico (sem espelhos) usado para rastrear código compilado.
    fn canonical(&self, addr: u32) -> u32 {
        addr & 0x1FFF_FFFF
    }
    /// O JIT avisa que compilou código em `[start, start+len)` (endereços canônicos).
    fn mark_code(&mut self, _start: u32, _len: u32) {}
    /// Devolve (e limpa) as faixas canônicas de código que foram sobrescritas.
    fn drain_dirty_code(&mut self, _out: &mut Vec<(u32, u32)>) {}
}

/// Gancho de depuração chamado antes de cada instrução quando o JIT compila em modo trace.
pub trait Tracer {
    fn on_instruction(&mut self, st: &Sh2State, pc: u32, bus: &mut dyn Sh2Bus);
}

/// Contexto passado (por ponteiro) ao código gerado. Guarda ponteiros crus porque só existe
/// durante uma chamada de bloco (o código nativo não entende lifetimes de Rust).
pub struct Sh2Runtime {
    bus: *mut dyn Sh2Bus,
    tracer: Option<*mut dyn Tracer>,
}

impl Sh2Runtime {
    /// # Safety (interna)
    /// O `Sh2Runtime` não pode sobreviver aos empréstimos recebidos; quem o cria
    /// (`Sh2Jit::run_block`) só o usa dentro da própria chamada.
    pub fn new(bus: &mut dyn Sh2Bus, tracer: Option<&mut dyn Tracer>) -> Self {
        unsafe {
            let bus: *mut dyn Sh2Bus = std::mem::transmute::<&mut dyn Sh2Bus, *mut dyn Sh2Bus>(bus);
            let tracer = tracer.map(|t| std::mem::transmute::<&mut dyn Tracer, *mut dyn Tracer>(t));
            Self { bus, tracer }
        }
    }
}

type Rt = Sh2Runtime;

pub extern "C" fn rt_read8(rt: *mut Rt, addr: u32) -> u32 {
    unsafe { (*(*rt).bus).read8(addr) as u32 }
}
pub extern "C" fn rt_read16(rt: *mut Rt, addr: u32) -> u32 {
    unsafe { (*(*rt).bus).read16(addr) as u32 }
}
pub extern "C" fn rt_read32(rt: *mut Rt, addr: u32) -> u32 {
    unsafe { (*(*rt).bus).read32(addr) }
}
pub extern "C" fn rt_write8(rt: *mut Rt, addr: u32, v: u32) {
    unsafe { (*(*rt).bus).write8(addr, v as u8) }
}
pub extern "C" fn rt_write16(rt: *mut Rt, addr: u32, v: u32) {
    unsafe { (*(*rt).bus).write16(addr, v as u16) }
}
pub extern "C" fn rt_write32(rt: *mut Rt, addr: u32, v: u32) {
    unsafe { (*(*rt).bus).write32(addr, v) }
}
pub extern "C" fn rt_trace(st: *const Sh2State, rt: *mut Rt, pc: u32) {
    unsafe {
        if let Some(t) = (*rt).tracer {
            (*t).on_instruction(&*st, pc, &mut *(*rt).bus);
        }
    }
}
