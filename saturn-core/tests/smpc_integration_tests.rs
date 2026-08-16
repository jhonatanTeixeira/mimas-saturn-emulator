use saturn_core::bus_arbiter::BusArbiter;
use saturn_core::sh2::Sh2;
use saturn_core::shared_buffers::WorkRam;
use saturn_core::smpc::{cmd, reg, Smpc};
use std::sync::{Arc, Mutex};

#[test]
fn drive_intback_from_real_sh2_and_read_oreg2() {
    let arbiter = Arc::new(BusArbiter::new());
    let work_ram = Arc::new(WorkRam::new());
    let mut cpu = Sh2::new(false, arbiter, work_ram.clone());

    // Wire up Smpc just like SaturnSystem::start
    let smpc = Arc::new(Mutex::new(Smpc::new()));
    cpu.smpc = Some(smpc.clone());

    const SMPC_BASE: u32 = 0x0010_0000;

    // Set port 1 to Pad with A and Start pressed.
    {
        let mut smpc_lock = smpc.lock().unwrap();
        let mut pad = saturn_core::peripheral::PadState::default();
        pad.a = true;
        pad.start = true;
        smpc_lock.set_pad_state(1, pad);
    }

    // Write INTBACK sequence
    // First clear IREG2 which means no multi-tap, standard 15-byte data requested
    cpu.write_byte(SMPC_BASE + reg::IREG2 as u32, 0xF0);
    // Request peripheral data + status
    cpu.write_byte(SMPC_BASE + reg::IREG1 as u32, 0x08); // bit 3 set = peripheral data
    cpu.write_byte(SMPC_BASE + reg::IREG0 as u32, 0x00); // bit 0 clear = peripheral mode

    // Set SF to busy (BIOS does this)
    cpu.write_byte(SMPC_BASE + reg::SF as u32, 0x01);

    // Issue INTBACK
    cpu.write_byte(SMPC_BASE + reg::COMREG as u32, cmd::INTBACK);

    // Wait for the command to complete.
    // INTBACK takes some time, so we just manually process the pending command for the test.
    let _effects = smpc.lock().unwrap().execute_expired_command(&work_ram);
    // cpu.apply_smpc_effects(_effects); // Not visible, but we only need OREGs which are updated in work_ram

    // The real hardware INTBACK flow populates OREGs.
    // OREG0 should be Port 1 status (Direct pad = 0xF1)
    assert_eq!(cpu.read_byte(SMPC_BASE + reg::oreg(0) as u32), 0xF1);
    // OREG1 should be Port 1 ID (Pad = 0x02)
    assert_eq!(cpu.read_byte(SMPC_BASE + reg::oreg(1) as u32), 0x02);
    // OREG2 should be Port 1 buttons high (Start is bit 3, Right is bit 7, A is bit 2, etc.)
    // Start=true -> bit 3 cleared. A=true -> bit 2 cleared. Default is 0xFF.
    // So 0xFF & !(1<<3) & !(1<<2) = 0xFF & !0x0C = 0xF3.
    assert_eq!(cpu.read_byte(SMPC_BASE + reg::oreg(2) as u32), 0xF3);
    // OREG3 should be Port 1 buttons low
    assert_eq!(cpu.read_byte(SMPC_BASE + reg::oreg(3) as u32), 0xFF);
}
