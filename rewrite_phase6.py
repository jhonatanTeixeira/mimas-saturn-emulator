import re

tests_code = """
    #[test]
    fn vdp1_colour_calc_3_replaces_when_msb_clear() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 1;
        state.tvmr = 0x0008;
        {
            let mut fb = ram.vdp1_framebuffers.banks[0].write().unwrap();
            let offset = ((8 * 512 + 32) * 2) as usize;
            let bg_bytes = 0x03FFu16.to_be_bytes(); // MSB clear
            fb[offset] = bg_bytes[0];
            fb[offset + 1] = bg_bytes[1];
        }
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            let bytes = 0x0004u16.to_be_bytes(); // COMM 4 (polygon)
            vram[0] = bytes[0];
            vram[1] = bytes[1]; // CMDCTRL
            let bytes = 0x0043u16.to_be_bytes(); // SPD=1, Colour calc mode 3
            vram[4] = bytes[0];
            vram[5] = bytes[1]; // CMDPMOD
            let bytes = 0x7C00u16.to_be_bytes(); // new pixel
            vram[6] = bytes[0];
            vram[7] = bytes[1]; // CMDCOLR

            let bytes = 32i16.to_be_bytes();
            vram[12] = bytes[0]; vram[13] = bytes[1]; // A
            vram[16] = bytes[0]; vram[17] = bytes[1]; // B
            vram[20] = bytes[0]; vram[21] = bytes[1]; // C
            vram[24] = bytes[0]; vram[25] = bytes[1]; // D
            let bytes = 8i16.to_be_bytes();
            vram[14] = bytes[0]; vram[15] = bytes[1];
            vram[18] = bytes[0]; vram[19] = bytes[1];
            vram[22] = bytes[0]; vram[23] = bytes[1];
            vram[26] = bytes[0]; vram[27] = bytes[1];
            vram[0x20] = 0x80; vram[0x21] = 0x00;
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        let offset = ((8 * 512 + 32) * 2) as usize;
        let val = u16::from_be_bytes([fb[offset], fb[offset + 1]]);
        assert_eq!(val, 0x7C00); // Because MSB clear, replaced!
    }

    #[test]
    fn vdp1_colour_calc_1_shadow_only_where_msb_set() {
        let ram = WorkRam::new();
        let mut state = Vdp1State::new();
        state.ptmr = 1;
        state.tvmr = 0x0008;
        {
            let mut fb = ram.vdp1_framebuffers.banks[0].write().unwrap();
            let o1 = ((8 * 512 + 32) * 2) as usize;
            let bg1 = 0x83FFu16.to_be_bytes();
            fb[o1] = bg1[0]; fb[o1+1] = bg1[1];
            
            let o2 = ((8 * 512 + 33) * 2) as usize;
            let bg2 = 0x03FFu16.to_be_bytes();
            fb[o2] = bg2[0]; fb[o2+1] = bg2[1];
        }
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            let bytes = 0x0004u16.to_be_bytes(); // COMM 4 (polygon)
            vram[0] = bytes[0]; vram[1] = bytes[1];
            let bytes = 0x0041u16.to_be_bytes(); // CC=1
            vram[4] = bytes[0]; vram[5] = bytes[1];
            let bytes = 0xFFFFu16.to_be_bytes(); // new pixel
            vram[6] = bytes[0]; vram[7] = bytes[1];

            // A=32,8 B=33,8 C=33,8 D=32,8
            let b32 = 32i16.to_be_bytes();
            let b33 = 33i16.to_be_bytes();
            let b8 = 8i16.to_be_bytes();
            vram[12] = b32[0]; vram[13] = b32[1]; vram[14] = b8[0]; vram[15] = b8[1]; // A
            vram[16] = b33[0]; vram[17] = b33[1]; vram[18] = b8[0]; vram[19] = b8[1]; // B
            vram[20] = b33[0]; vram[21] = b33[1]; vram[22] = b8[0]; vram[23] = b8[1]; // C
            vram[24] = b32[0]; vram[25] = b32[1]; vram[26] = b8[0]; vram[27] = b8[1]; // D
            vram[0x20] = 0x80; vram[0x21] = 0x00;
        }
        execute_vdp1(&mut state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();
        let o1 = ((8 * 512 + 32) * 2) as usize;
        let o2 = ((8 * 512 + 33) * 2) as usize;
        
        // p0 is halved. CC=1: alphablend(0x83FF, 0, 128) | 0x8000 -> 0x81EF
        assert_eq!(u16::from_be_bytes([fb[o1], fb[o1+1]]), 0x81EF);
        // p1 is untouched (0x03FF)
        assert_eq!(u16::from_be_bytes([fb[o2], fb[o2+1]]), 0x03FF);
    }
"""

with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

# Remove the previously injected tests for MSB clear and shadow so we can replace them!
import re
text = re.sub(r'#\[test\]\n\s+fn vdp1_colour_calc_3_replaces_when_msb_clear\(\) \{.*?(?=#\[test\])', '', text, flags=re.DOTALL)
text = re.sub(r'#\[test\]\n\s+fn vdp1_colour_calc_1_shadow_only_where_msb_set\(\) \{.*?(?=#\[test\])', '', text, flags=re.DOTALL)

idx = text.rfind("}")
if "mod tests {" in text:
    text = text[:idx] + tests_code + "\n}"

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

