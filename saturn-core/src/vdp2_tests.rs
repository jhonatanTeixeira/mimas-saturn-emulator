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
    }
}
