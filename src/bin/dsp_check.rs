//! Runs our effect DSP against a capture taken from a reference machine and reports where
//! the two disagree.
//!
//! The capture comes from the instrumented emulator (see `tools/trace-capture/`):
//!
//! - `dsp_program.txt` — the microcode, coefficients and delay taps in force;
//! - `dsp_io.txt` — one line per sample: the 16 mixer inputs and the 16 effect outputs;
//! - `dsp_state.txt` + `dsp_ram.bin` — the DSP state and sound RAM at the first captured
//!   sample, so the comparison starts from the same place instead of from an empty ring
//!   buffer, which on this program takes 12286 samples to fill;
//! - `dsp_steps.txt` — per-step state for the first samples, which is what turns "the
//!   output is wrong" into "step N is wrong".
//!
//! Feeding the reference's own inputs into our DSP isolates it from the rest of the
//! emulator: any difference in the outputs is ours.
//!
//!     cargo run --release --bin dsp_check -- dsp_program.txt dsp_io.txt

use mimasv2::devices::scsp_dsp::ScspDsp;

/// Sound RAM as the DSP addresses it: 16-bit words. The reference stores them in host
/// order, we store them big-endian, so the bytes swap on the way in.
fn load_ram(path: &str) -> Option<Vec<u8>> {
    let raw = std::fs::read(path).ok()?;
    let mut ram = vec![0u8; 0x8_0000];
    for (i, w) in raw.chunks_exact(2).enumerate().take(0x4_0000) {
        ram[i * 2] = w[1];
        ram[i * 2 + 1] = w[0];
    }
    Some(ram)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let prog = args.next().unwrap_or_else(|| "dsp_program.txt".into());
    let io = args.next().unwrap_or_else(|| "dsp_io.txt".into());

    let mut dsp = ScspDsp::new();
    for line in std::fs::read_to_string(&prog)
        .expect("program file")
        .lines()
    {
        let f: Vec<&str> = line.split_whitespace().collect();
        match f.as_slice() {
            ["rbp", p, "rbl", l] => {
                let (p, l): (u16, u16) = (p.parse().unwrap(), l.parse().unwrap());
                dsp.set_ring((l << 7) | p);
            }
            ["coef", i, v] => {
                let v: i32 = v.parse().unwrap();
                // The capture stores the 13-bit coefficient; our setter takes the register.
                dsp.set_coef(i.parse().unwrap(), ((v as u16) << 3) & 0xFFF8);
            }
            ["madrs", i, v] => dsp.set_madrs(i.parse().unwrap(), v.parse().unwrap()),
            ["mpro", i, v] => {
                let raw = u64::from_str_radix(v, 16).unwrap();
                dsp.set_program(
                    i.parse().unwrap(),
                    (raw >> 48) as u16,
                    (raw >> 32) as u16,
                    (raw >> 16) as u16,
                    raw as u16,
                );
            }
            _ => {}
        }
    }
    println!("program: {} steps (last non-zero + 1)", dsp.last_step());

    // The DSP's 16-bit float does not encode zero as 0x0000: a cleared buffer decodes to
    // half scale. Without the reference's own RAM, start from the format's own zero.
    let mut ram = vec![0u8; 0x8_0000];
    if std::env::var("DSP_ZERO_FLOAT").is_ok() {
        for c in ram.chunks_mut(2) {
            c[0] = 0x68;
            c[1] = 0x00;
        }
    }
    if let Ok(p) = std::env::var("DSP_RAM") {
        ram = load_ram(&p).expect("reference sound RAM");
        println!("sound RAM loaded from the reference");
    }

    if let Ok(p) = std::env::var("DSP_STATE") {
        let text = std::fs::read_to_string(&p).expect("state file");
        let mut get = |k: &str| -> u32 {
            text.lines()
                .find_map(|l| l.strip_prefix(&format!("{k} ")))
                .and_then(|v| v.trim().parse::<i64>().ok())
                .unwrap_or(0) as u32
        };
        let (mdec, shift, frc, adrs) = (
            get("mdec_ct"),
            get("shift_reg"),
            get("frc_reg"),
            get("adrs_reg"),
        );
        let (yr, io_addr, rp, wp) = (
            get("y_reg"),
            get("io_addr"),
            get("read_pending"),
            get("write_pending"),
        );
        let (rv, wv) = (get("read_value"), get("write_value"));
        let mut temp = [0u32; 128];
        let mut mems = [0u32; 32];
        for l in text.lines() {
            let f: Vec<&str> = l.split_whitespace().collect();
            match f.as_slice() {
                ["temp", i, v] => {
                    temp[i.parse::<usize>().unwrap()] = v.parse::<i64>().unwrap() as u32
                }
                ["mems", i, v] => {
                    mems[i.parse::<usize>().unwrap()] = v.parse::<i64>().unwrap() as u32
                }
                _ => {}
            }
        }
        dsp.load_state(
            mdec,
            shift,
            frc as u16,
            adrs as u16,
            yr,
            io_addr,
            rp as u8,
            wp != 0,
            rv,
            wv as u16,
            &temp,
            &mems,
        );
        println!("reference state loaded (mdec_ct {mdec})");
    }

    // Per-step comparison: the reference logs shift_reg, io_addr, read_value and inputs
    // after each step, for the first samples. The first line that differs is the bug.
    let steps_ref: Vec<(u32, usize, u32, u32, u32, i32)> = std::env::var("DSP_STEPS")
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| {
            t.lines()
                .filter_map(|l| {
                    let f: Vec<&str> = l.split_whitespace().collect();
                    if f.len() < 6 {
                        return None;
                    }
                    Some((
                        f[0].parse().ok()?,
                        f[1].parse().ok()?,
                        f[2].parse().ok()?,
                        f[3].parse().ok()?,
                        f[4].parse().ok()?,
                        f[5].parse().ok()?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut out = std::env::var("DSP_OUT")
        .ok()
        .map(|p| std::io::BufWriter::new(std::fs::File::create(p).expect("output file")));
    let (mut rows, mut matched, mut first_bad) = (0u32, 0u32, None);
    let (mut sum_ours, mut sum_theirs) = (0i64, 0i64);
    let (mut peak_ours, mut peak_theirs) = (0i32, 0i32);
    let mut step_report_done = steps_ref.is_empty();

    for line in std::fs::read_to_string(&io)
        .expect("input/output capture")
        .lines()
    {
        let v: Vec<i32> = line
            .split_whitespace()
            .filter_map(|t| t.parse().ok())
            .collect();
        if v.len() < 33 {
            continue;
        }
        for c in 0..16 {
            dsp.mixs[c] = v[1 + c];
        }

        let tracing = !step_report_done && steps_ref.iter().any(|s| s.0 == rows);
        if tracing {
            dsp.trace = Some(Vec::new());
        }
        dsp.run_sample(&mut ram);
        if tracing {
            let ours = dsp.trace.take().unwrap();
            let theirs: Vec<_> = steps_ref.iter().filter(|s| s.0 == rows).collect();
            let mut bad = 0;
            for (o, t) in ours.iter().zip(theirs.iter()) {
                if o.shift_reg != t.2 || o.io_addr != t.3 || o.read_value != t.4 || o.inputs != t.5
                {
                    if bad < 6 {
                        println!(
                            "sample {rows} step {:3}: shift_reg {:08X}/{:08X} io_addr {:6}/{:6} read {:08X}/{:08X} inputs {:9}/{:9}",
                            o.step,
                            o.shift_reg,
                            t.2,
                            o.io_addr,
                            t.3,
                            o.read_value,
                            t.4,
                            o.inputs,
                            t.5
                        );
                    }
                    bad += 1;
                }
            }
            println!(
                "sample {rows}: {} steps, {bad} differ (the reference ran {})",
                ours.len(),
                theirs.len()
            );
            if rows as usize >= steps_ref.last().map_or(0, |s| s.0 as usize) {
                step_report_done = true;
            }
        }

        let ours = dsp.efreg[0] as i32;
        let theirs = v[17];
        if let Some(f) = out.as_mut() {
            use std::io::Write;
            writeln!(f, "{ours} {theirs}").ok();
        }
        sum_ours += ours as i64;
        sum_theirs += theirs as i64;
        peak_ours = peak_ours.max(ours.abs());
        peak_theirs = peak_theirs.max(theirs.abs());
        if (ours - theirs).abs() <= 4 {
            matched += 1;
        } else if first_bad.is_none() {
            first_bad = Some((rows, ours, theirs));
        }
        rows += 1;
    }

    println!(
        "{rows} samples | equal {matched} ({:.1}%)",
        100.0 * matched as f64 / rows.max(1) as f64
    );
    println!(
        "channel 0 — ours: peak {peak_ours}, mean {:.0} | reference: peak {peak_theirs}, mean {:.0}",
        sum_ours as f64 / rows.max(1) as f64,
        sum_theirs as f64 / rows.max(1) as f64
    );
    if let Some((row, ours, theirs)) = first_bad {
        println!("first difference at sample {row}: ours {ours}, reference {theirs}");
    }
}
