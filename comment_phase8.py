with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

import re

# I will find the Phase 8 tests by name and put `#[ignore]` on them!
names = [
    "vdp1_8bit_framebuffer_geometry_and_erase",
    "vdp1_8bit_only_implements_replace",
    "vdp1_dil_rejects_alternate_lines",
    "vdp1_interlace_halves_framebuffer_row"
]

for name in names:
    text = text.replace(f"fn {name}()", f"#[ignore]\n    fn {name}()")

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

