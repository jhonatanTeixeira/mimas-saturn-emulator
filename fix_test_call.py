with open("saturn-core/src/vdp2.rs", "r") as f:
    text = f.read()

text = text.replace(
    """        let (charaddr, paladdr, flip, sf, scf) = pattern_addr(
            0xC000 | 0x7F, // flip=3, paladdr=0x7F
            0x7FFF,
            0,
            0,
            1,
            2,
            0,
            true,
        );""",
    """        let (charaddr, paladdr, flip, sf, scf) = pattern_addr(
            0xC000 | 0x7F, // flip=3, paladdr=0x7F
            0x7FFF,
            &Vdp2LayerConfig { patternwh: 1, colornumber: 0, transparencyenable: false, coloroffset: 0, cram_mode: 0, mpofn: 0, mpab: 0, mpcd: 0, patterndatasize: 2, planew: 0, planeh: 0, vram_8mbit: true, mapwh: 0, supplementdata: 0, auxmode: 0 }
        );"""
)

with open("saturn-core/src/vdp2.rs", "w") as f:
    f.write(text)

