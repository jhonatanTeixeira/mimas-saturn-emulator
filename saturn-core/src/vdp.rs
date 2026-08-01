use crate::shared_buffers::WorkRam;
use arc_swap::ArcSwap;
use std::sync::Arc;

pub struct Framebuffer {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
}

impl Framebuffer {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width * height],
        }
    }

    fn fill(&mut self, color: u32) {
        self.pixels.fill(color);
    }
}

pub struct Vdp {
    pub front_buffer: Arc<ArcSwap<Framebuffer>>,
}

impl Vdp {
    pub fn new() -> Self {
        Self {
            front_buffer: Arc::new(ArcSwap::new(Arc::new(Framebuffer::new(320, 224)))),
        }
    }

    pub fn present(&self, frame: Framebuffer) {
        self.front_buffer.store(Arc::new(frame));
    }
}

impl Default for Vdp {
    fn default() -> Self {
        Self::new()
    }
}

const REG_TVMD: usize = 0x000;
const REG_BKTAL: usize = 0x0AE;

fn read_reg_word(regs: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([regs[offset], regs[offset + 1]])
}

fn resolution_from_tvmd(tvmd: u16) -> (usize, usize) {
    let width = match tvmd & 0x7 {
        0 | 4 => 320,
        1 | 5 => 352,
        2 | 6 => 640,
        _ => 704,
    };
    let height = match (tvmd >> 4) & 0x3 {
        0 => 224,
        1 => 240,
        _ => 256,
    };
    (width, height)
}

fn rgb555_to_xrgb8888(color: u16) -> u32 {
    let r5 = (color & 0x1F) as u32;
    let g5 = ((color >> 5) & 0x1F) as u32;
    let b5 = ((color >> 10) & 0x1F) as u32;
    let r8 = (r5 << 3) | (r5 >> 2);
    let g8 = (g5 << 3) | (g5 >> 2);
    let b8 = (b5 << 3) | (b5 >> 2);
    (r8 << 16) | (g8 << 8) | b8
}

pub fn execute_vdp1(ram: &WorkRam) {
    let regs = ram.vdp1_regs.read().unwrap();
    let ptmr = u16::from_be_bytes([regs[0], regs[1]]);
    if ptmr & 1 == 0 {
        return;
    }
    drop(regs);

    let mut vram = ram.vdp1_vram.write().unwrap();
    let mut fb = ram.vdp1_framebuffer.write().unwrap();

    let mut cmd_addr = 0usize;
    loop {
        if cmd_addr + 32 > vram.len() {
            break;
        }
        let cmdctrl = u16::from_be_bytes([vram[cmd_addr], vram[cmd_addr + 1]]);
        let cmdcolr = u16::from_be_bytes([vram[cmd_addr + 4], vram[cmd_addr + 5]]);

        let xa = i16::from_be_bytes([vram[cmd_addr + 8], vram[cmd_addr + 9]]) as i32;
        let ya = i16::from_be_bytes([vram[cmd_addr + 10], vram[cmd_addr + 11]]) as i32;
        let xb = i16::from_be_bytes([vram[cmd_addr + 12], vram[cmd_addr + 13]]) as i32;
        let yb = i16::from_be_bytes([vram[cmd_addr + 14], vram[cmd_addr + 15]]) as i32;
        let xc = i16::from_be_bytes([vram[cmd_addr + 16], vram[cmd_addr + 17]]) as i32;
        let yc = i16::from_be_bytes([vram[cmd_addr + 18], vram[cmd_addr + 19]]) as i32;

        let comm_type = cmdctrl & 0x000F;
        if comm_type == 4 {
            // Polygon / Quad Draw
            let min_x = xa.min(xb).min(xc).max(0) as usize;
            let max_x = xa.max(xb).max(xc).min(319) as usize;
            let min_y = ya.min(yb).min(yc).max(0) as usize;
            let max_y = ya.max(yb).max(yc).min(223) as usize;

            for y in min_y..=max_y {
                for x in min_x..=max_x {
                    let offset = (y * 320 + x) * 2;
                    if offset + 1 < fb.len() {
                        let bytes = cmdcolr.to_be_bytes();
                        fb[offset] = bytes[0];
                        fb[offset + 1] = bytes[1];
                    }
                }
            }
        }

        if (cmdctrl & 0x8000) != 0 {
            break;
        }
        cmd_addr += 32;
    }
}

pub fn render_backdrop(ram: &WorkRam) -> Framebuffer {
    let regs = ram.vdp2_regs.read().unwrap();
    let tvmd = read_reg_word(&regs[..], REG_TVMD);
    let (width, height) = resolution_from_tvmd(tvmd);
    let disp_enabled = tvmd & 0x8000 != 0;

    let mut frame = Framebuffer::new(width, height);
    if !disp_enabled {
        return frame;
    }
    let backdrop = read_reg_word(&regs[..], REG_BKTAL);
    drop(regs);
    frame.fill(rgb555_to_xrgb8888(backdrop));

    // Overlay VDP1 Framebuffer if active
    let vdp1_fb = ram.vdp1_framebuffer.read().unwrap();
    for y in 0..height {
        for x in 0..width {
            let offset = (y * 320 + x) * 2;
            if offset + 1 < vdp1_fb.len() {
                let color16 = u16::from_be_bytes([vdp1_fb[offset], vdp1_fb[offset + 1]]);
                if color16 != 0 {
                    let pixel_idx = y * width + x;
                    if pixel_idx < frame.pixels.len() {
                        frame.pixels[pixel_idx] = rgb555_to_xrgb8888(color16);
                    }
                }
            }
        }
    }

    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_decodes_common_ntsc_modes() {
        assert_eq!(resolution_from_tvmd(0x0000), (320, 224));
        assert_eq!(resolution_from_tvmd(0x0001), (352, 224));
        assert_eq!(resolution_from_tvmd(0x0010), (320, 240));
    }

    #[test]
    fn rgb555_conversion_hits_full_white_and_pure_channels() {
        assert_eq!(
            rgb555_to_xrgb8888(0x7FFF),
            0x00FFFFFF,
            "all channels at max must be white"
        );
        assert_eq!(
            rgb555_to_xrgb8888(0x001F),
            0x00FF0000,
            "R is the low 5 bits, must land in the R byte"
        );
        assert_eq!(
            rgb555_to_xrgb8888(0x03E0),
            0x0000FF00,
            "G is bits 5-9, must land in the G byte"
        );
        assert_eq!(
            rgb555_to_xrgb8888(0x7C00),
            0x000000FF,
            "B is bits 10-14, must land in the B byte"
        );
        assert_eq!(rgb555_to_xrgb8888(0), 0);
    }

    #[test]
    fn render_backdrop_reads_real_registers() {
        let mut ram = WorkRam::new();
        {
            let regs = ram.vdp2_regs.get_mut().unwrap();
            regs[REG_TVMD] = 0x80;
            regs[REG_TVMD + 1] = 0x00;
            let blue = 0x1F << 10;
            regs[REG_BKTAL] = (blue >> 8) as u8;
            regs[REG_BKTAL + 1] = (blue & 0xFF) as u8;
        }

        let frame = render_backdrop(&ram);
        assert_eq!((frame.width, frame.height), (320, 224));
        assert_eq!(frame.pixels[0], 0x0000FF);
        assert!(frame.pixels.iter().all(|&p| p == 0x0000FF));
    }

    #[test]
    fn render_backdrop_is_black_when_display_disabled() {
        let ram = WorkRam::new();
        let frame = render_backdrop(&ram);
        assert!(frame.pixels.iter().all(|&p| p == 0));
    }

    #[test]
    fn test_vdp1_polygon_drawing() {
        let mut ram = WorkRam::new();
        // TVMD: DISP on, 320x224
        {
            let regs = ram.vdp2_regs.get_mut().unwrap();
            regs[REG_TVMD] = 0x80;
            // BKTAL: pure blue
            let blue = 0x1F << 10;
            regs[REG_BKTAL] = (blue >> 8) as u8;
            regs[REG_BKTAL + 1] = (blue & 0xFF) as u8;
        }

        // PTMR: enable VDP1 drawing (1)
        {
            let regs = ram.vdp1_regs.get_mut().unwrap();
            regs[0] = 0x00;
            regs[1] = 0x01;
        }

        // Configure VDP1 CMD list in VDP1 VRAM:
        // CMDCTRL: polygon command (4) with end bit set (0x8000) -> 0x8004
        // CMDCOLR: pure red (0x001F)
        // Coordinates: XA=10, YA=10, XB=20, YB=10, XC=20, YC=20
        {
            let vram = ram.vdp1_vram.get_mut().unwrap();
            let cmdctrl = 0x8004u16;
            let cmdcolr = 0x001Fu16;
            vram[0..2].copy_from_slice(&cmdctrl.to_be_bytes());
            vram[4..6].copy_from_slice(&cmdcolr.to_be_bytes());

            vram[8..10].copy_from_slice(&10i16.to_be_bytes()); // XA
            vram[10..12].copy_from_slice(&10i16.to_be_bytes()); // YA
            vram[12..14].copy_from_slice(&20i16.to_be_bytes()); // XB
            vram[14..16].copy_from_slice(&10i16.to_be_bytes()); // YB
            vram[16..18].copy_from_slice(&20i16.to_be_bytes()); // XC
            vram[18..20].copy_from_slice(&20i16.to_be_bytes()); // YC
        }

        execute_vdp1(&ram);
        let frame = render_backdrop(&ram);

        // Pixel at (15, 15) must be red (0xFF0000)
        assert_eq!(frame.pixels[15 * 320 + 15], 0xFF0000);
        // Pixel at (0, 0) must remain blue (0x0000FF)
        assert_eq!(frame.pixels[0], 0x0000FF);
    }
}
