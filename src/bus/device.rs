//! Interface comum de todo componente endereçável (ISP/DIP): a CPU e o barramento só
//! conhecem esta trait. Endereços chegam como deslocamento dentro da faixa mapeada.
//! O Saturn é big-endian: os métodos padrão compõem palavras a partir de bytes.

pub trait MemoryDevice {
    fn read_byte(&mut self, off: u32) -> u8;
    fn write_byte(&mut self, off: u32, v: u8);

    fn read_word(&mut self, off: u32) -> u16 {
        ((self.read_byte(off) as u16) << 8) | self.read_byte(off.wrapping_add(1)) as u16
    }
    fn read_long(&mut self, off: u32) -> u32 {
        ((self.read_word(off) as u32) << 16) | self.read_word(off.wrapping_add(2)) as u32
    }
    fn write_word(&mut self, off: u32, v: u16) {
        self.write_byte(off, (v >> 8) as u8);
        self.write_byte(off.wrapping_add(1), v as u8);
    }
    fn write_long(&mut self, off: u32, v: u32) {
        self.write_word(off, (v >> 16) as u16);
        self.write_word(off.wrapping_add(2), v as u16);
    }
}

/// Dispositivo compartilhado entre o barramento e quem precisa observá-lo (renderizador,
/// temporização, controlador de interrupções...). O emulador é single-thread.
pub struct Shared<T>(pub std::rc::Rc<std::cell::RefCell<T>>);

impl<T> Shared<T> {
    pub fn new(v: T) -> (Self, std::rc::Rc<std::cell::RefCell<T>>) {
        let rc = std::rc::Rc::new(std::cell::RefCell::new(v));
        (Shared(rc.clone()), rc)
    }
}

impl<T: MemoryDevice> MemoryDevice for Shared<T> {
    fn read_byte(&mut self, off: u32) -> u8 {
        self.0.borrow_mut().read_byte(off)
    }
    fn write_byte(&mut self, off: u32, v: u8) {
        self.0.borrow_mut().write_byte(off, v)
    }
    fn read_word(&mut self, off: u32) -> u16 {
        self.0.borrow_mut().read_word(off)
    }
    fn read_long(&mut self, off: u32) -> u32 {
        self.0.borrow_mut().read_long(off)
    }
    fn write_word(&mut self, off: u32, v: u16) {
        self.0.borrow_mut().write_word(off, v)
    }
    fn write_long(&mut self, off: u32, v: u32) {
        self.0.borrow_mut().write_long(off, v)
    }
}
