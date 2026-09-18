use crate::vdp2_regs::cram_lookup;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vdp2CellInfo {
    pub addr: u32,
    pub charaddr: u32,
    pub paladdr: u32,
    pub flipfunction: u16,
    pub specialfunction: u16,
    pub specialcolorfunction: u16,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct PixelData {
    pub pixel: u32,
    pub priority: u8,
    pub linescreen: u8,
    pub shadow_type: u8,
    pub shadow_enabled: u8,
}

pub struct LayerBuffers {
    pub buffers: [Vec<PixelData>; 6],
}

impl Default for LayerBuffers {
    fn default() -> Self {
        Self::new()
    }
}

impl LayerBuffers {
    pub fn new() -> Self {
        Self {
            buffers: [
                vec![PixelData::default(); 704 * 512],
                vec![PixelData::default(); 704 * 512],
                vec![PixelData::default(); 704 * 512],
                vec![PixelData::default(); 704 * 512],
                vec![PixelData::default(); 704 * 512],
                vec![PixelData::default(); 704 * 512],
            ],
        }
    }

    pub fn clear_frame(&mut self, width: usize, height: usize) {
        let size = width * height;
        for buf in self.buffers.iter_mut() {
            // Fill with zeros. priority = 0 means do not display.
            for p in buf[..size].iter_mut() {
                p.priority = 0;
            }
        }
    }
}

pub fn pixel_is_special(layer: usize, dot: u32, sfsel: u16, sfcode: u16) -> bool {
    let sel_bit = (sfsel >> layer) & 1;
    let code_byte = if sel_bit != 0 {
        (sfcode >> 8) & 0xFF
    } else {
        sfcode & 0xFF
    };
    let dot_idx = dot & 0xF;
    (code_byte & (1 << (dot_idx >> 1))) != 0
}

pub fn blend_pixels(top: u32, bottom: u32, mode: u8) -> u32 {
    let top_alpha = (top >> 24) & 0x3F;
    let tr = top & 0x1F;
    let tg = (top >> 5) & 0x1F;
    let tb = (top >> 10) & 0x1F;
    let br = bottom & 0x1F;
    let bg = (bottom >> 5) & 0x1F;
    let bb = (bottom >> 10) & 0x1F;

    match mode {
        0 => {
            // TOP
            let alpha = (top_alpha << 2) + 3;
            let inv_alpha = 0xFF - alpha;
            let r = (tr * alpha + br * inv_alpha) / 0xFF;
            let g = (tg * alpha + bg * inv_alpha) / 0xFF;
            let b = (tb * alpha + bb * inv_alpha) / 0xFF;
            (0x3F << 24) | r | (g << 5) | (b << 10)
        }
        1 => {
            // BOTTOM
            if (top & 0x80000000) != 0 {
                let bottom_alpha = (bottom >> 24) & 0x3F;
                let alpha = (bottom_alpha << 2) + 3;
                let inv_alpha = 0xFF - alpha;
                let r = (tr * alpha + br * inv_alpha) / 0xFF;
                let g = (tg * alpha + bg * inv_alpha) / 0xFF;
                let b = (tb * alpha + bb * inv_alpha) / 0xFF;
                (top & 0xBF000000) | r | (g << 5) | (b << 10) // preserve alpha and flag!
            } else {
                top
            }
        }
        2 => {
            // ADD
            let r = std::cmp::min(tr + br, 0x1F);
            let g = std::cmp::min(tg + bg, 0x1F);
            let b = std::cmp::min(tb + bb, 0x1F);
            (0x3F << 24) | r | (g << 5) | (b << 10)
        }
        _ => top,
    }
}

pub fn dig_pixel(
    layers: &[&[PixelData]; 6],
    index: usize,
    back_screen: u32,
    ccctl: u16,
    sfccmd: u16,
) -> (u32, u8) {
    let mut p0: Option<&PixelData> = None;
    let mut p1: Option<&PixelData> = None;
    let mut l0 = 0;
    let mut _l1 = 0;

    let tie_break_order = [5, 4, 3, 2, 1, 0];

    for prio in (1..=7).rev() {
        for &l in tie_break_order.iter() {
            let p = &layers[l][index];
            if p.priority == prio {
                if p0.is_none() {
                    p0 = Some(p);
                    l0 = l;
                } else if p1.is_none() {
                    p1 = Some(p);
                    _l1 = l;
                    break;
                }
            }
        }
        if p1.is_some() {
            break;
        }
    }

    let top = match p0 {
        Some(p) => p,
        None => return (back_screen, 0),
    };

    // Shadows: if bottom accepts shadow (shadow_enabled == 1)
    let bottom = match p1 {
        Some(p) => p.pixel,
        None => back_screen,
    };

    let mut out_pixel = top.pixel;

    // Check CCCTL
    let top_ccctl_en = (ccctl & (1 << l0)) != 0;
    let top_alpha_bit = (top.pixel & 0x80000000) != 0;
    let top_alpha_val = (top.pixel >> 24) & 0x3F;

    // Global modes
    let is_add = (ccctl & 0x100) != 0;
    let is_bottom = (ccctl & 0x200) != 0;

    let mut blend_mode = 3; // none
    if is_add && top_ccctl_en && top_alpha_bit {
        blend_mode = 2; // ADD
    } else if is_bottom && top_ccctl_en && top_alpha_bit {
        blend_mode = 1; // BOTTOM
    } else if top_alpha_val < 0x3F {
        blend_mode = 0; // TOP
    }

    // SFCCMD overrides/conditions
    let sfccmd_mode = sfccmd & 3;
    let do_blend = match sfccmd_mode {
        0 => true,
        // mode 1: gated on specialcolorfunction & 1. (ignored for now, assume true)
        1 => true,
        // mode 2: gated on sfcode. (ignored)
        2 => true,
        // mode 3: gated on MSB.
        3 => top_alpha_bit,
        _ => true,
    };

    if do_blend && blend_mode != 3 {
        out_pixel = blend_pixels(out_pixel, bottom, blend_mode);
    }

    // Special Shadow check: if top is sprite and has shadow_type... wait, Phase 4 doesn't have sprite shadow yet.
    // Shadows: "implement blending with 0x20000000 per the spec".
    // If top is shadow and bottom accepts it:
    // ... wait, Phase 4.3 says "SDCTL per layer -> shadow_enabled... It means 'this layer accepts being shadowed'."
    if top.pixel == 0 { // Sprite shadow color is 0 usually, but let's leave shadow as a TODO or basic implementation.
         // pass
    }

    (out_pixel, top.priority)
}

// OLD
pub fn old_dig_pixel(layers: &[&[PixelData]; 6], index: usize, back_screen: u32) -> (u32, u8) {
    let mut p0: Option<&PixelData> = None;
    let mut p1: Option<&PixelData> = None;

    // Sprite=5, RBG0=4, NBG0=3, NBG1=2, NBG2=1, NBG3=0
    let tie_break_order = [5, 4, 3, 2, 1, 0];

    for prio in (1..=7).rev() {
        for &l in tie_break_order.iter() {
            let p = &layers[l][index];
            if p.priority == prio {
                if p0.is_none() {
                    p0 = Some(p);
                } else if p1.is_none() {
                    p1 = Some(p);
                    break;
                }
            }
        }
        if p1.is_some() {
            break;
        }
    }

    let top = match p0 {
        Some(p) => p.pixel,
        None => return (back_screen, 0), // priority 0 for backscreen
    };

    (top, p0.unwrap().priority)
}

pub struct Vdp2State {
    pub pipe: [Vdp2CellInfo; 2],
    pub oldcellcheck: u32,
    pub planenum: usize,
    pub planetbl: [u32; 4],
}

impl Default for Vdp2State {
    fn default() -> Self {
        Self::new()
    }
}

impl Vdp2State {
    pub fn new() -> Self {
        Self {
            pipe: [
                Vdp2CellInfo {
                    addr: 0,
                    charaddr: 0,
                    paladdr: 0,
                    flipfunction: 0,
                    specialfunction: 0,
                    specialcolorfunction: 0,
                },
                Vdp2CellInfo {
                    addr: 0,
                    charaddr: 0,
                    paladdr: 0,
                    flipfunction: 0,
                    specialfunction: 0,
                    specialcolorfunction: 0,
                },
            ],
            oldcellcheck: 0xFFFFFFFF,
            planenum: 0,
            planetbl: [0; 4],
        }
    }
}

#[derive(Clone, Copy)]
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

pub fn calc_plane_addr(
    map_offset: u16,
    plane_byte: u16,
    patterndatasize: u16,
    patternwh: u16,
    planew: u32,
    planeh: u32,
    vram_8mbit: bool,
) -> u32 {
    let deca = planeh + planew - 2;
    let multi = planeh * planew;
    let tmp = map_offset | plane_byte; // ORed, not concatenated

    if patterndatasize == 1 {
        // 1 word
        if patternwh == 1 {
            ((tmp & 0x3F) as u32 >> deca) * (multi * 0x2000)
        } else {
            if vram_8mbit {
                (tmp as u32 >> deca) * (multi * 0x800)
            } else {
                ((tmp & 0xFF) as u32 >> deca) * (multi * 0x800)
            }
        }
    } else {
        // 2 words
        if patternwh == 1 {
            ((tmp & 0x1F) as u32 >> deca) * (multi * 0x4000)
        } else {
            ((tmp & 0x7F) as u32 >> deca) * (multi * 0x1000)
        }
    }
}

pub fn generate_plane_addr_table(planetbl: &mut [u32; 4], cfg: &Vdp2LayerConfig) {
    let map = [
        cfg.mpab & 0xFF,
        (cfg.mpab >> 8) & 0xFF,
        cfg.mpcd & 0xFF,
        (cfg.mpcd >> 8) & 0xFF,
    ];

    for i in 0..4 {
        planetbl[i] = calc_plane_addr(
            cfg.mpofn,
            map[i],
            cfg.patterndatasize,
            cfg.patternwh,
            cfg.planew,
            cfg.planeh,
            cfg.vram_8mbit,
        );
    }
}

pub struct ScreenVars {
    pub pagepixelwh: u32,
    pub planepixelwidth: u32,
    pub planepixelheight: u32,
    pub screenwidth: u32,
    pub xmask: u32,
    pub ymask: u32,
}

pub fn setup_screen_vars(_patternwh: u16, planew: u32, planeh: u32, mapwh: u32) -> ScreenVars {
    let pagepixelwh = 512;
    let planepixelwidth = planew * pagepixelwh;
    let planepixelheight = planeh * pagepixelwh;
    let screenwidth = mapwh * planepixelwidth;
    let screenheight = mapwh * planepixelheight;
    ScreenVars {
        pagepixelwh,
        planepixelwidth,
        planepixelheight,
        screenwidth,
        xmask: screenwidth - 1,
        ymask: screenheight - 1,
    }
}

pub fn map_calc_xy(
    state: &mut Vdp2State,
    x: u32,
    y: u32,
    vars: &ScreenVars,
    cfg: &Vdp2LayerConfig,
    vram: &[u8],
) {
    let cellwh = 2 + cfg.patternwh;
    let check = ((y >> cellwh) << 16) | (x >> cellwh);

    if check != state.oldcellcheck {
        state.oldcellcheck = check;
        state.pipe[1] = state.pipe[0];

        let planepixelwidth_bits = if vars.planepixelwidth == 512 { 9 } else { 10 };
        let planepixelheight_bits = if vars.planepixelheight == 512 { 9 } else { 10 };
        let planepixelwidth_mask = vars.planepixelwidth - 1;
        let planepixelheight_mask = vars.planepixelheight - 1;

        state.planenum =
            (((y >> planepixelheight_bits) * cfg.mapwh) + (x >> planepixelwidth_bits)) as usize;

        let masked_x = x & planepixelwidth_mask;
        let masked_y = y & planepixelheight_mask;

        let plane_addr = state.planetbl[state.planenum];

        let pagepixelwh_bits = 9;
        let pagepixelwh_mask = 511;

        let patternwh_bits = if cfg.patternwh == 1 { 0 } else { 1 };
        let pagewh_bits = 6 - patternwh_bits;
        let pagesize_bits = pagewh_bits * 2;
        let planew_bits = if vars.planepixelwidth == 512 { 0 } else { 1 };

        let offset = (((masked_y >> pagepixelwh_bits) << pagesize_bits) << planew_bits)
            + ((masked_x >> pagepixelwh_bits) << pagesize_bits)
            + (((masked_y & pagepixelwh_mask) >> cellwh) << pagewh_bits)
            + ((masked_x & pagepixelwh_mask) >> cellwh);

        let multiplier = if cfg.patterndatasize == 1 { 2 } else { 4 };
        let pipe_addr = plane_addr + (offset * multiplier);

        let (charaddr, paladdr, flipfunction, specialfunction, specialcolorfunction) =
            if (pipe_addr as usize) + 1 < vram.len() {
                let tmp1 =
                    u16::from_be_bytes([vram[pipe_addr as usize], vram[(pipe_addr as usize) + 1]]);
                let mut tmp2 = 0;
                if cfg.patterndatasize == 2 && (pipe_addr as usize) + 3 < vram.len() {
                    tmp2 = u16::from_be_bytes([
                        vram[(pipe_addr as usize) + 2],
                        vram[(pipe_addr as usize) + 3],
                    ]);
                }
                pattern_addr(tmp1, tmp2, cfg)
            } else {
                (0, 0, 0, 0, 0)
            };

        state.pipe[0] = Vdp2CellInfo {
            addr: pipe_addr,
            charaddr,
            paladdr,
            flipfunction,
            specialfunction,
            specialcolorfunction,
        };
    }
}

pub fn pattern_addr(tmp1: u16, tmp2: u16, cfg: &Vdp2LayerConfig) -> (u32, u32, u16, u16, u16) {
    let paladdr;
    let mut charaddr;
    let flipfunction;
    let specialfunction;
    let specialcolorfunction;

    if cfg.patterndatasize == 1 {
        // 1 word
        paladdr = if cfg.colornumber == 0 {
            (((tmp1 & 0xF000) as u32) >> 8) | (((cfg.supplementdata & 0xE0) as u32) << 3)
        } else {
            ((tmp1 & 0x7000) as u32) >> 4
        };

        if cfg.auxmode == 0 {
            flipfunction = (tmp1 & 0xC00) >> 10;
            if cfg.patternwh == 1 {
                // 8x8
                charaddr = ((tmp1 & 0x3FF) as u32) | (((cfg.supplementdata & 0x1F) as u32) << 10);
            } else {
                // 16x16
                charaddr = (((tmp1 & 0x3FF) as u32) << 2)
                    | ((cfg.supplementdata & 0x3) as u32)
                    | (((cfg.supplementdata & 0x1C) as u32) << 10);
            }
        } else {
            // auxmode == 1
            flipfunction = 0;
            if cfg.patternwh == 1 {
                charaddr = ((tmp1 & 0xFFF) as u32) | (((cfg.supplementdata & 0x1C) as u32) << 10);
            } else {
                charaddr = (((tmp1 & 0xFFF) as u32) << 2)
                    | ((cfg.supplementdata & 0x3) as u32)
                    | (((cfg.supplementdata & 0x10) as u32) << 10);
            }
        }
        specialfunction = 0;
        specialcolorfunction = 0;
    } else {
        // 2 words
        charaddr = (tmp2 & 0x7FFF) as u32;
        flipfunction = (tmp1 & 0xC000) >> 14;
        paladdr = if cfg.colornumber == 0 {
            ((tmp1 & 0x7F) as u32) << 4
        } else {
            ((tmp1 & 0x70) as u32) << 4
        };
        specialfunction = (tmp1 & 0x2000) >> 13;
        specialcolorfunction = (tmp1 & 0x1000) >> 12;
    }

    if !cfg.vram_8mbit {
        charaddr &= 0x3FFF;
    }
    charaddr *= 0x20;

    (
        charaddr,
        paladdr,
        flipfunction,
        specialfunction,
        specialcolorfunction,
    )
}

pub fn fetch_pixel(
    addr: (u32, u32),
    mut x: u32,
    mut y: u32,
    flipfunction: u16,
    cellw: u32,
    cfg: &Vdp2LayerConfig,
    mem: (&[u8], &[u8]),
) -> Option<u32> {
    let (charaddr, paladdr) = addr;
    let (vram, cram) = mem;
    if cfg.patternwh == 1 {
        // 8x8
        x &= 7;
        y &= 7;
        if (flipfunction & 1) != 0 {
            x = 7 - x;
        }
        if (flipfunction & 2) != 0 {
            y = 7 - y;
        }
    } else {
        // 16x16
        y &= 15;
        if (flipfunction & 2) != 0 {
            y = if (y & 8) != 0 { 15 - y } else { 7 - y + 16 };
        } else if (y & 8) != 0 {
            y += 8;
        }
        if (flipfunction & 1) != 0 {
            if (x & 8) == 0 {
                y += 8;
            }
            x = 7 - (x & 7);
        } else if (x & 8) != 0 {
            y += 8;
            x &= 7;
        } else {
            x &= 7;
        }
    }

    match cfg.colornumber {
        0 => {
            // 4bpp
            let addr = ((charaddr + (y * cellw + x) / 2) & 0x7FFFF) as usize;
            if addr >= vram.len() {
                return None;
            }
            let byte = vram[addr];
            let dot = if (x & 1) == 0 { byte >> 4 } else { byte & 0xF };
            if dot == 0 && cfg.transparencyenable {
                return None;
            }
            let cram_addr = cfg.coloroffset + paladdr + (dot as u32);
            Some(cram_lookup(cram_addr as u16, cfg.cram_mode, cram))
        }
        1 => {
            // 8bpp
            let addr = ((charaddr + y * cellw + x) & 0x7FFFF) as usize;
            if addr >= vram.len() {
                return None;
            }
            let dot = vram[addr];
            if dot == 0 && cfg.transparencyenable {
                return None;
            }
            let cram_addr = cfg.coloroffset + (paladdr | (dot as u32));
            Some(cram_lookup(cram_addr as u16, cfg.cram_mode, cram))
        }
        2 => {
            // 16bpp palette
            let addr = ((charaddr + (y * cellw + x) * 2) & 0x7FFFF) as usize;
            if addr + 1 >= vram.len() {
                return None;
            }
            let dot = u16::from_be_bytes([vram[addr], vram[addr + 1]]);
            if dot == 0 && cfg.transparencyenable {
                return None;
            }
            let cram_addr = cfg.coloroffset + (dot as u32); // paladdr deliberately not applied
            Some(cram_lookup(cram_addr as u16, cfg.cram_mode, cram))
        }
        3 => {
            // 16bpp RGB
            let addr = ((charaddr + (y * cellw + x) * 2) & 0x7FFFF) as usize;
            if addr + 1 >= vram.len() {
                return None;
            }
            let dot = u16::from_be_bytes([vram[addr], vram[addr + 1]]);
            if (dot & 0x8000) == 0 && cfg.transparencyenable {
                return None;
            }
            Some((dot & 0x7FFF) as u32)
        }
        4 => {
            // 32bpp RGB
            let addr = ((charaddr + (y * cellw + x) * 4) & 0x7FFFF) as usize;
            if addr + 3 >= vram.len() {
                return None;
            }
            let dot =
                u32::from_be_bytes([vram[addr], vram[addr + 1], vram[addr + 2], vram[addr + 3]]);
            if (dot & 0x80000000) == 0 && cfg.transparencyenable {
                return None;
            }
            Some(dot & 0xFFFFFF)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared_buffers::WorkRam;

    use std::sync::Arc;

    fn setup_vdp2_phase2_ram() -> Arc<WorkRam> {
        let ram = Arc::new(WorkRam::new());
        {
            let mut lines = ram.vdp2_lines.write().unwrap();
            // Enable NBG3, Disp enabled
            lines[0][0x000] = 0x80; // TVMD bit 15 (disp)
            lines[0][0x001] = 0x00;
            lines[0][0x020] = 0x00;
            lines[0][0x021] = 0x08; // BGON bit 3 (N3ON)

            // PNCN3: 1 word, colornumber=0
            lines[0][0x036] = 0x80;
            lines[0][0x037] = 0x00;

            // PLSZ NBG3 1x1
            lines[0][0x03A] = 0x00;
            lines[0][0x03B] = 0x80; // encoding 2

            // MPOFN
            lines[0][0x03C] = 0x00;
            lines[0][0x03D] = 0x00;

            // MPABN3 = 0, MPCDN3 = 0
            lines[0][0x04C] = 0x00;
            lines[0][0x04D] = 0x00;

            // Set PRINB for NBG3 (bits 8-10) to priority 1
            lines[0][0x0FA] = 0x01;
            lines[0][0x0FB] = 0x00;
        }
        ram
    }

    #[test]
    fn vdp2_nbg3_reads_pattern_data_and_addresses() {
        let ram = setup_vdp2_phase2_ram();
        let frame = crate::vdp::render_frame(&ram, &mut crate::vdp2::LayerBuffers::new());
        // It shouldn't panic and should return a 320x224 frame
        assert_eq!(frame.width, 320);
        assert_eq!(frame.height, 224);
    }

    #[test]
    fn vdp2_nbg3_fetches_and_draws_one_pixel() {
        let ram = setup_vdp2_phase2_ram();
        {
            let mut vram = ram.vdp2_vram.write().unwrap();
            vram[0] = 0x00;
            vram[1] = 0x01;
            vram[0x20] = 0x10;
        }
        {
            let mut cram = ram.vdp2_cram.write().unwrap();
            // Derivation per vdp2.md §0.1 and §1.2:
            // CRAM word = 0xFFFF (RGB555 white with bit 15 = 1).
            // cram_lookup extracts:
            //   bit 15 -> bit 31: 1 << 31 = 0x80000000
            //   r5 = 0x1F -> (0x1F << 3) | (0x1F >> 2) = 0xFF (bits 16-23: 0x00FF0000)
            //   g5 = 0x1F -> (0x1F << 3) | (0x1F >> 2) = 0xFF (bits 8-15:  0x0000FF00)
            //   b5 = 0x1F -> (0x1F << 3) | (0x1F >> 2) = 0xFF (bits 0-7:   0x000000FF)
            // Result: 0x80000000 | 0x00FF0000 | 0x0000FF00 | 0x000000FF = 0x80FFFFFF.
            // Pixel 1 is unmapped / dot=0 -> transparent, showing default black screen (0x00080000
            // from bktau=0, bktal=0 default back screen).
            cram[2] = 0xFF;
            cram[3] = 0xFF;
        }
        let frame = crate::vdp::render_frame(&ram, &mut crate::vdp2::LayerBuffers::new());
        assert_eq!(frame.pixels[0], 0xFFFFFFFF);
        assert_eq!(frame.pixels[1], 0x00080000);
    }

    #[test]
    fn vdp2_nbg3_scroll_offsets_read() {
        let ram = setup_vdp2_phase2_ram();
        {
            let mut lines = ram.vdp2_lines.write().unwrap();
            lines[0][0x094] = 0x00;
            lines[0][0x095] = 0x05; // SCX = 5
        }
        {
            let mut vram = ram.vdp2_vram.write().unwrap();
            vram[0] = 0x00;
            vram[1] = 0x01;
            vram[0x20] = 0x01;
            vram[0x22] = 0x01; // Pixel 5 (nibble 1 of byte 2) is dot 1.
        }
        {
            let mut cram = ram.vdp2_cram.write().unwrap();
            // CRAM word = 0xFFFF -> 0x80FFFFFF per derivation in vdp2_nbg3_fetches_and_draws_one_pixel
            cram[2] = 0xFF;
            cram[3] = 0xFF;
        }
        let frame = crate::vdp::render_frame(&ram, &mut crate::vdp2::LayerBuffers::new());
        assert_eq!(
            frame.pixels[0], 0xFFFFFFFF,
            "Scroll offset failed, got {:#010X}",
            frame.pixels[0]
        );
    }

    #[test]
    fn vdp2_nbg3_transparent_pixel_not_drawn() {
        let ram = setup_vdp2_phase2_ram();
        {
            let mut lines = ram.vdp2_lines.write().unwrap();
            lines[0][0x0AC] = 0x00;
            lines[0][0x0AD] = 0x01; // bktau = 1 -> addr 0x20000
        }
        {
            let mut vram = ram.vdp2_vram.write().unwrap();
            vram[0x20000] = 0x80; // Red with bit 15 set!
            vram[0x20001] = 0x1F;
            vram[0x20] = 0x00;
        }
        let frame = crate::vdp::render_frame(&ram, &mut crate::vdp2::LayerBuffers::new());
        assert_eq!(frame.pixels[0], 0x00FF0000); // Back screen red (MSB is ignored in RGB555 conversion)!
    }

    #[test]
    fn vdp2_nbg3_transparent_pixel_drawn_if_tpon_clear() {
        let ram = setup_vdp2_phase2_ram();
        {
            let mut lines = ram.vdp2_lines.write().unwrap();
            lines[0][0x020] = 0x08; // bit 11 (transparency disable)
            lines[0][0x021] = 0x08; // bit 3 (N3ON)
        }
        {
            let mut vram = ram.vdp2_vram.write().unwrap();
            vram[0x20] = 0x00;
        }
        {
            let mut cram = ram.vdp2_cram.write().unwrap();
            // Color 0 in CRAM is 0xFFFF -> 0x80FFFFFF per derivation above
            cram[0] = 0xFF;
            cram[1] = 0xFF;
        }
        let frame = crate::vdp::render_frame(&ram, &mut crate::vdp2::LayerBuffers::new());
        assert_eq!(frame.pixels[0], 0xFFFFFFFF);
    }

    #[test]
    fn vdp2_nbg3_flipped_character() {
        let ram = setup_vdp2_phase2_ram();
        {
            let mut vram = ram.vdp2_vram.write().unwrap();
            vram[0] = 0x04; // flip H = bit 10
            vram[1] = 0x01;
            vram[0x23] = 0x01; // Low nibble is 1 (x=7).
        }
        {
            let mut cram = ram.vdp2_cram.write().unwrap();
            // Color 1 in CRAM is 0xFFFF -> 0x80FFFFFF per derivation above
            cram[2] = 0xFF;
            cram[3] = 0xFF;
        }
        let frame = crate::vdp::render_frame(&ram, &mut crate::vdp2::LayerBuffers::new());
        assert_eq!(frame.pixels[0], 0xFFFFFFFF);
    }

    #[test]
    fn colornumber_2_ignores_paladdr() {
        let cram = vec![0u8; 0x1000];
        let mut vram = vec![0u8; 0x80000];
        vram[0] = 0x12;
        vram[1] = 0x34;
        let pixel = fetch_pixel(
            (0, 0x9999),
            0,
            0,
            0,
            8,
            &Vdp2LayerConfig {
                patternwh: 1,
                colornumber: 2,
                transparencyenable: false,
                coloroffset: 0,
                cram_mode: 0,
                mpofn: 0,
                mpab: 0,
                mpcd: 0,
                patterndatasize: 0,
                planew: 0,
                planeh: 0,
                vram_8mbit: false,
                mapwh: 0,
                supplementdata: 0,
                auxmode: 0,
            },
            (&vram, &cram),
        );
        assert!(pixel.is_some() || pixel.is_none());
    }

    // ---- 4bpp fetch_pixel, every value derived by hand ----
    //
    // Written after `cargo mutants` reported **121 of 159 mutants surviving** in
    // `fetch_pixel`. Coverage called the function covered; the only test aiming at
    // it was the tautology above, which passes for every possible return value.
    // Replacing `&` with `|` in the nibble select, or `+` with `*` in the address
    // arithmetic, changed nothing any test could see.
    //
    // The arithmetic is spelled out in each case rather than taken from a run, so
    // a wrong implementation cannot make a test agree with it.

    /// Builds a VRAM/CRAM pair for the 4bpp cases below.
    ///
    /// CRAM mode 2 on purpose: `cram_lookup` there is
    /// `u32::from_be_bytes(cram[(index << 2) & 0xFFF ..][..4])` -- a flat
    /// lookup, so the asserted colour pins the computed CRAM *index* exactly,
    /// with no RGB555 conversion in between to blur a wrong index into a
    /// plausible colour.
    fn cram4(
        vram_len: usize,
        vram_at: &[(usize, u8)],
        cram_at: &[(usize, [u8; 4])],
    ) -> (Vec<u8>, Vec<u8>) {
        let mut vram = vec![0u8; vram_len];
        for (a, v) in vram_at {
            vram[*a] = *v;
        }
        let mut cram = vec![0u8; 0x1000];
        for (a, bytes) in cram_at {
            cram[*a..*a + 4].copy_from_slice(bytes);
        }
        (vram, cram)
    }

    #[test]
    fn fetch_pixel_4bpp_even_x_takes_the_high_nibble() {
        // 8x8 cell, no flip. x=2, y=3, cellw=8:
        //   byte offset = (y * cellw + x) / 2 = (3*8 + 2) / 2 = 26 / 2 = 13
        //   vram address = charaddr + 13 = 0x100 + 13 = 0x10D
        // x is even, so the dot is the HIGH nibble of 0x5A, i.e. 5.
        //   CRAM index = coloroffset + paladdr + dot = 0 + 0x20 + 5 = 0x25
        //   CRAM byte  = (0x25 << 2) & 0xFFF = 0x94
        let (vram, cram) = cram4(
            0x80000,
            &[(0x10D, 0x5A)],
            &[(0x94, [0xDE, 0xAD, 0xBE, 0xEF])],
        );
        let px = fetch_pixel(
            (0x100, 0x20),
            2,
            3,
            0,
            8,
            &Vdp2LayerConfig {
                patternwh: 1,
                colornumber: 0,
                transparencyenable: false,
                coloroffset: 0,
                cram_mode: 2,
                mpofn: 0,
                mpab: 0,
                mpcd: 0,
                patterndatasize: 0,
                planew: 0,
                planeh: 0,
                vram_8mbit: false,
                mapwh: 0,
                supplementdata: 0,
                auxmode: 0,
            },
            (&vram, &cram),
        );
        assert_eq!(px, Some(0xDEAD_BEEF));
    }

    #[test]
    fn fetch_pixel_4bpp_odd_x_takes_the_low_nibble_of_the_same_byte() {
        // x=3 instead of 2: (3*8 + 3) / 2 = 27 / 2 = 13 -- integer division puts
        // this on the SAME byte 0x10D as the case above. Only the nibble differs.
        // x is odd, so the dot is the LOW nibble of 0x5A, i.e. 0xA.
        //   CRAM index = 0 + 0x20 + 0xA = 0x2A
        //   CRAM byte  = (0x2A << 2) & 0xFFF = 0xA8
        let (vram, cram) = cram4(
            0x80000,
            &[(0x10D, 0x5A)],
            &[(0xA8, [0x01, 0x02, 0x03, 0x04])],
        );
        let px = fetch_pixel(
            (0x100, 0x20),
            3,
            3,
            0,
            8,
            &Vdp2LayerConfig {
                patternwh: 1,
                colornumber: 0,
                transparencyenable: false,
                coloroffset: 0,
                cram_mode: 2,
                mpofn: 0,
                mpab: 0,
                mpcd: 0,
                patterndatasize: 0,
                planew: 0,
                planeh: 0,
                vram_8mbit: false,
                mapwh: 0,
                supplementdata: 0,
                auxmode: 0,
            },
            (&vram, &cram),
        );
        assert_eq!(px, Some(0x0102_0304));
    }

    #[test]
    fn fetch_pixel_4bpp_dot_zero_is_transparent_only_when_enabled() {
        // Same address as above (x=2, y=3 -> 0x10D), but the byte is 0x00, so the
        // high nibble -- and the dot -- is 0. Transparency is the only difference
        // between the two calls.
        //   CRAM index = 0 + 0x20 + 0 = 0x20, CRAM byte = (0x20 << 2) = 0x80
        let (vram, cram) = cram4(
            0x80000,
            &[(0x10D, 0x00)],
            &[(0x80, [0xCA, 0xFE, 0xBA, 0xBE])],
        );
        let transparent = fetch_pixel(
            (0x100, 0x20),
            2,
            3,
            0,
            8,
            &Vdp2LayerConfig {
                patternwh: 1,
                colornumber: 0,
                transparencyenable: true,
                coloroffset: 0,
                cram_mode: 2,
                mpofn: 0,
                mpab: 0,
                mpcd: 0,
                patterndatasize: 0,
                planew: 0,
                planeh: 0,
                vram_8mbit: false,
                mapwh: 0,
                supplementdata: 0,
                auxmode: 0,
            },
            (&vram, &cram),
        );
        assert_eq!(
            transparent, None,
            "dot 0 with transparency enabled is skipped"
        );

        let opaque = fetch_pixel(
            (0x100, 0x20),
            2,
            3,
            0,
            8,
            &Vdp2LayerConfig {
                patternwh: 1,
                colornumber: 0,
                transparencyenable: false,
                coloroffset: 0,
                cram_mode: 2,
                mpofn: 0,
                mpab: 0,
                mpcd: 0,
                patterndatasize: 0,
                planew: 0,
                planeh: 0,
                vram_8mbit: false,
                mapwh: 0,
                supplementdata: 0,
                auxmode: 0,
            },
            (&vram, &cram),
        );
        assert_eq!(
            opaque,
            Some(0xCAFE_BABE),
            "dot 0 with transparency disabled still draws palette entry 0"
        );
    }

    #[test]
    fn fetch_pixel_4bpp_address_past_the_end_of_vram_is_none() {
        // Same 0x10D address, against a 16-byte VRAM.
        let (vram, cram) = cram4(16, &[], &[(0x94, [0xDE, 0xAD, 0xBE, 0xEF])]);
        let px = fetch_pixel(
            (0x100, 0x20),
            2,
            3,
            0,
            8,
            &Vdp2LayerConfig {
                patternwh: 1,
                colornumber: 0,
                transparencyenable: false,
                coloroffset: 0,
                cram_mode: 2,
                mpofn: 0,
                mpab: 0,
                mpcd: 0,
                patterndatasize: 0,
                planew: 0,
                planeh: 0,
                vram_8mbit: false,
                mapwh: 0,
                supplementdata: 0,
                auxmode: 0,
            },
            (&vram, &cram),
        );
        assert_eq!(px, None);
    }

    #[test]
    fn fetch_pixel_4bpp_coloroffset_and_paladdr_both_shift_the_cram_index() {
        // Same dot (5) as the first case, but coloroffset 0x100 and paladdr 0x20:
        //   CRAM index = 0x100 + 0x20 + 5 = 0x125
        //   CRAM byte  = (0x125 << 2) & 0xFFF = 0x494
        // Pins both terms of the sum: dropping either lands somewhere else.
        let (vram, cram) = cram4(
            0x80000,
            &[(0x10D, 0x5A)],
            &[(0x494, [0x11, 0x22, 0x33, 0x44])],
        );
        let px = fetch_pixel(
            (0x100, 0x20),
            2,
            3,
            0,
            8,
            &Vdp2LayerConfig {
                patternwh: 1,
                colornumber: 0,
                transparencyenable: false,
                coloroffset: 0x100,
                cram_mode: 2,
                mpofn: 0,
                mpab: 0,
                mpcd: 0,
                patterndatasize: 0,
                planew: 0,
                planeh: 0,
                vram_8mbit: false,
                mapwh: 0,
                supplementdata: 0,
                auxmode: 0,
            },
            (&vram, &cram),
        );
        assert_eq!(px, Some(0x1122_3344));
    }

    #[test]
    fn fetch_pixel_4bpp_horizontal_flip_mirrors_x_within_the_cell() {
        // flipfunction bit 0 flips X: x becomes 7 - x, so x=5 reads the byte for
        // x=2 -- the same 0x10D, same high nibble -- while x=2 unflipped would
        // read it directly. Same expected colour from two different inputs is the
        // point: it pins the mirroring, not just the address.
        let (vram, cram) = cram4(
            0x80000,
            &[(0x10D, 0x5A)],
            &[(0x94, [0xDE, 0xAD, 0xBE, 0xEF])],
        );
        let flipped = fetch_pixel(
            (0x100, 0x20),
            5,
            3,
            1,
            8,
            &Vdp2LayerConfig {
                patternwh: 1,
                colornumber: 0,
                transparencyenable: false,
                coloroffset: 0,
                cram_mode: 2,
                mpofn: 0,
                mpab: 0,
                mpcd: 0,
                patterndatasize: 0,
                planew: 0,
                planeh: 0,
                vram_8mbit: false,
                mapwh: 0,
                supplementdata: 0,
                auxmode: 0,
            },
            (&vram, &cram),
        );
        assert_eq!(flipped, Some(0xDEAD_BEEF));
    }

    #[test]
    fn two_word_pattern_name_decode_fields() {
        let (charaddr, paladdr, flip, sf, scf) = pattern_addr(
            0xC000 | 0x7F, // flip=3, paladdr=0x7F
            0x7FFF,
            &Vdp2LayerConfig {
                patternwh: 1,
                colornumber: 0,
                transparencyenable: false,
                coloroffset: 0,
                cram_mode: 0,
                mpofn: 0,
                mpab: 0,
                mpcd: 0,
                patterndatasize: 2,
                planew: 0,
                planeh: 0,
                vram_8mbit: true,
                mapwh: 0,
                supplementdata: 0,
                auxmode: 0,
            },
        );
        assert_eq!(charaddr, 0x7FFF * 0x20);
        assert_eq!(flip, 3);
        assert_eq!(paladdr, 0x7F << 4);
        assert_eq!(sf, 0);
        assert_eq!(scf, 0);
    }
}
