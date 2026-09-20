//! Decodificador do conjunto de instruções do SH-2: `u16` -> `Insn`.
//! Não executa nada; é reutilizado pelo emissor do JIT e pelas ferramentas de depuração.

use super::state::Sys;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sz {
    B,
    W,
    L,
}

impl Sz {
    pub fn bytes(self) -> u32 {
        match self {
            Sz::B => 1,
            Sz::W => 2,
            Sz::L => 4,
        }
    }
}

/// Modo de endereçamento efetivo de uma transferência de memória.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ea {
    /// `@Rx`
    Ind(u8),
    /// `@Rx+`
    PostInc(u8),
    /// `@-Rx`
    PreDec(u8),
    /// `@(disp,Rx)` — deslocamento já em bytes
    Disp(u8, u32),
    /// `@(R0,Rx)`
    R0Idx(u8),
    /// `@(disp,GBR)` — deslocamento já em bytes
    Gbr(u32),
    /// `@(disp,PC)` — deslocamento já em bytes (o cálculo depende do tamanho)
    Pc(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Insn {
    Load {
        sz: Sz,
        ea: Ea,
        rd: u8,
    },
    Store {
        sz: Sz,
        ea: Ea,
        rs: u8,
    },
    MovImm {
        n: u8,
        imm: i32,
    },
    Mov {
        m: u8,
        n: u8,
    },
    Mova {
        disp: u32,
    },
    Movt {
        n: u8,
    },
    SwapB {
        m: u8,
        n: u8,
    },
    SwapW {
        m: u8,
        n: u8,
    },
    Xtrct {
        m: u8,
        n: u8,
    },

    Add {
        m: u8,
        n: u8,
    },
    AddImm {
        n: u8,
        imm: i32,
    },
    Addc {
        m: u8,
        n: u8,
    },
    Addv {
        m: u8,
        n: u8,
    },
    Sub {
        m: u8,
        n: u8,
    },
    Subc {
        m: u8,
        n: u8,
    },
    Subv {
        m: u8,
        n: u8,
    },
    Neg {
        m: u8,
        n: u8,
    },
    Negc {
        m: u8,
        n: u8,
    },
    And {
        m: u8,
        n: u8,
    },
    Or {
        m: u8,
        n: u8,
    },
    Xor {
        m: u8,
        n: u8,
    },
    Not {
        m: u8,
        n: u8,
    },
    AndImm {
        imm: u32,
    },
    OrImm {
        imm: u32,
    },
    XorImm {
        imm: u32,
    },
    Tst {
        m: u8,
        n: u8,
    },
    TstImm {
        imm: u32,
    },
    CmpEq {
        m: u8,
        n: u8,
    },
    CmpHs {
        m: u8,
        n: u8,
    },
    CmpGe {
        m: u8,
        n: u8,
    },
    CmpHi {
        m: u8,
        n: u8,
    },
    CmpGt {
        m: u8,
        n: u8,
    },
    CmpStr {
        m: u8,
        n: u8,
    },
    CmpPl {
        n: u8,
    },
    CmpPz {
        n: u8,
    },
    CmpEqImm {
        imm: i32,
    },
    Div0u,
    Div0s {
        m: u8,
        n: u8,
    },
    Div1 {
        m: u8,
        n: u8,
    },
    Dt {
        n: u8,
    },
    ExtuB {
        m: u8,
        n: u8,
    },
    ExtuW {
        m: u8,
        n: u8,
    },
    ExtsB {
        m: u8,
        n: u8,
    },
    ExtsW {
        m: u8,
        n: u8,
    },

    MulL {
        m: u8,
        n: u8,
    },
    MulsW {
        m: u8,
        n: u8,
    },
    MuluW {
        m: u8,
        n: u8,
    },
    Dmuls {
        m: u8,
        n: u8,
    },
    Dmulu {
        m: u8,
        n: u8,
    },
    MacL {
        m: u8,
        n: u8,
    },
    MacW {
        m: u8,
        n: u8,
    },
    Clrmac,

    Shll {
        n: u8,
    },
    Shlr {
        n: u8,
    },
    Shar {
        n: u8,
    },
    Rotl {
        n: u8,
    },
    Rotr {
        n: u8,
    },
    Rotcl {
        n: u8,
    },
    Rotcr {
        n: u8,
    },
    Shll2 {
        n: u8,
    },
    Shlr2 {
        n: u8,
    },
    Shll8 {
        n: u8,
    },
    Shlr8 {
        n: u8,
    },
    Shll16 {
        n: u8,
    },
    Shlr16 {
        n: u8,
    },
    Clrt,
    Sett,
    Nop,

    /// `stc/sts Sys,Rn`
    StsReg {
        s: Sys,
        n: u8,
    },
    /// `ldc/lds Rm,Sys`
    LdsReg {
        s: Sys,
        m: u8,
    },
    /// `stc.l/sts.l Sys,@-Rn`
    StsMem {
        s: Sys,
        n: u8,
    },
    /// `ldc.l/lds.l @Rm+,Sys`
    LdsMem {
        s: Sys,
        m: u8,
    },

    TstB {
        imm: u32,
    },
    AndB {
        imm: u32,
    },
    OrB {
        imm: u32,
    },
    XorB {
        imm: u32,
    },
    Tas {
        n: u8,
    },

    Bra {
        disp: i32,
    },
    Bsr {
        disp: i32,
    },
    Braf {
        m: u8,
    },
    Bsrf {
        m: u8,
    },
    Jmp {
        m: u8,
    },
    Jsr {
        m: u8,
    },
    Rts,
    Rte,
    Bt {
        disp: i32,
    },
    Bf {
        disp: i32,
    },
    BtS {
        disp: i32,
    },
    BfS {
        disp: i32,
    },
    Trapa {
        imm: u32,
    },
    Sleep,
    Unknown(u16),
}

impl Insn {
    /// Instrução que encerra o fluxo sequencial (com ou sem delay slot).
    pub fn is_branch(&self) -> bool {
        matches!(
            self,
            Insn::Bra { .. }
                | Insn::Bsr { .. }
                | Insn::Braf { .. }
                | Insn::Bsrf { .. }
                | Insn::Jmp { .. }
                | Insn::Jsr { .. }
                | Insn::Rts
                | Insn::Rte
                | Insn::Bt { .. }
                | Insn::Bf { .. }
                | Insn::BtS { .. }
                | Insn::BfS { .. }
        )
    }

    pub fn has_delay_slot(&self) -> bool {
        matches!(
            self,
            Insn::Bra { .. }
                | Insn::Bsr { .. }
                | Insn::Braf { .. }
                | Insn::Bsrf { .. }
                | Insn::Jmp { .. }
                | Insn::Jsr { .. }
                | Insn::Rts
                | Insn::Rte
                | Insn::BtS { .. }
                | Insn::BfS { .. }
        )
    }

    /// Instruções após as quais o bloco deve terminar mesmo sem ser um desvio
    /// (mudam o estado de interrupção, ou saem para o despachante).
    pub fn ends_block(&self) -> bool {
        matches!(
            self,
            Insn::LdsReg { s: Sys::Sr, .. }
                | Insn::LdsMem { s: Sys::Sr, .. }
                | Insn::Trapa { .. }
                | Insn::Sleep
                | Insn::Unknown(_)
        )
    }
}

fn sext8(v: u16) -> i32 {
    (v & 0xFF) as u8 as i8 as i32
}

fn sext12(v: u16) -> i32 {
    (((v & 0xFFF) << 4) as i16 >> 4) as i32
}

pub fn decode(op: u16) -> Insn {
    let n = ((op >> 8) & 0xF) as u8;
    let m = ((op >> 4) & 0xF) as u8;
    let d4 = (op & 0xF) as u32;
    let d8 = (op & 0xFF) as u32;
    use Insn::*;
    match op >> 12 {
        0x0 => match op & 0xF {
            0x2 => match m {
                0 => StsReg { s: Sys::Sr, n },
                1 => StsReg { s: Sys::Gbr, n },
                2 => StsReg { s: Sys::Vbr, n },
                _ => Unknown(op),
            },
            0x3 => match m {
                0 => Bsrf { m: n },
                2 => Braf { m: n },
                _ => Unknown(op),
            },
            0x4 => Store {
                sz: Sz::B,
                ea: Ea::R0Idx(n),
                rs: m,
            },
            0x5 => Store {
                sz: Sz::W,
                ea: Ea::R0Idx(n),
                rs: m,
            },
            0x6 => Store {
                sz: Sz::L,
                ea: Ea::R0Idx(n),
                rs: m,
            },
            0x7 => MulL { m, n },
            0x8 => match op {
                0x0008 => Clrt,
                0x0018 => Sett,
                0x0028 => Clrmac,
                _ => Unknown(op),
            },
            0x9 => match op & 0x00FF {
                0x09 => Nop,
                0x19 => Div0u,
                0x29 => Movt { n },
                _ => Unknown(op),
            },
            0xA => match m {
                0 => StsReg { s: Sys::Mach, n },
                1 => StsReg { s: Sys::Macl, n },
                2 => StsReg { s: Sys::Pr, n },
                _ => Unknown(op),
            },
            0xB => match op {
                0x000B => Rts,
                0x001B => Sleep,
                0x002B => Rte,
                _ => Unknown(op),
            },
            0xC => Load {
                sz: Sz::B,
                ea: Ea::R0Idx(m),
                rd: n,
            },
            0xD => Load {
                sz: Sz::W,
                ea: Ea::R0Idx(m),
                rd: n,
            },
            0xE => Load {
                sz: Sz::L,
                ea: Ea::R0Idx(m),
                rd: n,
            },
            0xF => MacL { m, n },
            _ => Unknown(op),
        },
        0x1 => Store {
            sz: Sz::L,
            ea: Ea::Disp(n, d4 * 4),
            rs: m,
        },
        0x2 => match op & 0xF {
            0x0 => Store {
                sz: Sz::B,
                ea: Ea::Ind(n),
                rs: m,
            },
            0x1 => Store {
                sz: Sz::W,
                ea: Ea::Ind(n),
                rs: m,
            },
            0x2 => Store {
                sz: Sz::L,
                ea: Ea::Ind(n),
                rs: m,
            },
            0x4 => Store {
                sz: Sz::B,
                ea: Ea::PreDec(n),
                rs: m,
            },
            0x5 => Store {
                sz: Sz::W,
                ea: Ea::PreDec(n),
                rs: m,
            },
            0x6 => Store {
                sz: Sz::L,
                ea: Ea::PreDec(n),
                rs: m,
            },
            0x7 => Div0s { m, n },
            0x8 => Tst { m, n },
            0x9 => And { m, n },
            0xA => Xor { m, n },
            0xB => Or { m, n },
            0xC => CmpStr { m, n },
            0xD => Xtrct { m, n },
            0xE => MuluW { m, n },
            0xF => MulsW { m, n },
            _ => Unknown(op),
        },
        0x3 => match op & 0xF {
            0x0 => CmpEq { m, n },
            0x2 => CmpHs { m, n },
            0x3 => CmpGe { m, n },
            0x4 => Div1 { m, n },
            0x5 => Dmulu { m, n },
            0x6 => CmpHi { m, n },
            0x7 => CmpGt { m, n },
            0x8 => Sub { m, n },
            0xA => Subc { m, n },
            0xB => Subv { m, n },
            0xC => Add { m, n },
            0xD => Dmuls { m, n },
            0xE => Addc { m, n },
            0xF => Addv { m, n },
            _ => Unknown(op),
        },
        0x4 => match op & 0xFF {
            0x00 => Shll { n },
            0x01 => Shlr { n },
            0x02 => StsMem { s: Sys::Mach, n },
            0x03 => StsMem { s: Sys::Sr, n },
            0x04 => Rotl { n },
            0x05 => Rotr { n },
            0x06 => LdsMem { s: Sys::Mach, m: n },
            0x07 => LdsMem { s: Sys::Sr, m: n },
            0x08 => Shll2 { n },
            0x09 => Shlr2 { n },
            0x0A => LdsReg { s: Sys::Mach, m: n },
            0x0B => Jsr { m: n },
            0x0E => LdsReg { s: Sys::Sr, m: n },
            0x10 => Dt { n },
            0x11 => CmpPz { n },
            0x12 => StsMem { s: Sys::Macl, n },
            0x13 => StsMem { s: Sys::Gbr, n },
            0x15 => CmpPl { n },
            0x16 => LdsMem { s: Sys::Macl, m: n },
            0x17 => LdsMem { s: Sys::Gbr, m: n },
            0x18 => Shll8 { n },
            0x19 => Shlr8 { n },
            0x1A => LdsReg { s: Sys::Macl, m: n },
            0x1B => Tas { n },
            0x1E => LdsReg { s: Sys::Gbr, m: n },
            0x20 => Shll { n },
            0x21 => Shar { n },
            0x22 => StsMem { s: Sys::Pr, n },
            0x23 => StsMem { s: Sys::Vbr, n },
            0x24 => Rotcl { n },
            0x25 => Rotcr { n },
            0x26 => LdsMem { s: Sys::Pr, m: n },
            0x27 => LdsMem { s: Sys::Vbr, m: n },
            0x28 => Shll16 { n },
            0x29 => Shlr16 { n },
            0x2A => LdsReg { s: Sys::Pr, m: n },
            0x2B => Jmp { m: n },
            0x2E => LdsReg { s: Sys::Vbr, m: n },
            _ if op & 0xF == 0xF => MacW { m, n },
            _ => Unknown(op),
        },
        0x5 => Load {
            sz: Sz::L,
            ea: Ea::Disp(m, d4 * 4),
            rd: n,
        },
        0x6 => match op & 0xF {
            0x0 => Load {
                sz: Sz::B,
                ea: Ea::Ind(m),
                rd: n,
            },
            0x1 => Load {
                sz: Sz::W,
                ea: Ea::Ind(m),
                rd: n,
            },
            0x2 => Load {
                sz: Sz::L,
                ea: Ea::Ind(m),
                rd: n,
            },
            0x3 => Mov { m, n },
            0x4 => Load {
                sz: Sz::B,
                ea: Ea::PostInc(m),
                rd: n,
            },
            0x5 => Load {
                sz: Sz::W,
                ea: Ea::PostInc(m),
                rd: n,
            },
            0x6 => Load {
                sz: Sz::L,
                ea: Ea::PostInc(m),
                rd: n,
            },
            0x7 => Not { m, n },
            0x8 => SwapB { m, n },
            0x9 => SwapW { m, n },
            0xA => Negc { m, n },
            0xB => Neg { m, n },
            0xC => ExtuB { m, n },
            0xD => ExtuW { m, n },
            0xE => ExtsB { m, n },
            0xF => ExtsW { m, n },
            _ => Unknown(op),
        },
        0x7 => AddImm { n, imm: sext8(op) },
        0x8 => match (op >> 8) & 0xF {
            0x0 => Store {
                sz: Sz::B,
                ea: Ea::Disp(m, d4),
                rs: 0,
            },
            0x1 => Store {
                sz: Sz::W,
                ea: Ea::Disp(m, d4 * 2),
                rs: 0,
            },
            0x4 => Load {
                sz: Sz::B,
                ea: Ea::Disp(m, d4),
                rd: 0,
            },
            0x5 => Load {
                sz: Sz::W,
                ea: Ea::Disp(m, d4 * 2),
                rd: 0,
            },
            0x8 => CmpEqImm { imm: sext8(op) },
            0x9 => Bt { disp: sext8(op) },
            0xB => Bf { disp: sext8(op) },
            0xD => BtS { disp: sext8(op) },
            0xF => BfS { disp: sext8(op) },
            _ => Unknown(op),
        },
        0x9 => Load {
            sz: Sz::W,
            ea: Ea::Pc(d8 * 2),
            rd: n,
        },
        0xA => Bra { disp: sext12(op) },
        0xB => Bsr { disp: sext12(op) },
        0xC => match (op >> 8) & 0xF {
            0x0 => Store {
                sz: Sz::B,
                ea: Ea::Gbr(d8),
                rs: 0,
            },
            0x1 => Store {
                sz: Sz::W,
                ea: Ea::Gbr(d8 * 2),
                rs: 0,
            },
            0x2 => Store {
                sz: Sz::L,
                ea: Ea::Gbr(d8 * 4),
                rs: 0,
            },
            0x3 => Trapa { imm: d8 },
            0x4 => Load {
                sz: Sz::B,
                ea: Ea::Gbr(d8),
                rd: 0,
            },
            0x5 => Load {
                sz: Sz::W,
                ea: Ea::Gbr(d8 * 2),
                rd: 0,
            },
            0x6 => Load {
                sz: Sz::L,
                ea: Ea::Gbr(d8 * 4),
                rd: 0,
            },
            0x7 => Mova { disp: d8 * 4 },
            0x8 => TstImm { imm: d8 },
            0x9 => AndImm { imm: d8 },
            0xA => XorImm { imm: d8 },
            0xB => OrImm { imm: d8 },
            0xC => TstB { imm: d8 },
            0xD => AndB { imm: d8 },
            0xE => XorB { imm: d8 },
            0xF => OrB { imm: d8 },
            _ => Unknown(op),
        },
        0xD => Load {
            sz: Sz::L,
            ea: Ea::Pc(d8 * 4),
            rd: n,
        },
        0xE => MovImm { n, imm: sext8(op) },
        _ => Unknown(op),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_bios_hot_opcodes() {
        assert_eq!(
            decode(0x2302),
            Insn::Store {
                sz: Sz::L,
                ea: Ea::Ind(3),
                rs: 0
            }
        );
        assert_eq!(decode(0x4410), Insn::Dt { n: 4 });
        assert_eq!(decode(0x8FFC), Insn::BfS { disp: -4 });
        assert_eq!(decode(0x7310), Insn::AddImm { n: 3, imm: 0x10 });
        assert_eq!(decode(0x000B), Insn::Rts);
        assert_eq!(decode(0xB00B), Insn::Bsr { disp: 0x0B });
        assert_eq!(decode(0xA015), Insn::Bra { disp: 0x15 });
        assert_eq!(decode(0x0723), Insn::Braf { m: 7 });
        assert_eq!(
            decode(0xD304),
            Insn::Load {
                sz: Sz::L,
                ea: Ea::Pc(16),
                rd: 3
            }
        );
        assert_eq!(
            decode(0x6606),
            Insn::Load {
                sz: Sz::L,
                ea: Ea::PostInc(0),
                rd: 6
            }
        );
        assert_eq!(decode(0x4F22), Insn::StsMem { s: Sys::Pr, n: 15 });
        assert_eq!(decode(0x0029), Insn::Movt { n: 0 });
        assert_eq!(decode(0x452A), Insn::LdsReg { s: Sys::Pr, m: 5 });
        assert_eq!(decode(0x4F26), Insn::LdsMem { s: Sys::Pr, m: 15 });
    }

    #[test]
    fn negative_displacements_are_sign_extended() {
        assert_eq!(decode(0xAFFF), Insn::Bra { disp: -1 });
        assert_eq!(decode(0x8B80), Insn::Bf { disp: -128 });
    }
}
