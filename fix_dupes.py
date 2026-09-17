with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

text = text.replace("""    let layer_cfg = crate::vdp2::Vdp2LayerConfig {
        mpofn, mpab, mpcd, patterndatasize, patternwh, planew, planeh,
        vram_8mbit: (tvmd & 0x0100) != 0, mapwh, supplementdata, auxmode,
        colornumber, transparencyenable: transparency_enable,
        coloroffset: coloroffset as u32, cram_mode
    };
    
""", "")
text = text.replace("vram_8mbit: vram_8mbit", "vram_8mbit")

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

