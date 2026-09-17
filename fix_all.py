import re

with open("saturn-core/src/cs2.rs", "r") as f:
    text = f.read()
text = text.replace("cs2.execute_command(cmd, &[0, 1, 2, 3], &mut resp);", "cs2.host_cr[0] = cmd; cs2.execute_command();")
with open("saturn-core/src/cs2.rs", "w") as f:
    f.write(text)

with open("saturn-core/src/scu_dsp.rs", "r") as f:
    text = f.read()
text = text.replace("dsp.prog_ram[0]", "dsp.program_ram[0]")
with open("saturn-core/src/scu_dsp.rs", "w") as f:
    f.write(text)

with open("saturn-core/src/vdp2_regs.rs", "r") as f:
    text = f.read()
text = text.replace("let _ = read_vdp2_reg(addr, &ram);", "")
text = text.replace("write_vdp2_reg(addr, addr as u16, &ram);", "let mut regs = Vdp2Registers::new(); regs.write_word(addr, addr as u16);")
with open("saturn-core/src/vdp2_regs.rs", "w") as f:
    f.write(text)

