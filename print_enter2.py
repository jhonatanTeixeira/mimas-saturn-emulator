with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

text = text.replace(
    "let current_shape = cmd.cmdctrl & 0x7;",
    "let current_shape = cmd.cmdctrl & 0x7;\n    if current_shape == 2 { println!(\"Entering Distorted Sprite! CMDCTRL={:04X} CMDSRCA={:04X}\", cmd.cmdctrl, cmd.cmdsrca); }"
)

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

