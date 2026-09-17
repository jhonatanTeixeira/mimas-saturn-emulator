import re
with open("saturn-core/src/vdp2.rs", "r") as f:
    text = f.read()

text = re.sub(
    r'fetch_pixel\(\s*(0),\s*(0x9999),\s*(0),\s*(0),\s*(0),\s*(1),\s*(2),\s*(false),\s*(0),\s*(0),\s*(8),\s*&vram,\s*&cram\s*\)',
    r'fetch_pixel((\1, \2), \3, \4, \5, 8, &Vdp2LayerConfig { patternwh: \6, colornumber: \7, transparencyenable: false, coloroffset: 0, cram_mode: 0, mpofn: 0, mpab: 0, mpcd: 0, patterndatasize: 0, planew: 0, planeh: 0, vram_8mbit: false, mapwh: 0, supplementdata: 0, auxmode: 0 }, (&vram, &cram))',
    text
)

with open("saturn-core/src/vdp2.rs", "w") as f:
    f.write(text)

