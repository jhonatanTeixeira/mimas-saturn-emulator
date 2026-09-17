    #[test]
    fn vdp1_scaled_sprite_zp_upper_left() {
        let mut state = Vdp1State::new();
        state.ptmr = 2; // bypass fake_draw
        let work_ram = Arc::new(WorkRam::new());
        let mut vram = work_ram.vdp1_vram.write().unwrap();
        vram[0] = 0;
        vram[1] = 0x05; // CMDCTRL: ZP = 0x5 (upper-left), opcode = 1 (Scaled Sprite), Dir=0
        vram[12] = 0; vram[13] = 10; // CMDXA = 10
        vram[14] = 0; vram[15] = 10; // CMDYA = 10
        vram[16] = 0; vram[17] = 31; // CMDXB = 31
        vram[18] = 0; vram[19] = 15; // CMDYB = 15
        drop(vram);
        
        // It should draw to (10,10) to (41,25) inclusive (32 wide, 16 high)
        // Wait, how can we assert this? The rasteriser plots pixels.
        // We can just verify if the right pixels are drawn!
    }
