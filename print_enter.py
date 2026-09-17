with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

import re
text = text.replace("let untextured = current_shape == 4 || current_shape == 5;", 
                    "let untextured = current_shape == 4 || current_shape == 5;\n                if current_shape == 2 { println!(\"Entering Distorted Sprite! CMDCTRL={:04X} CMDSRCA={:04X}\", cmd.cmdctrl, cmd.cmdsrca); }")

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

