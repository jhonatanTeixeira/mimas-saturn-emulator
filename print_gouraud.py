with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

import re
match = re.search(r'fn vdp1_gouraud_neutral_table_is_identity\(\) \{.*?(?=#\[test\]|\n})', text, flags=re.DOTALL)
if match:
    body = match.group(0)
    body = body.replace(
        "assert_eq!(left, right);",
        "println!(\"left: {} right: {} fb[0]: {:04X}\", left, right, p0);\n        assert_eq!(left, right);"
    )
    # actually let's just let it panic and we can see what fb[0] is.
    # We already know fb[0] is 32239 (0x7DEF).

