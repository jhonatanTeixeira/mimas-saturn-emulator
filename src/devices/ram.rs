//! Memórias simples: RAM e ROM (ambas big-endian, tamanho potência de dois).

use crate::bus::MemoryDevice;

pub struct Ram {
    data: Vec<u8>,
    mask: usize,
}

impl Ram {
    pub fn new(size: usize) -> Self {
        assert!(size.is_power_of_two());
        Self {
            data: vec![0; size],
            mask: size - 1,
        }
    }
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

impl MemoryDevice for Ram {
    fn read_byte(&mut self, off: u32) -> u8 {
        self.data[off as usize & self.mask]
    }
    fn write_byte(&mut self, off: u32, v: u8) {
        self.data[off as usize & self.mask] = v;
    }
    fn read_word(&mut self, off: u32) -> u16 {
        let o = off as usize & self.mask;
        u16::from_be_bytes([self.data[o], self.data[(o + 1) & self.mask]])
    }
    fn write_word(&mut self, off: u32, v: u16) {
        let o = off as usize & self.mask;
        let b = v.to_be_bytes();
        self.data[o] = b[0];
        self.data[(o + 1) & self.mask] = b[1];
    }
    fn read_long(&mut self, off: u32) -> u32 {
        let o = off as usize & self.mask;
        u32::from_be_bytes([
            self.data[o],
            self.data[(o + 1) & self.mask],
            self.data[(o + 2) & self.mask],
            self.data[(o + 3) & self.mask],
        ])
    }
    fn write_long(&mut self, off: u32, v: u32) {
        let o = off as usize & self.mask;
        for (i, b) in v.to_be_bytes().iter().enumerate() {
            self.data[(o + i) & self.mask] = *b;
        }
    }
}

pub struct Rom {
    ram: Ram,
}

impl Rom {
    /// A ROM é espelhada em `size` (potência de dois) — o conteúdo é copiado para o início.
    pub fn new(image: &[u8], size: usize) -> Self {
        let mut ram = Ram::new(size);
        let n = image.len().min(size);
        ram.data[..n].copy_from_slice(&image[..n]);
        Self { ram }
    }
}

impl MemoryDevice for Rom {
    fn read_byte(&mut self, off: u32) -> u8 {
        self.ram.read_byte(off)
    }
    fn read_word(&mut self, off: u32) -> u16 {
        self.ram.read_word(off)
    }
    fn read_long(&mut self, off: u32) -> u32 {
        self.ram.read_long(off)
    }
    fn write_byte(&mut self, _off: u32, _v: u8) {}
    fn write_word(&mut self, _off: u32, _v: u16) {}
    fn write_long(&mut self, _off: u32, _v: u32) {}
}

/// RAM de som do SCSP com o "driver de som" simulado. A BIOS conversa com o driver da CPU 68000
/// por uma caixa de correio: escreve parâmetros e depois o byte de comando em `DOORBELL`, e só
/// escreve de novo quando esse byte volta a 0. Como a 68000 não existe (fora do caminho de
/// vídeo), o driver simulado consome o comando na hora.
pub struct SoundRam {
    ram: Ram,
}

/// Offset do byte de comando da caixa de correio na RAM de som.
pub const SOUND_DOORBELL: u32 = 0x700;

impl SoundRam {
    pub fn new(size: usize) -> Self {
        Self {
            ram: Ram::new(size),
        }
    }
    /// Conteúdo bruto, para despejo e para rodar o driver de som fora do emulador.
    pub fn data(&self) -> &[u8] {
        self.ram.data()
    }
}

impl SoundRam {
    /// O driver simulado consome o comando: qualquer escrita (byte/word/long) que cubra o byte da
    /// campainha o devolve a 0, preservando os demais bytes escritos (parâmetros).
    fn consume_doorbell(&mut self, off: u32, size: u32) {
        let o = off & 0x7_FFFF;
        if o <= SOUND_DOORBELL && SOUND_DOORBELL < o + size {
            self.ram.write_byte(SOUND_DOORBELL, 0);
        }
    }
}

impl MemoryDevice for SoundRam {
    fn read_byte(&mut self, off: u32) -> u8 {
        self.ram.read_byte(off)
    }
    fn write_byte(&mut self, off: u32, v: u8) {
        self.ram.write_byte(off, v);
        self.consume_doorbell(off, 1);
    }
    fn read_word(&mut self, off: u32) -> u16 {
        self.ram.read_word(off)
    }
    fn write_word(&mut self, off: u32, v: u16) {
        self.ram.write_word(off, v);
        self.consume_doorbell(off, 2);
    }
    fn read_long(&mut self, off: u32) -> u32 {
        self.ram.read_long(off)
    }
    fn write_long(&mut self, off: u32, v: u32) {
        self.ram.write_long(off, v);
        self.consume_doorbell(off, 4);
    }
}

#[cfg(test)]
mod sound_tests {
    use super::*;

    #[test]
    fn doorbell_is_consumed_immediately_but_other_bytes_persist() {
        let mut s = SoundRam::new(0x8_0000);
        s.write_byte(0x702, 0x11);
        s.write_byte(SOUND_DOORBELL, 0x85);
        assert_eq!(s.read_byte(SOUND_DOORBELL), 0);
        assert_eq!(s.read_byte(0x702), 0x11);
    }

    #[test]
    fn wider_writes_covering_the_doorbell_are_consumed_too() {
        let mut s = SoundRam::new(0x8_0000);
        s.write_long(SOUND_DOORBELL, 0x0800_1234);
        assert_eq!(s.read_byte(SOUND_DOORBELL), 0);
        assert_eq!(s.read_word(0x702), 0x1234, "parâmetros preservados");
        s.write_word(SOUND_DOORBELL, 0x8500);
        assert_eq!(s.read_byte(SOUND_DOORBELL), 0);
    }
}
