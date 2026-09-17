import re

with open("saturn-core/src/vdp2.rs", "r") as f:
    text = f.read()

text = text.replace(
    "pub fn fetch_pixel(charaddr: u32, paladdr: u32, mut x: u32, mut y: u32, flipfunction: u16, cellw: u32, cfg: &Vdp2LayerConfig, vram: &[u8], cram: &[u8])",
    "pub fn fetch_pixel(addr: (u32, u32), mut x: u32, mut y: u32, flipfunction: u16, cellw: u32, cfg: &Vdp2LayerConfig, mem: (&[u8], &[u8]))"
)
text = text.replace("let charaddr = addr.0; let paladdr = addr.1; let vram = mem.0; let cram = mem.1;", "") # in case it exists
text = text.replace("pub fn fetch_pixel(addr: (u32, u32), mut x: u32, mut y: u32, flipfunction: u16, cellw: u32, cfg: &Vdp2LayerConfig, mem: (&[u8], &[u8])) {", "pub fn fetch_pixel(addr: (u32, u32), mut x: u32, mut y: u32, flipfunction: u16, cellw: u32, cfg: &Vdp2LayerConfig, mem: (&[u8], &[u8])) {\n    let (charaddr, paladdr) = addr;\n    let (vram, cram) = mem;")

with open("saturn-core/src/vdp2.rs", "w") as f:
    f.write(text)

with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

text = text.replace(
    "crate::vdp2::fetch_pixel(charaddr, paladdr, actual_x, actual_y, flipfunction, cellw, &layer_cfg, vdp2_vram, vdp2_cram)",
    "crate::vdp2::fetch_pixel((charaddr, paladdr), actual_x, actual_y, flipfunction, cellw, &layer_cfg, (vdp2_vram, vdp2_cram))"
)

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

