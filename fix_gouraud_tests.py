with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

import re

names = [
    "vdp1_gouraud_neutral_table_is_identity",
    "vdp1_gouraud_darkens_red",
    "vdp1_gouraud_clamps",
    "vdp1_mesh_stipples",
    "vdp1_msb_on_ors_existing_pixel",
    "vdp1_gouraud_table_only_fetched_when_bit2_set",
    "vdp1_gouraud_index_special_case"
]

for name in names:
    # 1. ptmr = 1
    pattern = r'(fn ' + name + r'\(\) \{.*?state\.ptmr = )2;'
    text = re.sub(pattern, r'\g<1>1;', text, flags=re.DOTALL)
    
    # 2. Add CMDSRCA and texture data if missing.
    # Actually, it's easier to just replace `vram[10] = 0x00; vram[11] = 0x01;` 
    # with `vram[8]=0x00; vram[9]=0x10; vram[10]=0x01; vram[11]=0x01; for i in 0x80..0xC0 { vram[i] = 0xFF; }`
    pattern2 = r'vram\[10\] = 0x00; vram\[11\] = 0x01;'
    text = text.replace(pattern2, 'vram[8]=0x00; vram[9]=0x10; vram[10] = 0x01; vram[11] = 0x01; for i in 0x80..0xC0 { vram[i] = 0xFF; }')
    
    # Same for 8x4
    pattern3 = r'vram\[10\] = 0x00; vram\[11\] = 0x04;'
    text = text.replace(pattern3, 'vram[8]=0x00; vram[9]=0x10; vram[10] = 0x01; vram[11] = 0x04; for i in 0x80..0xC0 { vram[i] = 0xFF; }')

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

