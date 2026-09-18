#[cfg(test)]
mod tests {
    use crate::vdp2::{blend_pixels, dig_pixel, PixelData};

    #[test]
    fn test_blend_pixels_top() {
        let top = (0x20 << 24) | (0x10 << 10) | (0x10 << 5) | 0x10;
        let bottom = 0x3F << 24;
        let blended = blend_pixels(top, bottom, 0);
        let r = (0x10 * 131) / 255;
        assert_eq!((blended & 0x1F) as u8, r as u8);
        assert_eq!((blended >> 24) & 0x3F, 0x3F);
    }

    #[test]
    fn test_blend_pixels_bottom() {
        let top = 0x80000000 | (0x20 << 24) | (0x10 << 10) | (0x10 << 5) | 0x10;
        let bottom = 0x10 << 24;
        let blended = blend_pixels(top, bottom, 1);
        let r = (0x10 * 67) / 255;
        assert_eq!((blended & 0x1F) as u8, r as u8);
        assert_eq!((blended >> 24) & 0x3F, 0x20);
    }

    #[test]
    fn test_blend_pixels_add() {
        let top = (0x20 << 24) | (0x10 << 10) | (0x10 << 5) | 0x10;
        let bottom = (0x3F << 24) | (0x0A << 10) | (0x0A << 5) | 0x0A;
        let blended = blend_pixels(top, bottom, 2);
        assert_eq!((blended & 0x1F) as u8, 0x1A);
    }

    #[test]
    fn test_dig_pixel_priority_tie_break() {
        let mut nbg3 = vec![PixelData::default(); 1];
        let mut nbg2 = vec![PixelData::default(); 1];
        let mut nbg1 = vec![PixelData::default(); 1];
        let mut nbg0 = vec![PixelData::default(); 1];
        let mut rbg0 = vec![PixelData::default(); 1];
        let mut sprite = vec![PixelData::default(); 1];

        nbg3[0].priority = 7;
        nbg3[0].pixel = 0x3F333333;
        nbg2[0].priority = 7;
        nbg2[0].pixel = 0x3F222222;
        nbg1[0].priority = 7;
        nbg1[0].pixel = 0x3F111111;
        nbg0[0].priority = 7;
        nbg0[0].pixel = 0x3F000000;
        rbg0[0].priority = 7;
        rbg0[0].pixel = 0x3F444444;
        sprite[0].priority = 7;
        sprite[0].pixel = 0x3F555555;

        let layers: [&[PixelData]; 6] = [&nbg3, &nbg2, &nbg1, &nbg0, &rbg0, &sprite];
        let (p, prio) = dig_pixel(&layers, 0, 0, 0, 0);

        assert_eq!(p, 0x3F555555);
        assert_eq!(prio, 7);

        sprite[0].priority = 6;
        let layers: [&[PixelData]; 6] = [&nbg3, &nbg2, &nbg1, &nbg0, &rbg0, &sprite];
        let (p2, _) = dig_pixel(&layers, 0, 0, 0, 0);
        assert_eq!(p2, 0x3F444444);

        rbg0[0].priority = 6;
        let layers: [&[PixelData]; 6] = [&nbg3, &nbg2, &nbg1, &nbg0, &rbg0, &sprite];
        let (p3, _) = dig_pixel(&layers, 0, 0, 0, 0);
        assert_eq!(p3, 0x3F000000);

        nbg0[0].priority = 6;
        let layers: [&[PixelData]; 6] = [&nbg3, &nbg2, &nbg1, &nbg0, &rbg0, &sprite];
        let (p4, _) = dig_pixel(&layers, 0, 0, 0, 0);
        assert_eq!(p4, 0x3F111111);

        nbg1[0].priority = 6;
        let layers: [&[PixelData]; 6] = [&nbg3, &nbg2, &nbg1, &nbg0, &rbg0, &sprite];
        let (p5, _) = dig_pixel(&layers, 0, 0, 0, 0);
        assert_eq!(p5, 0x3F222222);

        nbg2[0].priority = 6;
        let layers: [&[PixelData]; 6] = [&nbg3, &nbg2, &nbg1, &nbg0, &rbg0, &sprite];
        let (p6, _) = dig_pixel(&layers, 0, 0, 0, 0);
        assert_eq!(p6, 0x3F333333);
    }
}

#[test]
fn test_vdp2_regs_getters() {
    let mut regs = crate::vdp2_regs::Vdp2Registers::new();
    regs.regs[0x026 / 2] = 0x1234;
    regs.regs[0x0EA / 2] = 0x5678;
    regs.regs[0x0EE / 2] = 0x9ABC;
    assert_eq!(regs.sfcode(), 0x1234);
    assert_eq!(regs.sfprmd(), 0x5678);
    assert_eq!(regs.sfccmd(), 0x9ABC);
}

#[test]
fn test_layer_buffers_default() {
    let buffers = crate::vdp2::LayerBuffers::default();
    assert_eq!(buffers.buffers[0][0].pixel, 0);
}

#[test]
fn test_render_nbg_layers_and_ccr_mapping() {
    let work_ram = crate::shared_buffers::WorkRam::new();
    let mut layer_buffers = crate::vdp2::LayerBuffers::new();

    {
        let mut lines = work_ram.vdp2_lines.write().unwrap();

        // DISP=1
        lines[0][0] = 0x80;
        lines[0][1] = 0x00;

        // VBLANK=1
        lines[0][2] = 0x00;
        lines[0][3] = 0x08;

        // BGON
        lines[0][0x020] = 0x00;
        lines[0][0x021] = 0x0F;

        // PRINA
        lines[0][0x0F0] = 0x01;
        lines[0][0x0F1] = 0x23;

        // PRINB
        lines[0][0x0F2] = 0x04;
        lines[0][0x0F3] = 0x56;

        // CCRNA
        lines[0][0x108] = 0x07;
        lines[0][0x109] = 0x08;

        // CCRNB
        lines[0][0x10A] = 0x09;
        lines[0][0x10B] = 0x0A;
    }

    let frame = crate::vdp::render_frame(&work_ram, &mut layer_buffers);
    assert_eq!(frame.width, 320);
}

#[test]
fn test_blend_pixels_uncovered() {
    let top = 0x1234;
    let bottom = (0x3F << 24) | 0x5678;
    // Fallback mode
    assert_eq!(crate::vdp2::blend_pixels(top, bottom, 3), top);
}

#[test]
fn test_dig_pixel_blend_logic() {
    let empty = vec![crate::vdp2::PixelData::default(); 1];
    let mut nbg0 = vec![crate::vdp2::PixelData::default(); 1];
    let mut rbg0 = vec![crate::vdp2::PixelData::default(); 1];

    nbg0[0].priority = 6;
    nbg0[0].pixel = 0x10000000 | 0x1111;

    rbg0[0].priority = 5;
    rbg0[0].pixel = 0x80000000 | 0x2222;

    let layers: [&[crate::vdp2::PixelData]; 6] = [&empty, &empty, &empty, &nbg0, &rbg0, &empty];

    // Cover different modes and branches
    let _ = crate::vdp2::dig_pixel(&layers, 0, 0, 3, 1);
    let _ = crate::vdp2::dig_pixel(&layers, 0, 0, 0, 1);

    nbg0[0].pixel = 0x80000000 | 0x1111;
    let layers2: [&[crate::vdp2::PixelData]; 6] = [&empty, &empty, &empty, &nbg0, &rbg0, &empty];
    let _ = crate::vdp2::dig_pixel(&layers2, 0, 0, 0, 0x101); // ADD
    let _ = crate::vdp2::dig_pixel(&layers2, 0, 0, 0, 0x201); // BOTTOM
    
}
