//! Decodificação de endereços própria do SH-2 (bits A31-A29) sobre o barramento do sistema:
//!   000/001  cacheado / cache-through -> barramento externo
//!   010      purge associativo do cache (escrita ignorada)
//!   011      array de endereços do cache
//!   110      array de dados do cache (4 KiB)
//!   111      I/O; 0xFFFFFE00-0xFFFFFFFF são os registradores on-chip
//! Cache em si não é emulado: a BIOS só o configura/limpa.

use super::onchip::OnChip;
use super::sh2_bus::Sh2Bus;
use crate::bus::{MemoryDevice, SystemBus};

pub struct Sh2AddressSpace {
    pub sys: SystemBus,
    pub onchip: OnChip,
    tag_array: Vec<u8>,
    data_array: Vec<u8>,
}

impl Sh2AddressSpace {
    pub fn new(sys: SystemBus) -> Self {
        Self {
            sys,
            onchip: OnChip::new(),
            tag_array: vec![0; 0x400],
            data_array: vec![0; 0x1000],
        }
    }
}

fn be(mem: &[u8], off: usize, size: usize) -> u32 {
    let mut v = 0;
    for i in 0..size {
        v = (v << 8) | mem[(off + i) % mem.len()] as u32;
    }
    v
}

fn put_be(mem: &mut [u8], off: usize, size: usize, val: u32) {
    let n = mem.len();
    for i in 0..size {
        mem[(off + i) % n] = (val >> (8 * (size - 1 - i))) as u8;
    }
}

macro_rules! read_fn {
    ($name:ident, $sys:ident, $dev:ident, $ty:ty, $size:expr) => {
        fn $name(&mut self, addr: u32) -> $ty {
            match addr >> 29 {
                0 | 1 => self.sys.$sys(addr),
                3 => be(&self.tag_array, (addr & 0x3FF) as usize, $size) as $ty,
                6 => be(&self.data_array, (addr & 0xFFF) as usize, $size) as $ty,
                7 if addr >= 0xFFFF_FE00 => self.onchip.$dev(addr & 0x1FF),
                7 => self.sys.$sys(addr),
                _ => 0,
            }
        }
    };
}

macro_rules! write_fn {
    ($name:ident, $sys:ident, $dev:ident, $ty:ty, $size:expr) => {
        fn $name(&mut self, addr: u32, v: $ty) {
            match addr >> 29 {
                0 | 1 => self.sys.$sys(addr, v),
                3 => put_be(
                    &mut self.tag_array,
                    (addr & 0x3FF) as usize,
                    $size,
                    v as u32,
                ),
                6 => put_be(
                    &mut self.data_array,
                    (addr & 0xFFF) as usize,
                    $size,
                    v as u32,
                ),
                7 if addr >= 0xFFFF_FE00 => self.onchip.$dev(addr & 0x1FF, v),
                7 => self.sys.$sys(addr, v),
                _ => {}
            }
        }
    };
}

impl Sh2Bus for Sh2AddressSpace {
    read_fn!(read8, read8, read_byte, u8, 1);
    read_fn!(read16, read16, read_word, u16, 2);
    read_fn!(read32, read32, read_long, u32, 4);
    write_fn!(write8, write8, write_byte, u8, 1);
    write_fn!(write16, write16, write_word, u16, 2);
    write_fn!(write32, write32, write_long, u32, 4);

    fn canonical(&self, addr: u32) -> u32 {
        self.sys.canonical(addr)
    }
    fn mark_code(&mut self, start: u32, len: u32) {
        self.sys.mark_code(start, len)
    }
    fn drain_dirty_code(&mut self, out: &mut Vec<(u32, u32)>) {
        self.sys.drain_dirty_code(out)
    }
}
