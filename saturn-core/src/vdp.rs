use arc_swap::ArcSwap;
use std::sync::Arc;
use crate::shared_buffers::WorkRam;

/// One completed video frame, in the exact pixel format `minifb` (and most
/// software framebuffer consumers) expect directly: one `u32` per pixel,
/// `0x00RRGGBB`. Real Saturn hardware's own final color format -- verified
/// against Yabause's `COLSAT2YAB32` macro -- is the same R/G/B byte layout,
/// just wrapped with a priority/alpha byte we don't need here.
pub struct Framebuffer {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
}

impl Framebuffer {
    pub fn new(width: usize, height: usize) -> Self {
        Self { width, height, pixels: vec![0; width * height] }
    }

    fn fill(&mut self, color: u32) {
        self.pixels.fill(color);
    }
}

pub struct Vdp {
    /// Lock-free published frame: the renderer publishes a whole new frame
    /// here each time one completes; readers (e.g. a window's present loop)
    /// just load the latest one, never blocking the renderer and never
    /// tearing mid-frame.
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

/// TVMD (offset 0x000 in the VDP2 register window): display enable + screen
/// resolution. Bit positions and the H/V resolution tables below are from
/// `VIDSoftVdp2SetResolution` in a real, working VDP2 implementation
/// (Yabause `vidsoft.c`).
const REG_TVMD: usize = 0x000;
/// BKTAU/BKTAL (offsets 0x0AC/0x0AE): backdrop color source. Real hardware
/// can select between "one solid color" and "per-scanline color fetched
/// from VRAM" via a mode bit; this only implements the solid-color case
/// (reading BKTAL directly as an RGB555 value), which is the simplest
/// correct behavior for the vast majority of screens including the BIOS
/// boot backdrop. Per-scanline gradients would render as flat color
/// instead of a gradient with this simplification -- a visible but
/// non-crashing gap, not a silent correctness bug.
const REG_BKTAL: usize = 0x0AE;

fn read_reg_word(regs: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([regs[offset], regs[offset + 1]])
}

fn resolution_from_tvmd(tvmd: u16) -> (usize, usize) {
    let width = match tvmd & 0x7 {
        0 | 4 => 320,
        1 | 5 => 352,
        2 | 6 => 640,
        _ => 704, // 3 | 7
    };
    let height = match (tvmd >> 4) & 0x3 {
        0 => 224,
        1 => 240,
        _ => 256, // 2 (PAL only, but a harmless fallback for 3 too)
    };
    (width, height)
}

/// Saturn's native color format is 15-bit RGB (5 bits per channel, R in the
/// low bits). Expansion to 8 bits per channel replicates the top 3 bits
/// into the low bits (`(c5<<3)|(c5>>2)`) so 0x1F maps to 0xFF instead of
/// 0xF8, the standard bit-replication technique for N-to-8-bit channel
/// widening.
fn rgb555_to_xrgb8888(color: u16) -> u32 {
    let r5 = (color & 0x1F) as u32;
    let g5 = ((color >> 5) & 0x1F) as u32;
    let b5 = ((color >> 10) & 0x1F) as u32;
    let r8 = (r5 << 3) | (r5 >> 2);
    let g8 = (g5 << 3) | (g5 >> 2);
    let b8 = (b5 << 3) | (b5 >> 2);
    (r8 << 16) | (g8 << 8) | b8
}

/// Render exactly the VDP2 backdrop (solid color, no NBG tile layers) at
/// the screen's real configured resolution. This is deliberately the
/// smallest possible real rendering step: it reads genuine register state
/// (TVMD, BKTAL) instead of drawing a placeholder, so it can prove real
/// register writes reach the screen before the much larger NBG tile
/// decoding pipeline (pattern names, character data, CRAM lookup) exists.
pub fn render_backdrop(ram: &WorkRam) -> Framebuffer {
    // One held read-guard spans both register reads below (TVMD, then
    // BKTAL): `vdp2_regs` is its own lock now (see `shared_buffers.rs`),
    // and this function no longer rides on a caller-held whole-`WorkRam`
    // lock the way it used to -- acquiring twice here would let a
    // concurrent SH-2 write land between the two reads.
    let regs = ram.vdp2_regs.read().unwrap();
    let tvmd = read_reg_word(&regs[..], REG_TVMD);
    let (width, height) = resolution_from_tvmd(tvmd);
    let disp_enabled = tvmd & 0x8000 != 0;

    let mut frame = Framebuffer::new(width, height);
    if !disp_enabled {
        // Display off: real hardware outputs black, not "whatever was there
        // before" -- avoid stale-frame confusion while DISP is being set up.
        return frame;
    }
    let backdrop = read_reg_word(&regs[..], REG_BKTAL);
    drop(regs);
    frame.fill(rgb555_to_xrgb8888(backdrop));
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
        assert_eq!(rgb555_to_xrgb8888(0x7FFF), 0x00FFFFFF, "all channels at max must be white");
        assert_eq!(rgb555_to_xrgb8888(0x001F), 0x00FF0000, "R is the low 5 bits, must land in the R byte");
        assert_eq!(rgb555_to_xrgb8888(0x03E0), 0x0000FF00, "G is bits 5-9, must land in the G byte");
        assert_eq!(rgb555_to_xrgb8888(0x7C00), 0x000000FF, "B is bits 10-14, must land in the B byte");
        assert_eq!(rgb555_to_xrgb8888(0), 0);
    }

    #[test]
    fn render_backdrop_reads_real_registers() {
        let mut ram = WorkRam::new();
        // TVMD: DISP on, 320x224 (all zero apart from the DISP bit)
        {
            let regs = ram.vdp2_regs.get_mut().unwrap();
            regs[REG_TVMD] = 0x80;
            regs[REG_TVMD + 1] = 0x00;
            // BKTAL: pure blue (bits 10-14)
            let blue = 0x1F << 10;
            regs[REG_BKTAL] = (blue >> 8) as u8;
            regs[REG_BKTAL + 1] = (blue & 0xFF) as u8;
        }

        let frame = render_backdrop(&ram);
        assert_eq!((frame.width, frame.height), (320, 224));
        assert_eq!(frame.pixels[0], 0x0000FF, "backdrop must reflect the real BKTAL register, not a placeholder");
        assert!(frame.pixels.iter().all(|&p| p == 0x0000FF), "backdrop must fill the whole frame");
    }

    #[test]
    fn render_backdrop_is_black_when_display_disabled() {
        let ram = WorkRam::new(); // TVMD all zero: DISP bit unset
        let frame = render_backdrop(&ram);
        assert!(frame.pixels.iter().all(|&p| p == 0));
    }
}
