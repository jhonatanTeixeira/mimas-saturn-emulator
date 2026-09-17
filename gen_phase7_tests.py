import re

tests_code = """
    #[test]
    fn vdp1_polyline_draws_four_edges() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 1;
        state.tvmr = 0x0008;
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x00; vram[1] = 0x05; // Polyline (5)
            vram[4] = 0x00; vram[5] = 0xC0;
            vram[6] = 0xFF; vram[7] = 0xFF;
            
            // A=(10,10), B=(20,10), C=(20,20), D=(10,20)
            let b10 = 10i16.to_be_bytes();
            let b20 = 20i16.to_be_bytes();
            vram[12] = b10[0]; vram[13] = b10[1]; vram[14] = b10[0]; vram[15] = b10[1]; // A
            vram[16] = b20[0]; vram[17] = b20[1]; vram[18] = b10[0]; vram[19] = b10[1]; // B
            vram[20] = b20[0]; vram[21] = b20[1]; vram[22] = b20[0]; vram[23] = b20[1]; // C
            vram[24] = b10[0]; vram[25] = b10[1]; vram[26] = b20[0]; vram[27] = b20[1]; // D
            vram[0x20] = 0x80; vram[0x21] = 0x00;
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        // Edge drawn, so (15,10) is drawn. (15,15) is interior, not drawn.
        assert_ne!(u16::from_be_bytes([fb[(((10 * 512) + 15) * 2)], fb[(((10 * 512) + 15) * 2) + 1]]), 0);
        assert_eq!(u16::from_be_bytes([fb[(((15 * 512) + 15) * 2)], fb[(((15 * 512) + 15) * 2) + 1]]), 0);
    }

    #[test]
    fn vdp1_polyline_edge_direction() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 1;
        state.tvmr = 0x0008;
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x00; vram[1] = 0x05;
            vram[4] = 0x00; vram[5] = 0xC0; // CC=0
            vram[6] = 0xFF; vram[7] = 0xFF;
            
            // C=(0,0), D=(0,10)
            let b0 = 0i16.to_be_bytes();
            let b10 = 10i16.to_be_bytes();
            vram[12] = b0[0]; vram[13] = b0[1]; vram[14] = b0[0]; vram[15] = b0[1]; // A=0,0
            vram[16] = b0[0]; vram[17] = b0[1]; vram[18] = b0[0]; vram[19] = b0[1]; // B=0,0
            vram[20] = b0[0]; vram[21] = b0[1]; vram[22] = b0[0]; vram[23] = b0[1]; // C=0,0
            vram[24] = b0[0]; vram[25] = b0[1]; vram[26] = b10[0]; vram[27] = b10[1]; // D=0,10
            vram[28] = 0x00; vram[29] = 0x10; // CMDGRDA = 0x80
            
            // Set 4 corners
            for i in 0..4 {
                vram[0x80 + i*2] = 0x42;
                vram[0x80 + i*2 + 1] = 0x10;
            }
            vram[0x20] = 0x80; vram[0x21] = 0x00;
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        // Since CMDCOLR = FFFF, it draws. 
        // We aren't fully asserting gradient, just coverage that it executes the loop for C->D
        assert_ne!(u16::from_be_bytes([fb[(((5 * 512) + 0) * 2)], fb[(((5 * 512) + 0) * 2) + 1]]), 0);
    }

    #[test]
    fn vdp1_end_code_mode_3_never_matches() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 1;
        state.tvmr = 0x0008;
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x00; vram[1] = 0x00;
            vram[4] = 0x00; vram[5] = 0x23; // Mode 3, ECD clear
            vram[6] = 0xFF; vram[7] = 0xFF;
            vram[8] = 0x00; vram[9] = 0x10;
            vram[10] = 0x01; vram[11] = 0x01; // 8x1
            vram[0x20] = 0x80; vram[0x21] = 0x00;
            
            // Texture with 0xFF!
            for i in 0x80..0xC0 { vram[i] = 0xFF; }
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        // Mode 3 is 128 colors, masks 0xFF to 0x7F. So not an end code. Span continues!
        assert_ne!(u16::from_be_bytes([fb[(((0 * 512) + 6) * 2)], fb[(((0 * 512) + 6) * 2) + 1]]), 0);
    }

    #[test]
    fn vdp1_end_code_mode_2_is_transparent_not_terminal() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 1;
        state.tvmr = 0x0008;
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x00; vram[1] = 0x00;
            vram[4] = 0x00; vram[5] = 0x12; // Mode 2, ECD clear
            vram[6] = 0xFF; vram[7] = 0xFF;
            vram[8] = 0x00; vram[9] = 0x10;
            vram[10] = 0x01; vram[11] = 0x01; // 8x1
            vram[0x20] = 0x80; vram[0x21] = 0x00;
            
            // 63 in mode 2 is 0x3F. (64 color bank mode, 6bpp? Mode 2 is 64 colors, so 6 bits)
            // wait, mode 2 is 64-color bank. 8bpp pixels but uses 64 colors?
            // Actually, `0x3F` is 63.
            for i in 0x80..0xC0 { vram[i] = 0x3F; } // all 63
            vram[0x87] = 0x01; // put a valid pixel at the end to see if span continues
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        // Pixel at 0,0 is transparent (63). Pixel at 7 is drawn (1).
        assert_eq!(u16::from_be_bytes([fb[0], fb[1]]), 0);
        assert_ne!(u16::from_be_bytes([fb[7 * 2], fb[7 * 2 + 1]]), 0);
    }
"""

with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

idx = text.rfind("}")
if "mod tests {" in text:
    text = text[:idx] + tests_code + "\n}"

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

