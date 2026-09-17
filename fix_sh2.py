with open("saturn-core/src/sh2.rs", "r") as f:
    text = f.read()

import re

# Fix mem_cycles_r
text = re.sub(
    r'fn mem_cycles_r\(&self, addr: u32\) -> u32 \{.*?\n    \}',
    r'''fn mem_cycles_r(&self, addr: u32) -> u32 {
        let phys = addr & 0x1FFF_FFFF;
        match phys {
            0x0000_0000..=0x001F_FFFF => 16, // BIOS + Backup RAM
            0x0020_0000..=0x00FF_FFFF => 12, // Low Work RAM
            0x0200_0000..=0x03FF_FFFF => 24, // CS0
            0x0580_0000..=0x059F_FFFF => 24, // CS2
            0x05A0_0000..=0x05DF_FFFF => 50, // Sound RAM + Regs + VDP1
            0x05E0_0000..=0x05FF_FFFF => 2,  // VDP2 placeholder
            _ => 0,
        }
    }''',
    text,
    flags=re.DOTALL
)

# Fix mem_cycles_w
text = re.sub(
    r'fn mem_cycles_w\(&self, addr: u32\) -> u32 \{.*?\n    \}',
    r'''fn mem_cycles_w(&self, addr: u32) -> u32 {
        let phys = addr & 0x1FFF_FFFF;
        match phys {
            0x0020_0000..=0x00FF_FFFF => 7, // Low Work RAM
            0x05A0_0000..=0x05AF_FFFF => 7, // Sound RAM
            0x05C0_0000..=0x05DF_FFFF => 7, // VDP1
            0x0600_0000..=0x060F_FFFF => 7, // High Work RAM
            _ => 0,
        }
    }''',
    text,
    flags=re.DOTALL
)

# Fix macl/mach bug (identical blocks)
text = text.replace('''                    if sum > SAT_MAX {
                        sum = if mul < 0 { SAT_MIN } else { SAT_MAX };
                    } else if sum < SAT_MIN {
                        sum = if mul < 0 { SAT_MIN } else { SAT_MAX };
                    }''', '''                    if sum > SAT_MAX || sum < SAT_MIN {
                        sum = if mul < 0 { SAT_MIN } else { SAT_MAX };
                    }''')

text = text.replace('''                    if sum > SAT_MAX {
                        self.mach |= 1;
                        self.macl = if mul < 0 {
                            SAT_MIN as u32
                        } else {
                            SAT_MAX as u32
                        };
                    } else if sum < SAT_MIN {
                        self.mach |= 1;
                        self.macl = if mul < 0 {
                            SAT_MIN as u32
                        } else {
                            SAT_MAX as u32
                        };
                    } else {''', '''                    if sum > SAT_MAX || sum < SAT_MIN {
                        self.mach |= 1;
                        self.macl = if mul < 0 {
                            SAT_MIN as u32
                        } else {
                            SAT_MAX as u32
                        };
                    } else {''')


with open("saturn-core/src/sh2.rs", "w") as f:
    f.write(text)
