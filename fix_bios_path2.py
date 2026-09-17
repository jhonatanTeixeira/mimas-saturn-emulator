with open("e2e-tests/src/lib.rs", "r") as f:
    text = f.read()

new_bios = """let bios_paths = ["scratch/ra_system/saturn_bios.bin", "../scratch/ra_system/saturn_bios.bin", "../../scratch/ra_system/saturn_bios.bin", "../yabause/bios/saturn_bios.bin", "../../yabause/bios/saturn_bios.bin"];
    let mut bios = vec![0u8; 512*1024];
    for p in &bios_paths {
        if let Ok(b) = std::fs::read(p) {
            bios = b;
            break;
        }
    }"""

import re
text = re.sub(r'let bios = .*?;', new_bios, text)
with open("e2e-tests/src/lib.rs", "w") as f:
    f.write(text)
