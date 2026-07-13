#!/usr/bin/env python3
import sys

def sext(v, bits):
    m = 1 << (bits - 1)
    return (v ^ m) - m

def disasm(pc, op):
    n = (op >> 8) & 0xF
    m = (op >> 4) & 0xF
    d4 = op & 0xF
    d8 = op & 0xFF
    d12 = op & 0xFFF
    imm8 = op & 0xFF

    if op == 0x0009: return "NOP"
    if op == 0x000B: return "RTS"
    if op == 0x0018: return "SETT"
    if op == 0x0008: return "CLRT"
    if op == 0x0019: return "DIV0U"
    if op == 0x0028: return "CLRMAC"
    if op == 0x002B: return "RTE"
    if op == 0xFFFF: return "ILLEGAL"

    if (op & 0xFF00) == 0xC300:
        return f"TRAPA #{imm8:#x}"

    f0f = op & 0xF0FF
    tbl_stc = {
        0x0002: f"STC SR,R{n}", 0x0012: f"STC GBR,R{n}", 0x0022: f"STC VBR,R{n}",
        0x000A: f"STS MACH,R{n}", 0x001A: f"STS MACL,R{n}", 0x002A: f"STS PR,R{n}",
        0x0029: f"MOVT R{n}",
    }
    if f0f in tbl_stc: return tbl_stc[f0f]

    f00f = op & 0xF00F
    tbl_f00f_a = {
        0x0004: f"MOV.B R{m},@(R0,R{n})", 0x0005: f"MOV.W R{m},@(R0,R{n})",
        0x0006: f"MOV.L R{m},@(R0,R{n})", 0x0007: f"MUL.L R{m},R{n}",
        0x000C: f"MOV.B @(R0,R{m}),R{n}", 0x000D: f"MOV.W @(R0,R{m}),R{n}",
        0x000E: f"MOV.L @(R0,R{m}),R{n}",
    }
    if f00f in tbl_f00f_a: return tbl_f00f_a[f00f]

    f000 = op & 0xF000
    if f000 == 0x1000: return f"MOV.L R{m},@({d4},R{n})"
    if f000 == 0x5000: return f"MOV.L @({d4},R{m}),R{n}"
    if f000 == 0x7000: return f"ADD #{sext(imm8,8)},R{n}"
    if f000 == 0x9000:
        addr = ((pc + 4) & ~1) + d8 * 2
        return f"MOV.W @({d8:#x},PC),R{n}  ; -> {addr:#010x}"
    if f000 == 0xA000:
        target = (pc + 4) + (sext(d12, 12) << 1)
        return f"BRA {target:#010x}"
    if f000 == 0xB000:
        target = (pc + 4) + (sext(d12, 12) << 1)
        return f"BSR {target:#010x}"
    if f000 == 0xD000:
        addr = ((pc + 4) & ~3) + d8 * 4
        return f"MOV.L @({d8:#x},PC),R{n}  ; -> {addr:#010x}"
    if f000 == 0xE000: return f"MOV #{sext(imm8,8)},R{n}"

    ff00 = op & 0xFF00
    if ff00 == 0x8800: return f"CMP/EQ #{sext(imm8,8)},R0"
    if ff00 == 0x8900:
        target = (pc + 4) + (sext(d8, 8) << 1)
        return f"BT {target:#010x}"
    if ff00 == 0x8B00:
        target = (pc + 4) + (sext(d8, 8) << 1)
        return f"BF {target:#010x}"
    if ff00 == 0x8D00:
        target = (pc + 4) + (sext(d8, 8) << 1)
        return f"BT/S {target:#010x}"
    if ff00 == 0x8F00:
        target = (pc + 4) + (sext(d8, 8) << 1)
        return f"BF/S {target:#010x}"
    if ff00 == 0xC800: return f"TST #{imm8:#x},R0"
    if ff00 == 0xC900: return f"AND #{imm8:#x},R0"
    if ff00 == 0xCA00: return f"OR #{imm8:#x},R0"
    if ff00 == 0xCB00: return f"XOR #{imm8:#x},R0"
    if ff00 == 0xC700:
        addr = ((pc + 4) & ~3) + d8 * 4
        return f"MOVA @({d8:#x},PC),R0  ; -> {addr:#010x}"
    if ff00 == 0xC000: return f"MOV.B R0,@({d8:#x},GBR)"
    if ff00 == 0xC100: return f"MOV.W R0,@({d8:#x},GBR)"
    if ff00 == 0xC200: return f"MOV.L R0,@({d8:#x},GBR)"
    if ff00 == 0xC400: return f"MOV.B @({d8:#x},GBR),R0"
    if ff00 == 0xC500: return f"MOV.W @({d8:#x},GBR),R0"
    if ff00 == 0xC600: return f"MOV.L @({d8:#x},GBR),R0"

    tbl_f0ff_b = {
        0x4000: f"SHLL R{n}", 0x4001: f"SHLR R{n}", 0x4004: f"ROTL R{n}", 0x4005: f"ROTR R{n}",
        0x4008: f"SHLL2 R{n}", 0x4009: f"SHLR2 R{n}", 0x400B: f"JSR @R{n}",
        0x4010: f"DT R{n}", 0x4011: f"CMP/PZ R{n}", 0x4015: f"CMP/PL R{n}",
        0x4018: f"SHLL8 R{n}", 0x4019: f"SHLR8 R{n}", 0x401B: f"TAS.B @R{n}",
        0x4020: f"SHAL R{n}", 0x4021: f"SHAR R{n}", 0x4024: f"ROTCL R{n}", 0x4025: f"ROTCR R{n}",
        0x4028: f"SHLL16 R{n}", 0x4029: f"SHLR16 R{n}", 0x402B: f"JMP @R{n}",
        0x400E: f"LDC R{n},SR", 0x401E: f"LDC R{n},GBR", 0x402E: f"LDC R{n},VBR",
        0x400A: f"LDS R{n},MACH", 0x401A: f"LDS R{n},MACL", 0x402A: f"LDS R{n},PR",
        0x4006: f"LDS.L @R{n}+,MACH", 0x4016: f"LDS.L @R{n}+,MACL", 0x4026: f"LDS.L @R{n}+,PR",
        0x4002: f"STS.L MACH,@-R{n}", 0x4012: f"STS.L MACL,@-R{n}", 0x4022: f"STS.L PR,@-R{n}",
        0x4007: f"LDC.L @R{n}+,SR", 0x4017: f"LDC.L @R{n}+,GBR", 0x4027: f"LDC.L @R{n}+,VBR",
        0x4003: f"STC.L SR,@-R{n}", 0x4013: f"STC.L GBR,@-R{n}", 0x4023: f"STC.L VBR,@-R{n}",
    }
    if f0f in tbl_f0ff_b: return tbl_f0ff_b[f0f]

    tbl_f00f_c = {
        0x2000: f"MOV.B R{m},@R{n}", 0x2001: f"MOV.W R{m},@R{n}", 0x2002: f"MOV.L R{m},@R{n}",
        0x2004: f"MOV.B R{m},@-R{n}", 0x2005: f"MOV.W R{m},@-R{n}", 0x2006: f"MOV.L R{m},@-R{n}",
        0x2007: f"DIV0S R{m},R{n}", 0x2008: f"TST R{m},R{n}", 0x2009: f"AND R{m},R{n}",
        0x200A: f"XOR R{m},R{n}", 0x200B: f"OR R{m},R{n}", 0x200C: f"CMP/STR R{m},R{n}",
        0x200D: f"XTRCT R{m},R{n}", 0x200E: f"MULU.W R{m},R{n}", 0x200F: f"MULS.W R{m},R{n}",
        0x3000: f"CMP/EQ R{m},R{n}", 0x3002: f"CMP/HS R{m},R{n}", 0x3003: f"CMP/GE R{m},R{n}",
        0x3004: f"DIV1 R{m},R{n}", 0x3006: f"CMP/HI R{m},R{n}", 0x3007: f"CMP/GT R{m},R{n}",
        0x3008: f"SUB R{m},R{n}", 0x300A: f"SUBC R{m},R{n}", 0x300B: f"SUBV R{m},R{n}",
        0x300C: f"ADD R{m},R{n}", 0x300D: f"DMULS.L R{m},R{n}", 0x300E: f"ADDC R{m},R{n}",
        0x300F: f"ADDV R{m},R{n}", 0x3005: f"DMULU.L R{m},R{n}",
        0x6000: f"MOV.B @R{m},R{n}", 0x6001: f"MOV.W @R{m},R{n}", 0x6002: f"MOV.L @R{m},R{n}",
        0x6003: f"MOV R{m},R{n}", 0x6004: f"MOV.B @R{m}+,R{n}", 0x6005: f"MOV.W @R{m}+,R{n}",
        0x6006: f"MOV.L @R{m}+,R{n}", 0x6007: f"NOT R{m},R{n}", 0x6008: f"SWAP.B R{m},R{n}",
        0x6009: f"SWAP.W R{m},R{n}", 0x600A: f"NEGC R{m},R{n}", 0x600B: f"NEG R{m},R{n}",
        0x600E: f"EXTS.B R{m},R{n}", 0x600F: f"EXTS.W R{m},R{n}",
        0x600C: f"EXTU.B R{m},R{n}", 0x600D: f"EXTU.W R{m},R{n}",
    }
    if f00f in tbl_f00f_c: return tbl_f00f_c[f00f]

    if ff00 == 0x8000: return f"MOV.B R0,@({d4},R{m})"
    if ff00 == 0x8100: return f"MOV.W R0,@({d4},R{m})"
    if ff00 == 0x8400: return f"MOV.B @({d4},R{m}),R0"
    if ff00 == 0x8500: return f"MOV.W @({d4},R{m}),R0"

    return f"??? {op:#06x}"


def main():
    path = sys.argv[1]
    base = int(sys.argv[2], 0) if len(sys.argv) > 2 else 0x06028000
    with open(path, "rb") as f:
        data = f.read()
    for i in range(0, len(data), 2):
        pc = base + i
        op = (data[i] << 8) | data[i+1]
        print(f"{pc:#010x}: {op:#06x}  {disasm(pc, op)}")

if __name__ == "__main__":
    main()
