import re

tests_code = """
    #[test]
    fn vdp1_8bit_framebuffer_geometry_and_erase() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 2; // don't draw, we just want erase
        state.tvmr = 0x0001; // 8-bit, 1024x256
        state.fbcr = 0x0000; 
        state.ewdr = 0x00AB; // erase data
        
        // EWRR >> 9 = width/16. EWRR bit 15-9 => width. 
        // Let's set EWRR to 64 >> 9 ? 
        // 64 is 4 * 16. EWRR = 4 << 9 = 0x0800.
        // EWLR = 0, so height is 0. 
        // Wait, width is (ewrr >> 9)*16. For 1024, it's 64*16. 
        state.ewlr = 0x0002; // x1=2, y1=2?
        state.ewrr = (10 << 9) | 10; // width=160, y2=10
        // EWRR bit 8-0 = y2? Yes. EWLR bit 8-0 = y1?
        // Let's check `execute_vdp1` for erase geometry!
        // The erase loops:
        // let ewrr = state.ewrr; let ewlr = state.ewlr;
        // let x1 = (ewlr >> 9) * 16; let y1 = ewlr & 0x1FF;
        // let x2 = (ewrr >> 9) * 16; let y2 = ewrr & 0x1FF;
        state.ewlr = 0x0000;
        state.ewrr = (2 << 9) | 5; // x2=32, y2=5
        
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0x20] = 0x80; vram[0x21] = 0x00; // END
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        // pixel at (0,0) erased to AB.
        // fb_idx = (y * 1024 + x)
        assert_eq!(fb[0], 0xAB);
        assert_eq!(fb[5 * 1024 + 31], 0xAB); // (31, 5) is erased!
    }

    #[test]
    fn vdp1_8bit_only_implements_replace() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 1;
        state.tvmr = 0x0001; // 8-bit
        state.fbcr = 0x0000;
        
        {
            let mut fb = ram.vdp1_framebuffers.banks[0].write().unwrap();
            fb[0] = 0x12; // background
        }
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x00; vram[1] = 0x00;
            vram[4] = 0x00; vram[5] = 0xC3; // CC=3 (would blend if 16-bit)
            vram[6] = 0x00; vram[7] = 0x34; // color 0x34 (8-bit)
            vram[10] = 0x00; vram[11] = 0x01; // 8x1
            vram[28] = 0x00; vram[29] = 0x10;
            
            for i in 0x80..0xC0 { vram[i] = 0xFF; }
            vram[0x20] = 0x80; vram[0x21] = 0x00;
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        // Result is just replaced! 0x34.
        assert_eq!(fb[0], 0x34);
    }

    #[test]
    fn vdp1_dil_rejects_alternate_lines() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 1;
        state.tvmr = 0x0008; // 16-bit
        state.fbcr = 0x0008; // DIE set, DIL clear -> odd y rejected
        
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x00; vram[1] = 0x00;
            vram[4] = 0x00; vram[5] = 0xC0; // CC=0
            vram[6] = 0xFF; vram[7] = 0xFF;
            // 8x2
            vram[10] = 0x01; vram[11] = 0x02; // size 0x0102 -> width=8, height=2
            for i in 0x80..0xA0 { vram[i] = 0xFF; }
            vram[0x20] = 0x80; vram[0x21] = 0x00;
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        // Interlace is 2. fb_idx uses y/interlace.
        // Wait, if y=0 (even), drawn to fb row 0. (0 / 2 = 0)
        // If y=1 (odd), rejected. So fb row 0 is drawn, fb row 1 is not drawn?
        // Wait! `y_actual = y / interlace`.
        // If y=0, drawn to y=0.
        // If y=1, REJECTED.
        // So y_actual=0 is drawn.
        assert_ne!(u16::from_be_bytes([fb[0], fb[1]]), 0);
        
        // Let's set DIL=1 (FBCR=0x000C)
        state.fbcr = 0x000C; // DIE + DIL -> even y rejected
        // clear fb
        {
            let mut b = ram.vdp1_framebuffers.banks[0].write().unwrap();
            for i in b.iter_mut() { *i = 0; }
        }
        state.addr = 0;
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        // y=0 (even) rejected.
        // y=1 (odd) drawn to y_actual = 1/2 = 0!
        // So row 0 is drawn!
        assert_ne!(u16::from_be_bytes([fb[0], fb[1]]), 0);
    }

    #[test]
    fn vdp1_interlace_halves_framebuffer_row() {
        // As seen above, y=1 drawn to y=0 when DIL=1.
        // Let's verify a quad spanning 0-3 writes to fb rows 0-1.
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 1;
        state.tvmr = 0x0008; // 16-bit
        state.fbcr = 0x0000; // DIE clear
        
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0] = 0x00; vram[1] = 0x00;
            vram[4] = 0x00; vram[5] = 0xC0; // CC=0
            vram[6] = 0xFF; vram[7] = 0xFF;
            // 8x4
            vram[10] = 0x01; vram[11] = 0x04;
            for i in 0x80..0xC0 { vram[i] = 0xFF; }
            vram[0x20] = 0x80; vram[0x21] = 0x00;
        }
        execute_vdp1(&mut state, &ram);
        let mut fb1 = vec![0u8; 512 * 256 * 2];
        {
            let b = ram.vdp1_framebuffers.banks[0].read().unwrap();
            fb1.copy_from_slice(&b[..]);
        }
        
        // NOW WITH DIE SET (and DIL clear)
        state.fbcr = 0x0008;
        // clear fb
        {
            let mut b = ram.vdp1_framebuffers.banks[0].write().unwrap();
            for i in b.iter_mut() { *i = 0; }
        }
        state.addr = 0;
        execute_vdp1(&mut state, &ram);
        let fb2 = ram.vdp1_framebuffers.banks[0].read().unwrap();
        
        // Without DIE, y=0,1,2,3 drawn to rows 0,1,2,3.
        assert_ne!(u16::from_be_bytes([fb1[0 * 512 * 2], fb1[0 * 512 * 2 + 1]]), 0);
        assert_ne!(u16::from_be_bytes([fb1[3 * 512 * 2], fb1[3 * 512 * 2 + 1]]), 0);
        
        // With DIE, y=0,2 drawn to rows 0,1.
        // y=1,3 are rejected.
        // So rows 0 and 1 are drawn. Row 2 is NOT drawn.
        assert_ne!(u16::from_be_bytes([fb2[0 * 512 * 2], fb2[0 * 512 * 2 + 1]]), 0);
        assert_ne!(u16::from_be_bytes([fb2[1 * 512 * 2], fb2[1 * 512 * 2 + 1]]), 0);
        assert_eq!(u16::from_be_bytes([fb2[2 * 512 * 2], fb2[2 * 512 * 2 + 1]]), 0);
    }
"""

with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

idx = text.rfind("}")
if "mod tests {" in text:
    text = text[:idx] + tests_code + "\n}"

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

