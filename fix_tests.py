import re

with open("saturn-core/src/vdp2.rs", "r") as f:
    text = f.read()

# Update fetch_pixel in tests
text = re.sub(
    r'fetch_pixel\(\s*(0x100),\s*(0x20),\s*(\d+),\s*(\d+),\s*(\d+),\s*(1),\s*(0),\s*(true|false),\s*(0|0x100),\s*(2),\s*(8),\s*&vram,\s*&cram\s*\)',
    r'fetch_pixel((0x100, 0x20), \3, \4, \5, 8, &Vdp2LayerConfig { patternwh: \6, colornumber: \7, transparencyenable: \8, coloroffset: \9, cram_mode: \10, mpofn: 0, mpab: 0, mpcd: 0, patterndatasize: 0, planew: 0, planeh: 0, vram_8mbit: false, mapwh: 0, supplementdata: 0, auxmode: 0 }, (&vram, &cram))',
    text
)

text = re.sub(
    r'pattern_addr\(\s*(0x\w+|\w+),\s*(0x\w+|\w+),\s*(0),\s*(0),\s*(1),\s*(2),\s*(0),\s*(true)\s*\)',
    r'pattern_addr(\1, \2, &Vdp2LayerConfig { patternwh: \5, colornumber: \7, transparencyenable: false, coloroffset: 0, cram_mode: 0, mpofn: 0, mpab: 0, mpcd: 0, patterndatasize: \6, planew: 0, planeh: 0, vram_8mbit: \8, mapwh: 0, supplementdata: \3, auxmode: \4 })',
    text
)

with open("saturn-core/src/vdp2.rs", "w") as f:
    f.write(text)

