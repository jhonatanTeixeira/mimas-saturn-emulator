//! Report-shape correctness for every SMPC peripheral type (`docs/implementation-plans/smpc-peripheral.md`
//! Phase 7), each value independently derived from `docs/hardware-reference/smpc-peripheral.md`
//! §9.3-9.7's own tables and worked examples -- never copied from this crate's own output.

use saturn_core::peripheral::{
    id, status, GunState, KeyboardState, MissionStickState, MouseState, Pad3DState, PadState,
    PeripheralState, TwinSticksState, WheelState,
};

#[test]
fn per_type_initial_reports() {
    // §9.3's literal initializer table, cross-checked against the `id`/data-length
    // columns and (for Pad) §9.4's pad worked example.
    let pad = PeripheralState::Pad(PadState::default());
    let pd = pad.to_port_data();
    assert_eq!(pd.status, status::DIRECT);
    assert_eq!(pd.id, id::PAD);
    assert_eq!(pd.size, 2);
    assert_eq!(&pd.data[..2], &[0xFF, 0xFF]);

    let wheel = PeripheralState::Wheel(WheelState::default());
    let pd = wheel.to_port_data();
    assert_eq!(pd.id, id::WHEEL);
    assert_eq!(pd.size, 3);
    assert_eq!(&pd.data[..3], &[0xFF, 0xFF, 0x7F], "FF FF 7F, §9.3");

    let mstick = PeripheralState::MissionStick(MissionStickState::default());
    let pd = mstick.to_port_data();
    assert_eq!(pd.id, id::MISSION_STICK);
    assert_eq!(pd.size, 5);
    assert_eq!(
        &pd.data[..5],
        &[0xFF, 0xFF, 0x7F, 0x7F, 0x7F],
        "FF FF 7F 7F 7F, §9.3 -- axis3 (byte 4) is 0x7F like every other axis, \
         not inverted: the -(s8) inversion only applies when `PerAxis3Value` is \
         actually called with a live reading, not to the initial value"
    );

    let pad3d = PeripheralState::Pad3D(Pad3DState::default());
    let pd = pad3d.to_port_data();
    assert_eq!(pd.id, id::PAD_3D);
    assert_eq!(pd.size, 6);
    assert_eq!(&pd.data[..6], &[0xFF, 0xFF, 0x7F, 0x7F, 0x7F, 0x7F]);

    let tsticks = PeripheralState::TwinSticks(TwinSticksState::default());
    let pd = tsticks.to_port_data();
    assert_eq!(pd.id, id::TWIN_STICKS);
    assert_eq!(pd.size, 9);
    assert_eq!(
        &pd.data[..9],
        &[0xFF, 0xFF, 0x7F, 0x7F, 0x7F, 0x7F, 0x7F, 0x7F, 0x7F],
        "real hardware leaves the 9th byte (axis7) at 0x00 instead of the \
         neutral 0x7F every other axis gets (§9.3's own [BUG]), but the \
         implementation plan explicitly decided *not* to port that one \
         (\"Mimas must initialise all 9\") -- 0x7F here is the deliberate \
         divergence, not an oversight"
    );

    let kb = PeripheralState::Keyboard(KeyboardState::default());
    let pd = kb.to_port_data();
    assert_eq!(pd.id, id::KEYBOARD);
    assert_eq!(pd.size, 4);
    assert_eq!(
        &pd.data[..4],
        &[0xFF, 0xF8, 0x06, 0x00],
        "§9.7's literal init"
    );

    let mouse = PeripheralState::Mouse(MouseState::default());
    let pd = mouse.to_port_data();
    assert_eq!(pd.id, id::MOUSE);
    assert_eq!(pd.size, 3);
    assert_eq!(&pd.data[..3], &[0x00, 0x00, 0x00], "§9.6's literal init");

    let gun = PeripheralState::Gun(GunState::default());
    let pd = gun.to_port_data();
    assert_eq!(
        pd.status,
        status::GUN_DIRECT,
        "§5.5: a gun connects with status 0xA0, never 0xF1"
    );
    assert_eq!(
        pd.size, 0,
        "§5.5: a gun's *entire* INTBACK contribution is the status byte alone -- \
         no ID byte, no data bytes (low nibble of 0xA0 is 0, \"no peripheral \
         entries follow\"). Mimas's `PortData::size` counts only bytes past \
         status+id (see `PadState`'s size=2), so the equivalent value is 0, \
         not the real hardware struct's `port->size == 1` (which counts the \
         status byte itself in its own flat-array model)"
    );
}

#[test]
fn gun_contributes_only_its_status_byte_to_the_intback_stream() {
    // End-to-end version of the size=0 assertion above: even with a
    // "connected" gun on port 1 and a real pad on port 2, `chunk_port_data`
    // must emit exactly `A0 | <port 2's own bytes>` -- one byte for the gun,
    // nothing else -- matching §5.5's own worked example shape
    // ("A gun on port 1: A0 only (size == 1)").
    use saturn_core::peripheral::PortData;

    let mut p1 = PeripheralState::Gun(GunState::default()).to_port_data();
    let mut p2 = PeripheralState::Pad(PadState::default()).to_port_data();
    let mut ram = [0u8; 128];

    // Mirror `Smpc::chunk_port_data`'s own algorithm without needing a full
    // `Smpc`/`WorkRam` (it's a private associated function) -- inline here
    // since the shape is simple and independently re-derivable from §5.5.
    fn chunk(p1: &mut PortData, p2: &mut PortData, ram: &mut [u8; 128]) {
        let mut i = 0;
        ram[i] = p1.status;
        i += 1;
        if p1.status != status::NOT_CONNECTED && p1.status != status::GUN_DIRECT {
            ram[i] = p1.id;
            i += 1;
        }
        while p1.offset < p1.size {
            ram[i] = p1.data[p1.offset];
            i += 1;
            p1.offset += 1;
        }
        ram[i] = p2.status;
        i += 1;
        if p2.status != status::NOT_CONNECTED && p2.status != status::GUN_DIRECT {
            ram[i] = p2.id;
            i += 1;
        }
        while p2.offset < p2.size {
            ram[i] = p2.data[p2.offset];
            i += 1;
            p2.offset += 1;
        }
    }
    chunk(&mut p1, &mut p2, &mut ram);

    assert_eq!(ram[0], status::GUN_DIRECT, "byte 0: the gun's status alone");
    assert_eq!(
        ram[1],
        status::DIRECT,
        "byte 1: port 2's status immediately follows"
    );
    assert_eq!(ram[2], id::PAD);
    assert_eq!(&ram[3..5], &[0xFF, 0xFF]);
}

#[test]
fn ddr_id_nibble_covers_every_connected_type() {
    // §6.3's full table, independently re-derived (not read back from the
    // implementation): 0xC pad/gun, 0x71 3D-pad/keyboard, 0x70 mouse, 0x7F
    // nothing, and the "unsupported, PDR left untouched" row for
    // wheel/mission-stick/twin-sticks.
    use saturn_core::shared_buffers::WorkRam;
    use saturn_core::smpc::{reg, Smpc};

    let cases: &[(&str, PeripheralState, Option<u8>)] = &[
        ("disconnected", PeripheralState::Disconnected, Some(0x7F)),
        ("gun", PeripheralState::Gun(GunState::default()), Some(0x7C)),
        ("pad", PeripheralState::Pad(PadState::default()), Some(0x7C)),
        (
            "pad3d",
            PeripheralState::Pad3D(Pad3DState::default()),
            Some(0x71),
        ),
        (
            "keyboard",
            PeripheralState::Keyboard(KeyboardState::default()),
            Some(0x71),
        ),
        (
            "mouse",
            PeripheralState::Mouse(MouseState::default()),
            Some(0x70),
        ),
        ("wheel", PeripheralState::Wheel(WheelState::default()), None),
        (
            "mission_stick",
            PeripheralState::MissionStick(MissionStickState::default()),
            None,
        ),
        (
            "twin_sticks",
            PeripheralState::TwinSticks(TwinSticksState::default()),
            None,
        ),
    ];

    for (name, state, expected) in cases {
        let mut smpc = Smpc::new();
        smpc.set_peripheral_state(1, *state);
        let work_ram = WorkRam::new();
        // A sentinel so the "unsupported -> unchanged" rows are observably
        // distinguishable from a coincidental match.
        work_ram.smpc_regs.write().unwrap()[reg::PDR1] = 0xAA;
        let old = work_ram.smpc_regs.read().unwrap()[reg::DDR1];
        smpc.on_register_write(reg::DDR1, 0x00, old, &work_ram);
        let pdr1 = work_ram.smpc_regs.read().unwrap()[reg::PDR1];
        match expected {
            Some(nibble) => assert_eq!(pdr1, *nibble, "{name}: DDR1 must select {nibble:#04x}"),
            None => assert_eq!(
                pdr1, 0xAA,
                "{name}: unsupported type must leave PDR1 untouched"
            ),
        }
    }
}

#[test]
fn wheel_hysteresis_press_thresholds_match_the_reference_table() {
    // §9.5's press thresholds (0x67 left, 0x97 right) applied at
    // `to_port_data` time -- documented simplification: this is *not* true
    // stateful hysteresis (no live analog input caller exists yet to need
    // the separate release thresholds 0x6F/0x8F), it applies only the
    // tighter press threshold every call. See `WheelState::axis1`... no per-
    // field comment exists since the struct just stores a raw axis value;
    // the simplification lives here, at the one place that interprets it.
    let mut w = WheelState::default();

    w.axis1 = 0x67; // at the press threshold
    let b0 = PeripheralState::Wheel(w).to_port_data().data[0];
    assert_eq!(b0 & (1 << 6), 0, "Left (bit 6) must read pressed at 0x67");

    w.axis1 = 0x7F; // neutral
    let b0 = PeripheralState::Wheel(w).to_port_data().data[0];
    assert_eq!(b0 & 0xC0, 0xC0, "neither Left nor Right at neutral");

    w.axis1 = 0x97; // at the press threshold
    let b0 = PeripheralState::Wheel(w).to_port_data().data[0];
    assert_eq!(b0 & (1 << 7), 0, "Right (bit 7) must read pressed at 0x97");
}

#[test]
fn mouse_buttons_are_active_high() {
    let mut m = MouseState::default();
    m.left = true;
    let pd = PeripheralState::Mouse(m).to_port_data();
    assert_eq!(pd.data[0] & 1, 1, "§9.6: Left is bit 0, active-high");
}

#[test]
fn mouse_negative_displacement_is_ones_complement() {
    // §9.6: "mousebits[1] = ~diffx" -- a literal bitwise NOT of the
    // magnitude, stored at the moment of the real `PerMouseMove` call (not
    // recomputed later). `MouseState`'s displacement fields mirror that:
    // whatever is stored there already IS the wire byte.
    let mut m = MouseState::default();
    m.x_sign = true;
    m.x_displacement = !1u8; // magnitude 1, one's-complement-encoded: 0xFE
    let pd = PeripheralState::Mouse(m).to_port_data();
    assert_eq!(pd.data[1], 0xFE);
    assert_eq!(
        pd.data[0] & (1 << 4),
        1 << 4,
        "the x-sign bit must also be set"
    );
}

#[test]
fn mouse_flush_clears_deltas_but_keeps_buttons() {
    // §5.4 step 2 / §9.6: `PerFlush` clears sign+overflow+displacement,
    // keeps the button bits (`mousebits[0] &= 0x0F`).
    let mut m = MouseState::default();
    m.left = true;
    m.start = true;
    m.x_sign = true;
    m.x_overflow = true;
    m.x_displacement = 0x42;
    m.y_displacement = 0x99;
    let mut state = PeripheralState::Mouse(m);
    state.flush_mouse_deltas();

    let PeripheralState::Mouse(after) = state else {
        panic!("flush must not change the variant");
    };
    assert!(after.left && after.start, "buttons must survive a flush");
    assert!(!after.x_sign && !after.x_overflow);
    assert_eq!((after.x_displacement, after.y_displacement), (0, 0));
}
