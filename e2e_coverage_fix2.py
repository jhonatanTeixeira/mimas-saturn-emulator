with open("e2e-tests/src/lib.rs", "r") as f:
    text = f.read()

import re
# Replace the body of test_tier4_scenario_bios_boot_smoke_coverage
# starting from `let bios = ...` up to the end of the function.
start_idx = text.find("    let bios = std::fs::read(")
end_idx = text.find("}", start_idx)

new_body = """    let bios = std::fs::read("../yabause/bios/saturn_bios.bin").unwrap_or_else(|_| vec![0u8; 512*1024]);
    let mut sys = SaturnSystem::new();
    sys.cpu0_pc.store(0, std::sync::atomic::Ordering::Relaxed);
    sys.load_bios(bios);
    sys.start();
    std::thread::sleep(std::time::Duration::from_millis(500));
    sys.shutdown();
"""

text = text[:start_idx] + new_body + text[end_idx:]

with open("e2e-tests/src/lib.rs", "w") as f:
    f.write(text)
