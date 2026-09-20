mod bus;
mod cpu;
mod debug;
mod devices;
mod machine;
mod timing;
mod video;

use std::time::Instant;

use cpu::sh2_bus::{Sh2Bus, Tracer};
use cpu::state::Sh2State;
use debug::break_trace::BreakTrace;
use debug::trace_check::{Coverage, compare, load_reference};
use machine::Saturn;

struct Args {
    bios: String,
    frames: u32,
    trace_ref: Option<String>,
    verbose: bool,
    brk: Vec<u32>,
    brk_n: u64,
    brk_depth: usize,
    profile: bool,
    video: bool,
    vram: Vec<u32>,
    dump_sound_ram: Option<String>,
    dump: Option<String>,
    dump_from: u32,
    dump_to: u32,
    dump_every: u32,
}

/// Agrupa os tracers ativos (cobertura e/ou breakpoint) atrás da interface `Tracer`.
struct Debugger {
    cov: Option<Coverage>,
    brk: Option<BreakTrace>,
}

impl Tracer for Debugger {
    fn on_instruction(&mut self, st: &Sh2State, pc: u32, bus: &mut dyn Sh2Bus) {
        if let Some(c) = self.cov.as_mut() {
            c.on_instruction(st, pc, bus);
        }
        if let Some(b) = self.brk.as_mut() {
            b.on_instruction(st, pc, bus);
        }
    }
}

fn parse_args() -> Args {
    let mut a = Args {
        bios: "saturn_bios.bin".into(),
        frames: 60,
        trace_ref: None,
        verbose: false,
        brk: vec![],
        brk_n: 1,
        brk_depth: 40,
        profile: false,
        video: false,
        vram: vec![],
        dump_sound_ram: None,
        dump: None,
        dump_from: 0,
        dump_to: u32::MAX,
        dump_every: 1,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--bios" => a.bios = it.next().expect("--bios <arquivo>"),
            "--frames" => {
                a.frames = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--frames <n>")
            }
            "--trace-check" => a.trace_ref = it.next(),
            "-v" | "--verbose" => a.verbose = true,
            "--profile" => a.profile = true,
            "--video" => a.video = true,
            "--dump" => a.dump = it.next(),
            "--dump-from" => {
                a.dump_from = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--dump-from <n>")
            }
            "--dump-to" => {
                a.dump_to = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--dump-to <n>")
            }
            "--dump-every" => {
                a.dump_every = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--dump-every <n>")
            }
            "--dump-sound-ram" => a.dump_sound_ram = it.next(),
            "--vram" => {
                a.vram = it
                    .next()
                    .expect("--vram <hex,hex>")
                    .split(',')
                    .map(|p| u32::from_str_radix(p.trim_start_matches("0x"), 16).expect("hex"))
                    .collect()
            }
            "--break" => {
                a.brk = it
                    .next()
                    .expect("--break <pc[,pc]>")
                    .split(',')
                    .map(|p| u32::from_str_radix(p.trim_start_matches("0x"), 16).expect("pc hex"))
                    .collect()
            }
            "--break-n" => {
                a.brk_n = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--break-n <n>")
            }
            "--break-depth" => {
                a.brk_depth = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--break-depth <n>")
            }
            other => panic!("argumento desconhecido: {other}"),
        }
    }
    a
}

fn main() {
    let args = parse_args();
    let bios =
        std::fs::read(&args.bios).unwrap_or_else(|e| panic!("não consegui ler {}: {e}", args.bios));
    let mut saturn = Saturn::new(&bios);
    if let Some(dir) = &args.dump {
        match video::dumper::FrameDumper::new(
            dir,
            args.dump_from,
            args.dump_to.min(args.frames),
            args.dump_every,
        ) {
            Ok(d) => {
                println!("Renderizador OpenGL: {}", d.gpu());
                saturn.sink = Box::new(d);
            }
            Err(e) => {
                eprintln!("FALHA ao criar o renderizador OpenGL headless: {e}");
                std::process::exit(2);
            }
        }
    }
    println!(
        "BIOS {} bytes; reset PC={:08X} SP={:08X}",
        bios.len(),
        saturn.cpu.st.pc,
        saturn.cpu.st.r[15]
    );

    if args.profile {
        saturn.profile = Some(Default::default());
    }
    let mut dbg = Debugger {
        cov: args
            .trace_ref
            .as_ref()
            .map(|_| Coverage::new(saturn.frame_cell.clone())),
        brk: (!args.brk.is_empty()).then(|| {
            BreakTrace::new(
                args.brk.clone(),
                args.brk_n,
                args.brk_depth,
                saturn.frame_cell.clone(),
            )
        }),
    };
    let tracing = dbg.cov.is_some() || dbg.brk.is_some();
    if tracing {
        saturn.cpu.jit.set_trace(true);
    }

    let t0 = Instant::now();
    let mut blocks: u64 = 0;
    let mut last_pc = 0;
    while saturn.frame() < args.frames {
        let r = if tracing {
            saturn.step(Some(&mut dbg))
        } else {
            saturn.step(None)
        };
        if dbg.brk.as_ref().is_some_and(|b| b.done) {
            break;
        }
        blocks += 1;
        if let Err(f) = r {
            println!("FALHA da CPU no quadro {}: {:?}", saturn.frame(), f);
            break;
        }
        last_pc = saturn.cpu.st.pc;
    }
    let dt = t0.elapsed();
    println!(
        "{} quadros, {} blocos executados, {} compilados, {:.2}s ({:.1} Mciclos/s), último PC={:08X}",
        saturn.frame(),
        blocks,
        saturn.cpu.jit.compiled_total,
        dt.as_secs_f64(),
        saturn.cpu.st.cycles as f64 / dt.as_secs_f64() / 1e6,
        last_pc
    );

    if let (Some(path), Some(cov)) = (args.trace_ref.as_ref(), dbg.cov.as_ref()) {
        match load_reference(path) {
            Ok(reference) => {
                let rep = compare(&reference, cov);
                rep.print(&reference);
                let mut runs = rep.missing_runs(&reference);
                println!(
                    "TRACE: {} faixas ausentes; as maiores (início, tamanho, quadro ref, PC):",
                    runs.len()
                );
                runs.sort_by_key(|r| std::cmp::Reverse(r.1));
                for (i, n, f, pc) in runs.iter().take(12) {
                    println!("   ref[{i}] +{n} instruções, quadro {f}, PC {pc:08X}");
                }
            }
            Err(e) => println!("não consegui ler a referência {path}: {e}"),
        }
    }

    if let Some(path) = &args.dump_sound_ram {
        let ram = saturn.scsp_ram.borrow();
        match std::fs::write(path, ram.data()) {
            Ok(()) => println!("RAM de som ({} bytes) escrita em {path}", ram.data().len()),
            Err(e) => println!("não consegui escrever {path}: {e}"),
        }
    }

    for &a in &args.vram {
        let v = saturn.vdp2.borrow();
        let a = a as usize;
        println!("VDP2 VRAM @{a:05X}:");
        for row in 0..6 {
            let o = a + row * 16;
            println!(
                "  {o:05X}: {}",
                v.vram[o..o + 16]
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
    }
    if args.video {
        debug::video_state::print_vdp2(&saturn.vdp2.borrow());
        debug::video_state::print_vdp1(&saturn.vdp1.borrow());
    }

    if let Some(p) = saturn.profile.as_ref() {
        let mut v: Vec<_> = p.iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        println!("--- blocos mais executados (PC de entrada, execuções) ---");
        for (pc, n) in v.iter().take(14) {
            println!("  {:08X} {}", pc, n);
        }
    }

    if args.verbose {
        println!("--- acessos a stubs (nome, offset, escrita) -> (n, último valor) ---");
        for ((name, off, w), (n, v)) in saturn.stub_log.borrow().entries.iter().take(80) {
            println!(
                "  {name:<12} {off:06X} {} n={n:<6} v={v:08X}",
                if *w { "W" } else { "R" }
            );
        }
        let mut counts = std::collections::BTreeMap::new();
        for c in &saturn.smpc.borrow().commands {
            *counts.entry(*c).or_insert(0u32) += 1;
        }
        println!(
            "--- SMPC comandos (código: vezes): {}",
            counts
                .iter()
                .map(|(c, n)| format!("{c:02X}:{n}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        println!(
            "--- CD block comandos: {:04X?}",
            saturn
                .cd
                .borrow()
                .commands
                .iter()
                .take(20)
                .collect::<Vec<_>>()
        );
        let scu = saturn.scu.borrow();
        println!(
            "--- escritas nas portas do DSP do SCU: {} (offset:valor) ---",
            scu.dsp_log.len()
        );
        for chunk in scu.dsp_log.chunks(6).take(30) {
            println!(
                "  {}",
                chunk
                    .iter()
                    .map(|(o, v)| format!("{o:02X}:{v:08X}"))
                    .collect::<Vec<_>>()
                    .join("  ")
            );
        }
        for l in scu.dsp.log.iter() {
            println!("  {l}");
        }
        for l in scu.dma_log.iter().take(30) {
            println!("  {l}");
        }
        println!("--- acessos a endereços sem dispositivo ---");
        for (page, w, n) in saturn.space.sys.unmapped_report().iter().take(40) {
            println!("  {:08X} {} n={n}", page, if *w { "W" } else { "R" });
        }
    }
}
