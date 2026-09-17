with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

import re

# find `state.addr = 0;` before the second execute_vdp1 and add `state.edsr = 0;`
text = re.sub(
    r'(state\.addr = 0;\s*execute_vdp1\(&mut state, &ram\);)',
    r'state.edsr = 0;\n        \1',
    text
)

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

