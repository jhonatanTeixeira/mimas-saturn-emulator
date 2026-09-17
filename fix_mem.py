with open("saturn-core/src/sh2.rs", "r") as f:
    text = f.read()

import re
text = re.sub(r'fn mem_cycles_r\(&self, addr: u32\) -> u32 \{([\s\S]*?)\n    \}', 
                  r'''fn mem_cycles_r(&self, addr: u32) -> u32 {
        let phys = addr & 0x1FFF_FFFF;
        match phys {
            0..=0x001F_FFFF => 16,
            0x0020_0000..=0x00FF_FFFF => 12,
            0x0200_0000..=0x03FF_FFFF => 24,
            0x0580_0000..=0x059F_FFFF => 24,
            0x05A0_0000..=0x05DF_FFFF => 50,
            0x05E0_0000..=0x05FF_FFFF => 2,
            _ => 0,
        }
    }''', text)

text = re.sub(r'fn mem_cycles_w\(&self, addr: u32\) -> u32 \{([\s\S]*?)\n    \}', 
                  r'''fn mem_cycles_w(&self, addr: u32) -> u32 {
        let phys = addr & 0x1FFF_FFFF;
        match phys {
            0..=0x001F_FFFF => 0,
            0x0020_0000..=0x00FF_FFFF => 7,
            0x0200_0000..=0x03FF_FFFF => 0,
            0x0580_0000..=0x059F_FFFF => 0,
            0x05A0_0000..=0x05AF_FFFF => 7,
            0x05C0_0000..=0x060F_FFFF => 2,
            _ => 0,
        }
    }''', text)

with open("saturn-core/src/sh2.rs", "w") as f:
    f.write(text)

