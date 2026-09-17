import re

def rewrite(file):
    with open(file, "r") as f:
        text = f.read()

    # manual_range_contains
    text = text.replace("if sum > SAT_MAX || sum < SAT_MIN {", "if !(SAT_MIN..=SAT_MAX).contains(&sum) {")
    
    # needless_range_loop in telemetry.rs
    text = text.replace("for (i, val) in THREAD_IDLE_NS.iter().enumerate() {\n        ns[i] = val.swap(0, Ordering::Relaxed);", "for (i, val) in THREAD_IDLE_NS.iter().enumerate() {\n        ns[i] = val.swap(0, Ordering::Relaxed);")
    text = text.replace("for i in 0..8 {\n        ns[i] = THREAD_IDLE_NS[i].swap(0, Ordering::Relaxed);", "for (i, val) in THREAD_IDLE_NS.iter().enumerate() {\n        ns[i] = val.swap(0, Ordering::Relaxed);")

    # needless_range_loop in sh2.rs
    text = text.replace("for i in 0..64 {\n                    expected_dst[i] = m68k.ram[src_addr as usize + i];", "for (i, val) in expected_dst.iter_mut().enumerate() {\n                    *val = m68k.ram[src_addr as usize + i];")

    # needless_range_loop in vdp.rs
    text = text.replace("for i in 0..4 {\n                        let addr = (grda + i * 2) & 0x7FFFF;\n                        grd[i] = u16::from_be_bytes([vram[addr], vram[(addr + 1) & 0x7FFFF]]);", "for (i, val) in grd.iter_mut().enumerate() {\n                        let addr = (grda + i * 2) & 0x7FFFF;\n                        *val = u16::from_be_bytes([vram[addr], vram[(addr + 1) & 0x7FFFF]]);")
    text = text.replace("for i in 0..2 {\n                        let addr = (grda + i * 2) & 0x7FFFF;\n                        grd[i] = u16::from_be_bytes([vram[addr], vram[(addr + 1) & 0x7FFFF]]);", "for (i, val) in grd.iter_mut().take(2).enumerate() {\n                        let addr = (grda + i * 2) & 0x7FFFF;\n                        *val = u16::from_be_bytes([vram[addr], vram[(addr + 1) & 0x7FFFF]]);")

    # vdp.rs
    text = text.replace("if res < 0 {\n            res = 0;\n        }\n        if res > 0x1F {\n            res = 0x1F;\n        }", "res = res.clamp(0, 0x1F);")
    text = text.replace("let gouraud_en = (cmd.cmdpmod & 0x0004) != 0 || true;", "let gouraud_en = true;")

    # peripheral_tests
    text = text.replace("let mut m = MouseState { left: true, ..Default::default() };", "let m = MouseState { left: true, ..Default::default() };")
    text = text.replace("let mut m = MouseState { x_sign: true, x_displacement: !1u8, ..Default::default() };", "let m = MouseState { x_sign: true, x_displacement: !1u8, ..Default::default() };")
    text = text.replace("let mut w = WheelState::default();\n    w.axis1 = 0x67;", "let mut w = WheelState { axis1: 0x67, ..Default::default() };")

    with open(file, "w") as f:
        f.write(text)

rewrite("saturn-core/src/sh2.rs")
rewrite("saturn-core/src/vdp.rs")
rewrite("saturn-core/src/telemetry.rs")
rewrite("saturn-core/src/integration_tests/peripheral_tests.rs")
