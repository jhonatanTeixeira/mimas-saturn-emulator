with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

import re
text = text.replace(
    "let cmd = parse_cmd(&vram, state.addr as usize);",
    "let cmd = parse_cmd(&vram, state.addr as usize);\n            println!(\"Parsing command at addr: {} CMDCTRL: {:04X} fake_draw={}\", state.addr, cmd.cmdctrl, fake_draw);"
)

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

