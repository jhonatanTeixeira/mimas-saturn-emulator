//! Estado arquitetural do SH-2. `#[repr(C)]` + `offset_of!`: o código gerado pelo JIT
//! acessa os campos por deslocamento fixo a partir de `r14`.

use std::mem::offset_of;

pub const SR_T: u32 = 1 << 0;
pub const SR_S: u32 = 1 << 1;
pub const SR_Q: u32 = 1 << 8;
pub const SR_M: u32 = 1 << 9;
/// Bits escritáveis do SR (M, Q, I3-I0, S, T).
pub const SR_MASK: u32 = 0x0000_03F3;

/// Registrador de sistema endereçável por `ldc/stc/lds/sts`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sys {
    Mach,
    Macl,
    Pr,
    Sr,
    Gbr,
    Vbr,
}

#[repr(C)]
#[derive(Clone, Debug, Default)]
pub struct Sh2State {
    pub r: [u32; 16],
    pub pc: u32,
    pub pr: u32,
    pub sr: u32,
    pub gbr: u32,
    pub vbr: u32,
    pub mach: u32,
    pub macl: u32,
    /// Ciclos acumulados pelos blocos JIT (contagem aproximada, 1 por instrução + penalidades).
    pub cycles: u64,
    /// Preenchido pelo JIT ao sair de um bloco por causa de um evento excepcional.
    pub exit_reason: u32,
    pub exit_arg: u32,
}

pub const OFF_R: i32 = offset_of!(Sh2State, r) as i32;
pub const OFF_PR: i32 = offset_of!(Sh2State, pr) as i32;
pub const OFF_SR: i32 = offset_of!(Sh2State, sr) as i32;
pub const OFF_GBR: i32 = offset_of!(Sh2State, gbr) as i32;
pub const OFF_VBR: i32 = offset_of!(Sh2State, vbr) as i32;
pub const OFF_MACH: i32 = offset_of!(Sh2State, mach) as i32;
pub const OFF_MACL: i32 = offset_of!(Sh2State, macl) as i32;
pub const OFF_CYCLES: i32 = offset_of!(Sh2State, cycles) as i32;
pub const OFF_EXIT_REASON: i32 = offset_of!(Sh2State, exit_reason) as i32;
pub const OFF_EXIT_ARG: i32 = offset_of!(Sh2State, exit_arg) as i32;

/// Motivos de saída excepcional de um bloco (`exit_reason`).
pub const EXIT_NONE: u32 = 0;
pub const EXIT_ILLEGAL: u32 = 1;
pub const EXIT_TRAPA: u32 = 2;
pub const EXIT_SLEEP: u32 = 3;

impl Sh2State {
    pub fn reg_offset(n: u8) -> i32 {
        OFF_R + (n as i32) * 4
    }

    pub fn sys_offset(s: Sys) -> i32 {
        match s {
            Sys::Mach => OFF_MACH,
            Sys::Macl => OFF_MACL,
            Sys::Pr => OFF_PR,
            Sys::Sr => OFF_SR,
            Sys::Gbr => OFF_GBR,
            Sys::Vbr => OFF_VBR,
        }
    }

    pub fn imask(&self) -> u32 {
        (self.sr >> 4) & 0xF
    }

    pub fn t(&self) -> u32 {
        self.sr & 1
    }
}
