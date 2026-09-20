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
    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
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

/// Sound RAM, shared by the SH-2 and the sound 68000.
///
/// The BIOS and the driver talk through a mailbox here: the BIOS writes the parameters and
/// then the command byte at `SOUND_DOORBELL`, and waits for the driver to zero it before
/// sending the next one. Nothing in this device answers for the driver — the 68000 does,
/// and a stub that cleared the byte instead made the BIOS believe commands had been
/// consumed that the driver never saw.
pub struct SoundRam {
    ram: Ram,
    /// True while the sound 68000 is the one touching this memory, so the mailbox log can
    /// say who wrote what.
    pub m68k_side: bool,
    /// Mailbox traffic (offsets 0x700..0x7FF): (writer, offset, width in bytes, value).
    pub mailbox_log: Vec<(bool, u32, u8, u32)>,
}

/// Offset do byte de comando da caixa de correio na RAM de som.
pub const SOUND_DOORBELL: u32 = 0x700;

impl SoundRam {
    pub fn new(size: usize) -> Self {
        Self {
            ram: Ram::new(size),
            m68k_side: false,
            mailbox_log: Vec::new(),
        }
    }
    /// Records a mailbox write. The window is small on purpose: this is the handshake
    /// between the BIOS and the sound driver, not a general memory log.
    fn log_mailbox(&mut self, off: u32, width: u8, v: u32) {
        let o = off & 0x7_FFFF;
        // Zeros are the upload clearing the area; only commands are interesting here.
        if v != 0 && (0x700..0x800).contains(&o) && self.mailbox_log.len() < 200 {
            self.mailbox_log.push((self.m68k_side, o, width, v));
        }
    }

    /// Raw contents, for dumping and for running the sound driver outside the emulator.
    pub fn data(&self) -> &[u8] {
        self.ram.data()
    }
    /// Mutable contents: the effect DSP keeps its ring buffer in sound RAM.
    pub fn data_mut(&mut self) -> &mut [u8] {
        self.ram.data_mut()
    }
}

impl MemoryDevice for SoundRam {
    fn read_byte(&mut self, off: u32) -> u8 {
        self.ram.read_byte(off)
    }
    fn write_byte(&mut self, off: u32, v: u8) {
        self.log_mailbox(off, 1, v as u32);
        self.ram.write_byte(off, v);
    }
    fn read_word(&mut self, off: u32) -> u16 {
        self.ram.read_word(off)
    }
    fn write_word(&mut self, off: u32, v: u16) {
        self.log_mailbox(off, 2, v as u32);
        self.ram.write_word(off, v);
    }
    fn read_long(&mut self, off: u32) -> u32 {
        self.ram.read_long(off)
    }
    fn write_long(&mut self, off: u32, v: u32) {
        self.log_mailbox(off, 4, v);
        self.ram.write_long(off, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mailbox between the BIOS and the sound driver is written in every width: the
    /// command byte alone, and whole longs carrying command plus parameters. All of them
    /// have to show up in the log, or the handshake looks one-sided when it is not.
    #[test]
    fn the_mailbox_log_sees_byte_word_and_long_writes() {
        let mut ram = SoundRam::new(0x8_0000);
        ram.write_byte(0x700, 0x85);
        ram.write_word(0x710, 0x8700);
        ram.write_long(0x720, 0x8300_0000);
        // Outside the mailbox window nothing is recorded.
        ram.write_long(0x1000, 0xDEAD_BEEF);
        // Zeros are the upload clearing memory, not a command.
        ram.write_byte(0x704, 0x00);

        let seen: Vec<(u32, u32)> = ram
            .mailbox_log
            .iter()
            .map(|(_, off, _, v)| (*off, *v))
            .collect();
        assert_eq!(
            seen,
            vec![(0x700, 0x85), (0x710, 0x8700), (0x720, 0x8300_0000)]
        );
    }

    /// Sound RAM must hand the command back unchanged. It used to clear the command byte
    /// itself, standing in for a driver that did not exist yet; with the 68000 running that
    /// is a bug, because the BIOS reads the clear as "consumed" for commands the driver
    /// never saw, and reissues the whole init block when the answer never comes.
    #[test]
    fn the_command_byte_survives_for_the_driver_to_read() {
        let mut ram = SoundRam::new(0x8_0000);
        ram.write_byte(SOUND_DOORBELL, 0x85);
        assert_eq!(
            ram.read_byte(SOUND_DOORBELL),
            0x85,
            "only the 68000 clears the command"
        );
    }

    /// The effect DSP does not go through the bus: it reads and writes its ring buffer
    /// straight in this storage, so `data_mut` has to be the same bytes the bus sees.
    #[test]
    fn data_and_data_mut_are_the_same_storage_the_bus_writes() {
        let mut ram = SoundRam::new(0x8_0000);
        ram.write_word(0x2000, 0xBEEF);
        assert_eq!(&ram.data()[0x2000..0x2002], &[0xBE, 0xEF]);

        ram.data_mut()[0x2000] = 0xFE;
        assert_eq!(
            ram.read_word(0x2000),
            0xFEEF,
            "o barramento vê a escrita direta"
        );
    }
}
