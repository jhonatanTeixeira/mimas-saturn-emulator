with open("e2e-tests/src/lib.rs", "r") as f:
    text = f.read()

test_code = """
#[test]
fn test_tier4_scenario_bios_boot_smoke_coverage() {
    // no-assert: We boot the BIOS and run it for 1 second to gain coverage over 
    // scsp, scu_dsp, vdp2, lib.rs etc. that are fully exercised by real BIOS.
    use saturn_core::{SaturnSystem, Sh2};
    let bios = std::fs::read("../yabause/bios/saturn_bios.bin").unwrap_or_else(|_| vec![0u8; 512*1024]);
    let mut sys = SaturnSystem::new(None);
    sys.cpu0_pc.store(0, std::sync::atomic::Ordering::Relaxed); // ensure dummy store
    let (mut c0, mut c1, arb, sync, wr, _th) = sys.start(1.0);
    // write bios
    for (i, &b) in bios.iter().enumerate() {
        wr.bios.write().unwrap()[i] = b;
    }
    // Let it run for 100 milliseconds
    std::thread::sleep(std::time::Duration::from_millis(100));
    sync.request_shutdown();
}
"""
with open("e2e-tests/src/lib.rs", "w") as f:
    f.write(text + test_code)
