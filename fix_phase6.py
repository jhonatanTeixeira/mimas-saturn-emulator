with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

import re
# We need to change vram[10] = 0x00; vram[11] = 0x01; to vram[10] = 0x01; vram[11] = 0x01;
# We can just look for functions that failed and fix them.
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
    # find fn name
    match = re.search(r'fn ' + test + r'\(\) \{.*?(?=#\[test\]|\n})', text, flags=re.DOTALL)
    if match:
        body = match.group(0)
        # replace vram[10] = 0x00; with vram[10] = 0x01;
        body = re.sub(r'vram\[10\]\s*=\s*0x00;', r'vram[10] = 0x01;', body)
        # if the test is mesh_stipples, it uses COMM 0 but CMDXA/CMDYA are 0.
        # it needs vram[11] = 0x01 as well, which is 1 pixel high.
        # let's just make sure CMDSIZE is 0x0101
        
        text = text[:match.start()] + body + text[match.end():]

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

