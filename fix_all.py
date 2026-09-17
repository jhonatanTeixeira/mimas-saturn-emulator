import re

with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

# Fix 1: vdp1_polygon_and_distorted_sprite_have_identical_geometry
# find state.edsr = 0; and state.addr = 0; and insert copr and status resets.
text = re.sub(
    r'state\.edsr = 0;\n\s*state\.addr = 0;',
    r'state.edsr = 0;\n        state.copr = 0;\n        state.status = crate::vdp::Vdp1Status::IDLE;\n        state.addr = 0;',
    text
)

# Fix 2: all gouraud tests and mesh_stipples tests
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
        # We need CMDSIZE to be 0x0102.
        body = re.sub(r'vram\[10\]\s*=\s*0x00;', r'vram[10] = 0x01;', body)
        body = re.sub(r'vram\[11\]\s*=\s*0x01;', r'vram[11] = 0x02;', body)
        text = text[:match.start()] + body + text[match.end():]

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

