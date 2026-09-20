//! Resumo do estado dos VDPs para diagnóstico: registradores relevantes e a lista de comandos do VDP1.

use crate::devices::vdp1::Vdp1;
use crate::devices::vdp2::Vdp2;

const VDP2_NAMES: &[(usize, &str)] = &[
    (0x00, "TVMD"),
    (0x02, "EXTEN"),
    (0x0E, "RAMCTL"),
    (0x12, "CYCA0U"),
    (0x10, "CYCA0L"),
    (0x14, "CYCA1L"),
    (0x16, "CYCA1U"),
    (0x18, "CYCB0L"),
    (0x1A, "CYCB0U"),
    (0x1C, "CYCB1L"),
    (0x1E, "CYCB1U"),
    (0x20, "BGON"),
    (0x22, "MZCTL"),
    (0x24, "SFSEL"),
    (0x26, "SFCODE"),
    (0x28, "CHCTLA"),
    (0x2A, "CHCTLB"),
    (0x2C, "BMPNA"),
    (0x2E, "BMPNB"),
    (0x30, "PNCN0"),
    (0x32, "PNCN1"),
    (0x34, "PNCN2"),
    (0x36, "PNCN3"),
    (0x38, "PNCR"),
    (0x3A, "PLSZ"),
    (0x3C, "MPOFN"),
    (0x3E, "MPOFR"),
    (0x40, "MPABN0"),
    (0x42, "MPCDN0"),
    (0x44, "MPABN1"),
    (0x46, "MPCDN1"),
    (0x48, "MPABN2"),
    (0x4A, "MPCDN2"),
    (0x4C, "MPABN3"),
    (0x4E, "MPCDN3"),
    (0x70, "SCXIN0"),
    (0x72, "SCXDN0"),
    (0x74, "SCYIN0"),
    (0x76, "SCYDN0"),
    (0x78, "ZMXIN0"),
    (0x7A, "ZMXDN0"),
    (0x7C, "ZMYIN0"),
    (0x7E, "ZMYDN0"),
    (0x80, "SCXIN1"),
    (0x82, "SCXDN1"),
    (0x84, "SCYIN1"),
    (0x86, "SCYDN1"),
    (0x88, "ZMXIN1"),
    (0x8A, "ZMXDN1"),
    (0x8C, "ZMYIN1"),
    (0x8E, "ZMYDN1"),
    (0x90, "SCXN2"),
    (0x92, "SCYN2"),
    (0x94, "SCXN3"),
    (0x96, "SCYN3"),
    (0x98, "ZMCTL"),
    (0x9A, "SCRCTL"),
    (0xE0, "SPCTL"),
    (0xE2, "SDCTL"),
    (0xE4, "CRAOFA"),
    (0xE6, "CRAOFB"),
    (0xE8, "LNCLEN"),
    (0xEA, "SFPRMD"),
    (0xEC, "CCCTL"),
    (0xEE, "SFCCMD"),
    (0xF0, "PRISA"),
    (0xF2, "PRISB"),
    (0xF4, "PRISC"),
    (0xF6, "PRISD"),
    (0xF8, "PRINA"),
    (0xFA, "PRINB"),
    (0xFC, "PRIR"),
    (0x100, "CCRSA"),
    (0x102, "CCRSB"),
    (0x108, "CCRNA"),
    (0x10A, "CCRNB"),
    (0x10C, "CCRR"),
    (0x10E, "CCRLB"),
    (0x110, "CLOFEN"),
    (0x112, "CLOFSL"),
    (0x114, "COAR"),
    (0x116, "COAG"),
    (0x118, "COAB"),
    (0x11A, "COBR"),
    (0x11C, "COBG"),
    (0x11E, "COBB"),
];

pub fn print_vdp2(v: &Vdp2) {
    println!("--- VDP2 registradores não nulos (quadro {}) ---", v.frame);
    let mut line = String::new();
    for (off, name) in VDP2_NAMES {
        let val = v.reg16(*off);
        if val != 0 {
            line.push_str(&format!("{name}={val:04X} "));
        }
    }
    println!("  {line}");
    let nz = v.vram.iter().filter(|&&b| b != 0).count();
    let cz = v.cram.iter().filter(|&&b| b != 0).count();
    println!("  VRAM não-zero: {nz} bytes; CRAM não-zero: {cz} bytes");
}

pub fn print_vdp1(v: &Vdp1) {
    println!(
        "--- VDP1: TVMR={:04X} FBCR={:04X} PTMR={:04X} EWDR={:04X} EWLR={:04X} EWRR={:04X} draw_fb={} ---",
        v.tvmr, v.fbcr, v.ptmr, v.ewdr, v.ewlr, v.ewrr, v.draw
    );
    let mut n = 0;
    let mut addr = 0usize;
    let mut kinds = std::collections::BTreeMap::new();
    let mut pmods = std::collections::BTreeMap::new();
    while addr + 32 <= v.vram.len() && n < 4096 {
        let w = |o: usize| u16::from_be_bytes([v.vram[addr + o], v.vram[addr + o + 1]]);
        let ctrl = w(0);
        if ctrl & 0x8000 != 0 {
            break;
        }
        *kinds.entry(ctrl & 0xF).or_insert(0u32) += 1;
        *pmods.entry((ctrl & 0xF, w(4))).or_insert(0u32) += 1;
        if n < 6 {
            println!(
                "  cmd@{addr:05X}: CTRL={:04X} LINK={:04X} PMOD={:04X} COLR={:04X} SRCA={:04X} SIZE={:04X} XA={} YA={} XB={} YB={} XC={} YC={} XD={} YD={} GRDA={:04X}",
                ctrl,
                w(2),
                w(4),
                w(6),
                w(8),
                w(0xA),
                w(0xC) as i16,
                w(0xE) as i16,
                w(0x10) as i16,
                w(0x12) as i16,
                w(0x14) as i16,
                w(0x16) as i16,
                w(0x18) as i16,
                w(0x1A) as i16,
                w(0x1C)
            );
        }
        let link = w(2) as usize;
        addr = if link == 0 { addr + 32 } else { link * 8 };
        n += 1;
    }
    println!("  {n} comandos até o fim; tipos (CTRL&0xF): {kinds:?}");
    println!(
        "  (tipo, PMOD) -> contagem: {}",
        pmods
            .iter()
            .map(|((k, p), c)| format!("({k},{p:04X})={c}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
}
