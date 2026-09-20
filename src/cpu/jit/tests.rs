//! Testes do JIT: pequenos programas SH-2 escritos em opcodes crus, executados por blocos
//! compilados, com o resultado conferido contra a semântica do manual (ou contra a
//! aritmética nativa do host).

use super::Sh2Jit;
use crate::cpu::sh2_bus::Sh2Bus;
use crate::cpu::state::*;

const CODE: u32 = 0x1000;
const HALT: u32 = 0x2000;

struct TestBus {
    mem: Vec<u8>,
}

impl TestBus {
    fn new() -> Self {
        Self {
            mem: vec![0; 0x20000],
        }
    }
    fn a(&self, addr: u32) -> usize {
        (addr as usize) % self.mem.len()
    }
}

impl Sh2Bus for TestBus {
    fn read8(&mut self, addr: u32) -> u8 {
        self.mem[self.a(addr)]
    }
    fn read16(&mut self, addr: u32) -> u16 {
        let a = self.a(addr);
        u16::from_be_bytes([self.mem[a], self.mem[a + 1]])
    }
    fn read32(&mut self, addr: u32) -> u32 {
        let a = self.a(addr);
        u32::from_be_bytes([
            self.mem[a],
            self.mem[a + 1],
            self.mem[a + 2],
            self.mem[a + 3],
        ])
    }
    fn write8(&mut self, addr: u32, v: u8) {
        let a = self.a(addr);
        self.mem[a] = v;
    }
    fn write16(&mut self, addr: u32, v: u16) {
        let a = self.a(addr);
        self.mem[a..a + 2].copy_from_slice(&v.to_be_bytes());
    }
    fn write32(&mut self, addr: u32, v: u32) {
        let a = self.a(addr);
        self.mem[a..a + 4].copy_from_slice(&v.to_be_bytes());
    }
}

/// `Rn,Rm` em formato 0xXnmY.
fn rr(base: u16, m: u16, n: u16) -> u16 {
    base | (n << 8) | (m << 4)
}

const RTS: u16 = 0x000B;
const NOP: u16 = 0x0009;

fn run(code: &[u16], init: impl FnOnce(&mut Sh2State, &mut TestBus)) -> (Sh2State, TestBus) {
    let mut bus = TestBus::new();
    let mut st = Sh2State::default();
    st.pc = CODE;
    st.pr = HALT;
    st.r[15] = 0x10000;
    st.sr = 0xF0;
    for (i, w) in code.iter().enumerate() {
        bus.write16(CODE + 2 * i as u32, *w);
    }
    let end = CODE + 2 * code.len() as u32;
    bus.write16(end, RTS);
    bus.write16(end + 2, NOP);
    init(&mut st, &mut bus);
    let mut jit = Sh2Jit::new();
    let mut guard = 0;
    while st.pc != HALT {
        jit.run_block(&mut st, &mut bus, None);
        assert_eq!(
            st.exit_reason, 0,
            "saída excepcional em pc={:08X}",
            st.exit_arg
        );
        guard += 1;
        assert!(guard < 5000, "programa não terminou (pc={:08X})", st.pc);
    }
    (st, bus)
}

fn run_regs(code: &[u16], regs: &[(usize, u32)]) -> Sh2State {
    run(code, |st, _| {
        for &(r, v) in regs {
            st.r[r] = v;
        }
    })
    .0
}

#[test]
fn arithmetic_basics() {
    let st = run_regs(
        &[
            0xE005,           // mov #5,r0
            0xE103,           // mov #3,r1
            rr(0x300C, 1, 0), // add r1,r0  -> 8
            rr(0x600B, 0, 2), // neg r0,r2  -> -8
            rr(0x3008, 1, 0), // sub r1,r0  -> 5
            0x7A05,           // add #5,r10
            0x7BFE,           // add #-2,r11
        ],
        &[(10, 1), (11, 10)],
    );
    assert_eq!(st.r[0], 5);
    assert_eq!(st.r[2], (-8i32) as u32);
    assert_eq!(st.r[10], 6);
    assert_eq!(st.r[11], 8);
}

#[test]
fn addc_builds_a_64_bit_add() {
    // (r0:r1) + (r2:r3), baixos em r1/r3
    let st = run_regs(
        &[0x0008, rr(0x300E, 3, 1), rr(0x300E, 2, 0)],
        &[(0, 0), (1, 0xFFFF_FFFF), (2, 0), (3, 1)],
    );
    assert_eq!(st.r[1], 0);
    assert_eq!(st.r[0], 1);
}

#[test]
fn subc_and_negc_borrow() {
    let st = run_regs(&[0x0008, rr(0x300A, 1, 0), 0x0229], &[(0, 0), (1, 1)]);
    assert_eq!(st.r[0], 0xFFFF_FFFF);
    assert_eq!(st.r[2], 1, "T = borrow");
    let st = run_regs(&[0x0018, rr(0x600A, 0, 1), 0x0329], &[(0, 0)]);
    assert_eq!(st.r[1], 0xFFFF_FFFF);
    assert_eq!(st.r[3], 1);
    let st = run_regs(&[0x0008, rr(0x600A, 0, 1), 0x0329], &[(0, 5)]);
    assert_eq!(st.r[1], (-5i32) as u32);
    assert_eq!(st.r[3], 1);
}

#[test]
fn overflow_flags() {
    let st = run_regs(&[rr(0x300F, 1, 0), 0x0229], &[(0, 0x7FFF_FFFF), (1, 1)]);
    assert_eq!((st.r[0], st.r[2]), (0x8000_0000, 1));
    let st = run_regs(&[rr(0x300B, 1, 0), 0x0229], &[(0, 0x8000_0000), (1, 1)]);
    assert_eq!((st.r[0], st.r[2]), (0x7FFF_FFFF, 1));
    let st = run_regs(&[rr(0x300F, 1, 0), 0x0229], &[(0, 1), (1, 1)]);
    assert_eq!((st.r[0], st.r[2]), (2, 0));
}

#[test]
fn comparisons() {
    let code = [
        rr(0x3000, 1, 0),
        0x0229, // cmp/eq
        rr(0x3002, 1, 0),
        0x0329, // cmp/hs
        rr(0x3003, 1, 0),
        0x0429, // cmp/ge
        rr(0x3006, 1, 0),
        0x0529, // cmp/hi
        rr(0x3007, 1, 0),
        0x0629, // cmp/gt
        0x4015,
        0x0729, // cmp/pl r0
        0x4111,
        0x0829, // cmp/pz r1
        0x8805,
        0x0929, // cmp/eq #5,r0
    ];
    let st = run_regs(&code, &[(0, 5), (1, (-5i32) as u32)]);
    assert_eq!(&st.r[2..10], &[0, 0, 1, 0, 1, 1, 0, 1]);
}

#[test]
fn cmp_str_finds_equal_byte() {
    let st = run_regs(
        &[rr(0x200C, 1, 0), 0x0229],
        &[(0, 0x1122_3344), (1, 0x9922_3377)],
    );
    assert_eq!(st.r[2], 1);
    let st = run_regs(
        &[rr(0x200C, 1, 0), 0x0229],
        &[(0, 0x1122_3344), (1, 0x9988_7766)],
    );
    assert_eq!(st.r[2], 0);
}

#[test]
fn tst_sets_t_when_and_is_zero() {
    let st = run_regs(
        &[rr(0x2008, 1, 0), 0x0229, 0xC801, 0x0329],
        &[(0, 0xF0), (1, 0x0F)],
    );
    assert_eq!((st.r[2], st.r[3]), (1, 1));
}

#[test]
fn shifts_and_rotates_set_t_from_the_bit_shifted_out() {
    let cases: &[(u16, u32, bool, u32, u32)] = &[
        // opcode(n=r0), r0 in, T in, r0 out, T out
        (0x4000, 0x8000_0001, false, 0x0000_0002, 1), // shll
        (0x4001, 0x0000_0003, false, 0x0000_0001, 1), // shlr
        (0x4021, 0x8000_0001, false, 0xC000_0000, 1), // shar
        (0x4004, 0x8000_0000, false, 0x0000_0001, 1), // rotl
        (0x4005, 0x0000_0001, false, 0x8000_0000, 1), // rotr
        (0x4024, 0x8000_0000, true, 0x0000_0001, 1),  // rotcl
        (0x4024, 0x4000_0000, true, 0x8000_0001, 0),
        (0x4025, 0x0000_0001, true, 0x8000_0000, 1), // rotcr
        (0x4025, 0x0000_0002, false, 0x0000_0001, 0),
    ];
    for &(op, r0, t, out, tout) in cases {
        let pre = if t { 0x0018 } else { 0x0008 };
        let st = run_regs(&[pre, op, 0x0129], &[(0, r0)]);
        assert_eq!(
            (st.r[0], st.r[1]),
            (out, tout),
            "op {:04X} r0={:08X} t={}",
            op,
            r0,
            t
        );
    }
    let st = run_regs(
        &[0x4008, 0x4109, 0x4218, 0x4319, 0x4428, 0x4529],
        &[
            (0, 0x11),
            (1, 0x100),
            (2, 0x1),
            (3, 0x1200),
            (4, 0x1),
            (5, 0x12340000),
        ],
    );
    assert_eq!(st.r[0], 0x44);
    assert_eq!(st.r[1], 0x40);
    assert_eq!(st.r[2], 0x100);
    assert_eq!(st.r[3], 0x12);
    assert_eq!(st.r[4], 0x10000);
    assert_eq!(st.r[5], 0x1234);
}

#[test]
fn dt_decrements_and_flags_zero() {
    let st = run_regs(&[0x4010, 0x0129], &[(0, 1)]);
    assert_eq!((st.r[0], st.r[1]), (0, 1));
    let st = run_regs(&[0x4010, 0x0129], &[(0, 5)]);
    assert_eq!((st.r[0], st.r[1]), (4, 0));
}

/// Divisão sem sinal 32/16 do manual do SH-2 (16x DIV1 + ROTCL) contra a divisão nativa.
#[test]
fn div1_unsigned_32_by_16_matches_native_division() {
    let mut code = vec![0x4028, 0x0019]; // shll16 r0 ; div0u
    for _ in 0..16 {
        code.push(rr(0x3004, 0, 1)); // div1 r0,r1
    }
    code.push(0x4124); // rotcl r1
    code.push(rr(0x600D, 1, 1)); // extu.w r1,r1
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for _ in 0..300 {
        let divisor = (next() % 0xFFFF + 1) as u32;
        let q = (next() % 0x1_0000) as u32;
        let r = (next() % divisor as u64) as u32;
        let dividend = q * divisor + r;
        let st = run_regs(&code, &[(0, divisor), (1, dividend)]);
        assert_eq!(st.r[1], q, "{dividend} / {divisor}");
    }
}

#[test]
fn div0s_sets_q_m_and_t() {
    let st = run_regs(&[rr(0x2007, 1, 0)], &[(0, 0x8000_0000), (1, 1)]);
    assert_eq!(st.sr & (SR_Q | SR_M | SR_T), SR_Q | SR_T);
    let st = run_regs(&[rr(0x2007, 1, 0)], &[(0, 0x8000_0000), (1, 0x8000_0000)]);
    assert_eq!(st.sr & (SR_Q | SR_M | SR_T), SR_Q | SR_M);
}

#[test]
fn multiplies() {
    let st = run_regs(&[rr(0x0007, 1, 0), 0x021A], &[(0, 100_000), (1, 30_000)]);
    assert_eq!(st.r[2], 0xB2D0_5E00);
    let st = run_regs(&[rr(0x200F, 1, 0), 0x021A], &[(0, 0xFFFF), (1, 3)]);
    assert_eq!(st.r[2], (-3i32) as u32, "muls.w");
    let st = run_regs(&[rr(0x200E, 1, 0), 0x021A], &[(0, 0xFFFF), (1, 3)]);
    assert_eq!(st.r[2], 0x2_FFFD, "mulu.w");
    let st = run_regs(
        &[rr(0x300D, 1, 0)],
        &[(0, (-3i32) as u32), (1, 0x7FFF_FFFF)],
    );
    assert_eq!((st.mach, st.macl), (0xFFFF_FFFE, 0x8000_0003), "dmuls.l");
    let st = run_regs(&[rr(0x3005, 1, 0)], &[(0, 0xFFFF_FFFF), (1, 0xFFFF_FFFF)]);
    assert_eq!((st.mach, st.macl), (0xFFFF_FFFE, 1), "dmulu.l");
}

#[test]
fn mac_l_accumulates_and_post_increments() {
    let (st, _) = run(&[0x0028, rr(0x000F, 1, 2), rr(0x000F, 1, 2)], |st, bus| {
        st.r[1] = 0x8000;
        st.r[2] = 0x8010;
        bus.write32(0x8000, 3);
        bus.write32(0x8004, 4);
        bus.write32(0x8010, 5);
        bus.write32(0x8014, (-6i32) as u32);
    });
    assert_eq!(
        ((st.mach as u64) << 32) | st.macl as u64,
        (3 * 5 + 4 * -6i64) as u64
    );
    assert_eq!((st.r[1], st.r[2]), (0x8008, 0x8018));
}

#[test]
fn mac_l_saturates_to_48_bits_only_when_s_is_set() {
    let prog = [rr(0x000F, 1, 2)];
    let setup = |s: u32| {
        move |st: &mut Sh2State, bus: &mut TestBus| {
            st.r[1] = 0x8000;
            st.r[2] = 0x8010;
            st.sr |= s;
            st.mach = 0x0000_7FFF;
            st.macl = 0xFFFF_FFF0;
            bus.write32(0x8000, 0x1000);
            bus.write32(0x8010, 0x1000);
        }
    };
    let (st, _) = run(&prog, setup(SR_S));
    assert_eq!((st.mach, st.macl), (0x0000_7FFF, 0xFFFF_FFFF));
    let (st, _) = run(&prog, setup(0));
    let full = (0x0000_7FFF_FFFF_FFF0u64).wrapping_add(0x100_0000);
    assert_eq!(((st.mach as u64) << 32) | st.macl as u64, full);
}

#[test]
fn mac_w_accumulates_64_bits_when_s_clear() {
    let (st, _) = run(&[rr(0x400F, 1, 2)], |st, bus| {
        st.r[1] = 0x8000;
        st.r[2] = 0x8010;
        bus.write16(0x8000, 0xFFFE); // -2
        bus.write16(0x8010, 7);
    });
    assert_eq!(((st.mach as u64) << 32) | st.macl as u64, (-14i64) as u64);
    assert_eq!((st.r[1], st.r[2]), (0x8002, 0x8012));
}

#[test]
fn bra_executes_delay_slot_and_skips_the_fall_through() {
    let st = run_regs(&[0xE001, 0xA001, 0x7001, 0x700A, 0x7064], &[]);
    assert_eq!(st.r[0], 1 + 1 + 100);
}

#[test]
fn conditional_delayed_branches_always_run_the_slot() {
    // bt/s com T=0: nada desviado, slot e a instrução seguinte executam.
    let st = run_regs(&[0x0008, 0x8D01, 0x7001, 0x7002, 0x7004], &[]);
    assert_eq!(st.r[0], 1 + 2 + 4);
    // bt/s com T=1: slot executa, pula a seguinte.
    let st = run_regs(&[0x0018, 0x8D01, 0x7001, 0x7002, 0x7004], &[]);
    assert_eq!(st.r[0], 1 + 4);
    // bf/s com T=0: desvia.
    let st = run_regs(&[0x0008, 0x8F01, 0x7001, 0x7002, 0x7004], &[]);
    assert_eq!(st.r[0], 1 + 4);
    // bt sem delay slot com T=1 pula a instrução seguinte.
    let st = run_regs(&[0x0018, 0x8900, 0x7001, 0x7004], &[]);
    assert_eq!(st.r[0], 4);
}

#[test]
fn counted_loop_with_dt_and_bf_s() {
    // r1 = 5 iterações somando 3 em r0; o slot de bf/s (add) roda a cada iteração.
    let st = run_regs(&[0x4110, 0x8FFD, 0x7003], &[(1, 5)]);
    assert_eq!(st.r[0], 15);
    assert_eq!(st.r[1], 0);
}

#[test]
fn jsr_sets_pr_and_rts_returns() {
    let (st, _) = run(&[0x410B, 0x7001, 0x062A, 0x452A], |st, bus| {
        st.r[1] = 0x1100;
        st.r[5] = HALT;
        bus.write16(0x1100, 0x7010); // add #16,r0
        bus.write16(0x1102, RTS);
        bus.write16(0x1104, NOP);
    });
    assert_eq!(st.r[0], 17);
    assert_eq!(st.r[6], CODE + 4, "PR = endereço do jsr + 4");
}

#[test]
fn braf_and_bsrf_are_pc_relative() {
    // braf r1 em 0x1000: alvo = 0x1000 + 4 + r1.
    let st = run_regs(&[0x0123, 0x7001, 0x7010, 0x7064], &[(1, 2)]);
    assert_eq!(st.r[0], 1 + 100);
}

#[test]
fn post_increment_and_pre_decrement_edge_cases() {
    let (st, _) = run(&[0x6216], |st, bus| {
        st.r[1] = 0x8000;
        bus.write32(0x8000, 0xDEAD_BEEF);
    });
    assert_eq!((st.r[2], st.r[1]), (0xDEAD_BEEF, 0x8004));
    // Rm == Rn: o dado carregado vence o incremento.
    let (st, _) = run(&[0x6116], |st, bus| {
        st.r[1] = 0x8000;
        bus.write32(0x8000, 0x1234_5678);
    });
    assert_eq!(st.r[1], 0x1234_5678);
    let (st, bus) = run(&[0x2126], |st, _| {
        st.r[1] = 0x8010;
        st.r[2] = 0x1234_5678;
    });
    let mut bus = bus;
    assert_eq!((st.r[1], bus.read32(0x800C)), (0x800C, 0x1234_5678));
    // Rm == Rn: grava o valor anterior ao decremento.
    let (st, bus) = run(&[0x2116], |st, _| st.r[1] = 0x8010);
    let mut bus = bus;
    assert_eq!((st.r[1], bus.read32(0x800C)), (0x800C, 0x8010));
}

#[test]
fn loads_sign_extend_byte_and_word() {
    let (st, _) = run(&[0x6210, 0x6311], |st, bus| {
        st.r[1] = 0x8000;
        bus.write16(0x8000, 0x8001);
    });
    assert_eq!(st.r[2], 0xFFFF_FF80, "mov.b sinal-estendido");
    assert_eq!(st.r[3], 0xFFFF_8001, "mov.w sinal-estendido");
}

#[test]
fn indexed_and_displacement_addressing() {
    let (st, mut bus) = run(
        &[
            0x0126, // mov.l r2,@(r0,r1)
            0x031E, // mov.l @(r0,r1),r3
            0x1121, // mov.l r2,@(4,r1)
            0x5411, // mov.l @(4,r1),r4
            0x8013, // mov.b r0,@(3,r1)
            0x8112, // mov.w r0,@(2,r1)
        ],
        |st, _| {
            st.r[0] = 8;
            st.r[1] = 0x8000;
            st.r[2] = 0xCAFE_F00D;
        },
    );
    assert_eq!(bus.read32(0x8008), 0xCAFE_F00D);
    assert_eq!(st.r[3], 0xCAFE_F00D);
    assert_eq!(st.r[4], 0xCAFE_F00D);
    assert_eq!(bus.read8(0x8003), 0x08);
    assert_eq!(bus.read16(0x8004), 0x0008);
}

#[test]
fn gbr_relative_addressing() {
    let (st, mut bus) = run(&[0xC202, 0xC602, 0xC401], |st, bus| {
        st.gbr = 0x8000;
        st.r[0] = 0x1122_3344;
        bus.write8(0x8001, 0x90);
    });
    assert_eq!(bus.read32(0x8008), 0x1122_3344);
    // mov.l @(2,gbr),r0 recarrega o mesmo valor; mov.b @(1,gbr),r0 sinal-estende 0x90
    assert_eq!(st.r[0], 0xFFFF_FF90);
}

#[test]
fn pc_relative_loads_and_mova() {
    // Dados em 0x1010, longe do `rts; nop` que o harness anexa ao fim do código.
    let (st, _) = run(&[0x9006, 0xD103, 0xC701], |_, bus| {
        bus.write32(0x1010, 0xFF80_1234);
    });
    assert_eq!(
        st.r[0], 0x100C,
        "mova @(1,pc),r0 em 0x1004 = (0x1004&~3)+4+4"
    );
    assert_eq!(
        st.r[1], 0xFF80_1234,
        "mov.l @(3,pc),r1 em 0x1002 = (0x1002&~3)+4+12"
    );
}

#[test]
fn pc_relative_word_load_sign_extends() {
    let (st, _) = run(&[0x9002], |_, bus| {
        bus.write16(0x1008, 0xFF80);
    });
    assert_eq!(st.r[0], 0xFFFF_FF80);
}

#[test]
fn swap_word_xtrct_and_extensions() {
    let st = run_regs(
        &[
            rr(0x6009, 0, 2),
            rr(0x200D, 4, 3),
            rr(0x600C, 5, 6),
            rr(0x600D, 5, 7),
            rr(0x600E, 5, 8),
            rr(0x600F, 5, 9),
        ],
        &[
            (0, 0x1122_3344),
            (3, 0xAAAA_BBBB),
            (4, 0xCCCC_DDDD),
            (5, 0x0000_80F0),
        ],
    );
    assert_eq!(st.r[2], 0x3344_1122, "swap.w");
    assert_eq!(st.r[3], 0xDDDD_AAAA, "xtrct");
    assert_eq!(st.r[6], 0xF0, "extu.b");
    assert_eq!(st.r[7], 0x80F0, "extu.w");
    assert_eq!(st.r[8], 0xFFFF_FFF0, "exts.b");
    assert_eq!(st.r[9], 0xFFFF_80F0, "exts.w");
}

#[test]
fn swap_byte_swaps_the_two_low_bytes() {
    let st = run_regs(&[rr(0x6008, 0, 1)], &[(0, 0x1122_3344)]);
    assert_eq!(st.r[1], 0x1122_4433);
}

#[test]
fn tas_and_gbr_byte_operations() {
    let (st, mut bus) = run(&[0x411B, 0x0229], |st, _| st.r[1] = 0x8000);
    assert_eq!((st.r[2], bus.read8(0x8000)), (1, 0x80));
    let (st, mut bus) = run(&[0x411B, 0x0229], |st, bus| {
        st.r[1] = 0x8000;
        bus.write8(0x8000, 5);
    });
    assert_eq!((st.r[2], bus.read8(0x8000)), (0, 0x85));
    let (st, mut bus) = run(&[0xCD0F, 0xCF80, 0xCEFF, 0xCC01, 0x0329], |st, bus| {
        st.r[0] = 4;
        st.gbr = 0x8000;
        bus.write8(0x8004, 0xAB);
    });
    // 0xAB & 0x0F = 0x0B ; | 0x80 = 0x8B ; ^ 0xFF = 0x74 ; tst.b #1 -> (0x74 & 1) == 0 -> T = 1
    assert_eq!(bus.read8(0x8004), 0x74);
    assert_eq!(st.r[3], 1);
}

#[test]
fn rte_restores_pc_and_sr_and_runs_the_slot_after_popping() {
    let (st, _) = run(&[0x002B, 0x60F6], |st, bus| {
        st.r[15] = 0x9000;
        bus.write32(0x9000, HALT);
        bus.write32(0x9004, 0xFFFF_FCF3);
        bus.write32(0x9008, 0xCAFE_BABE);
    });
    assert_eq!(st.sr, 0x0000_00F3, "SR restaurado e mascarado com 0x3F3");
    assert_eq!(st.r[0], 0xCAFE_BABE);
    assert_eq!(st.r[15], 0x900C);
}

#[test]
fn ldc_sr_masks_reserved_bits_and_stc_reads_back() {
    let st = run_regs(&[0x400E, 0x0102], &[(0, 0xFFFF_FFFF)]);
    assert_eq!(st.sr, SR_MASK);
    assert_eq!(st.r[1], SR_MASK);
}

#[test]
fn system_register_transfers_and_stack_forms() {
    // sts.l pr,@-r15 ; lds.l @r15+,pr ; sts pr,r6 ; sts.l mach,@-r15 ; lds r15,mach ; lds r5,pr
    let (st, mut bus) = run(
        &[0x4F22, 0x4F26, 0x062A, 0x4F02, 0x4F0A, 0x452A],
        |st, _| {
            st.pr = 0x1357_9BDF;
            st.r[15] = 0x9000;
            st.r[5] = HALT;
            st.mach = 0x55;
        },
    );
    assert_eq!(st.r[6], 0x1357_9BDF, "PR passou pela pilha sem alteração");
    assert_eq!(bus.read32(0x8FFC), 0x55, "sts.l mach,@-r15");
    assert_eq!(st.mach, 0x8FFC, "lds r15,mach copia o valor atual de r15");
}

#[test]
fn illegal_opcode_reports_instead_of_running_garbage() {
    let mut bus = TestBus::new();
    bus.write16(CODE, 0xFFFF);
    let mut st = Sh2State::default();
    st.pc = CODE;
    let mut jit = Sh2Jit::new();
    jit.run_block(&mut st, &mut bus, None);
    assert_eq!(st.exit_reason, EXIT_ILLEGAL);
    assert_eq!(st.exit_arg, CODE);
}

#[test]
fn cycles_are_accumulated() {
    let st = run_regs(&[0x7001, 0x7001, 0x7001], &[]);
    assert!(st.cycles >= 3, "ciclos = {}", st.cycles);
}
