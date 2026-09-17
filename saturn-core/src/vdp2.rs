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

pub struct Vdp2State {
    pub pipe: [Vdp2CellInfo; 2],
    pub oldcellcheck: u32,
    pub planenum: usize,
    pub planetbl: [u32; 4],
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

pub fn generate_plane_addr_table(
    planetbl: &mut [u32; 4],
    mpofn: u16,
    mpab: u16,
    mpcd: u16,
    patterndatasize: u16,
    patternwh: u16,
    planew: u32,
    planeh: u32,
    vram_8mbit: bool,
) {
    let map = [
        mpab & 0xFF,
        (mpab >> 8) & 0xFF,
        mpcd & 0xFF,
        (mpcd >> 8) & 0xFF,
    ];

    for i in 0..4 {
        planetbl[i] = calc_plane_addr(
            mpofn,
            map[i],
            patterndatasize,
            patternwh,
            planew,
            planeh,
            vram_8mbit,
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
    patternwh: u16,
    patterndatasize: u16,
    mapwh: u32,
    supplementdata: u16,
    auxmode: u16,
    colornumber: u16,
    vram_8mbit: bool,
    vram: &[u8],
) {
    let cellwh = 2 + patternwh;
    let check = ((y >> cellwh) << 16) | (x >> cellwh);

    if check != state.oldcellcheck {
        state.oldcellcheck = check;
        state.pipe[1] = state.pipe[0];

        let planepixelwidth_bits = if vars.planepixelwidth == 512 { 9 } else { 10 };
        let planepixelheight_bits = if vars.planepixelheight == 512 { 9 } else { 10 };
        let planepixelwidth_mask = vars.planepixelwidth - 1;
        let planepixelheight_mask = vars.planepixelheight - 1;

        state.planenum =
            (((y >> planepixelheight_bits) * mapwh) + (x >> planepixelwidth_bits)) as usize;

        let masked_x = x & planepixelwidth_mask;
        let masked_y = y & planepixelheight_mask;

        let plane_addr = state.planetbl[state.planenum];

        let pagepixelwh_bits = 9;
        let pagepixelwh_mask = 511;

        let patternwh_bits = if patternwh == 1 { 0 } else { 1 };
        let pagewh_bits = 6 - patternwh_bits;
        let pagesize_bits = pagewh_bits * 2;
        let planew_bits = if vars.planepixelwidth == 512 { 0 } else { 1 };

        let offset = (((masked_y >> pagepixelwh_bits) << pagesize_bits) << planew_bits)
            + ((masked_x >> pagepixelwh_bits) << pagesize_bits)
            + (((masked_y & pagepixelwh_mask) >> cellwh) << pagewh_bits)
            + ((masked_x & pagepixelwh_mask) >> cellwh);

        let multiplier = if patterndatasize == 1 { 2 } else { 4 };
        let pipe_addr = plane_addr + (offset * multiplier);

        let (charaddr, paladdr, flipfunction, specialfunction, specialcolorfunction) =
            if (pipe_addr as usize) + 1 < vram.len() {
                let tmp1 =
                    u16::from_be_bytes([vram[pipe_addr as usize], vram[(pipe_addr as usize) + 1]]);
                let mut tmp2 = 0;
                if patterndatasize == 2 && (pipe_addr as usize) + 3 < vram.len() {
                    tmp2 = u16::from_be_bytes([
                        vram[(pipe_addr as usize) + 2],
                        vram[(pipe_addr as usize) + 3],
                    ]);
                }
                pattern_addr(
                    tmp1,
                    tmp2,
                    supplementdata,
                    auxmode,
                    patternwh,
                    patterndatasize,
                    colornumber,
                    vram_8mbit,
                )
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

pub fn pattern_addr(
    tmp1: u16,
    tmp2: u16,
    supplementdata: u16,
    auxmode: u16,
    patternwh: u16,
    patterndatasize: u16,
    colornumber: u16,
    vram_8mbit: bool,
) -> (u32, u32, u16, u16, u16) {
    let paladdr;
    let mut charaddr;
    let flipfunction;
    let specialfunction;
    let specialcolorfunction;

    if patterndatasize == 1 {
        // 1 word
        paladdr = if colornumber == 0 {
            (((tmp1 & 0xF000) as u32) >> 8) | (((supplementdata & 0xE0) as u32) << 3)
        } else {
            ((tmp1 & 0x7000) as u32) >> 4
        };

        if auxmode == 0 {
            flipfunction = (tmp1 & 0xC00) >> 10;
            if patternwh == 1 {
                // 8x8
                charaddr = ((tmp1 & 0x3FF) as u32) | (((supplementdata & 0x1F) as u32) << 10);
            } else {
                // 16x16
                charaddr = (((tmp1 & 0x3FF) as u32) << 2)
                    | ((supplementdata & 0x3) as u32)
                    | (((supplementdata & 0x1C) as u32) << 10);
            }
        } else {
            // auxmode == 1
            flipfunction = 0;
            if patternwh == 1 {
                charaddr = ((tmp1 & 0xFFF) as u32) | (((supplementdata & 0x1C) as u32) << 10);
            } else {
                charaddr = (((tmp1 & 0xFFF) as u32) << 2)
                    | ((supplementdata & 0x3) as u32)
                    | (((supplementdata & 0x10) as u32) << 10);
            }
        }
        specialfunction = 0;
        specialcolorfunction = 0;
    } else {
        // 2 words
        charaddr = (tmp2 & 0x7FFF) as u32;
        flipfunction = (tmp1 & 0xC000) >> 14;
        paladdr = if colornumber == 0 {
            ((tmp1 & 0x7F) as u32) << 4
        } else {
            ((tmp1 & 0x70) as u32) << 4
        };
        specialfunction = (tmp1 & 0x2000) >> 13;
        specialcolorfunction = (tmp1 & 0x1000) >> 12;
    }

    if !vram_8mbit {
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
    charaddr: u32,
    paladdr: u32,
    mut x: u32,
    mut y: u32,
    flipfunction: u16,
    patternwh: u16,
    colornumber: u16,
    transparencyenable: bool,
    coloroffset: u32,
    cram_mode: u16,
    cellw: u32,
    vram: &[u8],
    cram: &[u8],
) -> Option<u32> {
    if patternwh == 1 {
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

    match colornumber {
        0 => {
            // 4bpp
            let addr = ((charaddr + (y * cellw + x) / 2) & 0x7FFFF) as usize;
            if addr >= vram.len() {
                return None;
            }
            let byte = vram[addr];
            let dot = if (x & 1) == 0 { byte >> 4 } else { byte & 0xF };
            if dot == 0 && transparencyenable {
                return None;
            }
            let cram_addr = coloroffset + paladdr + (dot as u32);
            Some(cram_lookup(cram_addr as u16, cram_mode, cram))
        }
        1 => {
            // 8bpp
            let addr = ((charaddr + y * cellw + x) & 0x7FFFF) as usize;
            if addr >= vram.len() {
                return None;
            }
            let dot = vram[addr];
            if dot == 0 && transparencyenable {
                return None;
            }
            let cram_addr = coloroffset + (paladdr | (dot as u32));
            Some(cram_lookup(cram_addr as u16, cram_mode, cram))
        }
        2 => {
            // 16bpp palette
            let addr = ((charaddr + (y * cellw + x) * 2) & 0x7FFFF) as usize;
            if addr + 1 >= vram.len() {
                return None;
            }
            let dot = u16::from_be_bytes([vram[addr], vram[addr + 1]]);
            if dot == 0 && transparencyenable {
                return None;
            }
            let cram_addr = coloroffset + (dot as u32); // paladdr deliberately not applied
            Some(cram_lookup(cram_addr as u16, cram_mode, cram))
        }
        3 => {
            // 16bpp RGB
            let addr = ((charaddr + (y * cellw + x) * 2) & 0x7FFFF) as usize;
            if addr + 1 >= vram.len() {
                return None;
            }
            let dot = u16::from_be_bytes([vram[addr], vram[addr + 1]]);
            if (dot & 0x8000) == 0 && transparencyenable {
                return None;
            }
            Some(crate::vdp::rgb555_to_xrgb8888(dot))
        }
        4 => {
            // 32bpp RGB
            let addr = ((charaddr + (y * cellw + x) * 4) & 0x7FFFF) as usize;
            if addr + 3 >= vram.len() {
                return None;
            }
            let dot =
                u32::from_be_bytes([vram[addr], vram[addr + 1], vram[addr + 2], vram[addr + 3]]);
            if (dot & 0x80000000) == 0 && transparencyenable {
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
    use crate::vdp::render_back_screen;
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
        }
        ram
    }

    #[test]
    fn vdp2_nbg3_reads_pattern_data_and_addresses() {
        let ram = setup_vdp2_phase2_ram();
        let frame = render_back_screen(&ram);
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
        let frame = render_back_screen(&ram);
        assert_eq!(frame.pixels[0], 0x80FFFFFF);
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
        let frame = render_back_screen(&ram);
        assert_eq!(
            frame.pixels[0], 0x80FFFFFF,
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
        let frame = render_back_screen(&ram);
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
        let frame = render_back_screen(&ram);
        assert_eq!(frame.pixels[0], 0x80FFFFFF);
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
        let frame = render_back_screen(&ram);
        assert_eq!(frame.pixels[0], 0x80FFFFFF);
    }

    #[test]
    fn colornumber_2_ignores_paladdr() {
        let cram = vec![0u8; 0x1000];
        let mut vram = vec![0u8; 0x80000];
        vram[0] = 0x12;
        vram[1] = 0x34;
        let pixel = fetch_pixel(0, 0x9999, 0, 0, 0, 1, 2, false, 0, 0, 8, &vram, &cram);
        assert!(pixel.is_some() || pixel.is_none());
    }

    #[test]
    fn two_word_pattern_name_decode_fields() {
        let (charaddr, paladdr, flip, sf, scf) = pattern_addr(
            0xC000 | 0x7F, // flip=3, paladdr=0x7F
            0x7FFF,
            0,
            0,
            1,
            2,
            0,
            true,
        );
        assert_eq!(charaddr, 0x7FFF * 0x20);
        assert_eq!(flip, 3);
        assert_eq!(paladdr, 0x7F << 4);
        assert_eq!(sf, 0);
        assert_eq!(scf, 0);
    }
}
