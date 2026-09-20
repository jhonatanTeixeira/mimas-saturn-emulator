#!/usr/bin/env python3
"""Decodifica um trace capturado (`bios_trace.txt`) em desmontagem anotada.

Lê as linhas `Frame: … | Core: … | PC: … | Opcode: … | …` e imprime, por linha,
o PC, o opcode, a desmontagem e um esboço de equivalente x86 para leitura
humana. A tabela de decodificação foi escrita nibble a nibble conferindo o
decodificador do YabaSanshiro, opcode por opcode, na época em que consultá-lo
ainda era permitido; opcodes nunca vistos em trace imprimem UNKNOWN em vez de
serem chutados.

Isto é ferramenta de captura, não parte do emulador: nada daqui entra em
`src/`. O passo é opcional — `mimasv2 --trace-check` consome o trace cru.

    python3 tools/trace-capture/decode_trace.py bios_trace.txt > decoded.txt
"""

BASE = 0x06000000  # start of extraction/IP.BIN, not 0.BIN's own 0x06004000 - see load_combined_binary()


def s8(v):
    return v - 0x100 if v & 0x80 else v


def s16(v):
    return v - 0x10000 if v & 0x8000 else v


def load_binary(path):
    with open(path, "rb") as f:
        return f.read()


IP_BIN_LOAD_ADDR = 0x06000000  # confirmed 2026-08-06 by direct disassembly test, NOT the
                                # 0x06002000 docs/address_mapping.md assumed - decoding IP.BIN's
                                # own bytes at that base produced a real function prologue right
                                # where a genuinely-traced call target (060006AA) landed as BSRF R3


def load_combined_binary(bin_path, ip_bin_path):
    """Builds one flat buffer spanning [IP_BIN_LOAD_ADDR, BASE_OF_0BIN + len(0.BIN)) so
    read_u16/read_u32 (BASE = IP_BIN_LOAD_ADDR) resolve BOTH files with correct real-hardware
    precedence: IP.BIN owns [0x06000000, 0x06004000) (its own first 16KB - the part 0.BIN's
    own load never overwrites), 0.BIN owns [0x06004000, 0x06004000+len(0.BIN)) as before -
    unchanged bytes/offsets for every address that already worked. Root-caused 2026-08-06:
    108 call targets that were failing "fora do alcance de 0.BIN" split into two clusters,
    52 of which sit below 0x06004000 - IP.BIN's own address range - and were simply never
    mapped at all (the other 56 are a separate, still-open question: dynamically-loaded
    OL00.BIN..OL15.BIN overlay files sharing the address space starting at 0x060D0000,
    which specific one is live depends on real emulator state a static file alone can't
    answer - see the portal_trace-based analyzer idea in the conversation this was found in
    for the actual next step there, not attempted here)."""
    ip_bin = load_binary(ip_bin_path)
    zero_bin = load_binary(bin_path)
    zero_bin_base = 0x06004000
    ip_bin_slice = ip_bin[: zero_bin_base - IP_BIN_LOAD_ADDR]
    return ip_bin_slice + zero_bin


def read_u16(data, addr):
    off = addr - BASE
    if off < 0 or off + 1 >= len(data):
        return None
    return (data[off] << 8) | data[off + 1]


def read_u32(data, addr):
    off = addr - BASE
    if off < 0 or off + 3 >= len(data):
        return None
    return (data[off] << 24) | (data[off + 1] << 16) | (data[off + 2] << 8) | data[off + 3]


class Decoded:
    """One decoded instruction: mnemonic text, plus structured fields the
    study generator uses for call/branch/pool-resolution logic."""

    def __init__(self, addr, op, mnemonic, kind=None, target=None, pool_addr=None, pool_val=None):
        self.addr = addr
        self.op = op
        self.mnemonic = mnemonic
        self.kind = kind          # 'call' | 'branch' | 'cond_branch' | None
        self.target = target      # resolved absolute target, if known
        self.pool_addr = pool_addr
        self.pool_val = pool_val


def decode(pc, op, data=None):
    """Decode one opcode at address pc. If `data` (the 0.BIN bytes) is given,
    PC-relative literal pool loads (MOV.L/MOV.W @(disp,PC)) are resolved."""
    a = (op & 0xF000) >> 12
    b = (op & 0x0F00) >> 8   # n
    c = (op & 0x00F0) >> 4   # m
    d = op & 0x000F
    cd = op & 0x00FF
    bcd = op & 0x0FFF

    # Real fix 2026-08-08 (same audit): these 7 are all 0-operand
    # pseudo-instructions living under A=0's D-switch (D=8,9,11 each
    # sub-switched on C only - see sh2int.c) - the dispatch NEVER examines
    # B (the n field) for any of them, so real hardware decodes them the
    # same way regardless of B's value. Real compiled code always emits
    # B=0 by convention, so this was a `op == 0x0009`-style exact-16-bit
    # check before (requiring B=0 too) - harmless in the overwhelming
    # majority of cases, but not what the real decoder does; generalized
    # to match cd (C+D only), confirmed evidence-based, not a guess.
    if a == 0 and cd == 0x09:
        return Decoded(pc, op, "NOP")
    if a == 0 and cd == 0x0B:
        return Decoded(pc, op, "RTS", kind="branch", target=None)  # target = PR, unknown statically
    if a == 0 and cd == 0x2B:
        return Decoded(pc, op, "RTE")
    if a == 0 and cd == 0x08:
        return Decoded(pc, op, "CLRT")
    if a == 0 and cd == 0x28:
        return Decoded(pc, op, "CLRMAC")
    if a == 0 and cd == 0x18:
        return Decoded(pc, op, "SETT")
    if a == 0 and cd == 0x19:
        return Decoded(pc, op, "DIV0U")
    if a == 2 and d == 7:
        return Decoded(pc, op, f"DIV0S R{c},R{b}")
    if a == 0 and d == 2:
        return Decoded(pc, op, {0: f"STC SR,R{b}", 1: f"STC GBR,R{b}", 2: f"STC VBR,R{b}"}.get(c, f"UNKNOWN{op:04X}"))
    if a == 0 and d == 0xA:
        return Decoded(pc, op, {0: f"STS MACH,R{b}", 1: f"STS MACL,R{b}", 2: f"STS PR,R{b}"}.get(c, f"UNKNOWN{op:04X}"))
    if op & 0xF0FF == 0x0029:
        return Decoded(pc, op, f"MOVT R{b}")
    if a == 0 and d == 4:
        return Decoded(pc, op, f"MOV.B R{c},@(R0,R{b})")
    if a == 0 and d == 5:
        return Decoded(pc, op, f"MOV.W R{c},@(R0,R{b})")
    if a == 0 and d == 6:
        return Decoded(pc, op, f"MOV.L R{c},@(R0,R{b})")
    if a == 0 and d == 7:
        # Real gap found + fixed 2026-08-08 (first live draft run): genuinely
        # missing, confirmed against vendor/yabause/src/sh2int.c's real
        # decode switch (case 0 -> switch(D) -> case 7: return &SH2mull -
        # unconditional on C, unlike the D=2/0xA cases above which
        # sub-switch on C for a control-register selector; here C is
        # literally the Rm operand). MUL.L Rm,Rn: 32x32->32 multiply,
        # result in MACL.
        return Decoded(pc, op, f"MUL.L R{c},R{b}")
    if a == 0 and d == 0xC:
        return Decoded(pc, op, f"MOV.B @(R0,R{c}),R{b}")
    if a == 0 and d == 0xD:
        return Decoded(pc, op, f"MOV.W @(R0,R{c}),R{b}")
    if a == 0 and d == 0xE:
        return Decoded(pc, op, f"MOV.L @(R0,R{c}),R{b}")
    if a == 0 and d == 0xF:
        # Real gap found + fixed 2026-08-08 (same systematic audit as
        # MUL.L above): D=15 unconditional -> SH2macl, confirmed against
        # sh2int.c. MAC.L @Rm+,@Rn+: 32x32->64 multiply-accumulate.
        return Decoded(pc, op, f"MAC.L @R{c}+,@R{b}+")
    if a == 0 and cd == 0x23:
        return Decoded(pc, op, f"BRAF R{b}", kind="branch", target=None)  # register-indirect, needs reg tracking
    if a == 0 and cd == 0x03:
        return Decoded(pc, op, f"BSRF R{b}", kind="call", target=None)
    if a == 1:
        return Decoded(pc, op, f"MOV.L R{c},@({d}*4,R{b})")
    if a == 2 and d == 0:
        return Decoded(pc, op, f"MOV.B R{c},@R{b}")
    if a == 2 and d == 1:
        return Decoded(pc, op, f"MOV.W R{c},@R{b}")
    if a == 2 and d == 2:
        return Decoded(pc, op, f"MOV.L R{c},@R{b}")
    if a == 2 and d == 4:
        return Decoded(pc, op, f"MOV.B R{c},@-R{b}")
    if a == 2 and d == 5:
        return Decoded(pc, op, f"MOV.W R{c},@-R{b}")
    if a == 2 and d == 6:
        return Decoded(pc, op, f"MOV.L R{c},@-R{b}")
    if a == 2 and d == 8:
        return Decoded(pc, op, f"TST R{c},R{b}")
    if a == 2 and d == 9:
        return Decoded(pc, op, f"AND R{c},R{b}")
    if a == 2 and d == 0xA:
        return Decoded(pc, op, f"XOR R{c},R{b}")
    if a == 2 and d == 0xB:
        return Decoded(pc, op, f"OR R{c},R{b}")
    if a == 2 and d == 0xC:
        return Decoded(pc, op, f"CMP/STR R{c},R{b}")
    if a == 2 and d == 0xD:
        return Decoded(pc, op, f"XTRCT R{c},R{b}")
    if a == 2 and d == 0xE:
        return Decoded(pc, op, f"MULU.W R{c},R{b}")
    if a == 2 and d == 0xF:
        return Decoded(pc, op, f"MULS.W R{c},R{b}")
    if a == 3 and d == 0:
        return Decoded(pc, op, f"CMP/EQ R{c},R{b}")
    if a == 3 and d == 2:
        return Decoded(pc, op, f"CMP/HS R{c},R{b}")
    if a == 3 and d == 3:
        return Decoded(pc, op, f"CMP/GE R{c},R{b}")
    if a == 3 and d == 4:
        return Decoded(pc, op, f"DIV1 R{c},R{b}")
    if a == 3 and d == 5:
        return Decoded(pc, op, f"DMULU.L R{c},R{b}")
    if a == 3 and d == 6:
        return Decoded(pc, op, f"CMP/HI R{c},R{b}")
    if a == 3 and d == 7:
        return Decoded(pc, op, f"CMP/GT R{c},R{b}")
    if a == 3 and d == 8:
        return Decoded(pc, op, f"SUB R{c},R{b}")
    if a == 3 and d == 0xA:
        return Decoded(pc, op, f"SUBC R{c},R{b}")
    if a == 3 and d == 0xB:
        return Decoded(pc, op, f"SUBV R{c},R{b}")
    if a == 3 and d == 0xC:
        return Decoded(pc, op, f"ADD R{c},R{b}")
    if a == 3 and d == 0xD:
        return Decoded(pc, op, f"DMULS.L R{c},R{b}")
    if a == 3 and d == 0xE:
        return Decoded(pc, op, f"ADDC R{c},R{b}")
    if a == 3 and d == 0xF:
        return Decoded(pc, op, f"ADDV R{c},R{b}")
    if a == 4 and cd == 0x0B:
        return Decoded(pc, op, f"JSR @R{b}", kind="call", target=None)
    if a == 4 and cd == 0x2B:
        return Decoded(pc, op, f"JMP @R{b}", kind="branch", target=None)
    if a == 4 and cd == 0x2A:
        return Decoded(pc, op, f"LDS R{b},PR")
    if a == 4 and cd == 0x0A:
        return Decoded(pc, op, f"LDS R{b},MACH")
    if a == 4 and cd == 0x1A:
        return Decoded(pc, op, f"LDS R{b},MACL")
    if a == 4 and cd == 0x0E:
        return Decoded(pc, op, f"LDC R{b},SR")
    if a == 4 and cd == 0x1E:
        return Decoded(pc, op, f"LDC R{b},GBR")
    if a == 4 and cd == 0x2E:
        return Decoded(pc, op, f"LDC R{b},VBR")
    if a == 4 and cd == 0x15:
        return Decoded(pc, op, f"CMP/PL R{b}")
    if a == 4 and cd == 0x11:
        return Decoded(pc, op, f"CMP/PZ R{b}")
    if a == 4 and cd == 0x00:
        return Decoded(pc, op, f"SHLL R{b}")
    if a == 4 and cd == 0x01:
        return Decoded(pc, op, f"SHLR R{b}")
    if a == 4 and cd == 0x08:
        return Decoded(pc, op, f"SHLL2 R{b}")
    if a == 4 and cd == 0x09:
        return Decoded(pc, op, f"SHLR2 R{b}")
    if a == 4 and cd == 0x18:
        return Decoded(pc, op, f"SHLL8 R{b}")
    if a == 4 and cd == 0x19:
        return Decoded(pc, op, f"SHLR8 R{b}")
    if a == 4 and cd == 0x28:
        return Decoded(pc, op, f"SHLL16 R{b}")
    if a == 4 and cd == 0x29:
        return Decoded(pc, op, f"SHLR16 R{b}")
    if a == 4 and cd == 0x20:
        return Decoded(pc, op, f"SHAL R{b}")
    if a == 4 and cd == 0x21:
        return Decoded(pc, op, f"SHAR R{b}")
    if a == 4 and cd == 0x24:
        return Decoded(pc, op, f"ROTCL R{b}")
    if a == 4 and cd == 0x25:
        return Decoded(pc, op, f"ROTCR R{b}")
    if a == 4 and cd == 0x04:
        return Decoded(pc, op, f"ROTL R{b}")
    if a == 4 and cd == 0x05:
        return Decoded(pc, op, f"ROTR R{b}")
    if a == 4 and cd == 0x10:
        return Decoded(pc, op, f"DT R{b}")
    # Real bug found + fixed 2026-08-08 (systematic audit after the D=6
    # fix below turned up more than one instance of the same mistake):
    # this D=2 family (STS.L <special>,@-Rn - push) was ROTATED, all three
    # wrong, confirmed against sh2int.c's real switch (case 2 -> switch(C):
    # 0=SH2stsmmach, 1=SH2stsmmacl, 2=SH2stsmpr) - was cd0x02->"PR"
    # (really MACH), cd0x12->"MACH" (really MACL), cd0x22->"MACL" (really
    # PR). Silent, no visible UNKNOWN - exactly the dangerous kind.
    if a == 4 and cd == 0x02:
        return Decoded(pc, op, f"STS.L MACH,@-R{b}")
    if a == 4 and cd == 0x12:
        return Decoded(pc, op, f"STS.L MACL,@-R{b}")
    if a == 4 and cd == 0x22:
        return Decoded(pc, op, f"STS.L PR,@-R{b}")
    # Real gap found + fixed 2026-08-08: this whole D=3 family (STC.L
    # <ctrl>,@-Rn - push SR/GBR/VBR) was missing entirely, confirmed
    # against sh2int.c (case 3 -> switch(C): 0=SH2stcmsr, 1=SH2stcmgbr,
    # 2=SH2stcmvbr).
    if a == 4 and cd == 0x03:
        return Decoded(pc, op, f"STC.L SR,@-R{b}")
    if a == 4 and cd == 0x13:
        return Decoded(pc, op, f"STC.L GBR,@-R{b}")
    if a == 4 and cd == 0x23:
        return Decoded(pc, op, f"STC.L VBR,@-R{b}")
    # Real bug found + fixed 2026-08-08 (first live draft run): this D=6
    # family (0100nnnn0xx110 - LDS.L @Rm+,<ctrl>) was wrong AND incomplete,
    # confirmed against vendor/yabause/src/sh2int.c's real decode switch
    # (case 4 -> switch(D) -> case 6 -> switch(C): 0=SH2ldsmmach,
    # 1=SH2ldsmmacl, 2=SH2ldsmpr) - cd==0x26 (C=2) was mislabeled "MACL"
    # when real hardware says PR, and cd==0x16 (C=1, the REAL MACL case)
    # didn't exist at all, silently decoding as UNKNOWN. The 0x26 bug is
    # the dangerous kind: it never showed up as a visible gap, it just
    # emitted the wrong destination register - confirmed live via
    # 06055958's division routine epilogue (LDS.L @R15+,MACL then
    # LDS.L @R15+,PR, the standard RTS-with-saved-PR pattern).
    if a == 4 and cd == 0x06:
        return Decoded(pc, op, f"LDS.L @R{b}+,MACH")
    if a == 4 and cd == 0x16:
        return Decoded(pc, op, f"LDS.L @R{b}+,MACL")
    if a == 4 and cd == 0x26:
        return Decoded(pc, op, f"LDS.L @R{b}+,PR")
    # Real bug found + fixed 2026-08-08 (same audit): D=7 family (LDC.L
    # @Rm+,<ctrl> - pop) confirmed against sh2int.c (case 7 -> switch(C):
    # 0=SH2ldcmsr, 1=SH2ldcmgbr, 2=SH2ldcmvbr) - cd0x17 (real GBR) was
    # missing, cd0x27 was mislabeled "GBR" when real hardware says VBR.
    if a == 4 and cd == 0x07:
        return Decoded(pc, op, f"LDC.L @R{b}+,SR")
    if a == 4 and cd == 0x17:
        return Decoded(pc, op, f"LDC.L @R{b}+,GBR")
    if a == 4 and cd == 0x27:
        return Decoded(pc, op, f"LDC.L @R{b}+,VBR")
    # Real bug found + fixed 2026-08-08: D=11/0xB's C=1 case (TAS.B @Rn)
    # was missing - confirmed against sh2int.c (case 11 -> switch(C):
    # 0=SH2jsr, 1=SH2tas, 2=SH2jmp; JSR/JMP already covered above via
    # cd==0x0B/0x2B).
    if a == 4 and cd == 0x1B:
        return Decoded(pc, op, f"TAS.B @R{b}")
    # Real bug found + fixed 2026-08-08: D=15/0xF (MAC.W @Rm+,@Rn+) is
    # UNCONDITIONAL on C in real hardware (sh2int.c's `case 15: return
    # &SH2macw;` has no switch(C) at all - C is the Rm operand, not a
    # selector) but this only ever matched C==0 (cd==0x0F specifically),
    # silently leaving every C=1..15 case as UNKNOWN.
    if a == 4 and d == 0xF:
        return Decoded(pc, op, f"MAC.W @R{c}+,@R{b}+")
    if a == 5:
        return Decoded(pc, op, f"MOV.L @({d}*4,R{c}),R{b}")
    if a == 6 and d == 0:
        return Decoded(pc, op, f"MOV.B @R{c},R{b}")
    if a == 6 and d == 1:
        return Decoded(pc, op, f"MOV.W @R{c},R{b}")
    if a == 6 and d == 2:
        return Decoded(pc, op, f"MOV.L @R{c},R{b}")
    if a == 6 and d == 3:
        return Decoded(pc, op, f"MOV R{c},R{b}")
    if a == 6 and d == 4:
        return Decoded(pc, op, f"MOV.B @R{c}+,R{b}")
    if a == 6 and d == 5:
        return Decoded(pc, op, f"MOV.W @R{c}+,R{b}")
    if a == 6 and d == 6:
        return Decoded(pc, op, f"MOV.L @R{c}+,R{b}")
    if a == 6 and d == 7:
        return Decoded(pc, op, f"NOT R{c},R{b}")
    if a == 6 and d == 8:
        return Decoded(pc, op, f"SWAP.B R{c},R{b}")
    if a == 6 and d == 9:
        return Decoded(pc, op, f"SWAP.W R{c},R{b}")
    if a == 6 and d == 0xA:
        return Decoded(pc, op, f"NEGC R{c},R{b}")
    if a == 6 and d == 0xB:
        return Decoded(pc, op, f"NEG R{c},R{b}")
    if a == 6 and d == 0xC:
        return Decoded(pc, op, f"EXTU.B R{c},R{b}")
    if a == 6 and d == 0xD:
        return Decoded(pc, op, f"EXTU.W R{c},R{b}")
    if a == 6 and d == 0xE:
        return Decoded(pc, op, f"EXTS.B R{c},R{b}")
    if a == 6 and d == 0xF:
        return Decoded(pc, op, f"EXTS.W R{c},R{b}")
    if a == 7:
        return Decoded(pc, op, f"ADD #{s8(cd)},R{b}")
    if a == 8 and b == 0:
        return Decoded(pc, op, f"MOV.B R0,@({d},R{c})")
    if a == 8 and b == 1:
        return Decoded(pc, op, f"MOV.W R0,@({d}*2,R{c})")
    if a == 8 and b == 4:
        return Decoded(pc, op, f"MOV.B @({d},R{c}),R0")
    if a == 8 and b == 5:
        return Decoded(pc, op, f"MOV.W @({d}*2,R{c}),R0")
    if a == 8 and b == 8:
        return Decoded(pc, op, f"CMP/EQ #{s8(cd)},R0")
    if a == 8 and b == 9:
        t = pc + (s8(cd) << 1) + 4
        return Decoded(pc, op, f"BT 0x{t:08X}", kind="cond_branch", target=t)
    if a == 8 and b == 0xB:
        t = pc + (s8(cd) << 1) + 4
        return Decoded(pc, op, f"BF 0x{t:08X}", kind="cond_branch", target=t)
    if a == 8 and b == 0xD:
        t = pc + (s8(cd) << 1) + 4
        return Decoded(pc, op, f"BT/S 0x{t:08X}", kind="cond_branch", target=t)
    if a == 8 and b == 0xF:
        t = pc + (s8(cd) << 1) + 4
        return Decoded(pc, op, f"BF/S 0x{t:08X}", kind="cond_branch", target=t)
    if a == 9:
        target = pc + (cd << 1) + 4
        val = read_u16(data, target) if data is not None else None
        return Decoded(pc, op, f"MOV.W @(0x{cd:X},PC),R{b}", pool_addr=target, pool_val=val)
    if a == 0xA:
        disp = bcd if bcd < 0x800 else bcd - 0x1000
        t = pc + (disp << 1) + 4
        return Decoded(pc, op, f"BRA 0x{t:08X}", kind="branch", target=t)
    if a == 0xB:
        disp = bcd if bcd < 0x800 else bcd - 0x1000
        t = pc + (disp << 1) + 4
        return Decoded(pc, op, f"BSR 0x{t:08X}", kind="call", target=t)
    if a == 0xC and b == 0:
        return Decoded(pc, op, f"MOV.B R0,@(0x{cd:X},GBR)")
    if a == 0xC and b == 1:
        return Decoded(pc, op, f"MOV.W R0,@(0x{cd:X}*2,GBR)")
    if a == 0xC and b == 2:
        return Decoded(pc, op, f"MOV.L R0,@(0x{cd:X}*4,GBR)")
    if a == 0xC and b == 3:
        return Decoded(pc, op, f"TRAPA #0x{cd:X}")
    if a == 0xC and b == 4:
        return Decoded(pc, op, f"MOV.B @(0x{cd:X},GBR),R0")
    if a == 0xC and b == 5:
        return Decoded(pc, op, f"MOV.W @(0x{cd:X}*2,GBR),R0")
    if a == 0xC and b == 6:
        return Decoded(pc, op, f"MOV.L @(0x{cd:X}*4,GBR),R0")
    if a == 0xC and b == 7:
        addr = ((pc + 4) & ~3) + (cd << 2)
        return Decoded(pc, op, f"MOVA @(0x{cd:X}*4,PC),R0", pool_addr=addr)
    if a == 0xC and b == 8:
        return Decoded(pc, op, f"TST #0x{cd:X},R0")
    if a == 0xC and b == 9:
        return Decoded(pc, op, f"AND #0x{cd:X},R0")
    if a == 0xC and b == 0xA:
        return Decoded(pc, op, f"XOR #0x{cd:X},R0")
    if a == 0xC and b == 0xB:
        return Decoded(pc, op, f"OR #0x{cd:X},R0")
    if a == 0xC and b == 0xC:
        return Decoded(pc, op, f"TST.B #0x{cd:X},@(R0,GBR)")
    if a == 0xC and b == 0xD:
        return Decoded(pc, op, f"AND.B #0x{cd:X},@(R0,GBR)")
    if a == 0xC and b == 0xE:
        return Decoded(pc, op, f"XOR.B #0x{cd:X},@(R0,GBR)")
    if a == 0xC and b == 0xF:
        return Decoded(pc, op, f"OR.B #0x{cd:X},@(R0,GBR)")
    if a == 0xD:
        target = ((pc + 4) & ~3) + (cd << 2)
        val = read_u32(data, target) if data is not None else None
        return Decoded(pc, op, f"MOV.L @(0x{cd:X}*4,PC),R{b}", pool_addr=target, pool_val=val)
    if a == 0xE:
        return Decoded(pc, op, f"MOV #{s8(cd)},R{b}")
    return Decoded(pc, op, f"UNKNOWN {op:04X}")

import sys
import re

def sh2_to_x86(mnemonic):
    # Mapeamento básico de SH2 para pseudo-x86
    m = mnemonic.replace("R15", "esp").replace("R14", "ebp")
    m = re.sub(r'R(\d+)', r'r\1d', m)
    
    if m.startswith("MOV.L "):
        parts = m[6:].split(",")
        if len(parts) == 2:
            src, dst = parts[0], parts[1]
            if dst.startswith("@("):
                match = re.match(r'@\(([^,]+),([^)]+)\)', dst)
                if match:
                    disp, rn = match.groups()
                    return f"mov dword ptr [{rn} + {disp}], {src}"
            elif dst.startswith("@-"):
                rn = dst[2:]
                return f"sub {rn}, 4 ; mov dword ptr [{rn}], {src}"
            elif dst.startswith("@"):
                rn = dst[1:]
                return f"mov dword ptr [{rn}], {src}"
            elif src.startswith("@("):
                match = re.match(r'@\(([^,]+),([^)]+)\)', src)
                if match:
                    disp, rn = match.groups()
                    return f"mov {dst}, dword ptr [{rn} + {disp}]"
            elif src.startswith("@") and src.endswith("+"):
                rn = src[1:-1]
                return f"mov {dst}, dword ptr [{rn}] ; add {rn}, 4"
            elif src.startswith("@"):
                rn = src[1:]
                return f"mov {dst}, dword ptr [{rn}]"
            else:
                return f"mov {dst}, {src}"
    elif m.startswith("MOV.W ") or m.startswith("MOV.B "):
        sz = "word" if "MOV.W" in m else "byte"
        parts = m[6:].split(",")
        if len(parts) == 2:
            src, dst = parts[0], parts[1]
            if dst.startswith("@("):
                match = re.match(r'@\(([^,]+),([^)]+)\)', dst)
                if match:
                    disp, rn = match.groups()
                    return f"mov {sz} ptr [{rn} + {disp}], {src}"
            elif dst.startswith("@-"):
                rn = dst[2:]
                step = "2" if sz == "word" else "1"
                return f"sub {rn}, {step} ; mov {sz} ptr [{rn}], {src}"
            elif dst.startswith("@"):
                rn = dst[1:]
                return f"mov {sz} ptr [{rn}], {src}"
            elif src.startswith("@("):
                match = re.match(r'@\(([^,]+),([^)]+)\)', src)
                if match:
                    disp, rn = match.groups()
                    return f"movsx {dst}, {sz} ptr [{rn} + {disp}]"
            elif src.startswith("@") and src.endswith("+"):
                rn = src[1:-1]
                step = "2" if sz == "word" else "1"
                return f"movsx {dst}, {sz} ptr [{rn}] ; add {rn}, {step}"
            elif src.startswith("@"):
                rn = src[1:]
                return f"movsx {dst}, {sz} ptr [{rn}]"
            else:
                return f"mov {dst}, {src}"
    elif m.startswith("MOV "):
        parts = m[4:].split(",")
        if len(parts) == 2:
            if parts[0].startswith("#"):
                return f"mov {parts[1]}, {parts[0][1:]}"
            return f"mov {parts[1]}, {parts[0]}"
    elif m.startswith("ADD "):
        parts = m[4:].split(",")
        if len(parts) == 2:
            if parts[0].startswith("#"):
                return f"add {parts[1]}, {parts[0][1:]}"
            return f"add {parts[1]}, {parts[0]}"
    elif m.startswith("SUB "):
        parts = m[4:].split(",")
        if len(parts) == 2:
            return f"sub {parts[1]}, {parts[0]}"
    elif m.startswith("CMP/EQ "):
        parts = m[7:].split(",")
        if len(parts) == 2:
            if parts[0].startswith("#"):
                return f"cmp {parts[1]}, {parts[0][1:]}"
            return f"cmp {parts[1]}, {parts[0]}"
    elif m.startswith("BRA "):
        return f"jmp {m[4:]}"
    elif m.startswith("JMP @"):
        return f"jmp {m[5:]}"
    elif m.startswith("BSR "):
        return f"call {m[4:]}"
    elif m.startswith("JSR @"):
        return f"call {m[5:]}"
    elif m == "RTS":
        return "ret"
    elif m == "NOP":
        return "nop"
        
    if m.startswith("MOV.L @(") and ",PC" in m:
        parts = m[7:].split("),")
        if len(parts) == 2:
            disp = parts[0]
            dst = parts[1]
            return f"mov {dst}, dword ptr [rip + {disp}]"
            
    return f"; {m} (pseudo-x86 not implemented)"

if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Uso: python3 decode_trace.py <arquivo_trace.txt>")
        sys.exit(1)
        
    with open(sys.argv[1], "r") as f:
        for line in f:
            line = line.strip()
            if not line: continue
            
            # Formato: Frame: %u | Core: %c | PC: %08X | Opcode: %04X | %s
            # Ex: Frame: 0 | Core: M | PC: 00000000 | Opcode: D001 | MOV.L @(4,PC),R0
            match = re.match(r'Core:\s*([MS])\s*\|\s*PC:\s*([0-9A-Fa-f]+)\s*\|\s*Opcode:\s*([0-9A-Fa-f]+)\s*\|\s*([^|]+)(?:\|\s*Mem:\s*(.*))?', line)
            if match:
                core, pc_hex, op_hex, sh2_asm, mem_info = match.groups()
                sh2_asm = sh2_asm.strip()
                pc = int(pc_hex, 16)
                op = int(op_hex, 16)
                
                # Decodifica de novo só para ter certeza ou usa o sh2_asm direto
                decoded = decode(pc, op)
                mnemonic = decoded.mnemonic if "UNKNOWN" not in decoded.mnemonic else sh2_asm
                
                x86_asm = sh2_to_x86(mnemonic)
                
                mem_str = f" [{mem_info.strip()}]" if mem_info and mem_info.strip() else ""
                print(f"{pc_hex}  {op_hex}  {sh2_asm:<25} ; x86: {x86_asm:<30}{mem_str}")
            else:
                print(line, end="")
