use crate::shared_buffers::WorkRam;
use crossbeam::channel::{bounded, Receiver, Sender};

pub struct Scsp {
    pub volume: u8,
    pub audio_tx: Sender<f32>,
    pub audio_rx: Receiver<f32>,
    voices: [VoiceState; 32],
}

#[derive(Default, Clone)]
struct VoiceState {
    active: bool,
    sample_addr: u32,
    current_offset: f64,
    loop_start: u32,
    loop_end: u32,
    step: f64,
    volume: f32,
}

impl Scsp {
    pub fn new() -> Self {
        let (audio_tx, audio_rx) = bounded(44100 * 2);
        Self {
            volume: 0,
            audio_tx,
            audio_rx,
            voices: std::array::from_fn(|_| VoiceState::default()),
        }
    }

    pub fn set_volume(&mut self, vol: u8) {
        self.volume = vol.min(0x0F);
    }

    pub fn synthesize(&mut self, work_ram: &WorkRam, count: usize) {
        let regs = work_ram.scsp_regs.read().unwrap();
        let sound_ram = work_ram.sound_ram.read().unwrap();

        for i in 0..32 {
            let base = i * 32;
            if base + 30 >= regs.len() {
                continue;
            }
            // KYON is bit 11 of the first word (offset 0)
            let ky_on = (regs[base] & 0x08) != 0;
            let voice = &mut self.voices[i];

            if ky_on {
                if !voice.active {
                    voice.active = true;
                    // SA is in sound RAM
                    let sa = (((regs[base] & 7) as u32) << 16)
                        | ((regs[base + 1] as u32) << 8)
                        | (regs[base + 2] as u32);
                    voice.sample_addr = sa & 0x7FFFF;
                    voice.current_offset = 0.0;

                    let lsa = ((regs[base + 4] as u32) << 8) | (regs[base + 5] as u32);
                    voice.loop_start = lsa & 0x7FFFF;

                    let lea = ((regs[base + 6] as u32) << 8) | (regs[base + 7] as u32);
                    voice.loop_end = lea & 0x7FFFF;

                    // Pitch FNS
                    let fns = ((regs[base + 10] as u32) << 8) | (regs[base + 11] as u32);
                    voice.step = if fns > 0 { fns as f64 / 1024.0 } else { 1.0 };

                    let level = regs[base + 12];
                    voice.volume = level as f32 / 255.0;
                }
            } else {
                voice.active = false;
            }
        }

        for _ in 0..count {
            let mut left_sample = 0.0f32;
            let mut right_sample = 0.0f32;

            for voice in self.voices.iter_mut() {
                if !voice.active {
                    continue;
                }
                let ptr = (voice.sample_addr + voice.current_offset as u32) as usize;
                if ptr < sound_ram.len() {
                    let byte = sound_ram[ptr];
                    let sample = (byte as f32 - 128.0) / 128.0;

                    left_sample += sample * voice.volume;
                    right_sample += sample * voice.volume;

                    voice.current_offset += voice.step;
                    if voice.loop_end > voice.loop_start
                        && voice.current_offset as u32 >= voice.loop_end
                    {
                        voice.current_offset = voice.loop_start as f64;
                    }
                } else {
                    voice.active = false;
                }
            }

            // Mix and send samples (non-blocking try_send to prevent deadlocks when receiver is inactive)
            let _ = self.audio_tx.try_send(left_sample);
            let _ = self.audio_tx.try_send(right_sample);
        }
    }
}

impl Default for Scsp {
    fn default() -> Self {
        Self::new()
    }
}

pub struct SoundRingBuffer {
    pub sender: Sender<f32>,
    pub receiver: Receiver<f32>,
}

impl SoundRingBuffer {
    pub fn new(capacity: usize) -> Self {
        let (sender, receiver) = bounded(capacity);
        Self { sender, receiver }
    }
}
