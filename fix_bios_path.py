with open("e2e-tests/src/lib.rs", "r") as f:
    text = f.read()

text = text.replace('let bios = std::fs::read("../yabause/bios/saturn_bios.bin").unwrap_or_else(|_| vec![0u8; 512*1024]);', 
                    'let bios = std::fs::read("scratch/ra_system/saturn_bios.bin").or_else(|_| std::fs::read("../yabause/bios/saturn_bios.bin")).unwrap_or_else(|_| vec![0u8; 512*1024]);')

with open("e2e-tests/src/lib.rs", "w") as f:
    f.write(text)
