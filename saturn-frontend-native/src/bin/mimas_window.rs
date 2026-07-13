// Windowed frontend: boots a real Saturn BIOS and presents the actual VDP2
// output (currently: the backdrop layer -- see saturn_core::vdp) in a real
// window, continuously. Separate from `saturn-frontend-native`'s main
// binary, which is the headless/timed CLI the E2E test suite drives; this
// one runs until the window is closed.
use std::env;
use std::path::Path;
use std::process;
use minifb::{Window, WindowOptions};
use saturn_core::SaturnSystem;

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut bios_path = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--bios" | "-b" => {
                if i + 1 < args.len() {
                    bios_path = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!("Error: Missing value for --bios");
                    process::exit(1);
                }
            }
            _ => {
                eprintln!("Error: Unknown argument: {}", args[i]);
                process::exit(1);
            }
        }
    }
    let bios = bios_path.unwrap_or_else(|| {
        eprintln!("Error: --bios parameter is required");
        process::exit(1);
    });
    if !Path::new(&bios).is_file() {
        eprintln!("Error: BIOS file not found at: {}", bios);
        process::exit(1);
    }
    let bios_bytes = std::fs::read(&bios).unwrap_or_else(|e| {
        eprintln!("Error: Failed to read BIOS file {}: {}", bios, e);
        process::exit(1);
    });

    println!("Mimas: booting {} ({} bytes)...", bios, bios_bytes.len());
    let mut system = SaturnSystem::new();
    system.load_bios(bios_bytes);
    system.start();

    // Window starts at whatever resolution Vdp defaults to (320x224); it's
    // resized below once the BIOS actually configures TVMD, since real
    // resolution isn't known until the CPU has run for a bit.
    let mut win_w = 320usize;
    let mut win_h = 224usize;
    let mut window = Window::new(
        "Mimas -- Sega Saturn (real BIOS boot)",
        win_w,
        win_h,
        WindowOptions {
            resize: true,
            ..WindowOptions::default()
        },
    )
    .expect("failed to open window");
    window.set_target_fps(60);

    let mut frames_presented = 0u64;
    while window.is_open() && !window.is_key_down(minifb::Key::Escape) {
        let frame = system.vdp2_frame.load();
        if frame.width != win_w || frame.height != win_h {
            win_w = frame.width;
            win_h = frame.height;
            window = Window::new(
                "Mimas -- Sega Saturn (real BIOS boot)",
                win_w,
                win_h,
                WindowOptions { resize: true, ..WindowOptions::default() },
            )
            .expect("failed to resize window");
            window.set_target_fps(60);
            println!("Display resolution changed to {win_w}x{win_h} (real TVMD register)");
        }
        window
            .update_with_buffer(&frame.pixels, win_w, win_h)
            .expect("failed to present frame");
        frames_presented += 1;
        if frames_presented % 300 == 0 {
            println!(
                "Core 0 PC: {:#010X} -- {} frames presented",
                system.cpu0_pc.load(std::sync::atomic::Ordering::Relaxed),
                frames_presented
            );
        }
    }

    println!("Window closed after {frames_presented} frames. Shutting down...");
    system.shutdown();
}
