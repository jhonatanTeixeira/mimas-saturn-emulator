use crossbeam::channel::{bounded, Sender, Receiver};

pub struct Scsp {
    pub volume: u8,
    pub audio_tx: Sender<i16>,
    pub audio_rx: Receiver<i16>,
}

impl Scsp {
    pub fn new() -> Self {
        let (audio_tx, audio_rx) = bounded(4096);
        Self {
            volume: 0,
            audio_tx,
            audio_rx,
        }
    }

    pub fn set_volume(&mut self, vol: u8) {
        self.volume = vol.min(0x0F);
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
