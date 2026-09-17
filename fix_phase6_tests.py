with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

text = text.replace("vram[0x20] = 0x80; vram[0x21] = 0x00;\n        }", "vram[0x20] = 0x80; vram[0x21] = 0x00;\n            for i in 0x80..0xC0 { vram[i] = 0xFF; }\n        }")

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

