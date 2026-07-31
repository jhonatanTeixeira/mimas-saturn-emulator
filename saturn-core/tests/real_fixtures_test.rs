//! Tests against real state captured from a genuine Yabause libretro core
//! (patched with a temporary dump hook, see
//! `docs/yabause_test_fixtures_extraction_plan.md`) running a real Saturn
//! BIOS against a real boot disc -- not a self-consistent guess. See
//! `saturn-core/tests/fixtures/README.md` for exactly how each fixture was
//! captured and how to regenerate it.
use std::sync::{Arc, Mutex};
use saturn_core::{BusArbiter, Sh2, Smpc, WorkRam};

fn load_fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("failed to load fixture {}: {}", path, e))
}

/// Real SMPC register window (0x80 bytes, real odd-byte-per-register
/// layout) captured at the exact point real BIOS boot code's first INTBACK
/// (system status, no peripheral data: IREG1 = 0x02) call finishes on a
/// genuine Yabause core -- see `SmpcINTBACKStatus`/`SmpcINTBACK` in
/// `yabause/src/smpc.c`. This is what real hardware actually put in these
/// registers, not a hand-picked value.
#[test]
fn real_smpc_intback_status_reads_back_through_the_cpu_address_path() {
    let fixture = load_fixture("smpc_intback_status.bin");
    assert_eq!(fixture.len(), 0x80, "fixture must be the full real SMPC register window");

    let arbiter = Arc::new(BusArbiter::new());
    let work_ram = Arc::new(WorkRam::new());
    {
        let mut regs = work_ram.smpc_regs.write().unwrap();
        regs.copy_from_slice(&fixture);
    }
    let mut cpu = Sh2::new(false, arbiter, work_ram);

    // SMPC's real address (physical 0x00100000-0x0017FFFF, an 0x80-byte
    // register file mirrored across that window -- see `Sh2::translate`'s
    // `& 0x7F` and `docs/hardware-reference/smpc-peripheral.md` §1).
    const SMPC_BASE: u32 = 0x0010_0000;

    // IREG1 (offset 0x03): the real BIOS wrote 0x02 here to request status
    // data only (bit 3 clear -> no peripheral data requested this call).
    assert_eq!(cpu.read_byte(SMPC_BASE + 0x03), 0x02, "IREG1");

    // COMREG (offset 0x1F): 0x10 = INTBACK, still latched from the write
    // that triggered this exact command.
    assert_eq!(cpu.read_byte(SMPC_BASE + 0x1F), 0x10, "COMREG");

    // OREG0 (offset 0x21): 0xC0 = 0x80 (normal startup) | 0x40 (resd bit
    // set) -- real hardware, not the "0x80" a self-consistent guess would
    // assume.
    assert_eq!(cpu.read_byte(SMPC_BASE + 0x21), 0xC0, "OREG0");

    // OREG9 (offset 0x33): region ID. 0x01 = Japan -- the region this
    // particular real BIOS image (`saturn_bios.bin`) actually reports.
    assert_eq!(cpu.read_byte(SMPC_BASE + 0x33), 0x01, "OREG9 (region)");

    // OREG10 (offset 0x35): system state byte. Real value 0x34 confirms
    // the constant base bits (bits 5/4/2 always 1 per `smpc.c`) with
    // DOTSEL/MSHNMI/SYSRES/SNDRES all clear on this boot.
    assert_eq!(cpu.read_byte(SMPC_BASE + 0x35), 0x34, "OREG10");

    // OREG31 (offset 0x5F): 0x10 echoes the INTBACK command byte, per
    // `docs/hardware-reference/smpc-peripheral.md`'s INTBACK section.
    assert_eq!(cpu.read_byte(SMPC_BASE + 0x5F), 0x10, "OREG31 (command echo)");

    // SR (offset 0x61): real hardware sets 0x4F here (0x4F | intback<<5,
    // with intback=0 since no peripheral data was requested) -- Mimas's
    // own SMPC command handler currently hardcodes 0x6F instead (see
    // `docs/implementation-plans/smpc-peripheral.md` phase 1); this
    // fixture is the real, independently-derived value that fix must
    // reproduce.
    assert_eq!(cpu.read_byte(SMPC_BASE + 0x61), 0x4F, "SR");

    // Cross-check the same OREG9 through `read_long` (word/long access
    // decomposition path), not just `read_byte`, since that's a separate
    // code path in `Sh2` that could disagree with the byte reader.
    let oreg8_11 = cpu.read_long(SMPC_BASE + 0x31);
    assert_eq!((oreg8_11 >> 8) & 0xFF, 0x01, "OREG9 via read_long's byte 1");
}

/// SF (offset 0x63) is genuinely 1 in the real capture (busy flag still
/// set; real hardware only clears it once `SmpcExec` returns, one step
/// later than this capture). This used to be an `#[ignore]`d known-gap test
/// (`Sh2`'s SF read used to hardcode `0x00` for every bare `Sh2`) -- now
/// that `docs/implementation-plans/smpc-peripheral.md` Phase 1 landed a real
/// `Smpc` with a real SF/`bustmp` handshake (`Smpc::read_sf`), wiring one up
/// (mirroring exactly how `SaturnSystem::start` wires Core 0) makes this a
/// real, passing regression test instead.
#[test]
fn real_smpc_sf_reflects_the_real_busy_flag_when_a_real_smpc_is_wired_in() {
    let fixture = load_fixture("smpc_intback_status.bin");
    let arbiter = Arc::new(BusArbiter::new());
    let work_ram = Arc::new(WorkRam::new());
    {
        let mut regs = work_ram.smpc_regs.write().unwrap();
        regs.copy_from_slice(&fixture);
    }
    let mut cpu = Sh2::new(false, arbiter, work_ram);
    cpu.smpc = Some(Arc::new(Mutex::new(Smpc::new())));
    const SMPC_BASE: u32 = 0x0010_0000;
    assert_eq!(cpu.read_byte(SMPC_BASE + 0x63) & 1, 0x01, "SF bit 0");
}
