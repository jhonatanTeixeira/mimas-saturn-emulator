with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

text = text.replace("vram[10] = 0x00; vram[11] = 0x01; // 8x1", "vram[10] = 0x01; vram[11] = 0x01; // 8x1")
with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

