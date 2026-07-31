use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};

pub static TELEMETRY_ENABLED: AtomicBool = AtomicBool::new(true);

// Metrics
pub static WRAM_READS: AtomicU64 = AtomicU64::new(0);
pub static WRAM_WRITES: AtomicU64 = AtomicU64::new(0);

// Array of idle times per thread (0..8)
pub static THREAD_IDLE_NS: [AtomicU64; 8] = [
    AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
    AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
];

pub fn record_wram_read() {
    if TELEMETRY_ENABLED.load(Ordering::Relaxed) {
        WRAM_READS.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn record_wram_write() {
    if TELEMETRY_ENABLED.load(Ordering::Relaxed) {
        WRAM_WRITES.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn record_idle_time(core_id: usize, duration_ns: u64) {
    if TELEMETRY_ENABLED.load(Ordering::Relaxed) && core_id < 8 {
        THREAD_IDLE_NS[core_id].fetch_add(duration_ns, Ordering::Relaxed);
    }
}

pub fn print_report() {
    let reads = WRAM_READS.load(Ordering::Relaxed);
    let writes = WRAM_WRITES.load(Ordering::Relaxed);
    eprintln!("--- MIMAS TELEMETRY REPORT ---");
    eprintln!("WRAM Accesses: Reads={}, Writes={}", reads, writes);
    for i in 0..8 {
        let idle_ns = THREAD_IDLE_NS[i].load(Ordering::Relaxed);
        eprintln!("  Core {}: Idle Time = {:.3} ms", i, idle_ns as f64 / 1_000_000.0);
    }
    eprintln!("------------------------------");
}

pub fn dump_frame(frame: &crate::vdp::Framebuffer, path: &str) -> std::io::Result<()> {
    use std::fs::File;
    use std::io::Write;

    let mut file = File::create(path)?;
    let width = frame.width;
    let height = frame.height;
    let image_size = width * height * 4;
    let file_size = 54 + image_size;

    // File Header
    file.write_all(b"BM")?;
    file.write_all(&(file_size as u32).to_le_bytes())?;
    file.write_all(&[0; 4])?; // Reserved
    file.write_all(&(54u32).to_le_bytes())?; // Offset

    // DIB Header
    file.write_all(&(40u32).to_le_bytes())?; // DIB size
    file.write_all(&(width as i32).to_le_bytes())?;
    file.write_all(&(-(height as i32)).to_le_bytes())?; // Top-down
    file.write_all(&(1u16).to_le_bytes())?; // Planes
    file.write_all(&(32u16).to_le_bytes())?; // Bits per pixel
    file.write_all(&[0; 4])?; // Compression (0 = BI_RGB)
    file.write_all(&(image_size as u32).to_le_bytes())?;
    file.write_all(&(2835i32).to_le_bytes())?; // H-res
    file.write_all(&(2835i32).to_le_bytes())?; // V-res
    file.write_all(&[0; 8])?; // Colors + Important

    // Pixel data (converts 0x00RRGGBB to BGRA)
    for &pixel in &frame.pixels {
        let b = (pixel & 0xFF) as u8;
        let g = ((pixel >> 8) & 0xFF) as u8;
        let r = ((pixel >> 16) & 0xFF) as u8;
        let a = 0xFFu8;
        file.write_all(&[b, g, r, a])?;
    }

    Ok(())
}
