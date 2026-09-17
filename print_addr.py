with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

import re
text = re.sub(
    r'(execute_vdp1\(&mut state, &ram\);.*?let fb2 =)',
    r'println!("Before second execute, addr={} edsr={}", state.addr, state.edsr);\n        \1',
    text,
    flags=re.DOTALL
)

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

