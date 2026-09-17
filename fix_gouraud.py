with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

import re
tests = [
    "vdp1_gouraud_darkens_red",
    "vdp1_gouraud_index_special_case",
    "vdp1_gouraud_neutral_table_is_identity",
    "vdp1_gouraud_clamps",
    "vdp1_gouraud_table_only_fetched_when_bit2_set",
    "vdp1_mesh_stipples",
    "vdp1_msb_on_ors_existing_pixel"
]

for test in tests:
    match = re.search(r'fn ' + test + r'\(\) \{.*?(?=#\[test\]|\n})', text, flags=re.DOTALL)
    if match:
        body = match.group(0)
        # set COMM to 4 (Polygon)
        body = re.sub(r'vram\[1\]\s*=\s*0x00;', r'vram[1] = 0x04;', body)
        text = text[:match.start()] + body + text[match.end():]

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

