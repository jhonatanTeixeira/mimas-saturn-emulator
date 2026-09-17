with open("saturn-core/src/m68k.rs", "r") as f:
    text = f.read()

text = text.replace("if (opcode & 0x01F0) == 0x0140 {", "if (opcode & 0x01F8) == 0x0140 {")
text = text.replace("if (opcode & 0x01F0) == 0x0148 {", "if (opcode & 0x01F8) == 0x0148 {")
text = text.replace("if (opcode & 0x01F0) == 0x0188 {", "if (opcode & 0x01F8) == 0x0188 {")
text = text.replace("if (opcode & 0xFFF0) == 0x4E60 || (opcode & 0xFFF0) == 0x4E68", "if (opcode & 0xFFF8) == 0x4E60 || (opcode & 0xFFF8) == 0x4E68")
text = text.replace("if disp8 == 0 { 0 } else { 0 }", "0")

with open("saturn-core/src/m68k.rs", "w") as f:
    f.write(text)

with open("saturn-core/src/scu_dsp.rs", "r") as f:
    text = f.read()
text = text.replace("let instr = (opcode * 12345);", "let instr = opcode * 12345;")
with open("saturn-core/src/scu_dsp.rs", "w") as f:
    f.write(text)
