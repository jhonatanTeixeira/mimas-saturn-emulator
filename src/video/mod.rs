//! Caminho de vídeo. `FrameSink` desacopla a máquina de quem consome os quadros
//! (renderizador OpenGL, gravador de PNG, ...).

pub mod dumper;
pub mod egl;
pub mod renderer;

use crate::devices::vdp1::Vdp1;
use crate::devices::vdp2::Vdp2;

pub trait FrameSink {
    /// Chamado ao fim de cada quadro (VBlank-in) com o estado dos dois VDPs.
    fn on_frame(&mut self, frame: u32, vdp1: &Vdp1, vdp2: &Vdp2);
}

pub struct NullSink;

impl FrameSink for NullSink {
    fn on_frame(&mut self, _frame: u32, _vdp1: &Vdp1, _vdp2: &Vdp2) {}
}
