with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

import re

# find `state.edsr = 0;` and append `state.copr = 0;` and `state.status = Vdp1Status::IDLE;`
text = re.sub(
    r'state\.edsr = 0;\n\s*state\.addr = 0;',
    r'state.edsr = 0;\n        state.copr = 0;\n        state.status = Vdp1Status::IDLE;\n        state.addr = 0;',
    text
)

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

