import re

with open("saturn-core/src/vdp2.rs", "r") as f:
    text = f.read()

cfg_def = """#[derive(Clone, Copy)]
pub struct Vdp2LayerConfig {
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

if "pub struct Vdp2LayerConfig" not in text:
    text = text.replace("pub fn calc_plane_addr(", cfg_def + "\npub fn calc_plane_addr(")

# generate_plane_addr_table
text = re.sub(
    r'pub fn generate_plane_addr_table\(\s*planetbl: &mut \[u32; 4\],\s*mpofn: u16,\s*mpab: u16,\s*mpcd: u16,\s*patterndatasize: u16,\s*patternwh: u16,\s*planew: u32,\s*planeh: u32,\s*vram_8mbit: bool,\s*\)',
    r'pub fn generate_plane_addr_table(planetbl: &mut [u32; 4], cfg: &Vdp2LayerConfig)',
    text
)
text = text.replace("mpab & 0xFF", "cfg.mpab & 0xFF")
text = text.replace("(mpab >> 8)", "(cfg.mpab >> 8)")
text = text.replace("mpcd & 0xFF", "cfg.mpcd & 0xFF")
text = text.replace("(mpcd >> 8)", "(cfg.mpcd >> 8)")
text = re.sub(
    r'calc_plane_addr\(\s*mpofn,\s*map\[i\],\s*patterndatasize,\s*patternwh,\s*planew,\s*planeh,\s*vram_8mbit,\s*\)',
    r'calc_plane_addr(cfg.mpofn, map[i], cfg.patterndatasize, cfg.patternwh, cfg.planew, cfg.planeh, cfg.vram_8mbit)',
    text
)

# map_calc_xy
text = re.sub(
    r'pub fn map_calc_xy\(\s*state: &mut Vdp2State,\s*x: u32,\s*y: u32,\s*vars: &ScreenVars,\s*patternwh: u16,\s*patterndatasize: u16,\s*mapwh: u32,\s*supplementdata: u16,\s*auxmode: u16,\s*colornumber: u16,\s*vram_8mbit: bool,\s*vram: &\[u8\],\s*\)',
    r'pub fn map_calc_xy(state: &mut Vdp2State, x: u32, y: u32, vars: &ScreenVars, cfg: &Vdp2LayerConfig, vram: &[u8])',
    text
)
text = text.replace("2 + patternwh", "2 + cfg.patternwh")
text = text.replace(" * mapwh)", " * cfg.mapwh)")
text = re.sub(
    r'pattern_addr\(\s*tmp1,\s*tmp2,\s*supplementdata,\s*auxmode,\s*patternwh,\s*patterndatasize,\s*colornumber,\s*vram_8mbit,\s*\)',
    r'pattern_addr(tmp1, tmp2, cfg)',
    text
)
text = re.sub(
    r'pattern_addr\(\s*tmp\[0\],\s*tmp\[1\],\s*supplementdata,\s*auxmode,\s*patternwh,\s*patterndatasize,\s*colornumber,\s*vram_8mbit,\s*\)',
    r'pattern_addr(tmp[0], tmp[1], cfg)',
    text
)

# pattern_addr
text = re.sub(
    r'pub fn pattern_addr\(\s*tmp1: u16,\s*tmp2: u16,\s*supplementdata: u16,\s*auxmode: u16,\s*patternwh: u16,\s*patterndatasize: u16,\s*colornumber: u16,\s*vram_8mbit: bool,\s*\)',
    r'pub fn pattern_addr(tmp1: u16, tmp2: u16, cfg: &Vdp2LayerConfig)',
    text
)
text = text.replace("if patternwh ==", "if cfg.patternwh ==")
text = text.replace("if vram_8mbit {", "if cfg.vram_8mbit {")
text = text.replace("patterndatasize", "cfg.patterndatasize")
text = text.replace("colornumber", "cfg.colornumber")
text = text.replace("auxmode", "cfg.auxmode")
text = text.replace("supplementdata", "cfg.supplementdata")


# fetch_pixel
text = re.sub(
    r'pub fn fetch_pixel\(\s*charaddr: u32,\s*paladdr: u32,\s*mut x: u32,\s*mut y: u32,\s*flipfunction: u16,\s*patternwh: u16,\s*colornumber: u16,\s*transparencyenable: bool,\s*coloroffset: u32,\s*cram_mode: u16,\s*cellw: u32,\s*vram: &\[u8\],\s*cram: &\[u8\],\s*\)',
    r'pub fn fetch_pixel(charaddr: u32, paladdr: u32, mut x: u32, mut y: u32, flipfunction: u16, cellw: u32, cfg: &Vdp2LayerConfig, vram: &[u8], cram: &[u8])',
    text
)
text = text.replace("8 * patternwh as u32", "8 * cfg.patternwh as u32")
text = text.replace("match cfg.colornumber", "match cfg.colornumber") # handled above
text = text.replace("if transparencyenable", "if cfg.transparencyenable")
text = text.replace("cram_mode", "cfg.cram_mode")
text = text.replace("coloroffset", "cfg.coloroffset")


with open("saturn-core/src/vdp2.rs", "w") as f:
    f.write(text)


with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

# Update vdp.rs
text = re.sub(
    r'crate::vdp2::generate_plane_addr_table\(\s*&mut state.planetbl,\s*mpofn,\s*mpab,\s*mpcd,\s*patterndatasize,\s*patternwh,\s*planew,\s*planeh,\s*vram_8mbit,\s*\);',
    r'crate::vdp2::generate_plane_addr_table(&mut state.planetbl, &layer_cfg);',
    text
)

text = re.sub(
    r'crate::vdp2::map_calc_xy\(\s*&mut state,\s*actual_x,\s*actual_y,\s*&screen_vars,\s*patternwh,\s*patterndatasize,\s*mapwh,\s*supplementdata,\s*auxmode,\s*colornumber,\s*vram_8mbit,\s*vdp2_vram,\s*\);',
    r'crate::vdp2::map_calc_xy(&mut state, actual_x, actual_y, &screen_vars, &layer_cfg, vdp2_vram);',
    text
)

text = re.sub(
    r'crate::vdp2::fetch_pixel\(\s*charaddr,\s*paladdr,\s*actual_x,\s*actual_y,\s*flipfunction,\s*patternwh,\s*colornumber,\s*transparency_enable,\s*coloroffset as u32,\s*cram_mode,\s*cellw,\s*vdp2_vram,\s*vdp2_cram,\s*\)',
    r'crate::vdp2::fetch_pixel(charaddr, paladdr, actual_x, actual_y, flipfunction, cellw, &layer_cfg, vdp2_vram, vdp2_cram)',
    text
)

text = text.replace(
    "let screen_vars = if is_bitmap {",
    """let layer_cfg = crate::vdp2::Vdp2LayerConfig {
        mpofn, mpab, mpcd, patterndatasize, patternwh, planew, planeh,
        vram_8mbit: state.vram_8mbit, mapwh, supplementdata, auxmode,
        colornumber, transparencyenable: transparency_enable,
        coloroffset: coloroffset as u32, cram_mode
    };
    
    let screen_vars = if is_bitmap {"""
)

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

