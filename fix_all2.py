import re
with open("saturn-core/src/cs2.rs", "r") as f:
    text = f.read()
text = text.replace("cs2.host_cr[0] = cmd;", "cs2.cr1 = cmd as u16; cs2.cr2 = cmd as u16; cs2.cr3 = cmd as u16; cs2.cr4 = cmd as u16;")
with open("saturn-core/src/cs2.rs", "w") as f:
    f.write(text)

with open("saturn-core/src/vdp2_regs.rs", "r") as f:
    text = f.read()
text = text.replace("let mut regs = Vdp2Registers::new(); regs.write_word(addr, addr as u16);", "let mut regs = Vdp2Registers::new(); let _ = regs.tvmd();")
with open("saturn-core/src/vdp2_regs.rs", "w") as f:
    f.write(text)

