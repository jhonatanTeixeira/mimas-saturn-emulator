with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

text = text.replace(
    "vram[10] = 0x00;\n            vram[11] = 0x00; // 8x1 (1 char)",
    "vram[10] = 0x01;\n            vram[11] = 0x01; // 8x1 (1 char)"
)

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

