with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

import re

text = re.sub(
    r'(// draw distorted sprite\s*\{.*?)state\.addr = 0;',
    r'\g<1>{\n            let mut cram = ram.vdp1_cram.write().unwrap();\n            cram[30] = 0xFF;\n            cram[31] = 0xFF;\n        }\n        state.addr = 0;',
    text, flags=re.DOTALL
)

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

