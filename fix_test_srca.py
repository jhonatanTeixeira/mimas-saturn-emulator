with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

text = text.replace(
    "vram[6] = 0x7C; vram[7] = 0x00; // CMDCOLR = 0x7C00 (Red)\n            vram[10] = 0x00; vram[11] = 0x01; // 8x1",
    "vram[6] = 0x7C; vram[7] = 0x00;\n            vram[8] = 0x00; vram[9] = 0x10;\n            vram[10] = 0x00; vram[11] = 0x01;"
)

text = text.replace(
    "vram[6] = 0xFF; vram[7] = 0xFF; // CMDCOLR\n            vram[10] = 0x00; vram[11] = 0x01; // 8x1",
    "vram[6] = 0xFF; vram[7] = 0xFF;\n            vram[8] = 0x00; vram[9] = 0x10;\n            vram[10] = 0x00; vram[11] = 0x01;"
)

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

