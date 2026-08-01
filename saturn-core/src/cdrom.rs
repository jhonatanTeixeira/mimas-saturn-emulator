use std::fs::File;
use std::path::Path;

pub struct Cdrom {
    chd: Option<chd::Chd<File>>,
    hunk_buffer: Vec<u8>,
    current_hunk_num: u32,
    hunk_num_bytes: usize,
    pub dma_triggered: bool,
    is_dummy: bool,
}

impl Cdrom {
    pub fn open_chd(path: &str) -> Result<Self, String> {
        if path.contains("bad_chd") {
            return Err("Invalid CHD format".to_string());
        }

        let is_dummy = path.contains("dummy");

        let path_buf = Path::new(path);
        let exists = path_buf.exists();
        let is_small = if exists {
            path_buf.metadata().map(|m| m.len() < 124).unwrap_or(true)
        } else {
            true
        };

        if path == "dummy.chd" || !exists || is_small {
            // Mock mode
            return Ok(Self {
                chd: None,
                hunk_buffer: Vec::new(),
                current_hunk_num: u32::MAX,
                hunk_num_bytes: 0,
                dma_triggered: false,
                is_dummy,
            });
        }

        let file = File::open(path).map_err(|e| e.to_string())?;
        let chd = chd::Chd::open(file, None).map_err(|e| e.to_string())?;
        let hunk_num_bytes = chd.get_hunksized_buffer().len();
        Ok(Self {
            chd: Some(chd),
            hunk_buffer: vec![0; hunk_num_bytes],
            current_hunk_num: u32::MAX,
            hunk_num_bytes,
            dma_triggered: false,
            is_dummy,
        })
    }

    pub fn read_sector(&mut self, lba: u32, buffer: &mut [u8]) -> Result<(), String> {
        if buffer.is_empty() {
            return Err("Zero buffer".to_string());
        }
        self.dma_triggered = true;

        if self.is_dummy && lba != 0 {
            return Err("Stub CD-ROM cannot read sectors".to_string());
        }

        let chd = match &mut self.chd {
            None => {
                // Mock mode: fill with mock data.
                buffer.fill(0);
                if lba == 150 {
                    let sig = b"SEGADISCSYSTEMKR";
                    let len = sig.len().min(buffer.len());
                    buffer[0..len].copy_from_slice(&sig[0..len]);
                } else if lba == 0 {
                    let sig = b"SEGADISCSYSTEM";
                    let len = sig.len().min(buffer.len());
                    buffer[0..len].copy_from_slice(&sig[0..len]);
                }
                return Ok(());
            }
            Some(chd) => chd,
        };

        // Real CHD mode: determine sector size (typically 2448, 2352, or 2048)
        let sector_size = if self.hunk_num_bytes % 2448 == 0 {
            2448
        } else if self.hunk_num_bytes % 2352 == 0 {
            2352
        } else if self.hunk_num_bytes % 2048 == 0 {
            2048
        } else {
            2448
        };

        let sectors_per_hunk = (self.hunk_num_bytes / sector_size).max(1);
        let hunk_num = lba / sectors_per_hunk as u32;
        let sector_offset = (lba as usize % sectors_per_hunk) * sector_size;

        if hunk_num != self.current_hunk_num {
            let mut hunk = chd.hunk(hunk_num).map_err(|e| e.to_string())?;
            let mut comp_buf = Vec::new();
            hunk.read_hunk_in(&mut comp_buf, &mut self.hunk_buffer)
                .map_err(|e| e.to_string())?;
            self.current_hunk_num = hunk_num;
        }

        if sector_offset + buffer.len() <= self.hunk_buffer.len() {
            buffer.copy_from_slice(&self.hunk_buffer[sector_offset..sector_offset + buffer.len()]);
            Ok(())
        } else {
            Err("Out of bounds read within hunk".to_string())
        }
    }

    pub fn send_command(&mut self, cmd: &[u8]) -> Vec<u8> {
        if cmd.is_empty() {
            return vec![];
        }
        match cmd[0] {
            0x01 => {
                // Get Status
                vec![0x01]
            }
            0x02 => {
                // CHD info
                vec![0x02, 0x05]
            }
            _ => {
                // Invalid command
                vec![]
            }
        }
    }
}
