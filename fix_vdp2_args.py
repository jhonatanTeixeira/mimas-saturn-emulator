with open("saturn-core/src/vdp2.rs", "r") as f:
    text = f.read()

cfg_def = """#[derive(Clone, Copy)]
pub struct LayerConfig {
    pub mpofn: u16,
    pub mpab: u16,
    pub mpcd: u16,
    pub patterndatasize: u16,
    pub patternwh: u16,
    pub planew: u32,
    pub planeh: u32,
    pub vram_8mbit: bool,
    pub mapwh: u32,
    pub supplementdata: u16,
    pub auxmode: u16,
    pub colornumber: u16,
    pub transparencyenable: bool,
    pub coloroffset: u32,
    pub cram_mode: u16,
}
"""

if "pub struct LayerConfig" not in text:
    text = text.replace("pub fn calc_plane_addr(", cfg_def + "\npub fn calc_plane_addr(")

# generate_plane_addr_table
text = text.replace(
    "pub fn generate_plane_addr_table(\n    planetbl: &mut [u32; 4],\n    mpofn: u16,\n    mpab: u16,\n    mpcd: u16,\n    patterndatasize: u16,\n    patternwh: u16,\n    planew: u32,\n    planeh: u32,\n    vram_8mbit: bool,\n)",
    "pub fn generate_plane_addr_table(planetbl: &mut [u32; 4], cfg: &LayerConfig)"
)
text = text.replace(
    "let map = [\n        mpab & 0xFF,\n        (mpab >> 8) & 0xFF,\n        mpcd & 0xFF,\n        (mpcd >> 8) & 0xFF,\n    ];",
    "let map = [\n        cfg.mpab & 0xFF,\n        (cfg.mpab >> 8) & 0xFF,\n        cfg.mpcd & 0xFF,\n        (cfg.mpcd >> 8) & 0xFF,\n    ];"
)
text = text.replace(
    "planetbl[i] = calc_plane_addr(\n            mpofn,\n            map[i],\n            patterndatasize,\n            patternwh,\n            planew,\n            planeh,\n            vram_8mbit,\n        );",
    "planetbl[i] = calc_plane_addr(\n            cfg.mpofn,\n            map[i],\n            cfg.patterndatasize,\n            cfg.patternwh,\n            cfg.planew,\n            cfg.planeh,\n            cfg.vram_8mbit,\n        );"
)

# map_calc_xy
text = text.replace(
    "pub fn map_calc_xy(\n    state: &mut Vdp2State,\n    x: u32,\n    y: u32,\n    vars: &ScreenVars,\n    patternwh: u16,\n    patterndatasize: u16,\n    mapwh: u32,\n    supplementdata: u16,\n    auxmode: u16,\n    colornumber: u16,\n    vram_8mbit: bool,\n    vram: &[u8],\n)",
    "pub fn map_calc_xy(state: &mut Vdp2State, x: u32, y: u32, vars: &ScreenVars, cfg: &LayerConfig, vram: &[u8])"
)
text = text.replace("let cellwh = 2 + patternwh;", "let cellwh = 2 + cfg.patternwh;")
text = text.replace("(((y >> planepixelheight_bits) * mapwh) + (x >> planepixelwidth_bits)) as usize;", "(((y >> planepixelheight_bits) * cfg.mapwh) + (x >> planepixelwidth_bits)) as usize;")
text = text.replace(
    "let (charaddr, paladdr, flipfunction, specialfunction, specialcolorfunction) = pattern_addr(\n            tmp1,\n            tmp2,\n            supplementdata,\n            auxmode,\n            patternwh,\n            patterndatasize,\n            colornumber,\n            vram_8mbit,\n        );",
    "let (charaddr, paladdr, flipfunction, specialfunction, specialcolorfunction) = pattern_addr(\n            tmp1,\n            tmp2,\n            cfg.supplementdata,\n            cfg.auxmode,\n            cfg.patternwh,\n            cfg.patterndatasize,\n            cfg.colornumber,\n            cfg.vram_8mbit,\n        );"
)
text = text.replace(
    "let (charaddr, paladdr, flipfunction, specialfunction, specialcolorfunction) = pattern_addr(\n            tmp[0],\n            tmp[1],\n            supplementdata,\n            auxmode,\n            patternwh,\n            patterndatasize,\n            colornumber,\n            vram_8mbit,\n        );",
    "let (charaddr, paladdr, flipfunction, specialfunction, specialcolorfunction) = pattern_addr(\n            tmp[0],\n            tmp[1],\n            cfg.supplementdata,\n            cfg.auxmode,\n            cfg.patternwh,\n            cfg.patterndatasize,\n            cfg.colornumber,\n            cfg.vram_8mbit,\n        );"
)

# fetch_pixel
text = text.replace(
    "pub fn fetch_pixel(\n    charaddr: u32,\n    paladdr: u32,\n    mut x: u32,\n    mut y: u32,\n    flipfunction: u16,\n    patternwh: u16,\n    colornumber: u16,\n    transparencyenable: bool,\n    coloroffset: u32,\n    cram_mode: u16,\n    cellw: u32,\n    vram: &[u8],\n    cram: &[u8],\n)",
    "pub fn fetch_pixel(charaddr: u32, paladdr: u32, mut x: u32, mut y: u32, flipfunction: u16, cellw: u32, cfg: &LayerConfig, vram: &[u8], cram: &[u8])"
)

text = text.replace("if (flipfunction & 1) != 0 {", "if (flipfunction & 1) != 0 {") # nothing
text = text.replace("x = (8 * patternwh as u32) - 1 - x;", "x = (8 * cfg.patternwh as u32) - 1 - x;")
text = text.replace("y = (8 * patternwh as u32) - 1 - y;", "y = (8 * cfg.patternwh as u32) - 1 - y;")
text = text.replace("let cell_x = x / 8;\n    let cell_y = y / 8;", "let cell_x = x / 8;\n    let cell_y = y / 8;")
text = text.replace("let px = x % 8;\n    let py = y % 8;", "let px = x % 8;\n    let py = y % 8;")
text = text.replace(
    "let cell_idx = cell_y * cellw + cell_x;",
    "let cell_idx = cell_y * cellw + cell_x;"
)
text = text.replace("match colornumber {", "match cfg.colornumber {")
text = text.replace("let mut pal = 0;", "let mut pal = 0;")
text = text.replace("if transparencyenable && color == 0 {", "if cfg.transparencyenable && color == 0 {")
text = text.replace("pal |= paladdr;", "pal |= paladdr;")
text = text.replace("if cram_mode == 0 {", "if cfg.cram_mode == 0 {")
text = text.replace("pal += coloroffset;", "pal += cfg.coloroffset;")

with open("saturn-core/src/vdp2.rs", "w") as f:
    f.write(text)

