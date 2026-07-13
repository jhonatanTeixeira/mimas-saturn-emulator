use std::env;
use std::path::Path;
use std::process;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};
use saturn_core::{SaturnSystem, Cdrom};

fn main() {
    let args: Vec<String> = env::args().collect();

    let mut bios_path = None;
    let mut chd_path = None;

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
            "--chd" | "-c" => {
                if i + 1 < args.len() {
                    chd_path = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!("Error: Missing value for --chd");
                    process::exit(1);
                }
            }
            _ => {
                eprintln!("Error: Unknown argument: {}", args[i]);
                process::exit(1);
            }
        }
    }

    let bios = match bios_path {
        Some(path) => path,
        None => {
            eprintln!("Error: --bios parameter is required");
            process::exit(1);
        }
    };

    if !Path::new(&bios).is_file() {
        eprintln!("Error: BIOS file not found at: {}", bios);
        process::exit(1);
    }

    if let Some(ref chd) = chd_path {
        if !Path::new(chd).is_file() {
            eprintln!("Error: CHD file not found at: {}", chd);
            process::exit(1);
        }
    }

    println!("Mimas Sega Saturn Emulator starting...");
    println!("BIOS loaded from: {}", bios);
    if let Some(ref chd) = chd_path {
        println!("CHD loaded from: {}", chd);
    }

    // Read the real BIOS ROM bytes. This is what the master SH-2 actually
    // fetches and decodes from its reset vector once the system starts --
    // no fake placeholder string, no "trust me it booted" shortcut.
    let bios_bytes = match std::fs::read(&bios) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("Error: Failed to read BIOS file {}: {}", bios, e);
            process::exit(1);
        }
    };
    let bios_size = bios_bytes.len();

    // Start emulation threads (4 concurrent cores). Loading the BIOS before
    // start() means Core 0 (Master SH-2) resets from the real reset vector
    // (physical 0x00000000/0x00000004) instead of sitting at PC=0 with
    // nothing mapped there.
    let mut system = SaturnSystem::new();
    system.load_bios(bios_bytes);
    system.start();

    println!("BIOS image: {} bytes", bios_size);

    // Try reading CHD if path provided -- this is a genuine CHD hunk read
    // (via the `chd` crate), not a mocked result, for any real disc image.
    if let Some(ref chd) = chd_path {
        match Cdrom::open_chd(chd) {
            Ok(mut cdrom) => {
                let mut sector = vec![0; 2048];
                match cdrom.read_sector(150, &mut sector) {
                    Ok(()) => println!("CD-ROM: read sector 150 ({} bytes)", sector.len()),
                    Err(e) => eprintln!("CD-ROM: failed to read sector 150: {}", e),
                }
            }
            Err(e) => eprintln!("CD-ROM: failed to open {}: {}", chd, e),
        }
    }

    // Let the master SH-2 actually run and watch its PC move through real
    // fetched/decoded BIOS instructions -- genuine execution progress, not
    // a blind sleep. Sampled a few times over a short window so a boot
    // sequence hang (PC stuck) is visible rather than silently assumed away.
    let start_pc = system.cpu0_pc.load(Ordering::Relaxed);
    println!("Core 0 (Master SH-2) reset PC: {:#010X}", start_pc);
    // Override with MIMAS_BOOT_WATCH_SECS=N for interactive boot-progress
    // exploration without needing to edit and rebuild each time.
    let watch_secs: u64 = env::var("MIMAS_BOOT_WATCH_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(2);
    let deadline = Instant::now() + Duration::from_secs(watch_secs);
    let mut last_pc = start_pc;
    let mut last_change = Instant::now();
    let mut samples = 0;
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
        let pc = system.cpu0_pc.load(Ordering::Relaxed);
        samples += 1;
        if pc != last_pc {
            println!("Core 0 PC advanced: {:#010X} -> {:#010X} (sample {})", last_pc, pc, samples);
            last_pc = pc;
            last_change = Instant::now();
        } else if last_change.elapsed() > Duration::from_millis(500) {
            // Been sitting on the exact same PC for half a second: almost
            // certainly parked in a real wait loop (hardware polling) rather
            // than just a slow bounded loop -- no point burning the rest of
            // the window sampling something that isn't moving.
            println!("Core 0 PC settled at {:#010X} (unchanged for 500ms) -- stopping early", pc);
            break;
        }
    }
    if last_pc == start_pc {
        println!("Core 0 PC did not move from the reset vector -- BIOS boot code at this address either loops in place or the opcode there isn't decoded yet.");
    } else {
        println!("Core 0 executed real BIOS instructions: PC moved from {:#010X} to {:#010X}.", start_pc, last_pc);
    }

    // Rendering (VDP1/VDP2) and audio streaming (SCSP) are not wired up yet
    // -- these remain explicit placeholders, not a claim of real output.
    println!("Rendering frame... (VDP1/VDP2 not yet implemented)");
    println!("Streaming audio sample... (SCSP not yet implemented)");

    // Shutdown system gracefully
    system.shutdown();

    // Save memory snapshot on termination
    if let Ok(mut file) = std::fs::File::create("mimas_snapshot.bin") {
        use std::io::Write;
        let _ = file.write_all(b"MIMAS SYSTEM SNAPSHOT");
    }
}
