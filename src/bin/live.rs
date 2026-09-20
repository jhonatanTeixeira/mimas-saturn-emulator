//! Live window: boots the BIOS in real time, on screen, with sound.
//!
//! The same renderer as everywhere else, drawing on the window's own OpenGL context: the
//! finished frame is blitted from its FBO straight to the default framebuffer, so the
//! pixels never come back to the CPU. The headless EGL path is untouched and still what
//! the frame dumper and the quality gate use.
//!
//!     cargo run --release --bin live -- [--scale 3] [--frames N]

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use mimasv2::devices::vdp1::Vdp1;
use mimasv2::devices::vdp2::Vdp2;
use mimasv2::machine::Saturn;
use mimasv2::video::FrameSink;
use mimasv2::video::renderer::{GlRenderer, HEIGHT, WIDTH};

/// Draws each frame on the window's context and tells the loop it may present.
struct LiveSink {
    renderer: Rc<RefCell<GlRenderer>>,
    ready: Rc<Cell<bool>>,
}

impl FrameSink for LiveSink {
    fn on_frame(&mut self, _frame: u32, vdp1: &Vdp1, vdp2: &Vdp2) {
        self.renderer.borrow_mut().render(vdp1, vdp2);
        self.ready.set(true);
    }
}

fn main() -> Result<(), String> {
    let mut scale = 3u32;
    let mut max_frames = u32::MAX;
    let mut bios = "saturn_bios.bin".to_string();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--scale" => scale = it.next().and_then(|v| v.parse().ok()).unwrap_or(3),
            "--frames" => max_frames = it.next().and_then(|v| v.parse().ok()).unwrap_or(u32::MAX),
            "--bios" => bios = it.next().unwrap_or(bios),
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    let sdl = sdl2::init()?;
    let video = sdl.video()?;
    {
        let attr = video.gl_attr();
        attr.set_context_profile(sdl2::video::GLProfile::Core);
        attr.set_context_version(3, 3);
    }
    let (win_w, win_h) = (WIDTH as u32 * scale, HEIGHT as u32 * scale);
    let window = video
        .window("mimasv2 — BIOS", win_w, win_h)
        .position_centered()
        .opengl()
        .build()
        .map_err(|e| e.to_string())?;
    let _ctx = window.gl_create_context()?;
    gl::load_with(|s| video.gl_get_proc_address(s) as *const _);
    video.gl_set_swap_interval(sdl2::video::SwapInterval::Immediate)?;

    let rom = std::fs::read(&bios).map_err(|e| format!("could not read {bios}: {e}"))?;
    let mut saturn = Saturn::new(&rom);

    // The renderer draws on the context created above; no EGL surface is involved here.
    let mut renderer = GlRenderer::in_current_context()?;
    renderer.read_back = false; // straight to the screen, no CPU copy
    println!("GPU: {}", renderer.gpu());
    let renderer = Rc::new(RefCell::new(renderer));
    let ready = Rc::new(Cell::new(false));
    saturn.sink = Box::new(LiveSink {
        renderer: renderer.clone(),
        ready: ready.clone(),
    });

    let audio = sdl.audio()?;
    let want = sdl2::audio::AudioSpecDesired {
        freq: Some(44100),
        channels: Some(2),
        samples: Some(1024),
    };
    let queue: sdl2::audio::AudioQueue<i16> = audio.open_queue(None, &want)?;
    queue.resume();

    let mut events = sdl.event_pump()?;
    let frame_time = Duration::from_micros(16_683); // 59.94 Hz
    // Measurement mode: run as fast as the machine allows and report the rate. The number
    // only means something without the limiter, because with it every frame waits.
    let limit = std::env::var("MIMAS_NO_LIMIT").is_err();
    let mut next_frame = Instant::now() + frame_time;
    let mut frames = 0u32;
    let start = Instant::now();
    let mut last_report = start;
    let mut frames_at_report = 0u32;

    'outer: while frames < max_frames {
        for e in events.poll_iter() {
            use sdl2::event::Event;
            use sdl2::keyboard::Keycode;
            match e {
                Event::Quit { .. }
                | Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                } => break 'outer,
                _ => {}
            }
        }

        // Emulate until the renderer has a finished frame.
        ready.set(false);
        while !ready.get() {
            if saturn.step(None).is_err() {
                eprintln!("CPU fault at PC={:08X}", saturn.cpu.st.pc);
                break 'outer;
            }
        }
        frames += 1;

        renderer.borrow().blit_to_screen(win_w as i32, win_h as i32);
        window.gl_swap_window();

        // Hand the samples produced during this frame to the audio device.
        if !saturn.audio.is_empty() {
            let mut flat = Vec::with_capacity(saturn.audio.len() * 2);
            for (l, r) in saturn.audio.drain(..) {
                flat.push(l);
                flat.push(r);
            }
            queue.queue_audio(&flat)?;
        }

        if limit {
            // Pace to real time: one frame per 1/59.94 s, and wait a little longer whenever
            // the audio device already has more than a few frames buffered.
            let queued_frames = queue.size() as f32 / (4.0 * 735.0);
            if queued_frames > 3.0 {
                std::thread::sleep(frame_time);
            }
            let now = Instant::now();
            if next_frame > now {
                std::thread::sleep(next_frame - now);
            } else {
                next_frame = now;
            }
            next_frame += frame_time;
        } else {
            let now = Instant::now();
            if now.duration_since(last_report) >= Duration::from_secs(1) {
                let dt = now.duration_since(last_report).as_secs_f64();
                println!(
                    "{:.1} fps ({:.2}x real time)",
                    (frames - frames_at_report) as f64 / dt,
                    (frames - frames_at_report) as f64 / dt / 59.94
                );
                last_report = now;
                frames_at_report = frames;
            }
        }
    }

    let total = start.elapsed().as_secs_f64();
    println!(
        "{frames} frames shown in {total:.2}s — {:.1} fps ({:.2}x real time)",
        frames as f64 / total,
        frames as f64 / total / 59.94
    );
    Ok(())
}
