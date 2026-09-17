import re

tests_code = """
    #[test]
    fn vdp1_scaled_sprite_zp_upper_left() {
        // vdp1_scaled_sprite_zp_upper_left: CMDCTRL 0x0501, CMDXA 10, CMDYA 10, CMDXB 31, CMDYB 15
        // Hand-derived x1 = 32, y1 = 16 -> quad (10,10), (41,10), (41,25), (10,25)
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 2; // bypass fake_draw
        state.tvmr = 0x0008; // 512 width
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x05; vram[1] = 0x01; // CMDCTRL
            vram[4] = 0x00; vram[5] = 0xC0; // CMDPMOD SPD=1, ECD=1
            vram[6] = 0xFF; vram[7] = 0xFF; // CMDCOLR
            vram[8] = 0x00; vram[9] = 0x10; // CMDSRCA
            vram[10] = 0x02; vram[11] = 0x04; // CMDSIZE
            vram[12] = 0; vram[13] = 10; // CMDXA = 10
            vram[14] = 0; vram[15] = 10; // CMDYA = 10
            vram[16] = 0; vram[17] = 31; // CMDXB = 31
            vram[18] = 0; vram[19] = 15; // CMDYB = 15
            vram[0x20] = 0x80; vram[0x21] = 0x00; // END
            for i in 0x80..0xC0 { vram[i] = 0xFF; }
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        // check inside
        assert_ne!(u16::from_be_bytes([fb[(((10 * 512) + 10) * 2)], fb[(((10 * 512) + 10) * 2) + 1]]), 0);
        assert_ne!(u16::from_be_bytes([fb[(((25 * 512) + 41) * 2)], fb[(((25 * 512) + 41) * 2) + 1]]), 0);
        // check outside
        assert_eq!(u16::from_be_bytes([fb[(((9 * 512) + 10) * 2)], fb[(((9 * 512) + 10) * 2) + 1]]), 0);
        assert_eq!(u16::from_be_bytes([fb[(((26 * 512) + 41) * 2)], fb[(((26 * 512) + 41) * 2) + 1]]), 0);
    }

    #[test]
    fn vdp1_scaled_sprite_zp_two_point_matches_upper_left() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 2;
        state.tvmr = 0x0008;
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x00; vram[1] = 0x01; // CMDCTRL ZP=0
            vram[4] = 0x00; vram[5] = 0xC0; // CMDPMOD
            vram[6] = 0xFF; vram[7] = 0xFF;
            vram[8] = 0x00; vram[9] = 0x10;
            vram[10] = 0x02; vram[11] = 0x04;
            vram[12] = 0; vram[13] = 10; // CMDXA = 10
            vram[14] = 0; vram[15] = 10; // CMDYA = 10
            vram[16] = 0; vram[17] = 41; // CMDXC = 41
            vram[18] = 0; vram[19] = 25; // CMDYC = 25
            vram[0x20] = 0x80; vram[0x21] = 0x00;
            for i in 0x80..0xC0 { vram[i] = 0xFF; }
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        assert_ne!(u16::from_be_bytes([fb[(((10 * 512) + 10) * 2)], fb[(((10 * 512) + 10) * 2) + 1]]), 0);
        assert_ne!(u16::from_be_bytes([fb[(((25 * 512) + 41) * 2)], fb[(((25 * 512) + 41) * 2) + 1]]), 0);
        assert_eq!(u16::from_be_bytes([fb[(((9 * 512) + 10) * 2)], fb[(((9 * 512) + 10) * 2) + 1]]), 0);
    }

    #[test]
    fn vdp1_scaled_sprite_zp_two_point_ignores_local() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 2;
        state.tvmr = 0x0008;
        state.local_x = 100; // localX = 100
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x00; vram[1] = 0x01; // ZP=0
            vram[4] = 0x00; vram[5] = 0xC0;
            vram[6] = 0xFF; vram[7] = 0xFF;
            vram[8] = 0x00; vram[9] = 0x10;
            vram[10] = 0x02; vram[11] = 0x04;
            vram[12] = 0; vram[13] = 10; // CMDXA = 10
            vram[14] = 0; vram[15] = 10; // CMDYA = 10
            vram[16] = 0; vram[17] = 41; // CMDXC = 41
            vram[18] = 0; vram[19] = 25; // CMDYC = 25
            vram[0x20] = 0x80; vram[0x21] = 0x00;
            for i in 0x80..0xC0 { vram[i] = 0xFF; }
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        // The origin moves by localX (+100) -> X: 110. 
        // Size does NOT move! So it ends at 110 + 31 = 141.
        assert_ne!(u16::from_be_bytes([fb[(((10 * 512) + 110) * 2)], fb[(((10 * 512) + 110) * 2) + 1]]), 0);
        assert_ne!(u16::from_be_bytes([fb[(((25 * 512) + 141) * 2)], fb[(((25 * 512) + 141) * 2) + 1]]), 0);
        assert_eq!(u16::from_be_bytes([fb[(((10 * 512) + 10) * 2)], fb[(((10 * 512) + 10) * 2) + 1]]), 0);
    }

    #[test]
    fn vdp1_scaled_sprite_zp_centre_centre() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 2;
        state.tvmr = 0x0008;
        // ZP 0xA, 10, 10, 31, 15 -> (-5,3) to (26,18)
        state.local_x = 10; // offset it to positive just for array bounds check (0-512)
        state.local_y = 10;
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x0A; vram[1] = 0x01;
            vram[4] = 0x00; vram[5] = 0xC0;
            vram[6] = 0xFF; vram[7] = 0xFF;
            vram[8] = 0x00; vram[9] = 0x10;
            vram[10] = 0x02; vram[11] = 0x04;
            vram[12] = 0; vram[13] = 10;
            vram[14] = 0; vram[15] = 10;
            vram[16] = 0; vram[17] = 31;
            vram[18] = 0; vram[19] = 15;
            vram[0x20] = 0x80; vram[0x21] = 0x00;
            for i in 0x80..0xC0 { vram[i] = 0xFF; }
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        // tl: (-5+10, 3+10) = (5, 13)
        // br: (26+10, 18+10) = (36, 28)
        assert_ne!(u16::from_be_bytes([fb[(((13 * 512) + 5) * 2)], fb[(((13 * 512) + 5) * 2) + 1]]), 0);
        assert_ne!(u16::from_be_bytes([fb[(((28 * 512) + 36) * 2)], fb[(((28 * 512) + 36) * 2) + 1]]), 0);
        assert_eq!(u16::from_be_bytes([fb[(((12 * 512) + 5) * 2)], fb[(((12 * 512) + 5) * 2) + 1]]), 0);
    }

    #[test]
    fn vdp1_scaled_sprite_unimplemented_zp_falls_back_to_two_point() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 2;
        state.tvmr = 0x0008;
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x0C; vram[1] = 0x01; // ZP 0xC -> falls back to two point
            vram[4] = 0x00; vram[5] = 0xC0;
            vram[6] = 0xFF; vram[7] = 0xFF;
            vram[8] = 0x00; vram[9] = 0x10;
            vram[10] = 0x02; vram[11] = 0x04;
            vram[12] = 0; vram[13] = 10;
            vram[14] = 0; vram[15] = 10;
            vram[16] = 0; vram[17] = 41; // XC
            vram[18] = 0; vram[19] = 25; // YC
            vram[0x20] = 0x80; vram[0x21] = 0x00;
            for i in 0x80..0xC0 { vram[i] = 0xFF; }
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        assert_ne!(u16::from_be_bytes([fb[(((10 * 512) + 10) * 2)], fb[(((10 * 512) + 10) * 2) + 1]]), 0);
        assert_ne!(u16::from_be_bytes([fb[(((25 * 512) + 41) * 2)], fb[(((25 * 512) + 41) * 2) + 1]]), 0);
    }

    #[test]
    fn vdp1_distorted_sprite_vertex_order() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 2;
        state.tvmr = 0x0008;
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x00; vram[1] = 0x02; // COMM 2 Distorted Sprite
            vram[4] = 0x00; vram[5] = 0xC0;
            vram[6] = 0xFF; vram[7] = 0xFF;
            vram[8] = 0x00; vram[9] = 0x10;
            vram[10] = 0x02; vram[11] = 0x04;
            // (10, 10), (40, 15), (45, 40), (5, 30) -> A, B, C, D
            vram[12] = 0; vram[13] = 10; vram[14] = 0; vram[15] = 10; // A
            vram[16] = 0; vram[17] = 40; vram[18] = 0; vram[19] = 15; // B
            vram[20] = 0; vram[21] = 45; vram[22] = 0; vram[23] = 40; // C
            vram[24] = 0; vram[25] = 5;  vram[26] = 0; vram[27] = 30; // D
            vram[0x20] = 0x80; vram[0x21] = 0x00;
            for i in 0x80..0xC0 { vram[i] = 0xFF; }
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        // pixel at (20, 20) should be drawn inside quad
        assert_ne!(u16::from_be_bytes([fb[(((20 * 512) + 20) * 2)], fb[(((20 * 512) + 20) * 2) + 1]]), 0);
    }

    #[test]
    fn vdp1_polygon_and_distorted_sprite_have_identical_geometry() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 2;
        state.tvmr = 0x0008;
        
        // draw polygon
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x00; vram[1] = 0x04; // COMM 4 Polygon
            vram[4] = 0x00; vram[5] = 0xC0; // ECD=1, SPD=1
            vram[6] = 0xFF; vram[7] = 0xFF; // CMDCOLR
            vram[12] = 0; vram[13] = 10; vram[14] = 0; vram[15] = 10; // A
            vram[16] = 0; vram[17] = 40; vram[18] = 0; vram[19] = 15; // B
            vram[20] = 0; vram[21] = 45; vram[22] = 0; vram[23] = 40; // C
            vram[24] = 0; vram[25] = 5;  vram[26] = 0; vram[27] = 30; // D
            vram[0x20] = 0x80; vram[0x21] = 0x00;
        }
        execute_vdp1(&mut state, &ram);
        let mut fb1 = vec![0u8; 512 * 256 * 2];
        {
            let b = ram.vdp1_framebuffers.banks[0].read().unwrap();
            fb1.copy_from_slice(&b[..]);
        }
        
        // clear fb
        {
            let mut b = ram.vdp1_framebuffers.banks[0].write().unwrap();
            for i in b.iter_mut() { *i = 0; }
        }
        
        // draw distorted sprite
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x00; vram[1] = 0x02; // COMM 2 Distorted
            vram[4] = 0x00; vram[5] = 0xC0;
            vram[8] = 0x00; vram[9] = 0x10;
            vram[10] = 0x00; vram[11] = 0x00; // 8x1 (1 char)
            vram[0x20] = 0x80; vram[0x21] = 0x00;
            // 1 char = 32 bytes for 4bpp
            for i in 0x80..0xA0 { vram[i] = 0xFF; }
        }
        state.addr = 0;
        execute_vdp1(&mut state, &ram);
        
        let fb2 = ram.vdp1_framebuffers.banks[0].read().unwrap();
        // assert identical
        for i in 0..fb1.len() {
            // Note: color might differ slightly if polygon uses CMDCOLR exactly, 
            // and sprite uses texture + palette. But we are checking geometry mask.
            let a = fb1[i] != 0;
            let b = fb2[i] != 0;
            assert_eq!(a, b);
        }
    }

    #[test]
    fn vdp1_scaled_sprite_magnifies_texture() {
        // Just coverage test for magnification logic.
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 2;
        state.tvmr = 0x0008;
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x00; vram[1] = 0x01; // ZP=0
            vram[4] = 0x00; vram[5] = 0xC0;
            vram[6] = 0xFF; vram[7] = 0xFF;
            vram[8] = 0x00; vram[9] = 0x10;
            vram[10] = 0x00; vram[11] = 0x00; // 8x1 size
            vram[12] = 0; vram[13] = 0;
            vram[14] = 0; vram[15] = 0;
            vram[16] = 0; vram[17] = 31; // 32 wide
            vram[18] = 0; vram[19] = 1;  // 2 high
            vram[0x20] = 0x80; vram[0x21] = 0x00;
            for i in 0x80..0xA0 { vram[i] = 0xFF; }
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        assert_ne!(u16::from_be_bytes([fb[0], fb[1]]), 0);
    }
"""

with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

# Insert before the end of the module
idx = text.rfind("}")
# Actually find the end of `mod tests` block.
# We will just append to `mod tests`
# It's better to just search for `mod tests {` and append at the end of the file, assuming it's the last mod.
if "mod tests {" in text:
    # find last }
    text = text[:idx] + tests_code + "\n}"

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

