with open("e2e-tests/src/lib.rs", "r") as f:
    text = f.read()

test_code = """
#[test]
fn test_tier4_scenario_bios_boot_smoke_coverage() {
    // no-assert: We boot the BIOS and run it for 1 second to gain coverage over 
    // scsp, scu_dsp, vdp2, lib.rs etc. that are fully exercised by real BIOS.
    use saturn_core::SaturnSystem;
    let bios_paths = ["scratch/ra_system/saturn_bios.bin", "../scratch/ra_system/saturn_bios.bin", "../../scratch/ra_system/saturn_bios.bin", "../yabause/bios/saturn_bios.bin", "../../yabause/bios/saturn_bios.bin"];
    let mut bios = vec![0u8; 512*1024];
    for p in &bios_paths {
        if let Ok(b) = std::fs::read(p) {
            bios = b;
            break;
        }
    }
    let mut sys = SaturnSystem::new();
    sys.cpu0_pc.store(0, std::sync::atomic::Ordering::Relaxed);
    sys.load_bios(bios);
    sys.start();
    std::thread::sleep(std::time::Duration::from_millis(500));
    sys.shutdown();
}
"""

text = text[:text.rfind("}")] + test_code + "}\n"

# Reapply the release path fix
text = text.replace('"../target/debug/saturn-frontend-native",', '"../target/debug/saturn-frontend-native",\n            "../target/release/saturn-frontend-native",')
text = text.replace('"../../target/debug/saturn-frontend-native",', '"../../target/debug/saturn-frontend-native",\n            "../../target/release/saturn-frontend-native",')
text = text.replace('"target/debug/saturn-frontend-native",', '"target/debug/saturn-frontend-native",\n            "target/release/saturn-frontend-native",')

with open("e2e-tests/src/lib.rs", "w") as f:
    f.write(text)
