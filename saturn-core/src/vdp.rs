use crate::shared_buffers::WorkRam;
use arc_swap::ArcSwap;
use std::sync::Arc;

pub struct Framebuffer {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
}

impl Framebuffer {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width * height],
        }
    }

    fn fill(&mut self, color: u32) {
        self.pixels.fill(color);
    }
}

pub struct Vdp1Framebuffers {
    pub banks: [std::sync::RwLock<Box<[u8; 0x40000]>>; 2],
    pub back: std::sync::atomic::AtomicUsize,
}

impl Vdp1Framebuffers {
    pub fn new() -> Self {
        Self {
            banks: [
                std::sync::RwLock::new(Box::new([0; 0x40000])),
                std::sync::RwLock::new(Box::new([0; 0x40000])),
            ],
            back: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

pub struct Vdp {
    pub front_buffer: Arc<ArcSwap<Framebuffer>>,
}

impl Vdp {
    pub fn new() -> Self {
        Self {
            front_buffer: Arc::new(ArcSwap::new(Arc::new(Framebuffer::new(320, 224)))),
        }
    }

    pub fn present(&self, frame: Framebuffer) {
        self.front_buffer.store(Arc::new(frame));
    }
}

impl Default for Vdp {
    fn default() -> Self {
        Self::new()
    }
}

fn resolution_from_tvmd(tvmd: u16) -> (usize, usize) {
    let width = match tvmd & 0x7 {
        0 | 4 => 320,
        1 | 5 => 352,
        2 | 6 => 640,
        _ => 704,
    };
    let mut height = match (tvmd >> 4) & 0x3 {
        0 => 224,
        1 => 240,
        _ => 256,
    };
    if ((tvmd >> 6) & 0x3) == 3 {
        height *= 2;
    }
    (width, height)
}

/// `SAT2YAB1` (`docs/hardware-reference/vdp1.md` §on colour conversion,
/// **Deliberate divergence from Yabause, recorded per
/// `docs/implementation-plans/vdp2.md` §0.4/§1.2.** Real hardware/Yabause
/// (`SAT2YAB1`, `vdp1.cpp:1321-1325`; `COLSAT2YAB16`, `vidsoft.c:54-56`)
/// expand each 5-bit channel to 8 bits by a plain left-shift only
/// (`0x1F -> 0xF8`, never reaching `0xFF`). Mimas instead replicates the
/// high bits into the low bits (`(v<<3)|(v>>2)`, `0x1F -> 0xFF`) -- the more
/// common, better analogue reconstruction of a 5-bit DAC output, and what
/// this project's own tests have committed to since before this comment
/// existed. Keep it; do not "fix" this back to Yabause's shift-only formula
/// on a future pixel-for-pixel comparison against Yabause output.
fn rgb555_to_xrgb8888(color: u16) -> u32 {
    let r5 = (color & 0x1F) as u32;
    let g5 = ((color >> 5) & 0x1F) as u32;
    let b5 = ((color >> 10) & 0x1F) as u32;
    let r8 = (r5 << 3) | (r5 >> 2);
    let g8 = (g5 << 3) | (g5 >> 2);
    let b8 = (b5 << 3) | (b5 >> 2);
    (r8 << 16) | (g8 << 8) | b8
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Vdp1Status {
    IDLE,
    RUNNING,
}

pub struct Vdp1State {
    pub tvmr: u16,
    pub fbcr: u16,
    pub ptmr: u16,
    pub ewdr: u16,
    pub ewlr: u16,
    pub ewrr: u16,
    pub endr: u16,
    pub edsr: u16,
    pub lopr: u16,
    pub copr: u16,

    pub addr: usize,
    pub local_x: i16,
    pub local_y: i16,
    pub systemclip_x1: u16,
    pub systemclip_y1: u16,
    pub systemclip_x2: u16,
    pub systemclip_y2: u16,
    pub userclip_x1: u16,
    pub userclip_y1: u16,
    pub userclip_x2: u16,
    pub userclip_y2: u16,

    pub status: Vdp1Status,
    pub return_addr: Option<u32>,

    pub vblank_erase: bool,
    pub current_frame: u8,
    pub swap_frame_buffer: bool,
    pub manualerase: bool,
    pub manualchange: bool,
}

impl Vdp1State {
    pub fn new() -> Self {
        Self {
            tvmr: 0,
            fbcr: 0,
            ptmr: 0,
            ewdr: 0,
            ewlr: 0,
            ewrr: 0,
            endr: 0,
            edsr: 0,
            lopr: 0,
            copr: 0,

            addr: 0,
            local_x: 0,
            local_y: 0,
            systemclip_x1: 0,
            systemclip_y1: 0,
            systemclip_x2: 1024,
            systemclip_y2: 1024,
            userclip_x1: 0,
            userclip_y1: 0,
            userclip_x2: 1024,
            userclip_y2: 1024,

            status: Vdp1Status::IDLE,
            return_addr: None,

            vblank_erase: false,
            current_frame: 0,
            swap_frame_buffer: false,
            manualerase: false,
            manualchange: false,
        }
    }
}

pub struct CmdTable {
    pub cmdctrl: u16,
    pub cmdlink: u16,
    pub cmdpmod: u16,
    pub cmdcolr: u16,
    pub cmdsrca: u16,
    pub cmdsize: u16,
    pub cmdxa: i16,
    pub cmdya: i16,
    pub cmdxb: i16,
    pub cmdyb: i16,
    pub cmdxc: i16,
    pub cmdyc: i16,
    pub cmdxd: i16,
    pub cmdyd: i16,
    pub cmdgrda: u16,
}

impl CmdTable {
    pub fn read(vram: &[u8], addr: usize) -> Self {
        Self {
            cmdctrl: u16::from_be_bytes([vram[addr], vram[addr + 1]]),
            cmdlink: u16::from_be_bytes([vram[addr + 2], vram[addr + 3]]),
            cmdpmod: u16::from_be_bytes([vram[addr + 4], vram[addr + 5]]),
            cmdcolr: u16::from_be_bytes([vram[addr + 6], vram[addr + 7]]),
            cmdsrca: u16::from_be_bytes([vram[addr + 8], vram[addr + 9]]),
            cmdsize: u16::from_be_bytes([vram[addr + 10], vram[addr + 11]]),
            cmdxa: i16::from_be_bytes([vram[addr + 12], vram[addr + 13]]),
            cmdya: i16::from_be_bytes([vram[addr + 14], vram[addr + 15]]),
            cmdxb: i16::from_be_bytes([vram[addr + 16], vram[addr + 17]]),
            cmdyb: i16::from_be_bytes([vram[addr + 18], vram[addr + 19]]),
            cmdxc: i16::from_be_bytes([vram[addr + 20], vram[addr + 21]]),
            cmdyc: i16::from_be_bytes([vram[addr + 22], vram[addr + 23]]),
            cmdxd: i16::from_be_bytes([vram[addr + 24], vram[addr + 25]]),
            cmdyd: i16::from_be_bytes([vram[addr + 26], vram[addr + 27]]),
            cmdgrda: u16::from_be_bytes([vram[addr + 28], vram[addr + 29]]),
        }
    }
}

pub fn vdp1_erase_framebuffer(
    state: &Vdp1State,
    ram: &crate::shared_buffers::WorkRam,
    back: usize,
) {
    let mut fb = ram.vdp1_framebuffers.banks[back].write().unwrap();
    let erase_val = state.ewdr;

    let width = match state.tvmr & 0x3 {
        0 => 512,
        1 => 1024,
        _ => 512,
    };
    let height = match state.tvmr & 0x3 {
        3 => 512,
        _ => 256,
    };
    let pixel_size = if (state.tvmr & 0x3) == 1 || (state.tvmr & 0x3) == 3 {
        1
    } else {
        2
    };

    let y1 = (state.ewlr & 0x1FF) as usize;
    let x1 = ((state.ewlr >> 6) & 0x1F8) as usize;

    let mut h = ((state.ewrr & 0x1FF) + 1) as usize;
    let mut w = (((state.ewrr >> 6) & 0x3F8) + 8) as usize;

    // clamp logic
    if y1 >= height {
        return;
    }
    if x1 >= width {
        return;
    }
    h = h.min(height - y1);

    if pixel_size == 1 {
        // 8-bit path
        w = ((state.ewrr >> 9) * 16) as usize;
        w = w.min(width - x1);
        let byte_val = (state.ewdr & 0xFF) as u8;
        for y in y1..(y1 + h) {
            for x in x1..(x1 + w) {
                let offset = y * width + x;
                if offset < fb.len() {
                    fb[offset] = byte_val;
                }
            }
        }
    } else {
        // 16-bit path
        w = w.min(width - x1);
        let bytes = erase_val.to_be_bytes();
        for y in y1..(y1 + h) {
            for x in x1..(x1 + w) {
                let offset = (y * width + x) * 2;
                if offset + 1 < fb.len() {
                    fb[offset] = bytes[0];
                    fb[offset + 1] = bytes[1];
                }
            }
        }
    }
}

pub fn vdp1_swap_frame_buffers(state: &mut Vdp1State, ram: &crate::shared_buffers::WorkRam) {
    state.swap_frame_buffer = false;
    state.current_frame ^= 1;
    state.edsr >>= 1;
    let new_back = ram
        .vdp1_framebuffers
        .back
        .load(std::sync::atomic::Ordering::Relaxed)
        ^ 1;
    ram.vdp1_framebuffers
        .back
        .store(new_back, std::sync::atomic::Ordering::Release);
    vdp1_erase_framebuffer(state, ram, new_back);
}

pub fn execute_vdp1(state: &mut Vdp1State, ram: &crate::shared_buffers::WorkRam) -> bool {
    if state.vblank_erase {
        state.vblank_erase = false;
        let back = ram
            .vdp1_framebuffers
            .back
            .load(std::sync::atomic::Ordering::Relaxed);
        vdp1_erase_framebuffer(state, ram, back);
    }

    let fake_draw = state.ptmr == 0;

    if state.ptmr == 1 {
        // PTMR=1 shifts EDSR >>= 1 before drawing
        state.edsr >>= 1;
    }

    let vram = ram.vdp1_vram.read().unwrap();
    let back = ram
        .vdp1_framebuffers
        .back
        .load(std::sync::atomic::Ordering::Relaxed);
    let mut fb = ram.vdp1_framebuffers.banks[back].write().unwrap();

    if state.status == Vdp1Status::IDLE {
        state.addr = 0;
        state.copr = 0;
        state.status = Vdp1Status::RUNNING;
    }

    let mut runaway = 0;
    loop {
        if runaway > 4000 {
            break; // Mimas's runaway guard
        }
        runaway += 1;

        if state.addr > 0x7FFFF {
            state.status = Vdp1Status::IDLE;
            return false;
        }

        state.copr = (state.addr >> 3) as u16;

        if (state.edsr & 0x02) != 0 {
            // "Batsugun escape": EDSR & 2 at top of loop
            state.lopr = state.copr; // Write LOPR/COPR (COPR is already written)
            state.status = Vdp1Status::IDLE;
            return false;
        }

        if state.addr + 32 > vram.len() {
            break; // bounds guard
        }

        let cmd = CmdTable::read(&vram[..], state.addr);

        if (cmd.cmdctrl & 0x8000) != 0 {
            state.status = Vdp1Status::IDLE;
            if runaway == 1 {
                return false;
            }
            state.edsr |= 2;
            return true;
        }

        let comm_type = cmd.cmdctrl & 0x000F;
        if comm_type >= 12 {
            // Bad command -> EDSR |= 2, LOPR/COPR = addr>>3, abort
            state.edsr |= 2;
            state.lopr = (state.addr >> 3) as u16;
            state.status = Vdp1Status::IDLE;
            return false;
        }

        if (cmd.cmdctrl & 0x4000) == 0 {
            // Draw not skipped
            if comm_type == 10 {
                // Local Coordinates
                state.local_x = cmd.cmdxa;
                state.local_y = cmd.cmdya;
            } else if comm_type == 9 {
                // System Clipping
                state.systemclip_x1 = 0;
                state.systemclip_y1 = 0;
                state.systemclip_x2 = cmd.cmdxc as u16;
                state.systemclip_y2 = cmd.cmdyc as u16;
            } else if comm_type == 8 || comm_type == 11 {
                // User Clipping
                state.userclip_x1 = cmd.cmdxa as u16;
                state.userclip_y1 = cmd.cmdya as u16;
                state.userclip_x2 = cmd.cmdxc as u16;
                state.userclip_y2 = cmd.cmdyc as u16;
            } else if !fake_draw {
                let current_shape = cmd.cmdctrl & 0x7;
                if current_shape == 0 {
                    // Normal Sprite
                    let tl_x = (cmd.cmdxa as i16).wrapping_add(state.local_x) as i32;
                    let tl_y = (cmd.cmdya as i16).wrapping_add(state.local_y) as i32;
                    let char_width = (((cmd.cmdsize >> 8) & 0x3F) * 8) as i32;
                    let char_height = (cmd.cmdsize & 0xFF) as i32;

                    let tr_x = tl_x + char_width - 1;
                    let tr_y = tl_y;
                    let br_x = tr_x;
                    let br_y = tl_y + char_height - 1;
                    let bl_x = tl_x;
                    let bl_y = br_y;

                    let tl = Point { x: tl_x, y: tl_y };
                    let tr = Point { x: tr_x, y: tr_y };
                    let bl = Point { x: bl_x, y: bl_y };
                    let br = Point { x: br_x, y: br_y };

                    draw_quad(state, &cmd, &vram[..], &mut fb[..], tl, bl, tr, br);
                } else if current_shape == 1 {
                    // Scaled Sprite
                    let x0 = (cmd.cmdxa as i16).wrapping_add(state.local_x) as i32;
                    let y0 = (cmd.cmdya as i16).wrapping_add(state.local_y) as i32;
                    let zp = (cmd.cmdctrl >> 8) & 0xF;

                    let mut x_origin = x0;
                    let mut y_origin = y0;
                    let mut width = cmd.cmdxb as i32;
                    let mut height = cmd.cmdyb as i32;

                    match zp {
                        0x5 => {
                            // upper-left
                            width += 1;
                            height += 1;
                        }
                        0x6 => {
                            // upper-centre
                            x_origin -= width / 2;
                            width += 1;
                            height += 1;
                        }
                        0x7 => {
                            // upper-right
                            x_origin -= width;
                            width += 1;
                            height += 1;
                        }
                        0x9 => {
                            // centre-left
                            y_origin -= height / 2;
                            width += 1;
                            height += 1;
                        }
                        0xA => {
                            // centre-centre
                            x_origin -= width / 2;
                            y_origin -= height / 2;
                            width += 1;
                            height += 1;
                        }
                        0xB => {
                            // centre-right
                            x_origin -= width;
                            y_origin -= height / 2;
                            width += 1;
                            height += 1;
                        }
                        0xD => {
                            // lower-left
                            y_origin -= height;
                            width += 1;
                            height += 1;
                        }
                        0xE => {
                            // lower-centre
                            x_origin -= width / 2;
                            y_origin -= height;
                            width += 1;
                            height += 1;
                        }
                        0xF => {
                            // lower-right
                            x_origin -= width;
                            y_origin -= height;
                            width += 1;
                            height += 1;
                        }
                        _ => {
                            // two-point (0x0, 0x1-0x4, 0x8, 0xC)
                            width = cmd.cmdxc as i32 - x0 + state.local_x as i32 + 1;
                            height = cmd.cmdyc as i32 - y0 + state.local_y as i32 + 1;
                        }
                    }

                    let tl = Point {
                        x: x_origin,
                        y: y_origin,
                    };
                    let tr = Point {
                        x: x_origin + width - 1,
                        y: y_origin,
                    };
                    let bl = Point {
                        x: x_origin,
                        y: y_origin + height - 1,
                    };
                    let br = Point {
                        x: x_origin + width - 1,
                        y: y_origin + height - 1,
                    };

                    draw_quad(state, &cmd, &vram[..], &mut fb[..], tl, bl, tr, br);
                } else if current_shape == 2 || current_shape == 3 || current_shape == 4 {
                    // Distorted Sprite (2, 3) / Polygon (4)
                    let xa = (cmd.cmdxa as i16).wrapping_add(state.local_x) as i32;
                    let ya = (cmd.cmdya as i16).wrapping_add(state.local_y) as i32;
                    let xb = (cmd.cmdxb as i16).wrapping_add(state.local_x) as i32;
                    let yb = (cmd.cmdyb as i16).wrapping_add(state.local_y) as i32;
                    let xc = (cmd.cmdxc as i16).wrapping_add(state.local_x) as i32;
                    let yc = (cmd.cmdyc as i16).wrapping_add(state.local_y) as i32;
                    let xd = (cmd.cmdxd as i16).wrapping_add(state.local_x) as i32;
                    let yd = (cmd.cmdyd as i16).wrapping_add(state.local_y) as i32;

                    let tl = Point { x: xa, y: ya };
                    let tr = Point { x: xb, y: yb };
                    let br = Point { x: xc, y: yc };
                    let bl = Point { x: xd, y: yd };

                    draw_quad(state, &cmd, &vram[..], &mut fb[..], tl, bl, tr, br);
                } else if current_shape == 5 {
                    // Polyline
                    let xa = (cmd.cmdxa as i16).wrapping_add(state.local_x) as i32;
                    let ya = (cmd.cmdya as i16).wrapping_add(state.local_y) as i32;
                    let xb = (cmd.cmdxb as i16).wrapping_add(state.local_x) as i32;
                    let yb = (cmd.cmdyb as i16).wrapping_add(state.local_y) as i32;
                    let xc = (cmd.cmdxc as i16).wrapping_add(state.local_x) as i32;
                    let yc = (cmd.cmdyc as i16).wrapping_add(state.local_y) as i32;
                    let xd = (cmd.cmdxd as i16).wrapping_add(state.local_x) as i32;
                    let yd = (cmd.cmdyd as i16).wrapping_add(state.local_y) as i32;

                    let mut grd = [0u16; 4];
                    let grda = (cmd.cmdgrda as usize) << 3;
                    for i in 0..4 {
                        let addr = (grda + i * 2) & 0x7FFFF;
                        grd[i] = u16::from_be_bytes([vram[addr], vram[(addr + 1) & 0x7FFFF]]);
                    }

                    draw_line_impl(
                        state,
                        &cmd,
                        &vram[..],
                        &mut fb[..],
                        xa,
                        ya,
                        xb,
                        yb,
                        grd[0],
                        grd[1],
                        true,
                    );
                    draw_line_impl(
                        state,
                        &cmd,
                        &vram[..],
                        &mut fb[..],
                        xb,
                        yb,
                        xc,
                        yc,
                        grd[1],
                        grd[2],
                        true,
                    );
                    draw_line_impl(
                        state,
                        &cmd,
                        &vram[..],
                        &mut fb[..],
                        xd,
                        yd,
                        xc,
                        yc,
                        grd[3],
                        grd[2],
                        true,
                    );
                    draw_line_impl(
                        state,
                        &cmd,
                        &vram[..],
                        &mut fb[..],
                        xa,
                        ya,
                        xd,
                        yd,
                        grd[0],
                        grd[3],
                        true,
                    );
                } else if current_shape == 6 {
                    // Line
                    let xa = (cmd.cmdxa as i16).wrapping_add(state.local_x) as i32;
                    let ya = (cmd.cmdya as i16).wrapping_add(state.local_y) as i32;
                    let xb = (cmd.cmdxb as i16).wrapping_add(state.local_x) as i32;
                    let yb = (cmd.cmdyb as i16).wrapping_add(state.local_y) as i32;

                    let mut grd = [0u16; 4];
                    let grda = (cmd.cmdgrda as usize) << 3;
                    for i in 0..2 {
                        let addr = (grda + i * 2) & 0x7FFFF;
                        grd[i] = u16::from_be_bytes([vram[addr], vram[(addr + 1) & 0x7FFFF]]);
                    }
                    draw_line_impl(
                        state,
                        &cmd,
                        &vram[..],
                        &mut fb[..],
                        xa,
                        ya,
                        xb,
                        yb,
                        grd[0],
                        grd[1],
                        false,
                    );
                }
            }
        }

        // Jump processing
        let jp = (cmd.cmdctrl >> 12) & 0x3;
        match jp {
            0 => {
                // NEXT
                state.addr += 0x20;
            }
            1 => {
                // ASSIGN
                state.addr = (cmd.cmdlink as usize) * 8;
                if state.addr == 0 {
                    break; // Mimas guard against 0 hang
                }
            }
            2 => {
                // CALL
                if state.return_addr.is_none() {
                    state.return_addr = Some((state.addr + 0x20) as u32);
                }
                state.addr = (cmd.cmdlink as usize) * 8;
                if state.addr == 0 {
                    break; // Mimas guard against 0 hang
                }
            }
            3 => {
                // RETURN
                if let Some(ret) = state.return_addr.take() {
                    state.addr = ret as usize;
                } else {
                    state.addr += 0x20;
                }
                if state.addr == 0 {
                    break; // Mimas guard against 0 hang
                }
            }
            _ => unreachable!(),
        }
    }
    false
}

pub fn render_back_screen(ram: &WorkRam) -> Framebuffer {
    let mut regs = crate::vdp2_regs::Vdp2Registers::new();
    {
        let lines = ram.vdp2_lines.read().unwrap();
        let line0 = &lines[0];
        for i in 0..0x100 {
            regs.regs[i] = u16::from_be_bytes([line0[i * 2], line0[i * 2 + 1]]);
        }
    }
    let tvmd = regs.tvmd();
    let (width, height) = resolution_from_tvmd(tvmd);
    let disp_enabled = regs.disp() == 1;
    let bdclmd = regs.bdclmd() == 1;

    let mut frame = Framebuffer::new(width, height);

    if !disp_enabled && !bdclmd {
        frame.fill(0x00000000);
    } else {
        let bktal = regs.regs[0xAE / 2];
        let bktau = regs.regs[0xAC / 2];
        let per_line = (bktau & 0x8000) != 0;

        let mut addr = if regs.vram_8mbit() {
            (((bktau & 0x7) as u32) << 16 | (bktal as u32)) * 2
        } else {
            (((bktau & 0x3) as u32) << 16 | (bktal as u32)) * 2
        };

        let vram = ram.vdp2_vram.read().unwrap();
        let mask = vram.len() - 1;

        if per_line {
            for y in 0..height {
                let masked = (addr as usize) & mask;
                let val = u16::from_be_bytes([vram[masked], vram[masked + 1]]);
                let color = rgb555_to_xrgb8888(val);
                let row_start = (y * width) as usize;
                for x in 0..(width as usize) {
                    frame.pixels[row_start + x] = color;
                }
                addr += 2;
            }
        } else {
            let masked = (addr as usize) & mask;
            let val = u16::from_be_bytes([vram[masked], vram[masked + 1]]);
            let back_color = rgb555_to_xrgb8888(val);
            frame.fill(back_color);
        }
    }

    // Overlay VDP1 Framebuffer if active
    let vdp1_regs = ram.vdp1_regs.read().unwrap();
    let tvmr = u16::from_be_bytes([vdp1_regs[0], vdp1_regs[1]]);
    let vdp1_width = match tvmr & 0x3 {
        0 => 512,
        1 => 1024,
        _ => 512,
    };
    drop(vdp1_regs);

    let back = ram
        .vdp1_framebuffers
        .back
        .load(std::sync::atomic::Ordering::Relaxed);
    let vdp1_fb = ram.vdp1_framebuffers.banks[back ^ 1].read().unwrap();
    for y in 0..height {
        for x in 0..width {
            let offset = (y * vdp1_width + x) * 2;
            if offset + 1 < vdp1_fb.len() {
                let color16 = u16::from_be_bytes([vdp1_fb[offset], vdp1_fb[offset + 1]]);
                if color16 != 0 {
                    let pixel_idx = y * width + x;
                    if pixel_idx < frame.pixels.len() {
                        frame.pixels[pixel_idx] = rgb555_to_xrgb8888(color16);
                    }
                }
            }
        }
    }

    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    const REG_TVMD: usize = 0x000;
    const REG_VRSIZE: usize = 0x006;
    const REG_BKTAU: usize = 0x0AC;
    const REG_BKTAL: usize = 0x0AE;

    #[test]
    fn resolution_decodes_common_ntsc_modes() {
        assert_eq!(resolution_from_tvmd(0x0000), (320, 224));
        assert_eq!(resolution_from_tvmd(0x0001), (352, 224));
        assert_eq!(resolution_from_tvmd(0x0010), (320, 240));
    }

    #[test]
    fn rgb555_conversion_hits_full_white_and_pure_channels() {
        // Mimas deliberately keeps bit replication (see
        // `rgb555_to_xrgb8888`'s own doc comment) -- 0x1F << 3 | 0x1F >> 2
        // == 0xFF, the full 8-bit range, not real hardware's 0xF8 cap.
        assert_eq!(
            rgb555_to_xrgb8888(0x7FFF),
            0x00FFFFFF,
            "all channels at max must be white"
        );
        assert_eq!(
            rgb555_to_xrgb8888(0x001F),
            0x00FF0000,
            "R is the low 5 bits, must land in the R byte"
        );
        assert_eq!(
            rgb555_to_xrgb8888(0x03E0),
            0x0000FF00,
            "G is bits 5-9, must land in the G byte"
        );
        assert_eq!(
            rgb555_to_xrgb8888(0x7C00),
            0x000000FF,
            "B is bits 10-14, must land in the B byte"
        );
        assert_eq!(rgb555_to_xrgb8888(0), 0);
    }

    #[test]
    fn render_backdrop_reads_real_registers() {
        let mut ram = WorkRam::new();
        {
            let lines = ram.vdp2_lines.get_mut().unwrap();
            let regs = &mut lines[0];
            regs[REG_TVMD] = 0x80;
            regs[REG_TVMD + 1] = 0x00;
            let blue = 0x1F << 10;
            regs[REG_BKTAL] = 0;
            regs[REG_BKTAL + 1] = 0;
            let vram = ram.vdp2_vram.get_mut().unwrap();
            vram[0] = (blue >> 8) as u8;
            vram[1] = (blue & 0xFF) as u8;
        }

        let frame = render_back_screen(&ram);
        assert_eq!((frame.width, frame.height), (320, 224));
        assert_eq!(frame.pixels[0], 0x0000FF);
        assert!(frame.pixels.iter().all(|&p| p == 0x0000FF));
    }

    #[test]
    fn render_backdrop_is_black_when_display_disabled() {
        let ram = WorkRam::new();
        let frame = render_back_screen(&ram);
        assert!(frame.pixels.iter().all(|&p| p == 0));
    }

    #[test]
    fn vdp1_cmdtable_field_offsets() {
        let mut vram = [0u8; 32];
        let values: [u16; 15] = [
            0x1000, 0x1002, 0x1004, 0x1006, 0x1008, 0x100A, 0x100C, 0x100E, 0x1010, 0x1012, 0x1014,
            0x1016, 0x1018, 0x101A, 0x101C,
        ];

        for (i, &val) in values.iter().enumerate() {
            let bytes = val.to_be_bytes();
            vram[i * 2] = bytes[0];
            vram[i * 2 + 1] = bytes[1];
        }

        let cmd = CmdTable::read(&vram, 0);
        assert_eq!(cmd.cmdctrl, 0x1000);
        assert_eq!(cmd.cmdlink, 0x1002);
        assert_eq!(cmd.cmdpmod, 0x1004);
        assert_eq!(cmd.cmdcolr, 0x1006);
        assert_eq!(cmd.cmdsrca, 0x1008);
        assert_eq!(cmd.cmdsize, 0x100A);
        assert_eq!(cmd.cmdxa, 0x100C);
        assert_eq!(cmd.cmdya, 0x100E);
        assert_eq!(cmd.cmdxb, 0x1010);
        assert_eq!(cmd.cmdyb, 0x1012);
        assert_eq!(cmd.cmdxc, 0x1014);
        assert_eq!(cmd.cmdyc, 0x1016);
        assert_eq!(cmd.cmdxd, 0x1018);
        assert_eq!(cmd.cmdyd, 0x101A);
        assert_eq!(cmd.cmdgrda, 0x101C);
    }

    #[test]
    fn vdp1_polygon_draws_at_correct_offsets() {
        let ram = WorkRam::new();
        // TVMD: DISP on, 320x224
        {
            let mut lines = ram.vdp2_lines.write().unwrap();
            let regs = &mut lines[0];
            regs[REG_TVMD] = 0x80;
            // BKTAL: pure blue
            let blue = 0x1F << 10;
            regs[REG_BKTAL] = 0;
            regs[REG_BKTAL + 1] = 0;
            let mut vram = ram.vdp2_vram.write().unwrap();
            vram[0] = (blue >> 8) as u8;
            vram[1] = (blue & 0xFF) as u8;
        }

        let mut vdp1_state = Vdp1State::new();
        vdp1_state.ptmr = 1;

        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            // Command 0: Polygon (4), untextured. Color: 0x801F (pure red)
            // Vertices: (10,10), (20,10), (20,20), (10,20)
            let bytes = 0x0004u16.to_be_bytes();
            vram[0] = bytes[0];
            vram[1] = bytes[1]; // CMDCTRL
            let bytes = 0x801Fu16.to_be_bytes();
            vram[6] = bytes[0];
            vram[7] = bytes[1]; // CMDCOLR

            let mut write_coord = |offset: usize, x: i16, y: i16| {
                let xb = x.to_be_bytes();
                let yb = y.to_be_bytes();
                vram[offset] = xb[0];
                vram[offset + 1] = xb[1];
                vram[offset + 2] = yb[0];
                vram[offset + 3] = yb[1];
            };
            write_coord(0x0C, 10, 10); // A
            write_coord(0x10, 20, 10); // B
            write_coord(0x14, 20, 20); // C
            write_coord(0x18, 10, 20); // D

            // Command 1: END bit (0x8000)
            vram[0x20] = 0x80;
            vram[0x21] = 0x00;
        }

        execute_vdp1(&mut vdp1_state, &ram);
        vdp1_swap_frame_buffers(&mut vdp1_state, &ram);
        let frame = render_back_screen(&ram);

        // Background should be blue
        assert_eq!(frame.pixels[0], 0x0000FF);

        // Polygon at (15,15) should be red (rgb555 0x801F -> xrgb8888 0xFF0000)
        let center_idx = 15 * 320 + 15;
        assert_eq!(frame.pixels[center_idx], 0xFF0000);

        // Coordinate (5,5) should still be blue
        let out_idx = 5 * 320 + 5;
        assert_eq!(frame.pixels[out_idx], 0x0000FF);
    }

    #[test]
    fn vdp1_end_bit_on_first_command_draws_nothing() {
        let ram = WorkRam::new();
        // TVMD: DISP on, 320x224
        {
            let mut lines = ram.vdp2_lines.write().unwrap();
            let regs = &mut lines[0];
            regs[REG_TVMD] = 0x80;
            let blue = 0x1F << 10;
            regs[REG_BKTAL] = 0;
            regs[REG_BKTAL + 1] = 0;
            let mut vram = ram.vdp2_vram.write().unwrap();
            vram[0] = (blue >> 8) as u8;
            vram[1] = (blue & 0xFF) as u8;
        }

        let mut vdp1_state = Vdp1State::new();
        vdp1_state.ptmr = 1;

        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            // Command 0: Polygon (4) BUT end bit set (0x8004)
            let bytes = 0x8004u16.to_be_bytes();
            vram[0] = bytes[0];
            vram[1] = bytes[1]; // CMDCTRL
            let bytes = 0x801Fu16.to_be_bytes();
            vram[6] = bytes[0];
            vram[7] = bytes[1]; // CMDCOLR

            let mut write_coord = |offset: usize, x: i16, y: i16| {
                let xb = x.to_be_bytes();
                let yb = y.to_be_bytes();
                vram[offset] = xb[0];
                vram[offset + 1] = xb[1];
                vram[offset + 2] = yb[0];
                vram[offset + 3] = yb[1];
            };
            write_coord(0x0C, 10, 10);
            write_coord(0x10, 20, 10);
            write_coord(0x14, 20, 20);
            write_coord(0x18, 10, 20);
        }

        execute_vdp1(&mut vdp1_state, &ram);
        vdp1_swap_frame_buffers(&mut vdp1_state, &ram);
        let frame = render_back_screen(&ram);

        let center_idx = 15 * 320 + 15;
        // Should be blue (0x0000FF), not red!
        assert_eq!(frame.pixels[center_idx], 0x0000FF);
    }

    #[test]
    fn vdp1_skip_bit_suppresses_draw_not_link() {
        let ram = WorkRam::new();
        {
            let mut lines = ram.vdp2_lines.write().unwrap();
            let regs = &mut lines[0];
            regs[REG_TVMD] = 0x80;
            let blue = 0x1F << 10;
            regs[REG_BKTAL] = 0;
            regs[REG_BKTAL + 1] = 0;
            let mut vram = ram.vdp2_vram.write().unwrap();
            vram[0] = (blue >> 8) as u8;
            vram[1] = (blue & 0xFF) as u8;
        }
        let mut vdp1_state = Vdp1State::new();
        vdp1_state.ptmr = 1;

        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            // Command 0: JP skip bit set (0x4004)
            let bytes = 0x4004u16.to_be_bytes();
            vram[0] = bytes[0];
            vram[1] = bytes[1]; // CMDCTRL
            let bytes = 0x801Fu16.to_be_bytes();
            vram[6] = bytes[0];
            vram[7] = bytes[1]; // CMDCOLR
            let mut write_coord = |offset: usize, x: i16, y: i16| {
                let xb = x.to_be_bytes();
                let yb = y.to_be_bytes();
                vram[offset] = xb[0];
                vram[offset + 1] = xb[1];
                vram[offset + 2] = yb[0];
                vram[offset + 3] = yb[1];
            };
            write_coord(0x0C, 10, 10);
            write_coord(0x10, 20, 10);
            write_coord(0x14, 20, 20);
            write_coord(0x18, 10, 20);

            // Command 1: END bit (0x8000)
            vram[0x20] = 0x80;
            vram[0x21] = 0x00;
        }

        execute_vdp1(&mut vdp1_state, &ram);
        vdp1_swap_frame_buffers(&mut vdp1_state, &ram);
        let frame = render_back_screen(&ram);

        let center_idx = 15 * 320 + 15;
        // Should be blue (0x0000FF)! The skip bit suppresses the draw.
        assert_eq!(frame.pixels[center_idx], 0x0000FF);
    }

    #[test]
    fn vdp1_bad_command_aborts() {
        let ram = WorkRam::new();
        // TVMD: DISP on, 320x224
        {
            let mut lines = ram.vdp2_lines.write().unwrap();
            let regs = &mut lines[0];
            regs[REG_TVMD] = 0x80;
            let blue = 0x1F << 10;
            regs[REG_BKTAL] = 0;
            regs[REG_BKTAL + 1] = 0;
            let mut vram = ram.vdp2_vram.write().unwrap();
            vram[0] = (blue >> 8) as u8;
            vram[1] = (blue & 0xFF) as u8;
        }

        let mut vdp1_state = Vdp1State::new();
        vdp1_state.ptmr = 1;

        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            // Command 0: Bad command (0x000C)
            let bytes = 0x000Cu16.to_be_bytes();
            vram[0] = bytes[0];
            vram[1] = bytes[1]; // CMDCTRL

            // Command 1: Polygon (4)
            let bytes = 0x0004u16.to_be_bytes();
            vram[0x20] = bytes[0];
            vram[0x21] = bytes[1]; // CMDCTRL
            let bytes = 0x801Fu16.to_be_bytes();
            vram[0x26] = bytes[0];
            vram[0x27] = bytes[1]; // CMDCOLR
            let mut write_coord = |offset: usize, x: i16, y: i16| {
                let xb = x.to_be_bytes();
                let yb = y.to_be_bytes();
                vram[offset] = xb[0];
                vram[offset + 1] = xb[1];
                vram[offset + 2] = yb[0];
                vram[offset + 3] = yb[1];
            };
            write_coord(0x2C, 10, 10);
            write_coord(0x30, 20, 10);
            write_coord(0x34, 20, 20);
            write_coord(0x38, 10, 20);

            // Command 2: END bit (0x8000)
            vram[0x40] = 0x80;
            vram[0x41] = 0x00;
        }

        execute_vdp1(&mut vdp1_state, &ram);

        // Verify EDSR/LOPR/COPR before swap shifts EDSR
        assert_eq!(vdp1_state.edsr & 2, 2);
        assert_eq!(vdp1_state.lopr, 0); // opr is set to addr>>3. Since addr was 0, opr=0.

        vdp1_swap_frame_buffers(&mut vdp1_state, &ram);
        let frame = render_back_screen(&ram);

        let center_idx = 15 * 320 + 15;
        // Should be blue (0x0000FF), because bad command aborted the list.
        assert_eq!(frame.pixels[center_idx], 0x0000FF);
    }

    #[test]
    fn vdp1_normal_sprite_geometry() {
        let ram = WorkRam::new();
        let mut vdp1_state = Vdp1State::new();
        vdp1_state.ptmr = 1;
        vdp1_state.tvmr = 0; // 512 width
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            let bytes = 0x0000u16.to_be_bytes(); // COMM 0
            vram[0] = bytes[0];
            vram[1] = bytes[1]; // CMDCTRL
            let bytes = 0x00C0u16.to_be_bytes(); // SPD=1, ECD=1 so it draws regardless of texture
            vram[4] = bytes[0];
            vram[5] = bytes[1]; // CMDPMOD
            let bytes = 0xFFFFu16.to_be_bytes();
            vram[6] = bytes[0];
            vram[7] = bytes[1]; // CMDCOLR
            let bytes = 0x0010u16.to_be_bytes();
            vram[8] = bytes[0];
            vram[9] = bytes[1]; // CMDSRCA
            let bytes = 0x0204u16.to_be_bytes();
            vram[10] = bytes[0];
            vram[11] = bytes[1]; // CMDSIZE (16x4)
            let bytes = 32i16.to_be_bytes();
            vram[12] = bytes[0];
            vram[13] = bytes[1]; // CMDXA
            let bytes = 8i16.to_be_bytes();
            vram[14] = bytes[0];
            vram[15] = bytes[1]; // CMDYA
            vram[0x20] = 0x80;
            vram[0x21] = 0x00; // END

            // Texture data (all 0xFF -> idx 15 -> mode 0 yields 0xFFFF)
            for i in 0x80..0xC0 {
                vram[i] = 0xFF;
            }
        }
        execute_vdp1(&mut vdp1_state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();

        let check = |x: i32, y: i32| {
            let offset = ((y * 512 + x) * 2) as usize;
            let val = u16::from_be_bytes([fb[offset], fb[offset + 1]]);
            val == 0xFFFF
        };

        assert!(check(32, 8));
        assert!(check(47, 8));
        assert!(check(47, 11));
        assert!(check(32, 11));

        assert!(!check(48, 8));
        assert!(!check(32, 12));
    }

    #[test]
    fn vdp1_colour_mode_0_bank() {
        let ram = WorkRam::new();
        let mut vdp1_state = Vdp1State::new();
        vdp1_state.ptmr = 1;
        vdp1_state.tvmr = 0; // 512 width
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            let bytes = 0x0000u16.to_be_bytes(); // COMM 0
            vram[0] = bytes[0];
            vram[1] = bytes[1]; // CMDCTRL
            let bytes = 0x0000u16.to_be_bytes();
            vram[4] = bytes[0];
            vram[5] = bytes[1]; // CMDPMOD
            let bytes = 0x0120u16.to_be_bytes();
            vram[6] = bytes[0];
            vram[7] = bytes[1]; // CMDCOLR
            let bytes = 0x0010u16.to_be_bytes();
            vram[8] = bytes[0];
            vram[9] = bytes[1]; // CMDSRCA
            let bytes = 0x0101u16.to_be_bytes();
            vram[10] = bytes[0];
            vram[11] = bytes[1]; // CMDSIZE (8x1)
            let bytes = 0i16.to_be_bytes();
            vram[12] = bytes[0];
            vram[13] = bytes[1]; // CMDXA
            let bytes = 0i16.to_be_bytes();
            vram[14] = bytes[0];
            vram[15] = bytes[1]; // CMDYA

            // Texture
            vram[0x80] = 0x37;

            vram[0x20] = 0x80;
            vram[0x21] = 0x00; // END
        }
        execute_vdp1(&mut vdp1_state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();

        let val0 = u16::from_be_bytes([fb[0], fb[1]]);
        let val1 = u16::from_be_bytes([fb[2], fb[3]]);

        // Mode 0: (CMDCOLR & 0xFFF0) | index
        assert_eq!(val0, 0x0123);
        assert_eq!(val1, 0x0127);
    }
    #[test]
    fn vdp1_colour_calc_2_half_luminance() {
        let ram = WorkRam::new();
        let mut vdp1_state = Vdp1State::new();
        vdp1_state.ptmr = 1;
        vdp1_state.tvmr = 0; // 512 width
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            let bytes = 0x0004u16.to_be_bytes(); // COMM 4 (polygon)
            vram[0] = bytes[0];
            vram[1] = bytes[1]; // CMDCTRL
            let bytes = 0x0042u16.to_be_bytes(); // SPD=1, Colour calc mode 2
            vram[4] = bytes[0];
            vram[5] = bytes[1]; // CMDPMOD
            let bytes = 0x7FFFu16.to_be_bytes();
            vram[6] = bytes[0];
            vram[7] = bytes[1]; // CMDCOLR

            let bytes = 32i16.to_be_bytes();
            vram[12] = bytes[0];
            vram[13] = bytes[1]; // CMDXA
            let bytes = 8i16.to_be_bytes();
            vram[14] = bytes[0];
            vram[15] = bytes[1]; // CMDYA
            let bytes = 47i16.to_be_bytes();
            vram[16] = bytes[0];
            vram[17] = bytes[1]; // CMDXB
            let bytes = 8i16.to_be_bytes();
            vram[18] = bytes[0];
            vram[19] = bytes[1]; // CMDYB
            let bytes = 47i16.to_be_bytes();
            vram[20] = bytes[0];
            vram[21] = bytes[1]; // CMDXC
            let bytes = 11i16.to_be_bytes();
            vram[22] = bytes[0];
            vram[23] = bytes[1]; // CMDYC
            let bytes = 32i16.to_be_bytes();
            vram[24] = bytes[0];
            vram[25] = bytes[1]; // CMDXD
            let bytes = 11i16.to_be_bytes();
            vram[26] = bytes[0];
            vram[27] = bytes[1]; // CMDYD

            vram[0x20] = 0x80;
            vram[0x21] = 0x00; // END
        }
        execute_vdp1(&mut vdp1_state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();

        let offset = ((8 * 512 + 32) * 2) as usize;
        let val = u16::from_be_bytes([fb[offset], fb[offset + 1]]);
        assert_eq!(val, 0xBDEF);
    }

    #[test]
    fn vdp1_colour_calc_3_half_transparent() {
        let ram = WorkRam::new();
        let mut vdp1_state = Vdp1State::new();
        vdp1_state.ptmr = 1;
        vdp1_state.tvmr = 0; // 512 width

        {
            // Prefill framebuffer pixel
            let mut fb = ram.vdp1_framebuffers.banks[0].write().unwrap();
            let offset = ((8 * 512 + 32) * 2) as usize;
            let bg_bytes = 0x83FFu16.to_be_bytes();
            fb[offset] = bg_bytes[0];
            fb[offset + 1] = bg_bytes[1];
        }

        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            let bytes = 0x0004u16.to_be_bytes(); // COMM 4 (polygon)
            vram[0] = bytes[0];
            vram[1] = bytes[1]; // CMDCTRL
            let bytes = 0x0043u16.to_be_bytes(); // SPD=1, Colour calc mode 3
            vram[4] = bytes[0];
            vram[5] = bytes[1]; // CMDPMOD
            let bytes = 0x7C00u16.to_be_bytes(); // new pixel
            vram[6] = bytes[0];
            vram[7] = bytes[1]; // CMDCOLR

            let bytes = 32i16.to_be_bytes();
            vram[12] = bytes[0];
            vram[13] = bytes[1]; // CMDXA
            let bytes = 8i16.to_be_bytes();
            vram[14] = bytes[0];
            vram[15] = bytes[1]; // CMDYA
            let bytes = 47i16.to_be_bytes();
            vram[16] = bytes[0];
            vram[17] = bytes[1]; // CMDXB
            let bytes = 8i16.to_be_bytes();
            vram[18] = bytes[0];
            vram[19] = bytes[1]; // CMDYB
            let bytes = 47i16.to_be_bytes();
            vram[20] = bytes[0];
            vram[21] = bytes[1]; // CMDXC
            let bytes = 11i16.to_be_bytes();
            vram[22] = bytes[0];
            vram[23] = bytes[1]; // CMDYC
            let bytes = 32i16.to_be_bytes();
            vram[24] = bytes[0];
            vram[25] = bytes[1]; // CMDXD
            let bytes = 11i16.to_be_bytes();
            vram[26] = bytes[0];
            vram[27] = bytes[1]; // CMDYD

            vram[0x20] = 0x80;
            vram[0x21] = 0x00; // END
        }
        execute_vdp1(&mut vdp1_state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();

        let offset = ((8 * 512 + 32) * 2) as usize;
        let val = u16::from_be_bytes([fb[offset], fb[offset + 1]]);
        assert_eq!(val, 0xBDEF);
    }

    #[test]
    fn vdp1_end_code_mode_0_terminates_span() {
        let ram = WorkRam::new();
        let mut vdp1_state = Vdp1State::new();
        vdp1_state.ptmr = 1;
        vdp1_state.tvmr = 0;

        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            let bytes = 0x0000u16.to_be_bytes(); // COMM 0
            vram[0] = bytes[0];
            vram[1] = bytes[1]; // CMDCTRL
            let bytes = 0x0040u16.to_be_bytes(); // SPD=1, Colour calc mode 0, ECD=0 (End code enabled)
            vram[4] = bytes[0];
            vram[5] = bytes[1]; // CMDPMOD
            let bytes = 0x0201u16.to_be_bytes(); // 16x1
            vram[10] = bytes[0];
            vram[11] = bytes[1]; // CMDSIZE
            let bytes = 0x0201u16.to_be_bytes(); // 16x1
            vram[10] = bytes[0];
            vram[11] = bytes[1]; // CMDSIZE
            let bytes = 0x0200u16.to_be_bytes(); // CMDSRCA -> 0x1000
            vram[8] = bytes[0];
            vram[9] = bytes[1];
            let bytes = 0i16.to_be_bytes();
            vram[12] = bytes[0];
            vram[13] = bytes[1]; // CMDXA
            vram[14] = bytes[0];
            vram[15] = bytes[1]; // CMDYA
            vram[0x20] = 0x80;
            vram[0x21] = 0x00; // END

            // 4bpp character: 16 pixels
            // 0, 1, 2, F, 4, F, 6, 7
            vram[0x1000] = 0x01;
            vram[0x1001] = 0x2F;
            vram[0x1002] = 0x4F;
            vram[0x1003] = 0x67;
        }
        execute_vdp1(&mut vdp1_state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();

        // Pixels 0, 1, 2 should be drawn
        assert_eq!(fb[0], 0);
        assert_eq!(fb[1], 0);
        assert_eq!(fb[2], 0);
        assert_eq!(fb[3], 1);
        assert_eq!(fb[4], 0);
        assert_eq!(fb[5], 2);
        // Pixel 3 is F, not drawn.
        assert_eq!(fb[6], 0);
        assert_eq!(fb[7], 0);
        // Pixel 4 should be drawn.
        assert_eq!(fb[8], 0);
        assert_eq!(fb[9], 4);
        // Pixel 5 is F, second end code, terminates span!
        assert_eq!(fb[10], 0);
        assert_eq!(fb[11], 0);
        // Pixel 6 should NOT be drawn!
        assert_eq!(fb[12], 0);
        assert_eq!(fb[13], 0);
    }

    #[test]
    fn vdp1_end_code_disabled_by_ecd() {
        let ram = WorkRam::new();
        let mut vdp1_state = Vdp1State::new();
        vdp1_state.ptmr = 1;
        vdp1_state.tvmr = 0;

        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            let bytes = 0x0000u16.to_be_bytes(); // COMM 0
            vram[0] = bytes[0];
            vram[1] = bytes[1]; // CMDCTRL
            let bytes = 0x00C0u16.to_be_bytes(); // SPD=1, ECD=1 (End code disabled)
            vram[4] = bytes[0];
            vram[5] = bytes[1]; // CMDPMOD
            let bytes = 0x0201u16.to_be_bytes(); // 16x1
            vram[10] = bytes[0];
            vram[11] = bytes[1]; // CMDSIZE
            let bytes = 0x0200u16.to_be_bytes(); // CMDSRCA -> 0x1000
            vram[8] = bytes[0];
            vram[9] = bytes[1];
            let bytes = 0i16.to_be_bytes();
            vram[12] = bytes[0];
            vram[13] = bytes[1]; // CMDXA
            vram[14] = bytes[0];
            vram[15] = bytes[1]; // CMDYA
            vram[0x20] = 0x80;
            vram[0x21] = 0x00; // END

            // 4bpp character: 16 pixels
            // 0, 1, 2, F, 4, F, 6, 7
            vram[0x1000] = 0x01;
            vram[0x1001] = 0x2F;
            vram[0x1002] = 0x4F;
            vram[0x1003] = 0x67;
        }
        execute_vdp1(&mut vdp1_state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();

        // Pixel 3 is F, DRAWN
        assert_eq!(fb[6], 0);
        assert_eq!(fb[7], 0xF);
        // Pixel 6 is drawn!
        assert_eq!(fb[12], 0);
        assert_eq!(fb[13], 6);
    }

    #[test]
    fn vdp1_character_flip() {
        let ram = WorkRam::new();
        let mut vdp1_state = Vdp1State::new();
        vdp1_state.ptmr = 1;
        vdp1_state.tvmr = 0;

        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            let bytes = 0x0010u16.to_be_bytes(); // COMM 0, Dir = 1 (flip X)
            vram[0] = bytes[0];
            vram[1] = bytes[1]; // CMDCTRL
            let bytes = 0x00C0u16.to_be_bytes(); // SPD=1, ECD=1
            vram[4] = bytes[0];
            vram[5] = bytes[1]; // CMDPMOD
            let bytes = 0x0201u16.to_be_bytes(); // 16x1
            vram[10] = bytes[0];
            vram[11] = bytes[1]; // CMDSIZE
            let bytes = 0x0200u16.to_be_bytes(); // CMDSRCA -> 0x1000
            vram[8] = bytes[0];
            vram[9] = bytes[1];
            let bytes = 0i16.to_be_bytes();
            vram[12] = bytes[0];
            vram[13] = bytes[1]; // CMDXA
            vram[14] = bytes[0];
            vram[15] = bytes[1]; // CMDYA
            vram[0x20] = 0x80;
            vram[0x21] = 0x00; // END

            // 4bpp character: 16 pixels
            vram[0x1000] = 0x01;
            vram[0x1001] = 0x23;
            vram[0x1002] = 0x45;
            vram[0x1003] = 0x67;
            vram[0x1004] = 0x89;
            vram[0x1005] = 0xAB;
            vram[0x1006] = 0xCD;
            vram[0x1007] = 0xEF;
        }
        execute_vdp1(&mut vdp1_state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();

        // Dest column 0 gets index 0xF
        assert_eq!(fb[0], 0);
        assert_eq!(fb[1], 0xF);
        // Dest column 1 gets index 0xE
        assert_eq!(fb[2], 0);
        assert_eq!(fb[3], 0xE);
    }
    #[test]
    fn vdp1_line_endpoints() {
        let ram = WorkRam::new();
        let mut vdp1_state = Vdp1State::new();
        vdp1_state.ptmr = 1;
        vdp1_state.tvmr = 0;

        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            let bytes = 0x0006u16.to_be_bytes(); // COMM 6 (Line)
            vram[0] = bytes[0];
            vram[1] = bytes[1]; // CMDCTRL
            let bytes = 0x0040u16.to_be_bytes(); // SPD=1
            vram[4] = bytes[0];
            vram[5] = bytes[1]; // CMDPMOD
            let bytes = 0xFFFFu16.to_be_bytes();
            vram[6] = bytes[0];
            vram[7] = bytes[1]; // CMDCOLR

            let mut write_coord = |offset: usize, x: i16, y: i16| {
                let xb = x.to_be_bytes();
                let yb = y.to_be_bytes();
                vram[offset] = xb[0];
                vram[offset + 1] = xb[1];
                vram[offset + 2] = yb[0];
                vram[offset + 3] = yb[1];
            };
            write_coord(0x0C, 10, 20); // A
            write_coord(0x10, 20, 20); // B

            vram[0x20] = 0x80;
            vram[0x21] = 0x00; // END
        }
        execute_vdp1(&mut vdp1_state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();

        let check = |x: i32, y: i32| -> bool {
            let offset = ((y * 512 + x) * 2) as usize;
            u16::from_be_bytes([fb[offset], fb[offset + 1]]) != 0
        };

        let mut count = 0;
        for i in 10..=20 {
            if check(i, 20) {
                count += 1;
            }
        }
        assert_eq!(count, 11);
        assert!(!check(9, 20));
        assert!(!check(21, 20));
    }

    #[test]
    fn vdp1_line_is_not_greedy() {
        let ram = WorkRam::new();
        let mut vdp1_state = Vdp1State::new();
        vdp1_state.ptmr = 1;
        vdp1_state.tvmr = 0;

        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            let bytes = 0x0006u16.to_be_bytes(); // COMM 6 (Line)
            vram[0] = bytes[0];
            vram[1] = bytes[1]; // CMDCTRL
            let bytes = 0x0040u16.to_be_bytes(); // SPD=1
            vram[4] = bytes[0];
            vram[5] = bytes[1]; // CMDPMOD
            let bytes = 0xFFFFu16.to_be_bytes();
            vram[6] = bytes[0];
            vram[7] = bytes[1]; // CMDCOLR

            let mut write_coord = |offset: usize, x: i16, y: i16| {
                let xb = x.to_be_bytes();
                let yb = y.to_be_bytes();
                vram[offset] = xb[0];
                vram[offset + 1] = xb[1];
                vram[offset + 2] = yb[0];
                vram[offset + 3] = yb[1];
            };
            write_coord(0x0C, 0, 0); // A
            write_coord(0x10, 4, 4); // B

            vram[0x20] = 0x80;
            vram[0x21] = 0x00; // END
        }
        execute_vdp1(&mut vdp1_state, &ram);
        let fb = ram.vdp1_framebuffers.banks[0].read().unwrap();

        let check = |x: i32, y: i32| -> bool {
            let offset = ((y * 512 + x) * 2) as usize;
            u16::from_be_bytes([fb[offset], fb[offset + 1]]) != 0
        };

        let mut count = 0;
        for y in 0..10 {
            for x in 0..10 {
                if check(x, y) {
                    count += 1;
                }
            }
        }
        assert_eq!(count, 5);
        assert!(check(0, 0));
        assert!(check(1, 1));
        assert!(check(2, 2));
        assert!(check(3, 3));
        assert!(check(4, 4));
    }

    #[test]
    fn back_screen_reads_the_colour_from_vram_not_the_register() {
        let mut ram = WorkRam::new();
        {
            let mut lines = ram.vdp2_lines.write().unwrap();
            let regs = &mut lines[0];
            regs[REG_TVMD] = 0x80;
            regs[REG_TVMD + 1] = 0x00;
            // BKTAU = 0x0001, BKTAL = 0x2345
            regs[REG_BKTAU] = 0x00;
            regs[REG_BKTAU + 1] = 0x01;
            regs[REG_BKTAL] = 0x23;
            regs[REG_BKTAL + 1] = 0x45;
            // VRSIZE bit 15 set -> 8Mbit VRAM
            regs[REG_VRSIZE] = 0x80;
            regs[REG_VRSIZE + 1] = 0x00;

            // Expected address: (0x1 << 16 | 0x2345) * 2 = 0x12345 * 2 = 0x2468A
            let blue = 0x1F << 10;
            let mut vram = ram.vdp2_vram.write().unwrap();
            vram[0x2468A] = (blue >> 8) as u8;
            vram[0x2468B] = (blue & 0xFF) as u8;
        }

        let frame = render_back_screen(&ram);
        assert_eq!(frame.pixels[0], 0x0000FF);
    }

    #[test]
    fn back_screen_per_line_advances_two_bytes_per_line() {
        let mut ram = WorkRam::new();
        {
            let mut lines = ram.vdp2_lines.write().unwrap();
            let regs = &mut lines[0];
            regs[REG_TVMD] = 0x80;
            regs[REG_TVMD + 1] = 0x00;
            // BKTAU = 0x8001 (per-line mode), BKTAL = 0x2345
            regs[REG_BKTAU] = 0x80;
            regs[REG_BKTAU + 1] = 0x01;
            regs[REG_BKTAL] = 0x23;
            regs[REG_BKTAL + 1] = 0x45;
            regs[REG_VRSIZE] = 0x80;

            let vram = ram.vdp2_vram.get_mut().unwrap();
            // line 0: blue
            let blue = 0x1F << 10;
            vram[0x2468A] = (blue >> 8) as u8;
            vram[0x2468B] = (blue & 0xFF) as u8;
            // line 1: red
            let red = 0x1F;
            vram[0x2468C] = (red >> 8) as u8;
            vram[0x2468D] = (red & 0xFF) as u8;
            // line 2: green
            let green = 0x1F << 5;
            vram[0x2468E] = (green >> 8) as u8;
            vram[0x2468F] = (green & 0xFF) as u8;
        }

        let frame = render_back_screen(&ram);
        assert_eq!(frame.pixels[0], 0x0000FF);
        assert_eq!(frame.pixels[320], 0xFF0000); // Start of row 1
        assert_eq!(frame.pixels[640], 0x00FF00); // Start of row 2
    }

    #[test]
    fn back_screen_is_drawn_with_disp_clear_when_bdclmd_set() {
        let mut ram = WorkRam::new();
        {
            let mut lines = ram.vdp2_lines.write().unwrap();
            let regs = &mut lines[0];
            regs[REG_TVMD] = 0x00; // DISP bit 15 is 0
            regs[REG_TVMD + 1] = 0x00;
            regs[0x0D] = 0x01; // BDCLMD bit 0 is 1 in EXTEN? Wait, TVMD is 0x00, EXTEN is 0x0D?
                               // Actually BDCLMD is bit 8 of TVMD? No, BDCLMD is bit 8 of TVMD? Let's check TVMD.
                               // In vdp2_regs.rs bdclmd is self.regs[0x000 / 2] & 0x0100. So it's bit 8 of TVMD.
            regs[REG_TVMD] = 0x01; // BDCLMD is bit 8, which is the LSB of the first byte (byte 0x000).
            let blue = 0x1F << 10;
            let vram = ram.vdp2_vram.get_mut().unwrap();
            vram[0] = (blue >> 8) as u8;
            vram[1] = (blue & 0xFF) as u8;
        }

        let frame = render_back_screen(&ram);
        assert_eq!(frame.pixels[0], 0x0000FF);
    }

    #[test]
    fn back_screen_is_black_when_both_clear() {
        let mut ram = WorkRam::new();
        {
            let mut lines = ram.vdp2_lines.write().unwrap();
            let regs = &mut lines[0];
            regs[REG_TVMD] = 0x00; // DISP is 0, BDCLMD is 0
            regs[REG_TVMD + 1] = 0x00;
            let blue = 0x1F << 10;
            let vram = ram.vdp2_vram.get_mut().unwrap();
            vram[0] = (blue >> 8) as u8;
            vram[1] = (blue & 0xFF) as u8;
        }

        let frame = render_back_screen(&ram);
        assert_eq!(frame.pixels[0], 0x000000);
    }

    #[test]
    fn vreso_code_3_keeps_the_previous_height() {
        let work_ram = std::sync::Arc::new(WorkRam::new());
        let arbiter = std::sync::Arc::new(crate::BusArbiter::new());
        let mut sh2 = crate::sh2::Sh2::new(false, arbiter, work_ram.clone());

        // Write TVMD with VRESO=1 (240 lines)
        // 0x0010 in bits 4-5
        sh2.write_word(0x05F80000, 0x0010);

        let mut ram = work_ram.vdp2_regs.read().unwrap();
        let tvmd = u16::from_be_bytes([ram[0], ram[1]]);
        assert_eq!((tvmd >> 4) & 0x3, 1);
        drop(ram);

        // Write TVMD with VRESO=3 (keeps previous height)
        sh2.write_word(0x05F80000, 0x0030);

        let ram = work_ram.vdp2_regs.read().unwrap();
        let tvmd = u16::from_be_bytes([ram[0], ram[1]]);
        // The hardware keeps the previous value in the register
        assert_eq!((tvmd >> 4) & 0x3, 1);
    }

    #[test]
    fn lsmd_3_doubles_height_but_not_rbg0_height() {
        let tvmd = 0x00C0; // LSMD is bits 6-7 -> 3 << 6 = 0x00C0
        let (_, h) = resolution_from_tvmd(tvmd);
        assert_eq!(h, 448);

        let tvmd = 0x00D0; // VRESO=1 (240), LSMD=3 -> height=480
        let (_, h) = resolution_from_tvmd(tvmd);
        assert_eq!(h, 480);
    }

    #[test]
    fn vdp1_modr_is_synthesised() {
        let work_ram = std::sync::Arc::new(WorkRam::new());
        let arbiter = std::sync::Arc::new(crate::BusArbiter::new());
        let mut sh2 = crate::sh2::Sh2::new(false, arbiter, work_ram.clone());
        let vdp1_state = Vdp1State::new();
        sh2.vdp1 = Some(std::sync::Arc::new(std::sync::Mutex::new(vdp1_state)));

        sh2.write_word(0x05D00000, 0x000B); // TVMR
        sh2.write_word(0x05D00002, 0x001A); // FBCR
        sh2.write_word(0x05D00004, 0x0002); // PTMR

        let modr = sh2.read_word(0x05D00016);
        assert_eq!(modr, 0x11DB);

        sh2.write_word(0x05D00016, 0xFFFF);
        let modr = sh2.read_word(0x05D00016);
        assert_eq!(modr, 0x11DB); // Write discarded
    }

    #[test]
    fn vdp1_write_only_registers_read_zero() {
        let work_ram = std::sync::Arc::new(WorkRam::new());
        let arbiter = std::sync::Arc::new(crate::BusArbiter::new());
        let mut sh2 = crate::sh2::Sh2::new(false, arbiter, work_ram.clone());
        let vdp1_state = Vdp1State::new();
        sh2.vdp1 = Some(std::sync::Arc::new(std::sync::Mutex::new(vdp1_state)));

        sh2.write_word(0x05D00000, 0x1234);
        assert_eq!(sh2.read_word(0x05D00000), 0);
        assert_eq!(sh2.read_word(0x05D00002), 0);
        assert_eq!(sh2.read_byte(0x05D00010), 0);
        assert_eq!(sh2.read_long(0x05D00010), 0);

        // Byte write discarded
        sh2.write_byte(0x05D00004, 0x01);
        let vdp1 = sh2.vdp1.as_ref().unwrap().lock().unwrap();
        assert_eq!(vdp1.ptmr, 0);
    }

    #[test]
    fn vdp1_copr_tracks_command_index() {
        let mut state = Vdp1State::new();
        let ram = WorkRam::new();
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            // Command 0
            vram[0x00] = 0x10;
            vram[0x01] = 0x04; // CMDCTRL: JP=1 (ASSIGN), COMM=4
            vram[0x02] = 0x00;
            vram[0x03] = 0x04; // CMDLINK: 4 -> addr 0x20
                               // Command 1 (at 0x20)
            vram[0x20] = 0x00;
            vram[0x21] = 0x04; // CMDCTRL: JP=0 (NEXT), COMM=4
                               // Command 2 (at 0x40)
            vram[0x40] = 0x80;
            vram[0x41] = 0x00; // CMDCTRL: END
        }
        execute_vdp1(&mut state, &ram);
        assert_eq!(state.copr, 8);
    }

    #[test]
    fn vdp1_jp_assign_follows_cmdlink() {
        let mut state = Vdp1State::new();
        let ram = WorkRam::new();
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0x00] = 0x10;
            vram[0x01] = 0x04; // CMDCTRL: JP=1 (ASSIGN)
            vram[0x02] = 0x00;
            vram[0x03] = 0x10; // CMDLINK: 0x10 -> addr 0x80

            vram[0x80] = 0x80;
            vram[0x81] = 0x00; // CMDCTRL: END
        }
        execute_vdp1(&mut state, &ram);
        assert_eq!(state.copr, 16);
    }

    #[test]
    fn vdp1_call_and_return() {
        let mut state = Vdp1State::new();
        let ram = WorkRam::new();
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            // Cmd 0: CALL to 0x40 (CMDLINK=8)
            vram[0x00] = 0x20;
            vram[0x01] = 0x04; // JP=2 (CALL)
            vram[0x02] = 0x00;
            vram[0x03] = 0x08; // CMDLINK=8

            // Cmd 1 (at 0x20): The return point. Draws something, then ENDs
            vram[0x20] = 0x80;
            vram[0x21] = 0x00; // END

            // Cmd 2 (at 0x40): The subroutine. RETURNs
            vram[0x40] = 0x30;
            vram[0x41] = 0x04; // JP=3 (RETURN)
        }
        execute_vdp1(&mut state, &ram);
        // It went 0x00 -> 0x40 -> 0x20 -> END.
        // Final command processed was at 0x20.
        assert_eq!(state.copr, 4);
    }

    #[test]
    fn vdp1_resume_mid_list() {
        let mut state = Vdp1State::new();
        let ram = WorkRam::new();
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            // Cmd 0
            vram[0x00] = 0x00;
            vram[0x01] = 0x04;
            // Cmd 1
            vram[0x20] = 0x80;
            vram[0x21] = 0x00; // END
                               // Cmd 2
            vram[0x40] = 0x80;
            vram[0x41] = 0x00; // END
        }
        // First run
        execute_vdp1(&mut state, &ram);
        assert_eq!(state.copr, 4);

        // Alter memory so the walker would keep going if it resumed
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0x20] = 0x00;
            vram[0x21] = 0x04; // Change END to NEXT
        }

        state.status = Vdp1Status::RUNNING;
        state.edsr &= !2; // Clear completion flag so Batsugun escape doesn't abort
        execute_vdp1(&mut state, &ram);
        // Resumes at 0x20, advances to 0x40 and hits END.
        assert_eq!(state.copr, 8);
    }

    #[test]
    fn vdp1_clipping_and_local_coords_persist() {
        let mut state = Vdp1State::new();
        let ram = WorkRam::new();
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            // Cmd 0: Local Coords
            vram[0x00] = 0x00;
            vram[0x01] = 0x0A; // COMM 10
            vram[0x0C] = 0x12;
            vram[0x0D] = 0x34; // X = 0x1234
            vram[0x0E] = 0x56;
            vram[0x0F] = 0x78; // Y = 0x5678

            // Cmd 1: System Clipping
            vram[0x20] = 0x00;
            vram[0x21] = 0x09; // COMM 9
            vram[0x34] = 0x03;
            vram[0x35] = 0xE8; // X2 = 1000
            vram[0x36] = 0x01;
            vram[0x37] = 0x90; // Y2 = 400

            // Cmd 2: END
            vram[0x40] = 0x80;
            vram[0x41] = 0x00;
        }
        execute_vdp1(&mut state, &ram);

        // Assert state was updated
        assert_eq!(state.local_x, 0x1234);
        assert_eq!(state.local_y, 0x5678);
        assert_eq!(state.systemclip_x2, 1000);
        assert_eq!(state.systemclip_y2, 400);

        // Now clear memory and run a second frame
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0x00] = 0x80;
            vram[0x01] = 0x00; // Just END immediately
        }
        execute_vdp1(&mut state, &ram);

        // Assert state persisted!
        assert_eq!(state.local_x, 0x1234);
        assert_eq!(state.local_y, 0x5678);
        assert_eq!(state.systemclip_x2, 1000);
        assert_eq!(state.systemclip_y2, 400);
    }

    #[test]
    fn vdp1_nested_call_loses_inner_return() {
        let mut state = Vdp1State::new();
        let ram = WorkRam::new();
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            // Cmd 0: CALL -> 0x80
            vram[0x00] = 0x20;
            vram[0x01] = 0x04;
            vram[0x02] = 0x00;
            vram[0x03] = 0x10;
            // Cmd 1 (at 0x20): The true return point -> END
            vram[0x20] = 0x80;
            vram[0x21] = 0x00;
            // Cmd at 0x80: CALL -> 0x100
            vram[0x80] = 0x20;
            vram[0x81] = 0x04;
            vram[0x82] = 0x00;
            vram[0x83] = 0x20;
            // Cmd at 0xA0: The false return point -> END
            vram[0xA0] = 0x80;
            vram[0xA1] = 0x00;
            // Cmd at 0x100: RETURN
            vram[0x100] = 0x30;
            vram[0x101] = 0x04;
        }
        execute_vdp1(&mut state, &ram);
        // Returns to 0x20, hitting END at 0x20.
        assert_eq!(state.copr, 4);
    }

    #[test]
    fn vdp1_return_without_call_acts_as_next() {
        let mut state = Vdp1State::new();
        let ram = WorkRam::new();
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            // Cmd 0: RETURN
            vram[0x00] = 0x30;
            vram[0x01] = 0x04;
            // Cmd 1: END
            vram[0x20] = 0x80;
            vram[0x21] = 0x00;
        }
        execute_vdp1(&mut state, &ram);
        assert_eq!(state.copr, 4);
    }

    #[test]
    fn vdp1_local_coordinate_offsets_subsequent_draws() {
        let mut state = Vdp1State::new();
        state.ptmr = 1; // Actually draw
        let ram = WorkRam::new();
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            // Cmd 0: Local Coords (100, 50)
            vram[0x00] = 0x00;
            vram[0x01] = 0x0A;
            vram[0x0C] = 0x00;
            vram[0x0D] = 100;
            vram[0x0E] = 0x00;
            vram[0x0F] = 50;

            // Cmd 1: Polygon at (10, 10), (20, 10), (20, 20), (10, 20)
            vram[0x20] = 0x00;
            vram[0x21] = 0x04; // Polygon
            vram[0x26] = 0x80;
            vram[0x27] = 0x1F; // Color
            vram[0x2C] = 0x00;
            vram[0x2D] = 10;
            vram[0x2E] = 0x00;
            vram[0x2F] = 10;
            vram[0x30] = 0x00;
            vram[0x31] = 20;
            vram[0x32] = 0x00;
            vram[0x33] = 10;
            vram[0x34] = 0x00;
            vram[0x35] = 20;
            vram[0x36] = 0x00;
            vram[0x37] = 20;
            vram[0x38] = 0x00;
            vram[0x39] = 10;
            vram[0x3A] = 0x00;
            vram[0x3B] = 20;

            // Cmd 2: END
            vram[0x40] = 0x80;
            vram[0x41] = 0x00;
        }
        execute_vdp1(&mut state, &ram);

        let back = ram
            .vdp1_framebuffers
            .back
            .load(std::sync::atomic::Ordering::Relaxed);
        let fb = ram.vdp1_framebuffers.banks[back].read().unwrap();

        // Assert pixel at (110, 60) is drawn!
        let off = (60 * 512 + 110) * 2;
        let px = u16::from_be_bytes([fb[off], fb[off + 1]]);
        assert_eq!(px, 0x801F);
    }

    #[test]
    fn vdp1_system_clip_ignores_xa_ya() {
        let mut state = Vdp1State::new();
        let ram = WorkRam::new();
        {
            let mut vram = ram.vdp1_vram.write().unwrap();
            vram[0x00] = 0x00;
            vram[0x01] = 0x09;
            vram[0x0C] = 0x00;
            vram[0x0D] = 40; // XA
            vram[0x0E] = 0x00;
            vram[0x0F] = 40; // YA
            vram[0x14] = 0x00;
            vram[0x15] = 100; // XC
            vram[0x16] = 0x00;
            vram[0x17] = 50; // YC
            vram[0x20] = 0x80;
            vram[0x21] = 0x00;
        }
        execute_vdp1(&mut state, &ram);
        assert_eq!(state.systemclip_x1, 0);
        assert_eq!(state.systemclip_y1, 0);
        assert_eq!(state.systemclip_x2, 100);
        assert_eq!(state.systemclip_y2, 50);
    }

    #[test]
    fn vdp1_endr_forces_idle_without_touching_edsr() {
        let mut state = Vdp1State::new();
        state.edsr = 2;
        state.copr = 7;
        state.status = Vdp1Status::RUNNING;

        let arbiter = std::sync::Arc::new(crate::BusArbiter::new());
        let work_ram = std::sync::Arc::new(WorkRam::new());
        let mut sh2 = crate::sh2::Sh2::new(false, arbiter, work_ram);
        sh2.vdp1 = Some(std::sync::Arc::new(std::sync::Mutex::new(state)));

        sh2.write_word(0x05D0000C, 0x1234); // ENDR

        let state = sh2.vdp1.unwrap();
        let state = state.lock().unwrap();
        assert_eq!(state.status, Vdp1Status::IDLE);
        assert_eq!(state.edsr, 2);
        assert_eq!(state.copr, 7);
    }

    #[test]
    fn vdp1_addr_error_aborts_silently() {
        let mut state = Vdp1State::new();
        let ram = WorkRam::new();
        state.addr = 0x80000; // Out of bounds
        state.status = Vdp1Status::RUNNING;
        state.edsr = 0;

        let drew = execute_vdp1(&mut state, &ram);
        assert_eq!(drew, false);
        assert_eq!(state.status, Vdp1Status::IDLE);
        assert_eq!(state.edsr, 0); // Unchanged
    }
}

// --- VDP1 Rasterizer (Phase 4) ---

#[derive(Copy, Clone)]
struct Point {
    x: i32,
    y: i32,
}

fn read_pattern_16(vram: &[u8], base: usize, off: usize) -> u8 {
    let byte = vram[(base + (off >> 1)) & 0x7FFFF];
    if off % 2 == 0 {
        byte >> 4
    } else {
        byte & 0x0F
    }
}

fn read_pattern_64(vram: &[u8], base: usize, off: usize) -> u8 {
    vram[(base + off) & 0x7FFFF] & 0x3F
}

fn read_pattern_128(vram: &[u8], base: usize, off: usize) -> u8 {
    vram[(base + off) & 0x7FFFF] & 0x7F
}

fn read_pattern_256(vram: &[u8], base: usize, off: usize) -> u8 {
    vram[(base + off) & 0x7FFFF]
}

fn read_pattern_64k(vram: &[u8], base: usize, off: usize) -> u16 {
    let addr = (base + off * 2) & 0x7FFFF;
    u16::from_be_bytes([vram[addr], vram[(addr + 1) & 0x7FFFF]])
}

fn interpolate(start: i32, end: i32, n: i32) -> i32 {
    if n == 0 {
        1
    } else {
        (end - start) / n
    }
}

fn alphablend16(d: u16, s: u16, level: u32) -> u16 {
    let blend =
        |dc: u16, sc: u16| -> u16 { ((sc as u32 * level + dc as u32 * (256 - level)) >> 8) as u16 };
    let r = blend(d & 0x1F, s & 0x1F);
    let g = blend((d >> 5) & 0x1F, (s >> 5) & 0x1F);
    let b = blend((d >> 10) & 0x1F, (s >> 10) & 0x1F);
    r | (g << 5) | (b << 10)
}

fn gouraud_adjust(colour: u16, table_value: u16) -> u16 {
    let adjust = |c: u16, t: u16| -> u16 {
        let mut res = (c as i32) + (t as i32) - 0x10;
        if res < 0 {
            res = 0;
        }
        if res > 0x1F {
            res = 0x1F;
        }
        res as u16
    };
    let r = adjust(colour & 0x1F, table_value & 0x1F);
    let g = adjust((colour >> 5) & 0x1F, (table_value >> 5) & 0x1F);
    let b = adjust((colour >> 10) & 0x1F, (table_value >> 10) & 0x1F);
    r | (g << 5) | (b << 10)
}

fn draw_line_impl(
    state: &Vdp1State,
    cmd: &CmdTable,
    _vram: &[u8],
    fb: &mut [u8],
    x1: i32,
    y1: i32,
    x2: i32,
    y2: i32,
    c_g1: u16,
    c_g2: u16,
    _is_poly_edge: bool,
) {
    let dx = x2 - x1;
    let dy = y2 - y1;
    let ax = if dx > 0 {
        1
    } else if dx < 0 {
        -1
    } else {
        0
    };
    let ay = if dy > 0 {
        1
    } else if dy < 0 {
        -1
    } else {
        0
    };

    let abs_dx = dx.abs();
    let abs_dy = dy.abs();

    let interlace = if (state.fbcr & 8) != 0 { 2 } else { 1 };
    let dil = (state.fbcr & 4) != 0;

    let sys_x2 = state.systemclip_x2 as i32;
    let sys_y2 = if interlace == 2 {
        (state.systemclip_y2 as i32) * 2
    } else {
        state.systemclip_y2 as i32
    };

    // Yabause hard limit
    if abs_dx > 999 || abs_dy > 999 {
        return;
    }

    let len = abs_dx.max(abs_dy);
    let mut current_x = x1;
    let mut current_y = y1;

    let fb_width = match state.tvmr & 0x3 {
        0 | 2 | 3 => 512,
        1 => 1024,
        _ => 512,
    };
    let bpp8 = (state.tvmr & 1) != 0;

    let user_clip = (cmd.cmdpmod & 0x0400) != 0;
    let user_clip_invert = (cmd.cmdpmod & 0x0200) != 0;
    let color_calc_mode = cmd.cmdpmod & 0x0007;
    let msb_on = (cmd.cmdpmod & 0x8000) != 0;
    let mesh = (cmd.cmdpmod & 0x0100) != 0;

    let gouraud_en = (cmd.cmdpmod & 0x0004) != 0 || true; // Polyline/line always fetch gouraud!

    let (mut r, mut g, mut b) = if gouraud_en {
        (
            ((c_g1 & 0x1F) as i32) << 16,
            (((c_g1 >> 5) & 0x1F) as i32) << 16,
            (((c_g1 >> 10) & 0x1F) as i32) << 16,
        )
    } else {
        (0, 0, 0)
    };

    let (rs, gs, bs) = if gouraud_en {
        (
            interpolate((c_g1 & 0x1F) as i32, (c_g2 & 0x1F) as i32, len) << 16,
            interpolate(
                ((c_g1 >> 5) & 0x1F) as i32,
                ((c_g2 >> 5) & 0x1F) as i32,
                len,
            ) << 16,
            interpolate(
                ((c_g1 >> 10) & 0x1F) as i32,
                ((c_g2 >> 10) & 0x1F) as i32,
                len,
            ) << 16,
        )
    } else {
        (0, 0, 0)
    };

    let mut a = if abs_dx > abs_dy {
        abs_dx / 2
    } else {
        abs_dy / 2
    };

    let mut putpixel = |x: i32, y: i32, rr: i32, gg: i32, bb: i32| {
        if x < 0 || y < 0 || x > sys_x2 || y > sys_y2 {
            return;
        }
        if interlace == 2 {
            let reject = if dil { (y & 1) == 0 } else { (y & 1) != 0 };
            if reject {
                return;
            }
        }

        if user_clip {
            let user_clip_y_actual = y / interlace;
            let inside = x >= state.userclip_x1 as i32
                && x <= state.userclip_x2 as i32
                && user_clip_y_actual >= state.userclip_y1 as i32
                && user_clip_y_actual <= state.userclip_y2 as i32;
            if inside == user_clip_invert {
                return;
            }
        }

        let current_pixel = cmd.cmdcolr;
        let mut write_pixel = current_pixel;

        if gouraud_en {
            let g_val = ((rr >> 16).clamp(0, 31) as u16)
                | (((gg >> 16).clamp(0, 31) as u16) << 5)
                | (((bb >> 16).clamp(0, 31) as u16) << 10);

            if color_calc_mode == 4 {
                if ((g_val >> 5) & 0x1F) == 0x10 && ((g_val >> 10) & 0x1F) == 0x10 {
                    let r_val = g_val & 0x1F;
                    let add = if r_val > 0x10 { r_val - 0x10 } else { 0 };
                    write_pixel = current_pixel + add;
                } else {
                    write_pixel = gouraud_adjust(current_pixel, g_val);
                }
            } else if color_calc_mode == 5 || color_calc_mode == 6 || color_calc_mode == 7 {
                write_pixel = alphablend16(g_val, current_pixel, 128) | 0x8000;
            }
        }

        if mesh && (x ^ y) & 1 == 1 {
            return;
        }

        let y_actual = y / interlace;
        if bpp8 {
            write_pixel &= 0xFF;
            let fb_idx = (y_actual * fb_width + x) as usize;
            if fb_idx < fb.len() {
                fb[fb_idx] = write_pixel as u8;
            }
        } else {
            if msb_on {
                write_pixel |= 0x8000;
            }

            let fb_idx = ((y_actual * fb_width + x) * 2) as usize;
            if fb_idx + 1 < fb.len() {
                let bg_pixel = u16::from_be_bytes([fb[fb_idx], fb[fb_idx + 1]]);

                if color_calc_mode != 0 {
                    match color_calc_mode {
                        1 => {
                            if (bg_pixel & 0x8000) != 0 {
                                write_pixel = alphablend16(bg_pixel, current_pixel, 128) | 0x8000;
                            }
                        }
                        2 => write_pixel = ((current_pixel & !0x8421) >> 1) | 0x8000,
                        3 => {
                            if (bg_pixel & 0x8000) != 0 {
                                write_pixel = alphablend16(bg_pixel, current_pixel, 128) | 0x8000;
                            }
                        }
                        _ => {}
                    }
                }

                let bytes = write_pixel.to_be_bytes();
                fb[fb_idx] = bytes[0];
                fb[fb_idx + 1] = bytes[1];
            }
        }
    };

    if len == 0 {
        r += rs;
        g += gs;
        b += bs;
        putpixel(x1, y1, r, g, b);
        return;
    }

    if abs_dx > abs_dy {
        for _ in 0..abs_dx {
            r += rs;
            g += gs;
            b += bs;
            // No greedy emission for actual draw
            putpixel(current_x, current_y, r, g, b);

            a += abs_dy;
            if a >= abs_dx {
                a -= abs_dx;
                current_y += ay;
            }
            current_x += ax;
        }
    } else {
        for _ in 0..abs_dy {
            r += rs;
            g += gs;
            b += bs;
            putpixel(current_x, current_y, r, g, b);

            a += abs_dx;
            if a >= abs_dy {
                a -= abs_dy;
                current_x += ax;
            }
            current_y += ay;
        }
    }

    r += rs;
    g += gs;
    b += bs;
    putpixel(x2, y2, r, g, b);
}

fn draw_quad(
    state: &Vdp1State,
    cmd: &CmdTable,
    vram: &[u8],
    fb: &mut [u8],
    tl: Point,
    bl: Point,
    tr: Point,
    br: Point,
) {
    let interlace = if (state.fbcr & 8) != 0 { 2 } else { 1 };
    let dil = (state.fbcr & 4) != 0;

    let sys_x2 = state.systemclip_x2 as i32;
    let sys_y2 = if interlace == 2 {
        (state.systemclip_y2 as i32) * 2
    } else {
        state.systemclip_y2 as i32
    };

    // Pre-clip trivial reject (sys rect only)
    if (tl.x < 0 && bl.x < 0 && tr.x < 0 && br.x < 0)
        || (tl.x > sys_x2 && bl.x > sys_x2 && tr.x > sys_x2 && br.x > sys_x2)
        || (tl.y < 0 && bl.y < 0 && tr.y < 0 && br.y < 0)
        || (tl.y > sys_y2 && bl.y > sys_y2 && tr.y > sys_y2 && br.y > sys_y2)
    {
        return;
    }

    let char_width = (((cmd.cmdsize >> 8) & 0x3F) * 8) as i32;
    let char_height = (cmd.cmdsize & 0xFF) as i32;
    let char_base = (cmd.cmdsrca as usize) << 3;

    let len_left = bl.y - tl.y;
    let len_right = br.y - tr.y;
    let total = len_left.max(len_right).max(1);

    let left_step = interpolate(tl.x, bl.x, len_left);
    let right_step = interpolate(tr.x, br.x, len_right);

    let y_tex_step = if total > 0 {
        (char_height << 16) / total
    } else {
        0
    };

    let dir = (cmd.cmdctrl >> 4) & 0x3;
    let flip_x = (dir & 0x1) != 0;
    let flip_y = (dir & 0x2) != 0;

    let fb_width = match state.tvmr & 0x3 {
        0 | 2 | 3 => 512,
        1 => 1024,
        _ => 512,
    };
    let bpp8 = (state.tvmr & 1) != 0;

    let user_clip = (cmd.cmdpmod & 0x0400) != 0;
    let user_clip_invert = (cmd.cmdpmod & 0x0200) != 0;
    let spd = (cmd.cmdpmod & 0x0040) != 0;
    let color_mode = (cmd.cmdpmod >> 3) & 0x7;
    let color_calc_mode = cmd.cmdpmod & 0x0007;
    let msb_on = (cmd.cmdpmod & 0x8000) != 0;
    let mesh = (cmd.cmdpmod & 0x0100) != 0;

    let current_shape = cmd.cmdctrl & 0x7;
    let untextured = current_shape == 4 || current_shape == 5 || current_shape == 6;

    let gouraud_en = (cmd.cmdpmod & 0x0004) != 0 || current_shape == 5 || current_shape == 6;
    let mut grd = [0u16; 4];
    if gouraud_en {
        let grda = (cmd.cmdgrda as usize) << 3;
        for i in 0..4 {
            let addr = (grda + i * 2) & 0x7FFFF;
            grd[i] = u16::from_be_bytes([vram[addr], vram[(addr + 1) & 0x7FFFF]]);
        }
    }

    let (mut lr, mut lg, mut lb) = if gouraud_en {
        (
            ((grd[0] & 0x1F) as i32) << 16,
            (((grd[0] >> 5) & 0x1F) as i32) << 16,
            (((grd[0] >> 10) & 0x1F) as i32) << 16,
        )
    } else {
        (0, 0, 0)
    };
    let (mut rr, mut rg, mut rb) = if gouraud_en {
        (
            ((grd[1] & 0x1F) as i32) << 16,
            (((grd[1] >> 5) & 0x1F) as i32) << 16,
            (((grd[1] >> 10) & 0x1F) as i32) << 16,
        )
    } else {
        (0, 0, 0)
    };

    let (lrs, lgs, lbs) = if gouraud_en {
        (
            interpolate((grd[0] & 0x1F) as i32, (grd[3] & 0x1F) as i32, len_left) << 16,
            interpolate(
                ((grd[0] >> 5) & 0x1F) as i32,
                ((grd[3] >> 5) & 0x1F) as i32,
                len_left,
            ) << 16,
            interpolate(
                ((grd[0] >> 10) & 0x1F) as i32,
                ((grd[3] >> 10) & 0x1F) as i32,
                len_left,
            ) << 16,
        )
    } else {
        (0, 0, 0)
    };

    let (rrs, rgs, rbs) = if gouraud_en {
        (
            interpolate((grd[1] & 0x1F) as i32, (grd[2] & 0x1F) as i32, len_right) << 16,
            interpolate(
                ((grd[1] >> 5) & 0x1F) as i32,
                ((grd[2] >> 5) & 0x1F) as i32,
                len_right,
            ) << 16,
            interpolate(
                ((grd[1] >> 10) & 0x1F) as i32,
                ((grd[2] >> 10) & 0x1F) as i32,
                len_right,
            ) << 16,
        )
    } else {
        (0, 0, 0)
    };

    let mut left_x = tl.x << 16;
    let mut right_x = tr.x << 16;

    for i in 0..=total {
        let y = tl.y + i;
        let lx = left_x >> 16;
        let rx = right_x >> 16;
        let span_len = (rx - lx).max(1);
        let x_tex_step = (char_width << 16) / span_len;

        let x_r_step = if gouraud_en { (rr - lr) / span_len } else { 0 };
        let x_g_step = if gouraud_en { (rg - lg) / span_len } else { 0 };
        let x_b_step = if gouraud_en { (rb - lb) / span_len } else { 0 };

        let tex_y_base = (i * y_tex_step) >> 16;
        let tex_y = if flip_y {
            (char_height - 1 - tex_y_base).max(0)
        } else {
            tex_y_base
        };
        let mut tex_x_acc = 0;

        let mut row_r = lr;
        let mut row_g = lg;
        let mut row_b = lb;

        let ecd_disabled = (cmd.cmdpmod & 0x0080) != 0;
        let mut end_codes_in_span = 0;
        let mut last_tex_x = !0;

        for x in lx..=rx {
            let tex_x_base = tex_x_acc >> 16;
            tex_x_acc += x_tex_step;
            let tex_x = if flip_x {
                (char_width - 1 - tex_x_base).max(0)
            } else {
                tex_x_base
            };

            if gouraud_en {
                row_r += x_r_step;
                row_g += x_g_step;
                row_b += x_b_step;
            }

            if y < 0 || y > sys_y2 || x < 0 || x > sys_x2 {
                continue;
            }

            if interlace == 2 {
                let reject = if dil { (y & 1) == 0 } else { (y & 1) != 0 };
                if reject {
                    continue;
                }
            }

            if mesh && ((x ^ y) & 1) != 0 {
                continue;
            }

            if user_clip {
                let user_clip_y_actual = y / interlace;
                let in_user = x >= state.userclip_x1 as i32
                    && x <= state.userclip_x2 as i32
                    && user_clip_y_actual >= state.userclip_y1 as i32
                    && user_clip_y_actual <= state.userclip_y2 as i32;
                if in_user == user_clip_invert {
                    continue;
                }
            }

            let mut current_pixel = 0;
            let mut vis_mask = 0xFFFF;
            let mut is_end_code = false;

            if !untextured {
                let row_stride = match color_mode {
                    0 | 1 => char_width / 2,
                    5 => char_width * 2,
                    _ => char_width,
                } as usize;

                let off = (tex_y as usize) * row_stride + (tex_x as usize);

                match color_mode {
                    0 => {
                        let idx = read_pattern_16(vram, char_base, off) as u16;
                        if !ecd_disabled && idx == 0xF {
                            is_end_code = true;
                        } else if idx == 0 && !spd {
                            continue;
                        }
                        current_pixel = (cmd.cmdcolr & 0xFFF0) | idx;
                        vis_mask = 0x000F;
                    }
                    1 => {
                        let idx = read_pattern_16(vram, char_base, off) as u16;
                        if !ecd_disabled && idx == 0xF {
                            is_end_code = true;
                        } else if idx == 0 && !spd {
                            continue;
                        }
                        let lut_addr = ((idx * 2) + (cmd.cmdcolr << 3)) as usize;
                        let lut_word = u16::from_be_bytes([
                            vram[lut_addr & 0x7FFFF],
                            vram[(lut_addr + 1) & 0x7FFFF],
                        ]);
                        current_pixel = lut_word;
                        vis_mask = 0xFFFF;
                    }
                    2 => {
                        let idx = read_pattern_64(vram, char_base, off) as u16;
                        if idx == 63 {
                            continue;
                        } // Mode 2: 63 is transparent, not an end code
                        if idx == 0 && !spd {
                            continue;
                        }
                        current_pixel = (cmd.cmdcolr & 0xFFC0) | idx;
                        vis_mask = 0x003F;
                    }
                    3 => {
                        let idx = read_pattern_128(vram, char_base, off) as u16;
                        if idx == 0 && !spd {
                            continue;
                        }
                        current_pixel = (cmd.cmdcolr & 0xFF80) | idx;
                        vis_mask = 0x007F;
                    }
                    4 => {
                        let idx = read_pattern_256(vram, char_base, off) as u16;
                        if !ecd_disabled && idx == 0xFF {
                            is_end_code = true;
                        } else if idx == 0 && !spd {
                            continue;
                        }
                        current_pixel = (cmd.cmdcolr & 0xFF00) | idx;
                        vis_mask = 0x00FF;
                    }
                    5 => {
                        let word = read_pattern_64k(vram, char_base, off);
                        if !ecd_disabled && word == 0x7FFF {
                            is_end_code = true;
                        } else if (word & 0x8000) == 0 && !spd {
                            continue;
                        }
                        current_pixel = word;
                        vis_mask = 0xFFFF;
                    }
                    _ => {}
                }
            } else {
                current_pixel = cmd.cmdcolr;
            }

            if is_end_code {
                if tex_x != last_tex_x {
                    end_codes_in_span += 1;
                    last_tex_x = tex_x;
                }
                if end_codes_in_span >= 2 {
                    break;
                }
                continue;
            }

            // Write to framebuffer
            if spd || (current_pixel & vis_mask) != 0 {
                let y_actual = y / interlace;
                let mut write_pixel = current_pixel;

                if bpp8 {
                    write_pixel &= 0xFF;
                    let fb_idx = (y_actual * fb_width + x) as usize;
                    if fb_idx < fb.len() {
                        fb[fb_idx] = write_pixel as u8;
                    }
                } else {
                    let fb_idx = ((y_actual * fb_width + x) * 2) as usize;
                    if fb_idx + 1 < fb.len() {
                        if msb_on {
                            let bg_pixel = u16::from_be_bytes([fb[fb_idx], fb[fb_idx + 1]]);
                            write_pixel = bg_pixel | 0x8000;
                        } else {
                            let bg_pixel = u16::from_be_bytes([fb[fb_idx], fb[fb_idx + 1]]);
                            match color_calc_mode {
                                0 => {
                                    write_pixel = current_pixel;
                                }
                                1 => {
                                    if (bg_pixel & 0x8000) != 0 {
                                        write_pixel = alphablend16(bg_pixel, 0, 128) | 0x8000;
                                    } else {
                                        write_pixel = bg_pixel;
                                    }
                                }
                                2 => {
                                    write_pixel = ((current_pixel & !0x8421) >> 1) | 0x8000;
                                }
                                3 => {
                                    if (bg_pixel & 0x8000) != 0 {
                                        write_pixel =
                                            alphablend16(bg_pixel, current_pixel, 128) | 0x8000;
                                    } else {
                                        write_pixel = current_pixel;
                                    }
                                }
                                4 => {
                                    let g_val = ((row_r >> 16).clamp(0, 31) as u16)
                                        | (((row_g >> 16).clamp(0, 31) as u16) << 5)
                                        | (((row_b >> 16).clamp(0, 31) as u16) << 10);
                                    if color_mode != 1
                                        && color_mode != 5
                                        && ((g_val >> 5) & 0x1F) == 0x10
                                        && ((g_val >> 10) & 0x1F) == 0x10
                                    {
                                        let r_val = g_val & 0x1F;
                                        let add = if r_val > 0x10 { r_val - 0x10 } else { 0 };
                                        write_pixel = current_pixel + add;
                                    } else {
                                        write_pixel = gouraud_adjust(current_pixel, g_val);
                                    }
                                }
                                5 | 6 | 7 => {
                                    let g_val = ((row_r >> 16).clamp(0, 31) as u16)
                                        | (((row_g >> 16).clamp(0, 31) as u16) << 5)
                                        | (((row_b >> 16).clamp(0, 31) as u16) << 10);
                                    write_pixel = alphablend16(g_val, current_pixel, 128) | 0x8000;
                                }
                                _ => {}
                            }
                        }

                        let bytes = write_pixel.to_be_bytes();
                        fb[fb_idx] = bytes[0];
                        fb[fb_idx + 1] = bytes[1];
                    }
                }
            }
        }

        left_x += left_step << 16;
        right_x += right_step << 16;
        lr += lrs;
        lg += lgs;
        lb += lbs;
        rr += rrs;
        rg += rgs;
        rb += rbs;
    }
}
