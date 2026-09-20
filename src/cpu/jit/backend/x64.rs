//! The x86-64 backend: compiles a block of SH-2 to native code with `dynasm`. No
//! interpreter exists — every instruction is translated to native code. Only memory/MMIO
//! access goes through runtime helpers (`rt_*`), which belong to the bus, not the CPU.
//!
//! This is the only backend today (see `backend/mod.rs` for what a second one — ARM64 for
//! the weak-hardware targets this project cares about — would need to implement, and the
//! one piece of this file's control flow that is not simply "x86-64 syntax" and needs
//! reading first: the delay-slot ordering inside `emit_branch`).
//!
//! Convention inside a block (System V):
//!   r14 = *mut Sh2State     r15 = *mut Sh2Runtime
//!   rbx, r12, r13           guardam valores através de chamadas ao runtime
//!   eax, ecx, edx, esi, r8-r11  temporários (destruídos por chamadas)
//! O bloco devolve em `eax` o próximo PC. O prólogo empurra 5 registradores, o que deixa
//! a pilha alinhada em 16 bytes para as chamadas.

use dynasmrt::x64::Assembler;
use dynasmrt::{AssemblyOffset, DynamicLabel, DynasmApi, DynasmLabelApi, ExecutableBuffer, dynasm};

use crate::cpu::decode::{Ea, Insn, Sz, decode};
use crate::cpu::sh2_bus::*;
use crate::cpu::state::*;

pub const MAX_BLOCK_INSNS: u32 = 64;

pub struct CompiledBlock {
    pub buf: ExecutableBuffer,
    pub entry: AssemblyOffset,
    /// Faixa de PCs (virtuais) cobertos: `[start, end)`.
    pub start: u32,
    pub end: u32,
    pub insns: u32,
}

pub struct Compiler<'a> {
    ops: Assembler,
    fetch: &'a mut dyn FnMut(u32) -> u16,
    trace: bool,
    epilogue: DynamicLabel,
    cycles: u32,
}

fn ro(n: u8) -> i32 {
    Sh2State::reg_offset(n)
}

fn cost(i: &Insn) -> u32 {
    match i {
        Insn::MacL { .. } | Insn::MacW { .. } => 3,
        Insn::MulL { .. }
        | Insn::Dmuls { .. }
        | Insn::Dmulu { .. }
        | Insn::MulsW { .. }
        | Insn::MuluW { .. } => 2,
        Insn::Bra { .. }
        | Insn::Bsr { .. }
        | Insn::Braf { .. }
        | Insn::Bsrf { .. }
        | Insn::Jmp { .. }
        | Insn::Jsr { .. }
        | Insn::Rts => 2,
        Insn::Rte => 4,
        _ => 1,
    }
}

impl<'a> Compiler<'a> {
    pub fn compile(start: u32, fetch: &'a mut dyn FnMut(u32) -> u16, trace: bool) -> CompiledBlock {
        let mut ops = Assembler::new().expect("assembler");
        let epilogue = ops.new_dynamic_label();
        let entry = ops.offset();
        dynasm!(ops
            ; .arch x64
            ; push rbx
            ; push r12
            ; push r13
            ; push r14
            ; push r15
            ; mov r14, rdi
            ; mov r15, rsi
        );
        let mut c = Compiler {
            ops,
            fetch,
            trace,
            epilogue,
            cycles: 0,
        };

        let mut pc = start;
        let mut count = 0u32;
        let next_pc_reg_set;
        loop {
            let op = (c.fetch)(pc);
            let insn = decode(op);
            count += 1;
            c.cycles += cost(&insn);
            if c.trace {
                c.emit_trace(pc);
            }
            if insn.is_branch() {
                let slot_used = insn.has_delay_slot();
                c.emit_branch(insn, pc);
                if slot_used {
                    pc += 2;
                    count += 1;
                }
                pc += 2;
                next_pc_reg_set = true;
                break;
            }
            match insn {
                Insn::Unknown(_) => {
                    c.emit_exit(EXIT_ILLEGAL, pc, pc);
                    pc += 2;
                    next_pc_reg_set = true;
                    break;
                }
                Insn::Trapa { imm } => {
                    c.emit_exit(EXIT_TRAPA, imm, pc + 2);
                    pc += 2;
                    next_pc_reg_set = true;
                    break;
                }
                Insn::Sleep => {
                    c.emit_exit(EXIT_SLEEP, 0, pc + 2);
                    pc += 2;
                    next_pc_reg_set = true;
                    break;
                }
                _ => {}
            }
            c.emit_insn(insn, pc);
            pc += 2;
            if insn.ends_block() || count >= MAX_BLOCK_INSNS {
                next_pc_reg_set = false;
                break;
            }
        }
        if !next_pc_reg_set {
            let next = pc as i32;
            dynasm!(c.ops ; .arch x64 ; mov eax, next);
        }
        let ep = c.epilogue;
        let cyc = c.cycles as i32;
        let ocyc = OFF_CYCLES;
        dynasm!(c.ops
            ; .arch x64
            ; =>ep
            ; add QWORD [r14 + ocyc], cyc
            ; pop r15
            ; pop r14
            ; pop r13
            ; pop r12
            ; pop rbx
            ; ret
        );
        let buf = match c.ops.finalize() {
            Ok(b) => b,
            Err(_) => panic!("falha ao finalizar bloco JIT em {:08X}", start),
        };
        CompiledBlock {
            buf,
            entry,
            start,
            end: pc,
            insns: count,
        }
    }

    // ---------------------------------------------------------------- utilidades

    fn emit_trace(&mut self, pc: u32) {
        let f = rt_trace as usize as i64;
        let pc = pc as i32;
        dynasm!(self.ops
            ; .arch x64
            ; mov rdi, r14
            ; mov rsi, r15
            ; mov edx, pc
            ; mov rax, QWORD f
            ; call rax
        );
    }

    /// Sai do bloco com um motivo excepcional; `eax` = próximo PC.
    fn emit_exit(&mut self, reason: u32, arg: u32, next_pc: u32) {
        let (or, oa) = (OFF_EXIT_REASON, OFF_EXIT_ARG);
        let (reason, arg, next) = (reason as i32, arg as i32, next_pc as i32);
        let ep = self.epilogue;
        dynasm!(self.ops
            ; .arch x64
            ; mov DWORD [r14 + oa], arg
            ; mov DWORD [r14 + or], reason
            ; mov eax, next
            ; jmp =>ep
        );
    }

    /// T := al (0/1)
    fn t_from_al(&mut self) {
        let sr = OFF_SR;
        dynasm!(self.ops
            ; .arch x64
            ; movzx eax, al
            ; and DWORD [r14 + sr], -2
            ; or DWORD [r14 + sr], eax
        );
    }

    fn call_read(&mut self, sz: Sz) {
        let f = match sz {
            Sz::B => rt_read8 as usize,
            Sz::W => rt_read16 as usize,
            Sz::L => rt_read32 as usize,
        } as i64;
        dynasm!(self.ops ; .arch x64 ; mov rdi, r15 ; mov rax, QWORD f ; call rax);
    }

    fn call_write(&mut self, sz: Sz) {
        let f = match sz {
            Sz::B => rt_write8 as usize,
            Sz::W => rt_write16 as usize,
            Sz::L => rt_write32 as usize,
        } as i64;
        dynasm!(self.ops ; .arch x64 ; mov rdi, r15 ; mov rax, QWORD f ; call rax);
    }

    /// Calcula o endereço efetivo em `esi` (aplicando pós-incremento/pré-decremento).
    /// Não toca em `edx` (valor a armazenar) nem em ebx/r12/r13.
    fn ea_to_esi(&mut self, sz: Sz, ea: Ea, pc: u32) {
        let size = sz.bytes() as i32;
        match ea {
            Ea::Ind(x) => {
                let ox = ro(x);
                dynasm!(self.ops ; .arch x64 ; mov esi, DWORD [r14 + ox]);
            }
            Ea::PostInc(x) => {
                let ox = ro(x);
                dynasm!(self.ops
                    ; .arch x64
                    ; mov esi, DWORD [r14 + ox]
                    ; lea eax, [rsi + size]
                    ; mov DWORD [r14 + ox], eax
                );
            }
            Ea::PreDec(x) => {
                let ox = ro(x);
                dynasm!(self.ops
                    ; .arch x64
                    ; mov esi, DWORD [r14 + ox]
                    ; sub esi, size
                    ; mov DWORD [r14 + ox], esi
                );
            }
            Ea::Disp(x, d) => {
                let (ox, d) = (ro(x), d as i32);
                dynasm!(self.ops ; .arch x64 ; mov esi, DWORD [r14 + ox] ; add esi, d);
            }
            Ea::R0Idx(x) => {
                let (o0, ox) = (ro(0), ro(x));
                dynasm!(self.ops ; .arch x64 ; mov esi, DWORD [r14 + o0] ; add esi, DWORD [r14 + ox]);
            }
            Ea::Gbr(d) => {
                let (og, d) = (OFF_GBR, d as i32);
                dynasm!(self.ops ; .arch x64 ; mov esi, DWORD [r14 + og] ; add esi, d);
            }
            Ea::Pc(d) => {
                let a = match sz {
                    Sz::W => pc.wrapping_add(4).wrapping_add(d),
                    _ => (pc & !3).wrapping_add(4).wrapping_add(d),
                } as i32;
                dynasm!(self.ops ; .arch x64 ; mov esi, a);
            }
        }
    }

    // ------------------------------------------------------- instruções lineares

    fn emit_insn(&mut self, insn: Insn, pc: u32) {
        use Insn::*;
        let sr = OFF_SR;
        match insn {
            Nop => {}
            Load { sz, ea, rd } => {
                self.ea_to_esi(sz, ea, pc);
                self.call_read(sz);
                match sz {
                    Sz::B => dynasm!(self.ops ; .arch x64 ; movsx eax, al),
                    Sz::W => dynasm!(self.ops ; .arch x64 ; movsx eax, ax),
                    Sz::L => {}
                }
                let od = ro(rd);
                dynasm!(self.ops ; .arch x64 ; mov DWORD [r14 + od], eax);
            }
            Store { sz, ea, rs } => {
                let os = ro(rs);
                dynasm!(self.ops ; .arch x64 ; mov edx, DWORD [r14 + os]);
                self.ea_to_esi(sz, ea, pc);
                self.call_write(sz);
            }
            MovImm { n, imm } => {
                let on = ro(n);
                dynasm!(self.ops ; .arch x64 ; mov DWORD [r14 + on], imm);
            }
            Mov { m, n } => {
                let (om, on) = (ro(m), ro(n));
                dynasm!(self.ops ; .arch x64 ; mov eax, DWORD [r14 + om] ; mov DWORD [r14 + on], eax);
            }
            Mova { disp } => {
                let o0 = ro(0);
                let a = (pc & !3).wrapping_add(4).wrapping_add(disp) as i32;
                dynasm!(self.ops ; .arch x64 ; mov DWORD [r14 + o0], a);
            }
            Movt { n } => {
                let on = ro(n);
                dynasm!(self.ops
                    ; .arch x64
                    ; mov eax, DWORD [r14 + sr]
                    ; and eax, 1
                    ; mov DWORD [r14 + on], eax
                );
            }
            SwapB { m, n } => {
                let (om, on) = (ro(m), ro(n));
                dynasm!(self.ops
                    ; .arch x64
                    ; mov eax, DWORD [r14 + om]
                    ; rol ax, 8
                    ; mov DWORD [r14 + on], eax
                );
            }
            SwapW { m, n } => {
                let (om, on) = (ro(m), ro(n));
                dynasm!(self.ops
                    ; .arch x64
                    ; mov eax, DWORD [r14 + om]
                    ; rol eax, 16
                    ; mov DWORD [r14 + on], eax
                );
            }
            Xtrct { m, n } => {
                let (om, on) = (ro(m), ro(n));
                dynasm!(self.ops
                    ; .arch x64
                    ; mov eax, DWORD [r14 + on]
                    ; shr eax, 16
                    ; mov ecx, DWORD [r14 + om]
                    ; shl ecx, 16
                    ; or eax, ecx
                    ; mov DWORD [r14 + on], eax
                );
            }

            Add { m, n } => {
                let (om, on) = (ro(m), ro(n));
                dynasm!(self.ops ; .arch x64 ; mov eax, DWORD [r14 + om] ; add DWORD [r14 + on], eax);
            }
            AddImm { n, imm } => {
                let on = ro(n);
                dynasm!(self.ops ; .arch x64 ; add DWORD [r14 + on], imm);
            }
            Addc { m, n } => {
                let (om, on) = (ro(m), ro(n));
                dynasm!(self.ops
                    ; .arch x64
                    ; bt DWORD [r14 + sr], 0
                    ; mov eax, DWORD [r14 + om]
                    ; adc DWORD [r14 + on], eax
                    ; setc al
                );
                self.t_from_al();
            }
            Addv { m, n } => {
                let (om, on) = (ro(m), ro(n));
                dynasm!(self.ops
                    ; .arch x64
                    ; mov eax, DWORD [r14 + om]
                    ; add DWORD [r14 + on], eax
                    ; seto al
                );
                self.t_from_al();
            }
            Sub { m, n } => {
                let (om, on) = (ro(m), ro(n));
                dynasm!(self.ops ; .arch x64 ; mov eax, DWORD [r14 + om] ; sub DWORD [r14 + on], eax);
            }
            Subc { m, n } => {
                let (om, on) = (ro(m), ro(n));
                dynasm!(self.ops
                    ; .arch x64
                    ; bt DWORD [r14 + sr], 0
                    ; mov eax, DWORD [r14 + om]
                    ; sbb DWORD [r14 + on], eax
                    ; setc al
                );
                self.t_from_al();
            }
            Subv { m, n } => {
                let (om, on) = (ro(m), ro(n));
                dynasm!(self.ops
                    ; .arch x64
                    ; mov eax, DWORD [r14 + om]
                    ; sub DWORD [r14 + on], eax
                    ; seto al
                );
                self.t_from_al();
            }
            Neg { m, n } => {
                let (om, on) = (ro(m), ro(n));
                dynasm!(self.ops
                    ; .arch x64
                    ; mov eax, DWORD [r14 + om]
                    ; neg eax
                    ; mov DWORD [r14 + on], eax
                );
            }
            Negc { m, n } => {
                let (om, on) = (ro(m), ro(n));
                dynasm!(self.ops
                    ; .arch x64
                    ; bt DWORD [r14 + sr], 0
                    ; mov eax, 0
                    ; sbb eax, DWORD [r14 + om]
                    ; mov DWORD [r14 + on], eax
                    ; setc al
                );
                self.t_from_al();
            }
            And { m, n } => {
                let (om, on) = (ro(m), ro(n));
                dynasm!(self.ops ; .arch x64 ; mov eax, DWORD [r14 + om] ; and DWORD [r14 + on], eax);
            }
            Or { m, n } => {
                let (om, on) = (ro(m), ro(n));
                dynasm!(self.ops ; .arch x64 ; mov eax, DWORD [r14 + om] ; or DWORD [r14 + on], eax);
            }
            Xor { m, n } => {
                let (om, on) = (ro(m), ro(n));
                dynasm!(self.ops ; .arch x64 ; mov eax, DWORD [r14 + om] ; xor DWORD [r14 + on], eax);
            }
            Not { m, n } => {
                let (om, on) = (ro(m), ro(n));
                dynasm!(self.ops
                    ; .arch x64
                    ; mov eax, DWORD [r14 + om]
                    ; not eax
                    ; mov DWORD [r14 + on], eax
                );
            }
            AndImm { imm } => {
                let (o0, imm) = (ro(0), imm as i32);
                dynasm!(self.ops ; .arch x64 ; and DWORD [r14 + o0], imm);
            }
            OrImm { imm } => {
                let (o0, imm) = (ro(0), imm as i32);
                dynasm!(self.ops ; .arch x64 ; or DWORD [r14 + o0], imm);
            }
            XorImm { imm } => {
                let (o0, imm) = (ro(0), imm as i32);
                dynasm!(self.ops ; .arch x64 ; xor DWORD [r14 + o0], imm);
            }
            Tst { m, n } => {
                let (om, on) = (ro(m), ro(n));
                dynasm!(self.ops
                    ; .arch x64
                    ; mov eax, DWORD [r14 + on]
                    ; test DWORD [r14 + om], eax
                    ; setz al
                );
                self.t_from_al();
            }
            TstImm { imm } => {
                let (o0, imm) = (ro(0), imm as i32);
                dynasm!(self.ops ; .arch x64 ; test DWORD [r14 + o0], imm ; setz al);
                self.t_from_al();
            }
            CmpEq { m, n } => self.cmp_rr(m, n, 0),
            CmpHs { m, n } => self.cmp_rr(m, n, 1),
            CmpGe { m, n } => self.cmp_rr(m, n, 2),
            CmpHi { m, n } => self.cmp_rr(m, n, 3),
            CmpGt { m, n } => self.cmp_rr(m, n, 4),
            CmpStr { m, n } => {
                let (om, on) = (ro(m), ro(n));
                let c1 = 0x0101_0101u32 as i32;
                let c2 = 0x8080_8080u32 as i32;
                dynasm!(self.ops
                    ; .arch x64
                    ; mov eax, DWORD [r14 + on]
                    ; xor eax, DWORD [r14 + om]
                    ; mov ecx, eax
                    ; sub ecx, c1
                    ; not eax
                    ; and eax, ecx
                    ; and eax, c2
                    ; setnz al
                );
                self.t_from_al();
            }
            CmpPl { n } => {
                let on = ro(n);
                dynasm!(self.ops ; .arch x64 ; cmp DWORD [r14 + on], 0 ; setg al);
                self.t_from_al();
            }
            CmpPz { n } => {
                let on = ro(n);
                dynasm!(self.ops ; .arch x64 ; cmp DWORD [r14 + on], 0 ; setge al);
                self.t_from_al();
            }
            CmpEqImm { imm } => {
                let o0 = ro(0);
                dynasm!(self.ops ; .arch x64 ; cmp DWORD [r14 + o0], imm ; sete al);
                self.t_from_al();
            }
            Div0u => {
                let mask = !(SR_T | SR_Q | SR_M) as i32;
                dynasm!(self.ops ; .arch x64 ; and DWORD [r14 + sr], mask);
            }
            Div0s { m, n } => {
                let (om, on) = (ro(m), ro(n));
                let mask = !(SR_T | SR_Q | SR_M) as i32;
                dynasm!(self.ops
                    ; .arch x64
                    ; mov eax, DWORD [r14 + on]
                    ; shr eax, 31
                    ; mov ecx, DWORD [r14 + om]
                    ; shr ecx, 31
                    ; mov edx, eax
                    ; xor edx, ecx
                    ; shl eax, 8
                    ; shl ecx, 9
                    ; or eax, ecx
                    ; or eax, edx
                    ; mov ecx, DWORD [r14 + sr]
                    ; and ecx, mask
                    ; or ecx, eax
                    ; mov DWORD [r14 + sr], ecx
                );
            }
            Div1 { m, n } => self.emit_div1(m, n),
            Dt { n } => {
                let on = ro(n);
                dynasm!(self.ops ; .arch x64 ; sub DWORD [r14 + on], 1 ; setz al);
                self.t_from_al();
            }
            ExtuB { m, n } => {
                let (om, on) = (ro(m), ro(n));
                dynasm!(self.ops ; .arch x64 ; movzx eax, BYTE [r14 + om] ; mov DWORD [r14 + on], eax);
            }
            ExtuW { m, n } => {
                let (om, on) = (ro(m), ro(n));
                dynasm!(self.ops ; .arch x64 ; movzx eax, WORD [r14 + om] ; mov DWORD [r14 + on], eax);
            }
            ExtsB { m, n } => {
                let (om, on) = (ro(m), ro(n));
                dynasm!(self.ops ; .arch x64 ; movsx eax, BYTE [r14 + om] ; mov DWORD [r14 + on], eax);
            }
            ExtsW { m, n } => {
                let (om, on) = (ro(m), ro(n));
                dynasm!(self.ops ; .arch x64 ; movsx eax, WORD [r14 + om] ; mov DWORD [r14 + on], eax);
            }

            MulL { m, n } => {
                let (om, on, oml) = (ro(m), ro(n), OFF_MACL);
                dynasm!(self.ops
                    ; .arch x64
                    ; mov eax, DWORD [r14 + on]
                    ; imul eax, DWORD [r14 + om]
                    ; mov DWORD [r14 + oml], eax
                );
            }
            MulsW { m, n } => {
                let (om, on, oml) = (ro(m), ro(n), OFF_MACL);
                dynasm!(self.ops
                    ; .arch x64
                    ; movsx eax, WORD [r14 + on]
                    ; movsx ecx, WORD [r14 + om]
                    ; imul eax, ecx
                    ; mov DWORD [r14 + oml], eax
                );
            }
            MuluW { m, n } => {
                let (om, on, oml) = (ro(m), ro(n), OFF_MACL);
                dynasm!(self.ops
                    ; .arch x64
                    ; movzx eax, WORD [r14 + on]
                    ; movzx ecx, WORD [r14 + om]
                    ; imul eax, ecx
                    ; mov DWORD [r14 + oml], eax
                );
            }
            Dmuls { m, n } => {
                let (om, on, oml, omh) = (ro(m), ro(n), OFF_MACL, OFF_MACH);
                dynasm!(self.ops
                    ; .arch x64
                    ; mov eax, DWORD [r14 + on]
                    ; imul DWORD [r14 + om]
                    ; mov DWORD [r14 + oml], eax
                    ; mov DWORD [r14 + omh], edx
                );
            }
            Dmulu { m, n } => {
                let (om, on, oml, omh) = (ro(m), ro(n), OFF_MACL, OFF_MACH);
                dynasm!(self.ops
                    ; .arch x64
                    ; mov eax, DWORD [r14 + on]
                    ; mul DWORD [r14 + om]
                    ; mov DWORD [r14 + oml], eax
                    ; mov DWORD [r14 + omh], edx
                );
            }
            MacL { m, n } => self.emit_mac_l(m, n),
            MacW { m, n } => self.emit_mac_w(m, n),
            Clrmac => {
                let (oml, omh) = (OFF_MACL, OFF_MACH);
                dynasm!(self.ops ; .arch x64 ; mov DWORD [r14 + oml], 0 ; mov DWORD [r14 + omh], 0);
            }

            Shll { n } => self.shift_t(n, 0),
            Shlr { n } => self.shift_t(n, 1),
            Shar { n } => self.shift_t(n, 2),
            Rotl { n } => self.shift_t(n, 3),
            Rotr { n } => self.shift_t(n, 4),
            Rotcl { n } => self.shift_t(n, 5),
            Rotcr { n } => self.shift_t(n, 6),
            Shll2 { n } => {
                let on = ro(n);
                dynasm!(self.ops ; .arch x64 ; shl DWORD [r14 + on], 2);
            }
            Shlr2 { n } => {
                let on = ro(n);
                dynasm!(self.ops ; .arch x64 ; shr DWORD [r14 + on], 2);
            }
            Shll8 { n } => {
                let on = ro(n);
                dynasm!(self.ops ; .arch x64 ; shl DWORD [r14 + on], 8);
            }
            Shlr8 { n } => {
                let on = ro(n);
                dynasm!(self.ops ; .arch x64 ; shr DWORD [r14 + on], 8);
            }
            Shll16 { n } => {
                let on = ro(n);
                dynasm!(self.ops ; .arch x64 ; shl DWORD [r14 + on], 16);
            }
            Shlr16 { n } => {
                let on = ro(n);
                dynasm!(self.ops ; .arch x64 ; shr DWORD [r14 + on], 16);
            }
            Clrt => {
                dynasm!(self.ops ; .arch x64 ; and DWORD [r14 + sr], -2);
            }
            Sett => {
                dynasm!(self.ops ; .arch x64 ; or DWORD [r14 + sr], 1);
            }

            StsReg { s, n } => {
                let (os, on) = (Sh2State::sys_offset(s), ro(n));
                dynasm!(self.ops ; .arch x64 ; mov eax, DWORD [r14 + os] ; mov DWORD [r14 + on], eax);
            }
            LdsReg { s, m } => {
                let (os, om) = (Sh2State::sys_offset(s), ro(m));
                let mask = SR_MASK as i32;
                dynasm!(self.ops ; .arch x64 ; mov eax, DWORD [r14 + om]);
                if s == Sys::Sr {
                    dynasm!(self.ops ; .arch x64 ; and eax, mask);
                }
                dynasm!(self.ops ; .arch x64 ; mov DWORD [r14 + os], eax);
            }
            StsMem { s, n } => {
                let (os, on) = (Sh2State::sys_offset(s), ro(n));
                dynasm!(self.ops
                    ; .arch x64
                    ; mov edx, DWORD [r14 + os]
                    ; mov esi, DWORD [r14 + on]
                    ; sub esi, 4
                    ; mov DWORD [r14 + on], esi
                );
                self.call_write(Sz::L);
            }
            LdsMem { s, m } => {
                let (os, om) = (Sh2State::sys_offset(s), ro(m));
                let mask = SR_MASK as i32;
                dynasm!(self.ops
                    ; .arch x64
                    ; mov esi, DWORD [r14 + om]
                    ; lea eax, [rsi + 4]
                    ; mov DWORD [r14 + om], eax
                );
                self.call_read(Sz::L);
                if s == Sys::Sr {
                    dynasm!(self.ops ; .arch x64 ; and eax, mask);
                }
                dynasm!(self.ops ; .arch x64 ; mov DWORD [r14 + os], eax);
            }

            TstB { imm } => {
                let (o0, og, imm) = (ro(0), OFF_GBR, imm as i32);
                dynasm!(self.ops
                    ; .arch x64
                    ; mov esi, DWORD [r14 + o0]
                    ; add esi, DWORD [r14 + og]
                );
                self.call_read(Sz::B);
                dynasm!(self.ops ; .arch x64 ; test eax, imm ; setz al);
                self.t_from_al();
            }
            AndB { imm } => self.rmw_gbr_byte(imm, 0),
            OrB { imm } => self.rmw_gbr_byte(imm, 1),
            XorB { imm } => self.rmw_gbr_byte(imm, 2),
            Tas { n } => {
                let on = ro(n);
                dynasm!(self.ops
                    ; .arch x64
                    ; mov esi, DWORD [r14 + on]
                    ; mov r12d, esi
                );
                self.call_read(Sz::B);
                dynasm!(self.ops
                    ; .arch x64
                    ; mov r13d, eax
                    ; mov esi, r12d
                    ; mov edx, r13d
                    ; or edx, 0x80
                );
                self.call_write(Sz::B);
                dynasm!(self.ops ; .arch x64 ; test r13d, r13d ; sete al);
                self.t_from_al();
            }

            // Desvios são tratados em `emit_branch`; Trapa/Sleep/Unknown em `compile`.
            _ => unreachable!("instrução não linear em emit_insn: {:?}", insn),
        }
    }

    fn cmp_rr(&mut self, m: u8, n: u8, cond: u8) {
        let (om, on) = (ro(m), ro(n));
        dynasm!(self.ops ; .arch x64 ; mov eax, DWORD [r14 + on] ; cmp eax, DWORD [r14 + om]);
        match cond {
            0 => dynasm!(self.ops ; .arch x64 ; sete al),
            1 => dynasm!(self.ops ; .arch x64 ; setae al),
            2 => dynasm!(self.ops ; .arch x64 ; setge al),
            3 => dynasm!(self.ops ; .arch x64 ; seta al),
            _ => dynasm!(self.ops ; .arch x64 ; setg al),
        }
        self.t_from_al();
    }

    /// 0 shll/shal, 1 shlr, 2 shar, 3 rotl, 4 rotr, 5 rotcl, 6 rotcr — todos definem T = bit expulso.
    fn shift_t(&mut self, n: u8, kind: u8) {
        let (on, sr) = (ro(n), OFF_SR);
        match kind {
            0 => dynasm!(self.ops ; .arch x64 ; shl DWORD [r14 + on], 1 ; setc al),
            1 => dynasm!(self.ops ; .arch x64 ; shr DWORD [r14 + on], 1 ; setc al),
            2 => dynasm!(self.ops ; .arch x64 ; sar DWORD [r14 + on], 1 ; setc al),
            3 => dynasm!(self.ops ; .arch x64 ; rol DWORD [r14 + on], 1 ; setc al),
            4 => dynasm!(self.ops ; .arch x64 ; ror DWORD [r14 + on], 1 ; setc al),
            5 => {
                dynasm!(self.ops ; .arch x64 ; bt DWORD [r14 + sr], 0 ; rcl DWORD [r14 + on], 1 ; setc al)
            }
            _ => {
                dynasm!(self.ops ; .arch x64 ; bt DWORD [r14 + sr], 0 ; rcr DWORD [r14 + on], 1 ; setc al)
            }
        }
        self.t_from_al();
    }

    /// and.b/or.b/xor.b #imm,@(R0,GBR)
    fn rmw_gbr_byte(&mut self, imm: u32, kind: u8) {
        let (o0, og, imm) = (ro(0), OFF_GBR, imm as i32);
        dynasm!(self.ops
            ; .arch x64
            ; mov esi, DWORD [r14 + o0]
            ; add esi, DWORD [r14 + og]
            ; mov r12d, esi
        );
        self.call_read(Sz::B);
        match kind {
            0 => dynasm!(self.ops ; .arch x64 ; and eax, imm),
            1 => dynasm!(self.ops ; .arch x64 ; or eax, imm),
            _ => dynasm!(self.ops ; .arch x64 ; xor eax, imm),
        }
        dynasm!(self.ops ; .arch x64 ; mov edx, eax ; mov esi, r12d);
        self.call_write(Sz::B);
    }

    /// DIV1 Rm,Rn — ver o algoritmo no manual do SH-2:
    /// Q' = carry_out ^ MSB(Rn) ^ M; soma/subtrai conforme (Q == M); T = (Q' == M).
    fn emit_div1(&mut self, m: u8, n: u8) {
        let (om, on, sr) = (ro(m), ro(n), OFF_SR);
        let do_add = self.ops.new_dynamic_label();
        let done = self.ops.new_dynamic_label();
        let keep = !(SR_Q | SR_T) as i32;
        dynasm!(self.ops
            ; .arch x64
            ; mov eax, DWORD [r14 + on]
            ; mov ecx, DWORD [r14 + om]
            ; mov edx, DWORD [r14 + sr]
            ; mov r8d, eax
            ; shr r8d, 31
            ; mov r9d, edx
            ; shr r9d, 8
            ; and r9d, 1
            ; mov r10d, edx
            ; shr r10d, 9
            ; and r10d, 1
            ; mov r11d, edx
            ; and r11d, 1
            ; shl eax, 1
            ; or eax, r11d
            ; cmp r9d, r10d
            ; jne =>do_add
            ; sub eax, ecx
            ; setc r11b
            ; jmp =>done
            ; =>do_add
            ; add eax, ecx
            ; setc r11b
            ; =>done
            ; mov DWORD [r14 + on], eax
            ; xor r11d, r8d
            ; xor r11d, r10d
            ; and r11d, 1
            ; mov r8d, r11d
            ; xor r8d, r10d
            ; xor r8d, 1
            ; shl r11d, 8
            ; or r11d, r8d
            ; mov edx, DWORD [r14 + sr]
            ; and edx, keep
            ; or edx, r11d
            ; mov DWORD [r14 + sr], edx
        );
    }

    /// MAC.L @Rm+,@Rn+ — acumulador de 64 bits; com S=1 satura em 48 bits.
    fn emit_mac_l(&mut self, m: u8, n: u8) {
        let (om, on, sr, oml, omh) = (ro(m), ro(n), OFF_SR, OFF_MACL, OFF_MACH);
        let nosat = self.ops.new_dynamic_label();
        let lo = self.ops.new_dynamic_label();
        dynasm!(self.ops
            ; .arch x64
            ; mov esi, DWORD [r14 + on]
            ; lea eax, [rsi + 4]
            ; mov DWORD [r14 + on], eax
        );
        self.call_read(Sz::L);
        dynasm!(self.ops
            ; .arch x64
            ; mov r12d, eax
            ; mov esi, DWORD [r14 + om]
            ; lea eax, [rsi + 4]
            ; mov DWORD [r14 + om], eax
        );
        self.call_read(Sz::L);
        let hi = 0x0000_7FFF_FFFF_FFFFi64;
        let lo_c = -0x0000_8000_0000_0000i64;
        dynasm!(self.ops
            ; .arch x64
            ; mov r13d, eax
            ; movsxd rax, r12d
            ; movsxd rcx, r13d
            ; imul rax, rcx
            ; mov edx, DWORD [r14 + omh]
            ; shl rdx, 32
            ; mov ecx, DWORD [r14 + oml]
            ; or rdx, rcx
            ; add rax, rdx
            ; test DWORD [r14 + sr], 2
            ; jz =>nosat
            ; mov rdx, QWORD hi
            ; cmp rax, rdx
            ; jle =>lo
            ; mov rax, rdx
            ; jmp =>nosat
            ; =>lo
            ; mov rdx, QWORD lo_c
            ; cmp rax, rdx
            ; jge =>nosat
            ; mov rax, rdx
            ; =>nosat
            ; mov DWORD [r14 + oml], eax
            ; shr rax, 32
            ; mov DWORD [r14 + omh], eax
        );
    }

    /// MAC.W @Rm+,@Rn+ — S=0: acumula 64 bits; S=1: soma saturada de 32 bits em MACL (MACH=1 no overflow).
    fn emit_mac_w(&mut self, m: u8, n: u8) {
        let (om, on, sr, oml, omh) = (ro(m), ro(n), OFF_SR, OFF_MACL, OFF_MACH);
        let sat = self.ops.new_dynamic_label();
        let store = self.ops.new_dynamic_label();
        let done = self.ops.new_dynamic_label();
        dynasm!(self.ops
            ; .arch x64
            ; mov esi, DWORD [r14 + on]
            ; lea eax, [rsi + 2]
            ; mov DWORD [r14 + on], eax
        );
        self.call_read(Sz::W);
        dynasm!(self.ops
            ; .arch x64
            ; mov r12d, eax
            ; mov esi, DWORD [r14 + om]
            ; lea eax, [rsi + 2]
            ; mov DWORD [r14 + om], eax
        );
        self.call_read(Sz::W);
        let smin = i32::MIN;
        dynasm!(self.ops
            ; .arch x64
            ; mov r13d, eax
            ; movsx r12d, r12w
            ; movsx r13d, r13w
            ; imul r12d, r13d
            ; test DWORD [r14 + sr], 2
            ; jnz =>sat
            ; movsxd rax, r12d
            ; mov edx, DWORD [r14 + omh]
            ; shl rdx, 32
            ; mov ecx, DWORD [r14 + oml]
            ; or rdx, rcx
            ; add rax, rdx
            ; mov DWORD [r14 + oml], eax
            ; shr rax, 32
            ; mov DWORD [r14 + omh], eax
            ; jmp =>done
            ; =>sat
            ; mov eax, DWORD [r14 + oml]
            ; add eax, r12d
            ; jno =>store
            ; mov DWORD [r14 + omh], 1
            ; mov eax, 0x7FFF_FFFF
            ; test r12d, r12d
            ; jns =>store
            ; mov eax, smin
            ; =>store
            ; mov DWORD [r14 + oml], eax
            ; =>done
        );
    }

    // ------------------------------------------------------------------- desvios

    /// Emite o delay slot (uma instrução linear) em `slot_pc`.
    fn emit_slot(&mut self, slot_pc: u32) {
        let op = (self.fetch)(slot_pc);
        let insn = decode(op);
        self.cycles += cost(&insn);
        if self.trace {
            self.emit_trace(slot_pc);
        }
        if insn.is_branch()
            || insn.ends_block() && !matches!(insn, Insn::LdsReg { .. } | Insn::LdsMem { .. })
        {
            // Instrução ilegal em delay slot.
            self.emit_exit(EXIT_ILLEGAL, slot_pc, slot_pc);
        } else {
            self.emit_insn(insn, slot_pc);
        }
    }

    /// The target is computed and stashed in a register **before** `emit_slot` runs, on
    /// every arm below. That order is SH-2 semantics, not an x86-64 quirk: the delay slot
    /// executes using the pre-branch register state, and can overwrite the very register
    /// the target was read from (`jmp @rN` with the slot doing `mov rN, ...` is legal and
    /// the branch must still go to the old value). A future backend has to preserve this
    /// ordering — compute-target-then-emit-slot — not just port the instruction selection.
    fn emit_branch(&mut self, insn: Insn, pc: u32) {
        use Insn::*;
        let ep = self.epilogue;
        let slot = pc + 2;
        let (opr, sr) = (OFF_PR, OFF_SR);
        let ret_addr = pc.wrapping_add(4) as i32;
        match insn {
            Bra { disp } => {
                let target = pc.wrapping_add(4).wrapping_add((disp * 2) as u32) as i32;
                self.emit_slot(slot);
                dynasm!(self.ops ; .arch x64 ; mov eax, target ; jmp =>ep);
            }
            Bsr { disp } => {
                let target = pc.wrapping_add(4).wrapping_add((disp * 2) as u32) as i32;
                dynasm!(self.ops ; .arch x64 ; mov DWORD [r14 + opr], ret_addr);
                self.emit_slot(slot);
                dynasm!(self.ops ; .arch x64 ; mov eax, target ; jmp =>ep);
            }
            Braf { m } => {
                let om = ro(m);
                dynasm!(self.ops ; .arch x64 ; mov ebx, DWORD [r14 + om] ; add ebx, ret_addr);
                self.emit_slot(slot);
                dynasm!(self.ops ; .arch x64 ; mov eax, ebx ; jmp =>ep);
            }
            Bsrf { m } => {
                let om = ro(m);
                dynasm!(self.ops
                    ; .arch x64
                    ; mov DWORD [r14 + opr], ret_addr
                    ; mov ebx, DWORD [r14 + om]
                    ; add ebx, ret_addr
                );
                self.emit_slot(slot);
                dynasm!(self.ops ; .arch x64 ; mov eax, ebx ; jmp =>ep);
            }
            Jmp { m } => {
                let om = ro(m);
                dynasm!(self.ops ; .arch x64 ; mov ebx, DWORD [r14 + om]);
                self.emit_slot(slot);
                dynasm!(self.ops ; .arch x64 ; mov eax, ebx ; jmp =>ep);
            }
            Jsr { m } => {
                let om = ro(m);
                dynasm!(self.ops
                    ; .arch x64
                    ; mov DWORD [r14 + opr], ret_addr
                    ; mov ebx, DWORD [r14 + om]
                );
                self.emit_slot(slot);
                dynasm!(self.ops ; .arch x64 ; mov eax, ebx ; jmp =>ep);
            }
            Rts => {
                dynasm!(self.ops ; .arch x64 ; mov ebx, DWORD [r14 + opr]);
                self.emit_slot(slot);
                dynasm!(self.ops ; .arch x64 ; mov eax, ebx ; jmp =>ep);
            }
            Rte => {
                // O SR restaurado fica em `exit_arg` (memória) durante o delay slot, para não
                // colidir com r12/r13, que instruções do slot (mac, tas, and.b...) podem usar.
                let (o15, mask, oarg) = (ro(15), SR_MASK as i32, OFF_EXIT_ARG);
                dynasm!(self.ops ; .arch x64 ; mov esi, DWORD [r14 + o15]);
                self.call_read(Sz::L);
                dynasm!(self.ops
                    ; .arch x64
                    ; mov ebx, eax
                    ; mov esi, DWORD [r14 + o15]
                    ; add esi, 4
                );
                self.call_read(Sz::L);
                dynasm!(self.ops
                    ; .arch x64
                    ; and eax, mask
                    ; mov DWORD [r14 + oarg], eax
                    ; add DWORD [r14 + o15], 8
                );
                self.emit_slot(slot);
                dynasm!(self.ops
                    ; .arch x64
                    ; mov eax, DWORD [r14 + oarg]
                    ; mov DWORD [r14 + sr], eax
                    ; mov eax, ebx
                    ; jmp =>ep
                );
            }
            Bt { disp } | Bf { disp } => {
                let target = pc.wrapping_add(4).wrapping_add((disp * 2) as u32) as i32;
                let not_taken = self.ops.new_dynamic_label();
                let fall = (pc + 2) as i32;
                dynasm!(self.ops ; .arch x64 ; test DWORD [r14 + sr], 1);
                if matches!(insn, Bt { .. }) {
                    dynasm!(self.ops ; .arch x64 ; jz =>not_taken);
                } else {
                    dynasm!(self.ops ; .arch x64 ; jnz =>not_taken);
                }
                dynasm!(self.ops
                    ; .arch x64
                    ; mov eax, target
                    ; jmp =>ep
                    ; =>not_taken
                    ; mov eax, fall
                    ; jmp =>ep
                );
            }
            BtS { disp } | BfS { disp } => {
                let target = pc.wrapping_add(4).wrapping_add((disp * 2) as u32) as i32;
                let not_taken = self.ops.new_dynamic_label();
                let fall = (pc + 4) as i32;
                dynasm!(self.ops ; .arch x64 ; mov ebx, DWORD [r14 + sr] ; and ebx, 1);
                if matches!(insn, BfS { .. }) {
                    dynasm!(self.ops ; .arch x64 ; xor ebx, 1);
                }
                self.emit_slot(slot);
                dynasm!(self.ops
                    ; .arch x64
                    ; test ebx, ebx
                    ; jz =>not_taken
                    ; mov eax, target
                    ; jmp =>ep
                    ; =>not_taken
                    ; mov eax, fall
                    ; jmp =>ep
                );
            }
            _ => unreachable!(),
        }
    }
}
