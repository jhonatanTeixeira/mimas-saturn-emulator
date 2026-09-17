import re

tests_code = """
    #[test]
    fn vdp1_colour_calc_3_replaces_when_msb_clear() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 2;
        state.tvmr = 0x0008;
        {
            let mut fb = ram.vdp1_framebuffers.banks[0].write().unwrap();
            let bg_color = 0x03FF; // MSB clear
            fb[0] = (bg_color >> 8) as u8;
            fb[1] = (bg_color & 0xFF) as u8;
        }
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x00; vram[1] = 0x00; // Normal Sprite
            vram[4] = 0x00; vram[5] = 0xC3; // CMDPMOD SPD=1, ECD=1, CC=3
            vram[6] = 0x7C; vram[7] = 0x00; // CMDCOLR = 0x7C00 (Red)
            vram[10] = 0x00; vram[11] = 0x01; // 8x1
            vram[12] = 0; vram[13] = 0; // x0
            vram[14] = 0; vram[15] = 0; // y0
            vram[0x20] = 0x80; vram[0x21] = 0x00;
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        // Since MSB is clear, CC=3 replaces. current_pixel is 0x7C00.
        // Wait! The pixel will have MSB set if COLOR() sets it. Wait, normal drawing sets MSB!
        // But the assertion checks if it's replaced verbatim.
        let val = u16::from_be_bytes([fb[0], fb[1]]);
        assert_eq!(val, 0xFC00); // 0x7C00 | 0x8000
    }

    #[test]
    fn vdp1_colour_calc_1_shadow_only_where_msb_set() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 2;
        state.tvmr = 0x0008;
        {
            let mut fb = ram.vdp1_framebuffers.banks[0].write().unwrap();
            // Pixel 0: MSB set (0x83FF)
            fb[0] = 0x83; fb[1] = 0xFF;
            // Pixel 1: MSB clear (0x03FF)
            fb[2] = 0x03; fb[3] = 0xFF;
        }
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x00; vram[1] = 0x00;
            vram[4] = 0x00; vram[5] = 0xC1; // CC=1 (Shadow)
            vram[6] = 0xFF; vram[7] = 0xFF; // CMDCOLR
            vram[10] = 0x00; vram[11] = 0x01; // 8x1
            vram[0x20] = 0x80; vram[0x21] = 0x00;
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        let p0 = u16::from_be_bytes([fb[0], fb[1]]);
        let p1 = u16::from_be_bytes([fb[2], fb[3]]);
        
        // p0 is halved: 0x83FF halved is 0x81EF (wait, shadow makes it half luminance of itself, or zero source?)
        // CC=1: alphablend(*pix, 0, 128) | 0x8000. 0x83FF -> r=31,g=31,b=31. Halved -> 15,15,15 -> 0x3DEF | 0x8000 = 0xBDEF
        assert_eq!(p0, 0xBDEF);
        
        // p1 is untouched
        assert_eq!(p1, 0x03FF);
    }

    #[test]
    fn vdp1_gouraud_neutral_table_is_identity() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 2;
        state.tvmr = 0x0008;
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x00; vram[1] = 0x00;
            vram[4] = 0x00; vram[5] = 0xC4; // CC=4 (Gouraud)
            vram[6] = 0x7C; vram[7] = 0x00; // CMDCOLR: 0x7C00 (Red 31)
            vram[10] = 0x00; vram[11] = 0x01; // 8x1
            vram[28] = 0x00; vram[29] = 0x10; // CMDGRDA = 0x10 * 8 = 0x80
            
            // Set 4 corners to 0x4210
            for i in 0..4 {
                vram[0x80 + i*2] = 0x42;
                vram[0x80 + i*2 + 1] = 0x10;
            }
            vram[0x20] = 0x80; vram[0x21] = 0x00;
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        let p0 = u16::from_be_bytes([fb[0], fb[1]]);
        assert_eq!(p0, 0xFC00); // 0x7C00 | 0x8000
    }

    #[test]
    fn vdp1_gouraud_darkens_red() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 2;
        state.tvmr = 0x0008;
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x00; vram[1] = 0x00;
            vram[4] = 0x00; vram[5] = 0xC4; // CC=4
            vram[6] = 0x7C; vram[7] = 0x00; // r=31, g=0, b=0
            vram[10] = 0x00; vram[11] = 0x01;
            vram[28] = 0x00; vram[29] = 0x10;
            
            // 0x4208: r=0x08, g=0x10, b=0x10
            for i in 0..4 {
                vram[0x80 + i*2] = 0x42;
                vram[0x80 + i*2 + 1] = 0x08;
            }
            vram[0x20] = 0x80; vram[0x21] = 0x00;
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        let p0 = u16::from_be_bytes([fb[0], fb[1]]);
        // 31 + (8 - 16) = 23 -> 0x17. So color should be 0x17 (and MSB=1) -> 0x8017
        assert_eq!(p0, 0x8017);
    }

    #[test]
    fn vdp1_gouraud_clamps() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 2;
        state.tvmr = 0x0008;
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x00; vram[1] = 0x00;
            vram[4] = 0x00; vram[5] = 0xC4; // CC=4
            vram[6] = 0x00; vram[7] = 0x02; // r=2
            vram[10] = 0x00; vram[11] = 0x01;
            vram[28] = 0x00; vram[29] = 0x10;
            
            // 0x4200: r=0x00, g=0x10, b=0x10
            for i in 0..4 {
                vram[0x80 + i*2] = 0x42;
                vram[0x80 + i*2 + 1] = 0x00;
            }
            vram[0x20] = 0x80; vram[0x21] = 0x00;
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        let p0 = u16::from_be_bytes([fb[0], fb[1]]);
        // 2 + (0 - 16) = -14 -> clamps to 0. 0x8000
        assert_eq!(p0, 0x8000);
    }

    #[test]
    fn vdp1_gouraud_index_special_case() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 2;
        state.tvmr = 0x0008;
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x00; vram[1] = 0x00;
            vram[4] = 0x00; vram[5] = 0x84; // Color mode 0 (bank), CC=4
            vram[6] = 0x01; vram[7] = 0x23; // CMDCOLR raw 0x0123
            vram[10] = 0x00; vram[11] = 0x01;
            vram[28] = 0x00; vram[29] = 0x10;
            
            // 0x4218: r=0x18, g=0x10, b=0x10
            for i in 0..4 {
                vram[0x80 + i*2] = 0x42;
                vram[0x80 + i*2 + 1] = 0x18;
            }
            vram[0x20] = 0x80; vram[0x21] = 0x00;
            
            for i in 0x80..0xC0 { vram[i] = 0xFF; } // Texture to draw anything
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        let p0 = u16::from_be_bytes([fb[0], fb[1]]);
        // r=24. 24 - 16 = 8. 0x0123 + 8 = 0x012B. Raw!
        assert_eq!(p0, 0x012B);
    }

    #[test]
    fn vdp1_mesh_stipples() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 2;
        state.tvmr = 0x0008;
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x00; vram[1] = 0x00;
            vram[4] = 0x01; vram[5] = 0xC0; // Mesh=1
            vram[6] = 0xFF; vram[7] = 0xFF; // CMDCOLR
            vram[10] = 0x00; vram[11] = 0x04; // 8x4 size
            vram[0x20] = 0x80; vram[0x21] = 0x00;
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        // (x^y)&1 == 0 means drawn.
        // (0,0) -> 0^0=0 (drawn)
        // (1,0) -> 1^0=1 (skipped)
        // (0,1) -> 0^1=1 (skipped)
        // (1,1) -> 1^1=0 (drawn)
        assert_ne!(u16::from_be_bytes([fb[0], fb[1]]), 0);
        assert_eq!(u16::from_be_bytes([fb[2], fb[3]]), 0);
        assert_eq!(u16::from_be_bytes([fb[512*2], fb[512*2+1]]), 0);
        assert_ne!(u16::from_be_bytes([fb[512*2+2], fb[512*2+3]]), 0);
    }

    #[test]
    fn vdp1_msb_on_ors_existing_pixel() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 2;
        state.tvmr = 0x0008;
        {
            let mut fb = ram.vdp1_framebuffers.banks[0].write().unwrap();
            fb[0] = 0x12; fb[1] = 0x34; // 0x1234
        }
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x00; vram[1] = 0x00;
            vram[4] = 0x80; vram[5] = 0xC0; // MSB-on = 1
            vram[6] = 0xFF; vram[7] = 0xFF;
            vram[10] = 0x00; vram[11] = 0x01;
            vram[0x20] = 0x80; vram[0x21] = 0x00;
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        let p0 = u16::from_be_bytes([fb[0], fb[1]]);
        // 0x1234 | 0x8000 = 0x9234
        assert_eq!(p0, 0x9234);
    }

    #[test]
    fn vdp1_gouraud_table_only_fetched_when_bit2_set() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 2;
        state.tvmr = 0x0008;
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x00; vram[1] = 0x00;
            vram[4] = 0x00; vram[5] = 0xC0; // CC=0
            vram[6] = 0x7C; vram[7] = 0x00;
            vram[10] = 0x00; vram[11] = 0x01;
            vram[28] = 0x00; vram[29] = 0x10; // CMDGRDA points to 0x80
            
            // poisoned table! If it was read and applied, the color would change.
            for i in 0..4 {
                vram[0x80 + i*2] = 0x42;
                vram[0x80 + i*2 + 1] = 0x00; // darkens!
            }
            vram[0x20] = 0x80; vram[0x21] = 0x00;
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        let p0 = u16::from_be_bytes([fb[0], fb[1]]);
        // Color is exactly 0x7C00 | 0x8000 = 0xFC00. Not darkened to 0x8000.
        assert_eq!(p0, 0xFC00);
    }
"""

with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

idx = text.rfind("}")
if "mod tests {" in text:
    text = text[:idx] + tests_code + "\n}"

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

