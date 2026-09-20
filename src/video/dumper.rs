//! Consome quadros: renderiza via OpenGL e grava `frameN.png` (mesma orientação das capturas).

use std::path::PathBuf;

use super::FrameSink;
use super::renderer::{GlRenderer, HEIGHT, WIDTH};
use crate::devices::vdp1::Vdp1;
use crate::devices::vdp2::Vdp2;

pub struct FrameDumper {
    renderer: GlRenderer,
    dir: PathBuf,
    from: u32,
    to: u32,
    every: u32,
    pub written: u32,
}

impl FrameDumper {
    pub fn new(dir: &str, from: u32, to: u32, every: u32) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("não consegui criar {dir}: {e}"))?;
        Ok(Self {
            renderer: GlRenderer::new()?,
            dir: PathBuf::from(dir),
            from,
            to,
            every: every.max(1),
            written: 0,
        })
    }

    pub fn gpu(&self) -> String {
        self.renderer.gpu()
    }
}

impl FrameSink for FrameDumper {
    fn on_frame(&mut self, frame: u32, vdp1: &Vdp1, vdp2: &Vdp2) {
        if frame < self.from || frame > self.to || frame % self.every != 0 {
            return;
        }
        self.renderer.render(vdp1, vdp2);
        let path = self.dir.join(format!("frame{frame}.png"));
        if let Err(e) = image::save_buffer(
            &path,
            &self.renderer.pixels,
            WIDTH as u32,
            HEIGHT as u32,
            image::ColorType::Rgba8,
        ) {
            eprintln!("erro ao gravar {}: {e}", path.display());
        } else {
            self.written += 1;
        }
    }
}
