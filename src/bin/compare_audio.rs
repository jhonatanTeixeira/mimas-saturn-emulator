//! Compares our audio against a real capture (`stubs/captures/audio/boot.wav`, taken by
//! recording a real console's audio loopback while an instrumented, real BIOS-booting
//! YabaSanshiro ran inside RetroArch — see the provenance note in `docs/sound.md`).
//!
//! What this does **not** do: compare samples one to one. Two emulators — let alone an
//! emulator and a loopback recording with its own start offset and device noise floor —
//! never produce identical samples; one sample of misalignment at the attack changes the
//! whole file without saying what broke (`docs/sound.md` already says so about the DSP
//! capture, and it holds here too). Instead this compares **loudness over time**: an RMS
//! envelope in fixed windows, searched over a time offset for the alignment that correlates
//! best. That answers "does the shape — attack, decay, silence — line up", which is what a
//! human listening for "does this sound similar" is actually judging.
//!
//! Usage: cargo run --release --bin compare_audio -- <reference.wav> <ours.wav> [--window-ms N]

use std::io::Read;

struct Wav {
    sample_rate: u32,
    channels: u16,
    samples: Vec<i16>, // interleaved
}

fn load_wav(path: &str) -> Wav {
    let mut f = std::fs::File::open(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).expect("read");
    assert_eq!(&buf[0..4], b"RIFF", "{path}: not a RIFF file");
    assert_eq!(&buf[8..12], b"WAVE", "{path}: not a WAVE file");

    let (mut sample_rate, mut channels, mut bits) = (44100u32, 2u16, 16u16);
    let mut data: &[u8] = &[];
    let mut pos = 12;
    while pos + 8 <= buf.len() {
        let id = &buf[pos..pos + 4];
        let size = u32::from_le_bytes(buf[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let end = (pos + 8 + size).min(buf.len());
        let body = &buf[pos + 8..end];
        match id {
            b"fmt " => {
                channels = u16::from_le_bytes(body[2..4].try_into().unwrap());
                sample_rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
                bits = u16::from_le_bytes(body[14..16].try_into().unwrap());
            }
            b"data" => data = body,
            _ => {}
        }
        pos = end + (size & 1); // chunks are word-aligned
    }
    assert_eq!(
        bits, 16,
        "{path}: only 16-bit PCM is supported, got {bits}-bit"
    );

    let samples = data
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();
    Wav {
        sample_rate,
        channels,
        samples,
    }
}

/// Downmix to mono so channel layout differences do not affect the comparison.
fn mono(w: &Wav) -> Vec<f64> {
    let ch = w.channels.max(1) as usize;
    w.samples
        .chunks_exact(ch)
        .map(|frame| frame.iter().map(|&s| s as f64).sum::<f64>() / ch as f64)
        .collect()
}

fn rms_envelope(samples: &[f64], sample_rate: u32, window_ms: u32) -> Vec<f64> {
    let win = ((sample_rate as u64 * window_ms as u64) / 1000).max(1) as usize;
    samples
        .chunks(win)
        .map(|c| (c.iter().map(|v| v * v).sum::<f64>() / c.len() as f64).sqrt())
        .collect()
}

/// Pearson correlation of `b` against `a`, `b` shifted right by `offset` windows (negative
/// shifts `b` left). Windows that fall outside either envelope are dropped from the sum, so
/// the two only need to overlap, not have equal length.
fn correlation_at(a: &[f64], b: &[f64], offset: i64) -> (f64, usize) {
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for (i, &av) in a.iter().enumerate() {
        let j = i as i64 - offset;
        if j >= 0 && (j as usize) < b.len() {
            xs.push(av);
            ys.push(b[j as usize]);
        }
    }
    let n = xs.len();
    if n < 4 {
        return (0.0, n);
    }
    let mx = xs.iter().sum::<f64>() / n as f64;
    let my = ys.iter().sum::<f64>() / n as f64;
    let (mut cov, mut vx, mut vy) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let (dx, dy) = (xs[i] - mx, ys[i] - my);
        cov += dx * dy;
        vx += dx * dx;
        vy += dy * dy;
    }
    if vx <= 0.0 || vy <= 0.0 {
        return (0.0, n);
    }
    (cov / (vx.sqrt() * vy.sqrt()), n)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let reference = args.next().unwrap_or_else(|| {
        eprintln!("usage: compare_audio <reference.wav> <ours.wav> [--window-ms N]");
        std::process::exit(1);
    });
    let ours = args.next().expect("ours.wav");
    let window_ms: u32 = args
        .skip_while(|a| a != "--window-ms")
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);

    let (rw, ow) = (load_wav(&reference), load_wav(&ours));
    let (rm, om) = (mono(&rw), mono(&ow));
    println!(
        "reference: {:.2}s ({} Hz, {} ch) | ours: {:.2}s ({} Hz, {} ch)",
        rm.len() as f64 / rw.sample_rate as f64,
        rw.sample_rate,
        rw.channels,
        om.len() as f64 / ow.sample_rate as f64,
        ow.sample_rate,
        ow.channels,
    );
    if rw.sample_rate != ow.sample_rate {
        println!(
            "note: sample rates differ ({} vs {}); windows below are not the same duration",
            rw.sample_rate, ow.sample_rate
        );
    }

    let renv = rms_envelope(&rm, rw.sample_rate, window_ms);
    let oenv = rms_envelope(&om, ow.sample_rate, window_ms);

    // Search a generous window range for the best alignment: the loopback recording's start
    // has no fixed relationship to our sample 0.
    let max_lag = (10_000 / window_ms) as i64; // +/- 10 s
    let mut best = (f64::MIN, 0i64, 0usize);
    for offset in -max_lag..=max_lag {
        let (c, n) = correlation_at(&renv, &oenv, offset);
        if n >= 8 && c > best.0 {
            best = (c, offset, n);
        }
    }
    let (zero_corr, _) = correlation_at(&renv, &oenv, 0);

    println!(
        "envelope windows: {window_ms} ms | reference {} | ours {}",
        renv.len(),
        oenv.len()
    );
    println!(
        "correlation at zero offset: {zero_corr:.3} | best: {:.3} at offset {:+} windows ({:+.2}s), {} overlapping windows",
        best.0,
        best.1,
        best.1 as f64 * window_ms as f64 / 1000.0,
        best.2
    );

    let rpeak = rm.iter().fold(0.0f64, |m, &v| m.max(v.abs()));
    let opeak = om.iter().fold(0.0f64, |m, &v| m.max(v.abs()));
    println!("peak sample: reference {rpeak:.0} | ours {opeak:.0}");

    // A table around the reference's loudest window, so a human can see where the shapes
    // agree or not, rather than trusting a single correlation number. The head of the file
    // is usually silence either side, which says nothing.
    let peak_i = renv
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map_or(0, |(i, _)| i);
    let start = peak_i.saturating_sub(10);
    let end = (peak_i + 50).min(renv.len());
    println!(
        "\ntime      ref RMS   ours RMS  (aligned at the offset above, around the reference's peak)"
    );
    for (i, &r) in renv.iter().enumerate().take(end).skip(start) {
        let j = i as i64 - best.1;
        let ov = if j >= 0 {
            oenv.get(j as usize).copied()
        } else {
            None
        };
        let t = i as f64 * window_ms as f64 / 1000.0;
        match ov {
            Some(o) => println!("{t:6.2}s  {r:8.0}  {o:8.0}"),
            None => println!("{t:6.2}s  {r:8.0}  (no overlap)"),
        }
    }
}
