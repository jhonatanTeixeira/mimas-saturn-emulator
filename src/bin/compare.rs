//! Compara os quadros gerados com as capturas de referência (stubs/captures). Para cada captura
//! procura, entre todos os quadros gerados, o mais parecido (erro médio absoluto por canal) e
//! resume o deslocamento de quadros — a temporização do nosso boot não é idêntica à da referência.
//! Uso: compare <capturas> <gerados> [--max-frame N]

use std::path::Path;

fn load(p: &Path) -> Option<image::RgbaImage> {
    image::open(p).ok().map(|i| i.to_rgba8())
}

fn mae(a: &image::RgbaImage, b: &image::RgbaImage, step: usize) -> f64 {
    if a.dimensions() != b.dimensions() {
        return f64::MAX;
    }
    let (pa, pb) = (a.as_raw(), b.as_raw());
    let (mut sum, mut n) = (0u64, 0u64);
    let mut i = 0;
    while i + 3 < pa.len() {
        for c in 0..3 {
            sum += (pa[i + c] as i32 - pb[i + c] as i32).unsigned_abs() as u64;
        }
        n += 3;
        i += 4 * step;
    }
    sum as f64 / n.max(1) as f64
}

fn frames_in(dir: &str) -> Vec<(u32, image::RgbaImage)> {
    let mut v: Vec<(u32, image::RgbaImage)> = std::fs::read_dir(dir)
        .expect("diretório")
        .filter_map(|e| {
            let e = e.ok()?;
            let name = e.file_name().to_string_lossy().into_owned();
            let n: u32 = name
                .strip_prefix("frame")?
                .strip_suffix(".png")?
                .parse()
                .ok()?;
            Some((n, load(&e.path())?))
        })
        .collect();
    v.sort_by_key(|x| x.0);
    v
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (caps, gens) = (&args[1], &args[2]);
    let max_frame: u32 = args
        .iter()
        .position(|a| a == "--max-frame")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(u32::MAX);
    let caps: Vec<_> = frames_in(caps)
        .into_iter()
        .filter(|c| c.0 <= max_frame)
        .collect();
    let gens = frames_in(gens);
    println!(
        "{} capturas de referência, {} quadros gerados",
        caps.len(),
        gens.len()
    );
    let mut deltas = Vec::new();
    let mut total = 0.0;
    for (n, cap) in &caps {
        let mut best = (f64::MAX, 0u32);
        for (g, img) in &gens {
            let e = mae(cap, img, 7);
            if e < best.0 {
                best = (e, *g);
            }
        }
        let full = gens
            .iter()
            .find(|x| x.0 == best.1)
            .map(|(_, i)| mae(cap, i, 1))
            .unwrap_or(f64::MAX);
        total += full;
        deltas.push(best.1 as i64 - *n as i64);
        println!(
            "captura {n:>5} -> nosso {:>5} (Δ{:+5})  erro médio {full:6.2}/255",
            best.1,
            best.1 as i64 - *n as i64
        );
    }
    deltas.sort_unstable();
    println!(
        "erro médio geral {:.2}/255; Δ de quadro mediano {:+}",
        total / caps.len().max(1) as f64,
        deltas.get(deltas.len() / 2).copied().unwrap_or(0)
    );
}
