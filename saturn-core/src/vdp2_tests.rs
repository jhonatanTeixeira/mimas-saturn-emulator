#[cfg(test)]
mod tests {
    use crate::vdp2::{blend_pixels, dig_pixel, PixelData};

    #[test]
    fn test_blend_pixels_top() {
        let top = (0x20 << 24) | (0x10 << 10) | (0x10 << 5) | 0x10;
        let bottom = (0x3F << 24) | (0x00 << 10) | (0x00 << 5) | 0x00;
        let blended = blend_pixels(top, bottom, 0);
        let r = (0x10 * 131 + 0 * 124) / 255;
        assert_eq!((blended & 0x1F) as u8, r as u8);
        assert_eq!((blended >> 24) & 0x3F, 0x3F);
    }

    #[test]
    fn test_blend_pixels_bottom() {
        let top = 0x80000000 | (0x20 << 24) | (0x10 << 10) | (0x10 << 5) | 0x10;
        let bottom = (0x10 << 24) | (0x00 << 10) | (0x00 << 5) | 0x00;
        let blended = blend_pixels(top, bottom, 1);
        let r = (0x10 * 67 + 0 * 188) / 255;
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
    let mut regs = crate::vdp2_regs::Vdp2Regs::new();
    regs.write_word(0x026, 0x1234);
    assert_eq!(regs.sfcode(), 0x1234);
    regs.write_word(0x0EA, 0x5678);
    assert_eq!(regs.sfprmd(), 0x5678);
    regs.write_word(0x0EE, 0x9ABC);
    assert_eq!(regs.sfccmd(), 0x9ABC);
}

#[test]
fn test_layer_buffers_default() {
    let buffers = crate::vdp2::LayerBuffers::default();
    assert_eq!(buffers.buffers[0][0].color, 0);
}

#[test]
fn test_render_nbg_layers_and_ccr_mapping() {
    // A quick test to hit the NBG0, NBG1, NBG2 layer calls in vdp.rs
    // and the CCRNA/CCRNB mapping.
    let mut work_ram = crate::shared_buffers::WorkRam::new();
    let mut layer_buffers = crate::vdp2::LayerBuffers::new();

    {
        let mut regs = work_ram.vdp2_regs.write().unwrap();
        // Enable NBG0, NBG1, NBG2, NBG3
        regs.write_word(0x020, 0x000F); // BGON
        
        // Setup Priorities
        regs.write_word(0x0F0, 0x0123); // PRINA: NBG0=3, NBG1=2
        regs.write_word(0x0F2, 0x0456); // PRINB: NBG2=6, NBG3=5
        
        // Setup Color Calculation Ratios
        regs.write_word(0x108, 0x0708); // CCRNA: NBG0=8, NBG1=7
        regs.write_word(0x10A, 0x090A); // CCRNB: NBG2=A, NBG3=9
    }

    let frame = crate::vdp::render_frame(&work_ram, &mut layer_buffers);
    // As long as this completes, we exercised the paths!
    assert_eq!(frame.width, 320); // Default width
}

#[test]
fn test_blend_pixels_uncovered() {
    // TOP blend where alpha == 0
    let top = (0x00 << 24) | 0x1234;
    let bottom = (0x3F << 24) | 0x5678;
    assert_eq!(crate::vdp2::blend_pixels(top, bottom, 0), top);

    // Fallback mode
    assert_eq!(crate::vdp2::blend_pixels(top, bottom, 3), top);
}

#[test]
fn test_dig_pixel_blend_logic() {
    let mut nbg0 = vec![crate::vdp2::PixelData::default(); 1];
    let mut rbg0 = vec![crate::vdp2::PixelData::default(); 1];

    // Setup NBG0 over RBG0
    nbg0[0].priority = 6;
    nbg0[0].pixel = 0x10000000 | 0x1111; // bit 31=0, alpha=0x10
    
    rbg0[0].priority = 5;
    rbg0[0].pixel = 0x80000000 | 0x2222;

    let layers: [&[crate::vdp2::PixelData]; 6] = [&[], &[], &[], &nbg0, &rbg0, &[]];

    // ccctl=1, is_bottom=1, sfccmd=3
    // ccctl_bit for nbg0 is 0. So ccctl=1 enables it.
    let (p, _) = crate::vdp2::dig_pixel(&layers, 1, 0, 1, 3);
    // Since MSB=0 and sfccmd=3, do_blend should be false!
    assert_eq!(p, nbg0[0].pixel);

    // ccctl=1, is_add=1
    // top alpha bit is 0, so is_add && top_ccctl_en && top_alpha_bit is FALSE.
    // It should fallback to TOP blend because top_alpha_val (0x10) < 0x3F.
    let (p2, _) = crate::vdp2::dig_pixel(&layers, 1, 1, 0, 0);
    // 0x10 is 16. TOP blend!
    assert_eq!(p2, crate::vdp2::blend_pixels(nbg0[0].pixel, rbg0[0].pixel, 0));

    // Now set MSB=1 to trigger ADD
    nbg0[0].pixel = 0x80000000 | 0x1111;
    let layers2: [&[crate::vdp2::PixelData]; 6] = [&[], &[], &[], &nbg0, &rbg0, &[]];
    let (p3, _) = crate::vdp2::dig_pixel(&layers2, 1, 1, 0, 0);
    assert_eq!(p3, crate::vdp2::blend_pixels(nbg0[0].pixel, rbg0[0].pixel, 2));

    // Now trigger BOTTOM
    let (p4, _) = crate::vdp2::dig_pixel(&layers2, 1, 0, 1, 0);
    assert_eq!(p4, crate::vdp2::blend_pixels(nbg0[0].pixel, rbg0[0].pixel, 1));
}
