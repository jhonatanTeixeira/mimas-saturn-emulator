// saturn-core/src/peripheral.rs
//! SMPC peripheral data model and constants.

/// Maximum number of bytes a single port can report.
pub const PORT_DATA_MAX: usize = 256;

/// Standard `PortData_struct` layout from Yabause (see §0.1).
#[derive(Clone, Copy)]
pub struct PortData {
    pub status: u8,
    pub id: u8,
    pub data: [u8; PORT_DATA_MAX],
    pub size: usize,
    /// The offset cursor is the chunker's read position (§5.4).
    pub offset: usize,
}

impl Default for PortData {
    fn default() -> Self {
        Self {
            status: status::NOT_CONNECTED,
            id: 0,
            data: [0; PORT_DATA_MAX],
            size: 0,
            offset: 0,
        }
    }
}

/// Peripheral IDs (§5.5). The low nibble (`ID & 0x0F`) encodes the data size in bytes.
pub mod id {
    pub const PAD: u8 = 0x02; // 2 bytes
    pub const WHEEL: u8 = 0x13; // 3 bytes
    pub const MISSION_STICK: u8 = 0x15; // 5 bytes
    pub const PAD_3D: u8 = 0x16; // 6 bytes
    pub const TWIN_STICKS: u8 = 0x19; // 9 bytes
    pub const GUN: u8 = 0x25; // 5 bytes
    pub const KEYBOARD: u8 = 0x34; // 4 bytes
    pub const MOUSE: u8 = 0xE3; // 3 bytes
    pub const EMPTY_TAP_SLOT: u8 = 0xFF; // Written as a single byte.
}

/// Port status bytes (§5.5).
pub mod status {
    // Standard produced values
    pub const DIRECT: u8 = 0xF1;
    pub const NOT_CONNECTED: u8 = 0xF0;
    pub const GUN_DIRECT: u8 = 0xA0;
    pub const MULTITAP: u8 = 0x16;

    // Values listed in §5.5 but never produced by the reference
    // Do not synthesize these in the emulator.
    pub const SEGA_TAP: u8 = 0x04;
    pub const CLOCK_SERIAL_MIN: u8 = 0x21;
    pub const CLOCK_SERIAL_MAX: u8 = 0x2F;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeripheralKind {
    Pad,
    Wheel,
    MissionStick,
    Pad3D,
    TwinSticks,
    Gun,
    Keyboard,
    Mouse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PadState {
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
    pub start: bool,
    pub a: bool,
    pub b: bool,
    pub c: bool,
    pub x: bool,
    pub y: bool,
    pub z: bool,
    pub l: bool,
    pub r: bool,
}

impl Default for PadState {
    fn default() -> Self {
        Self {
            up: false,
            down: false,
            left: false,
            right: false,
            start: false,
            a: false,
            b: false,
            c: false,
            x: false,
            y: false,
            z: false,
            l: false,
            r: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WheelState {
    pub axis1: u8,
    pub right: bool,
    pub left: bool,
    pub down: bool,
    pub up: bool,
    pub start: bool,
    pub a: bool,
    pub c: bool,
    pub b: bool,
    pub r: bool,
    pub x: bool,
    pub y: bool,
    pub z: bool,
    pub l: bool,
}

impl Default for WheelState {
    fn default() -> Self {
        Self {
            axis1: 0x7F,
            up: false,
            down: false,
            left: false,
            right: false,
            start: false,
            a: false,
            b: false,
            c: false,
            x: false,
            y: false,
            z: false,
            l: false,
            r: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MissionStickState {
    pub axis1: u8,
    pub axis2: u8,
    /// The wire byte for `analogbits[4]` directly -- real hardware's
    /// `PerAxis3Value` setter (§9.5) applies `-(s8)val` (two's complement
    /// negation) to a *live* joystick reading before storing it, but no live
    /// analog input frontend calls a setter here yet, so there is nowhere to
    /// apply that transform except once, honestly, at the single call site
    /// that would ever set this. Treat it as pre-encoded (same convention
    /// `MouseState`'s displacement bytes already use) -- defaults to `0x7F`
    /// like every other axis (§9.3's literal initializer table), not an
    /// inverted value. A future live-input caller computing this from a raw
    /// analog reading is responsible for negating it first.
    pub axis3: u8,
    pub right: bool,
    pub left: bool,
    pub down: bool,
    pub up: bool,
    pub start: bool,
    pub a: bool,
    pub c: bool,
    pub b: bool,
    pub r: bool,
    pub x: bool,
    pub y: bool,
    pub z: bool,
    pub l: bool,
}

impl Default for MissionStickState {
    fn default() -> Self {
        Self {
            axis1: 0x7F,
            axis2: 0x7F,
            axis3: 0x7F,
            up: false,
            down: false,
            left: false,
            right: false,
            start: false,
            a: false,
            b: false,
            c: false,
            x: false,
            y: false,
            z: false,
            l: false,
            r: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pad3DState {
    pub axis1: u8,
    pub axis2: u8,
    pub axis3: u8,
    pub axis4: u8,
    pub right: bool,
    pub left: bool,
    pub down: bool,
    pub up: bool,
    pub start: bool,
    pub a: bool,
    pub c: bool,
    pub b: bool,
    pub r: bool,
    pub x: bool,
    pub y: bool,
    pub z: bool,
    pub l: bool,
}

impl Default for Pad3DState {
    fn default() -> Self {
        Self {
            axis1: 0x7F,
            axis2: 0x7F,
            axis3: 0x7F,
            axis4: 0x7F,
            up: false,
            down: false,
            left: false,
            right: false,
            start: false,
            a: false,
            b: false,
            c: false,
            x: false,
            y: false,
            z: false,
            l: false,
            r: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TwinSticksState {
    pub axis1: u8,
    pub axis2: u8,
    pub axis3: u8,
    pub axis4: u8,
    pub axis5: u8,
    pub axis6: u8,
    /// The wire byte for `analogbits[8]` directly (see `MissionStickState::axis3`'s
    /// doc comment for why this is pre-encoded, not inverted in
    /// `to_port_data`). Defaults to the neutral `0x7F`, matching every other
    /// axis -- real hardware's own `PERTWINSTICKS` initializer leaves this
    /// specific byte at `0x00` instead (§9.3's `[BUG]` #39), but
    /// `docs/implementation-plans/smpc-peripheral.md` Phase 7 explicitly
    /// decided *not* to port that one ("Mimas must initialise all 9") --
    /// this is a deliberate, already-made project decision, not an
    /// independent judgment call made here.
    pub axis7: u8,
    pub right: bool,
    pub left: bool,
    pub down: bool,
    pub up: bool,
    pub start: bool,
    pub a: bool,
    pub c: bool,
    pub b: bool,
    pub r: bool,
    pub x: bool,
    pub y: bool,
    pub z: bool,
    pub l: bool,
}

impl Default for TwinSticksState {
    fn default() -> Self {
        Self {
            axis1: 0x7F,
            axis2: 0x7F,
            axis3: 0x7F,
            axis4: 0x7F,
            axis5: 0x7F,
            axis6: 0x7F,
            axis7: 0x7F,
            up: false,
            down: false,
            left: false,
            right: false,
            start: false,
            a: false,
            b: false,
            c: false,
            x: false,
            y: false,
            z: false,
            l: false,
            r: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GunState {
    pub trigger: bool,
    pub start: bool,
    pub x: u16,
    pub y: u16,
}

impl Default for GunState {
    fn default() -> Self {
        Self {
            trigger: false,
            start: false,
            x: 0,
            y: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyboardState {
    // Unimplemented in reference, keep it inert
}

impl Default for KeyboardState {
    fn default() -> Self {
        Self {}
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseState {
    pub left: bool,
    pub right: bool,
    pub middle: bool,
    pub start: bool,
    pub x_sign: bool,
    pub y_sign: bool,
    pub x_overflow: bool,
    pub y_overflow: bool,
    pub x_displacement: u8,
    pub y_displacement: u8,
}

impl Default for MouseState {
    fn default() -> Self {
        Self {
            left: false,
            right: false,
            middle: false,
            start: false,
            x_sign: false,
            y_sign: false,
            x_overflow: false,
            y_overflow: false,
            x_displacement: 0,
            y_displacement: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeripheralState {
    Disconnected,
    Pad(PadState),
    Wheel(WheelState),
    MissionStick(MissionStickState),
    Pad3D(Pad3DState),
    TwinSticks(TwinSticksState),
    Gun(GunState),
    Keyboard(KeyboardState),
    Mouse(MouseState),
}

impl Default for PeripheralState {
    fn default() -> Self {
        PeripheralState::Disconnected
    }
}

impl PeripheralState {
    /// `PerFlush` (§5.4 step 2, `peripheral.c:791-804`): ends a "mouse
    /// frame" by clearing the accumulated relative-motion deltas (and the
    /// sign/overflow bits derived from them) while leaving the button bits
    /// alone. Called on both ports immediately after `SmpcINTBACKPeripheral`
    /// takes its snapshot, so the next accumulation period starts clean. A
    /// no-op for every other peripheral type.
    pub fn flush_mouse_deltas(&mut self) {
        if let PeripheralState::Mouse(m) = self {
            m.x_sign = false;
            m.y_sign = false;
            m.x_overflow = false;
            m.y_overflow = false;
            m.x_displacement = 0;
            m.y_displacement = 0;
        }
    }

    pub fn to_port_data(&self) -> PortData {
        let mut pd = PortData::default();
        match self {
            PeripheralState::Disconnected => {
                pd.status = status::NOT_CONNECTED;
                pd.size = 0;
            }
            PeripheralState::Pad(state) => {
                pd.status = status::DIRECT;
                pd.id = id::PAD;
                pd.size = 2;
                let b = serialize_buttons(
                    state.right,
                    state.left,
                    state.down,
                    state.up,
                    state.start,
                    state.a,
                    state.c,
                    state.b,
                    state.r,
                    state.x,
                    state.y,
                    state.z,
                    state.l,
                );
                pd.data[0] = b[0];
                pd.data[1] = b[1];
            }
            PeripheralState::Wheel(state) => {
                pd.status = status::DIRECT;
                pd.id = id::WHEEL;
                pd.size = 3;

                let mut b = serialize_buttons(
                    state.right,
                    state.left,
                    state.down,
                    state.up,
                    state.start,
                    state.a,
                    state.c,
                    state.b,
                    state.r,
                    state.x,
                    state.y,
                    state.z,
                    state.l,
                );

                // Digital synthesis with hysteresis for Wheel
                if state.axis1 <= 0x67 {
                    b[0] &= !(1 << 6);
                } // Left
                if state.axis1 >= 0x97 {
                    b[0] &= !(1 << 7);
                } // Right

                pd.data[0] = b[0];
                pd.data[1] = b[1];
                pd.data[2] = state.axis1;
            }
            PeripheralState::MissionStick(state) => {
                pd.status = status::DIRECT;
                pd.id = id::MISSION_STICK;
                pd.size = 5;

                let mut b = serialize_buttons(
                    state.right,
                    state.left,
                    state.down,
                    state.up,
                    state.start,
                    state.a,
                    state.c,
                    state.b,
                    state.r,
                    state.x,
                    state.y,
                    state.z,
                    state.l,
                );

                // Digital synthesis
                if state.axis1 <= 0x56 {
                    b[0] &= !(1 << 6);
                } // Left
                if state.axis1 >= 0xAB {
                    b[0] &= !(1 << 7);
                } // Right
                if state.axis2 <= 0x65 {
                    b[0] &= !(1 << 4);
                } // Up
                if state.axis2 >= 0xA9 {
                    b[0] &= !(1 << 5);
                } // Down

                pd.data[0] = b[0];
                pd.data[1] = b[1];
                pd.data[2] = state.axis1;
                pd.data[3] = state.axis2;
                // Not inverted here -- `axis3` is already wire-format; see
                // its own doc comment.
                pd.data[4] = state.axis3;
            }
            PeripheralState::TwinSticks(state) => {
                pd.status = status::DIRECT;
                pd.id = id::TWIN_STICKS;
                pd.size = 9;

                let mut b = serialize_buttons(
                    state.right,
                    state.left,
                    state.down,
                    state.up,
                    state.start,
                    state.a,
                    state.c,
                    state.b,
                    state.r,
                    state.x,
                    state.y,
                    state.z,
                    state.l,
                );

                // Digital synthesis
                if state.axis1 <= 0x56 {
                    b[0] &= !(1 << 6);
                } // Left
                if state.axis1 >= 0xAB {
                    b[0] &= !(1 << 7);
                } // Right
                if state.axis2 <= 0x65 {
                    b[0] &= !(1 << 4);
                } // Up
                if state.axis2 >= 0xA9 {
                    b[0] &= !(1 << 5);
                } // Down

                pd.data[0] = b[0];
                pd.data[1] = b[1];
                pd.data[2] = state.axis1;
                pd.data[3] = state.axis2;
                pd.data[4] = state.axis3;
                pd.data[5] = state.axis4;
                pd.data[6] = state.axis5;
                pd.data[7] = state.axis6;
                // Not inverted here -- `axis7` is already wire-format; see
                // its own doc comment for why its default is the neutral
                // 0x7F (a deliberate divergence from the real 0x00 [BUG]).
                pd.data[8] = state.axis7;
            }
            PeripheralState::Pad3D(state) => {
                pd.status = status::DIRECT;
                pd.id = id::PAD_3D;
                pd.size = 6;
                let b = serialize_buttons(
                    state.right,
                    state.left,
                    state.down,
                    state.up,
                    state.start,
                    state.a,
                    state.c,
                    state.b,
                    state.r,
                    state.x,
                    state.y,
                    state.z,
                    state.l,
                );
                pd.data[0] = b[0];
                pd.data[1] = b[1];
                pd.data[2] = state.axis1;
                pd.data[3] = state.axis2;
                pd.data[4] = state.axis3;
                pd.data[5] = state.axis4;
            }
            PeripheralState::Keyboard(_) => {
                pd.status = status::DIRECT;
                pd.id = id::KEYBOARD;
                pd.size = 4;
                pd.data[0] = 0xFF;
                pd.data[1] = 0xF8;
                pd.data[2] = 0x06;
                pd.data[3] = 0x00;
            }
            PeripheralState::Mouse(state) => {
                pd.status = status::DIRECT;
                pd.id = id::MOUSE;
                pd.size = 3;
                let mut b0 = 0x00;
                if state.left {
                    b0 |= 1 << 0;
                }
                if state.right {
                    b0 |= 1 << 1;
                }
                if state.middle {
                    b0 |= 1 << 2;
                }
                if state.start {
                    b0 |= 1 << 3;
                }
                if state.x_sign {
                    b0 |= 1 << 4;
                }
                if state.y_sign {
                    b0 |= 1 << 5;
                }
                if state.x_overflow {
                    b0 |= 1 << 6;
                }
                if state.y_overflow {
                    b0 |= 1 << 7;
                }
                pd.data[0] = b0;
                pd.data[1] = state.x_displacement;
                pd.data[2] = state.y_displacement;
            }
            PeripheralState::Gun(state) => {
                // §5.5: a gun's *entire* INTBACK contribution is the single
                // status byte 0xA0 -- no ID byte, no data bytes follow (low
                // nibble 0). Real hardware's own `port->size == 1` counts
                // that status byte itself (its flat byte-array model starts
                // counting from index 0); Mimas's `PortData::size` instead
                // counts only the *extra* bytes `chunk_port_data` copies
                // after the status/id bytes it already writes unconditionally
                // (see `PadState`'s `size = 2`, two data bytes past status+id)
                // -- so the equivalent value here is `0`, not `1`. Getting
                // this wrong made `chunk_port_data` emit one extra,
                // unspecified byte into the INTBACK stream for every gun.
                pd.status = status::GUN_DIRECT;
                pd.size = 0;
                // Gun position/buttons flow through PDR1 (§6.2 mode 0x00)
                // and the VDP2 external latch (§6.4/§6.5), never through this
                // report -- these bytes mirror real hardware's own initial
                // `gunbits` values (§9.3: `7C FF FF FF FF`) for when that
                // direct-access path is wired, but are otherwise inert since
                // `size = 0` means `chunk_port_data` never reads them.
                pd.id = id::GUN;
                let mut b = 0xFF;
                if state.trigger {
                    b &= !(1 << 4);
                }
                if state.start {
                    b &= !(1 << 5);
                }
                pd.data[0] = 0x7C;
                pd.data[1] = 0xFF;
                pd.data[2] = b;
                pd.data[3] = (state.x >> 8) as u8;
                pd.data[4] = (state.x & 0xFF) as u8;
                pd.data[5] = (state.y >> 8) as u8;
                pd.data[6] = (state.y & 0xFF) as u8;
            }
        }
        pd
    }
}

fn serialize_buttons(
    right: bool,
    left: bool,
    down: bool,
    up: bool,
    start: bool,
    a: bool,
    c: bool,
    b: bool,
    r: bool,
    x: bool,
    y: bool,
    z: bool,
    l: bool,
) -> [u8; 2] {
    let mut b0 = 0xFF;
    if right {
        b0 &= !(1 << 7);
    }
    if left {
        b0 &= !(1 << 6);
    }
    if down {
        b0 &= !(1 << 5);
    }
    if up {
        b0 &= !(1 << 4);
    }
    if start {
        b0 &= !(1 << 3);
    }
    if a {
        b0 &= !(1 << 2);
    }
    if c {
        b0 &= !(1 << 1);
    }
    if b {
        b0 &= !(1 << 0);
    }

    let mut b1 = 0xFF;
    if r {
        b1 &= !(1 << 7);
    }
    if x {
        b1 &= !(1 << 6);
    }
    if y {
        b1 &= !(1 << 5);
    }
    if z {
        b1 &= !(1 << 4);
    }
    if l {
        b1 &= !(1 << 3);
    }
    [b0, b1]
}
