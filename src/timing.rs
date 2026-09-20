//! Temporização de vídeo NTSC 60 Hz em ciclos do SH-2: 263 linhas de 1820 ciclos; a área
//! ativa tem 224 linhas. Gera os eventos que movem VDP2/SCU (HBlank, VBlank-in/out).

pub const CYCLES_PER_LINE: u64 = 1820;
pub const LINES_PER_FRAME: u32 = 263;
pub const ACTIVE_LINES: u32 = 224;
/// Ponto da linha (em ciclos) em que o HBlank começa.
const HBLANK_START: u64 = 1500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoEvent {
    /// Início de uma linha (limpa o HBlank).
    LineStart(u32),
    HBlankIn(u32),
    VBlankIn,
    VBlankOut,
}

#[derive(Default)]
pub struct VideoTiming {
    pub line: u32,
    pub frame: u32,
    in_line: u64,
    hblank_sent: bool,
}

impl VideoTiming {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn advance(&mut self, cycles: u64, out: &mut Vec<VideoEvent>) {
        self.in_line += cycles;
        loop {
            if !self.hblank_sent && self.in_line >= HBLANK_START {
                self.hblank_sent = true;
                if self.line < ACTIVE_LINES {
                    out.push(VideoEvent::HBlankIn(self.line));
                }
            }
            if self.in_line < CYCLES_PER_LINE {
                break;
            }
            self.in_line -= CYCLES_PER_LINE;
            self.hblank_sent = false;
            self.line += 1;
            if self.line == ACTIVE_LINES {
                out.push(VideoEvent::VBlankIn);
            }
            if self.line == LINES_PER_FRAME {
                self.line = 0;
                self.frame += 1;
                out.push(VideoEvent::VBlankOut);
            }
            out.push(VideoEvent::LineStart(self.line));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_frame_has_one_vblank_in_and_out_in_order() {
        let mut t = VideoTiming::new();
        let mut ev = Vec::new();
        t.advance(CYCLES_PER_LINE * LINES_PER_FRAME as u64, &mut ev);
        let vi = ev.iter().position(|e| *e == VideoEvent::VBlankIn).unwrap();
        let vo = ev.iter().position(|e| *e == VideoEvent::VBlankOut).unwrap();
        assert!(vi < vo);
        assert_eq!(t.frame, 1);
        assert_eq!(
            ev.iter()
                .filter(|e| matches!(e, VideoEvent::HBlankIn(_)))
                .count(),
            ACTIVE_LINES as usize
        );
    }
}
