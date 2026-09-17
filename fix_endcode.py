with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

text = text.replace("vram[4] = 0x00; vram[5] = 0x23; // Mode 3, ECD clear", "vram[4] = 0x00; vram[5] = 0x18; // Mode 3, ECD clear")

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

