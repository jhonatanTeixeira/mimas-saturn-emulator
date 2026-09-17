with open("saturn-core/src/cs2.rs", "r") as f:
    t = f.read()
t = t.replace("0u32 & self.mpegintmask", "0u32")
with open("saturn-core/src/cs2.rs", "w") as f:
    f.write(t)

with open("saturn-core/src/m68k.rs", "r") as f:
    t = f.read()
t = t.replace("std::sync::Mutex<Vec<(u32, u16, u32, u32, u32)>>", "std::sync::Mutex<Vec<TraceEntry>>")
with open("saturn-core/src/m68k.rs", "w") as f:
    f.write(t)

with open("saturn-core/src/telemetry.rs", "r") as f:
    t = f.read()
t = t.replace("for i in 0..8 {\n        ns[i] = THREAD_IDLE_NS[i].swap(0, Ordering::Relaxed);", "for (i, idle) in THREAD_IDLE_NS.iter().enumerate() {\n        ns[i] = idle.swap(0, Ordering::Relaxed);")
with open("saturn-core/src/telemetry.rs", "w") as f:
    f.write(t)

with open("saturn-core/src/vdp.rs", "r") as f:
    t = f.read()
t = t.replace("for i in 0..4 {\n                        let addr = (grda + i * 2) & 0x7FFFF;\n                        grd[i] = u16::from_be_bytes([vram[addr], vram[(addr + 1) & 0x7FFFF]]);\n                    }", "for (i, val) in grd.iter_mut().enumerate() {\n                        let addr = (grda + i * 2) & 0x7FFFF;\n                        *val = u16::from_be_bytes([vram[addr], vram[(addr + 1) & 0x7FFFF]]);\n                    }")
with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(t)

import re
with open("saturn-core/src/integration_tests/peripheral_tests.rs", "r") as f:
    t = f.read()
t = re.sub(
    r'let m = MouseState \{\s*left: true,\s*\.\.Default::default\(\)\s*\};\n\s*m\.start = true;',
    r'let mut m = MouseState { left: true, ..Default::default() };\n    m.start = true;',
    t
)
with open("saturn-core/src/integration_tests/peripheral_tests.rs", "w") as f:
    f.write(t)

