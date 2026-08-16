use std::fs::File;
use std::path::Path;

pub const SYNC_HDR: [u8; 12] = [
    0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00,
];

#[derive(Debug, Clone)]
pub struct CdromTrack {
    pub track_num: u8,
    pub ctl_addr: u8,
    pub sector_size: u32,
    pub fad_start: u32,
    pub fad_end: u32,
    pub frames: u32,
    pub pregap: u32,
    pub postgap: u32,
    pub chd_frame_offset: u32,
    pub is_audio: bool,
}

pub struct Cdrom {
    chd: chd::Chd<File>,
    hunk_buffer: Vec<u8>,
    current_hunk_num: u32,
    hunk_num_bytes: usize,
    pub tracks: Vec<CdromTrack>,
    pub lead_out_fad: u32,
    pub toc: [u32; 102],
    pub total_sessions: u8,
}

impl Cdrom {
    pub fn open_chd(path: &str) -> Result<Self, String> {
        let path_buf = Path::new(path);
        if !path_buf.exists() {
            return Err(format!("File does not exist: {}", path));
        }

        let mut file_for_meta = File::open(path).map_err(|e| e.to_string())?;
        let file = File::open(path).map_err(|e| e.to_string())?;
        let mut chd = chd::Chd::open(file, None).map_err(|e| e.to_string())?;
        let hunk_num_bytes = chd.get_hunksized_buffer().len();

        use chd::metadata::MetadataTag;
        const CHT2_TAG: u32 = u32::from_be_bytes(*b"CHT2");
        const CHTR_TAG: u32 = u32::from_be_bytes(*b"CHTR");
        const CHTD_TAG: u32 = u32::from_be_bytes(*b"CHTD");

        let mut track_meta_strings = Vec::new();
        for meta in chd.metadata_refs() {
            let tag = meta.metatag();
            if tag == CHT2_TAG || tag == CHTR_TAG || tag == CHTD_TAG {
                if let Ok(entry) = meta.read(&mut file_for_meta) {
                    let text = String::from_utf8_lossy(&entry.value).to_string();
                    track_meta_strings.push(text);
                }
            }
        }
        if std::env::var("MIMAS_DEBUG_CD").is_ok() || cfg!(test) {
            eprintln!("Found metadata strings: {:?}", track_meta_strings);
        }

        let mut raw_tracks = Vec::new();
        for s in &track_meta_strings {
            if let Some(track) = Self::parse_track_metadata(s) {
                raw_tracks.push(track);
            }
        }

        // Fallback: If no metadata tracks were found, fabricate a single MODE1/2048 track from hunk count
        if raw_tracks.is_empty() {
            let total_hunks = chd.header().hunk_count() as u32;
            let total_bytes = total_hunks as usize * hunk_num_bytes;
            let total_frames = (total_bytes / 2448) as u32;
            raw_tracks.push(RawTrackMeta {
                track_num: 1,
                track_type: "MODE1/2048".to_string(),
                subtype: String::new(),
                frames: total_frames.max(16),
                pregap: 0,
                postgap: 0,
            });
        }

        // Sort by track number
        raw_tracks.sort_by_key(|t| t.track_num);

        let mut tracks = Vec::new();
        let mut current_fad = 150u32;
        let mut chd_frame_offset = 0u32;

        for (idx, raw) in raw_tracks.iter().enumerate() {
            let (ctl_addr, sector_size, is_audio) = Self::decode_track_type(&raw.track_type);
            let fad_start = current_fad + raw.pregap;
            let fad_end = fad_start + raw.frames.saturating_sub(1);
            let track_num = if raw.track_num > 0 {
                raw.track_num as u8
            } else {
                (idx + 1) as u8
            };

            tracks.push(CdromTrack {
                track_num,
                ctl_addr,
                sector_size,
                fad_start,
                fad_end,
                frames: raw.frames,
                pregap: raw.pregap,
                postgap: raw.postgap,
                chd_frame_offset,
                is_audio,
            });

            current_fad = fad_end + 1 + raw.postgap;
            let padded_frames = (raw.frames + 3) & !3;
            chd_frame_offset += padded_frames;
        }

        let lead_out_fad = current_fad;

        // Build 102-entry TOC
        let mut toc = [0xFFFFFFFFu32; 102];
        if !tracks.is_empty() {
            for track in &tracks {
                let idx = (track.track_num.saturating_sub(1)) as usize;
                if idx < 99 {
                    toc[idx] = ((track.ctl_addr as u32) << 24) | (track.fad_start & 0x00FF_FFFF);
                }
            }

            let first_track = &tracks[0];
            let last_track = &tracks[tracks.len() - 1];

            toc[99] =
                ((first_track.ctl_addr as u32) << 24) | ((first_track.track_num as u32) << 16);
            toc[100] =
                ((first_track.ctl_addr as u32) << 24) | ((last_track.track_num as u32) << 16);
            toc[101] = ((last_track.ctl_addr as u32) << 24) | (lead_out_fad & 0x00FF_FFFF);
        }

        Ok(Self {
            chd,
            hunk_buffer: vec![0; hunk_num_bytes],
            current_hunk_num: u32::MAX,
            hunk_num_bytes,
            tracks,
            lead_out_fad,
            toc,
            total_sessions: 1,
        })
    }

    fn parse_track_metadata(s: &str) -> Option<RawTrackMeta> {
        let mut track_num = 0u32;
        let mut track_type = String::new();
        let mut subtype = String::new();
        let mut frames = 0u32;
        let mut pregap = 0u32;
        let mut postgap = 0u32;

        for token in s.split_whitespace() {
            if let Some((k, v)) = token.split_once(':') {
                match k {
                    "TRACK" => track_num = v.parse().unwrap_or(0),
                    "TYPE" => track_type = v.to_string(),
                    "SUBTYPE" => subtype = v.to_string(),
                    "FRAMES" => frames = v.parse().unwrap_or(0),
                    "PREGAP" => pregap = v.parse().unwrap_or(0),
                    "POSTGAP" => postgap = v.parse().unwrap_or(0),
                    _ => {}
                }
            }
        }

        if track_type.is_empty() {
            return None;
        }

        Some(RawTrackMeta {
            track_num,
            track_type,
            subtype,
            frames,
            pregap,
            postgap,
        })
    }

    fn decode_track_type(t: &str) -> (u8, u32, bool) {
        match t {
            "MODE1" | "MODE1/2048" | "MODE2_FORM1" | "MODE2/2048" => (0x41, 2048, false),
            "MODE1_RAW" | "MODE1/2352" | "MODE2_RAW" | "MODE2/2352" => (0x41, 2352, false),
            "MODE2" | "MODE2/2336" | "MODE2_FORM_MIX" => (0x41, 2336, false),
            "MODE2_FORM2" | "MODE2/2324" => (0x41, 2324, false),
            "AUDIO" => (0x01, 2352, true),
            _ => (0x41, 2048, false),
        }
    }

    pub fn fad_to_msf(fad: u32) -> (u8, u8, u8) {
        let frame = (fad % 75) as u8;
        let total_sec = fad / 75;
        let sec = (total_sec % 60) as u8;
        let min = (total_sec / 60) as u8;
        (min, sec, frame)
    }

    pub fn msf_to_fad(m: u8, s: u8, f: u8) -> u32 {
        (m as u32) * 4500 + (s as u32) * 75 + (f as u32)
    }

    pub fn fad_to_track(&self, fad: u32) -> Option<u8> {
        for t in &self.tracks {
            if fad >= t.fad_start && fad <= t.fad_end {
                return Some(t.track_num);
            }
        }
        None
    }

    pub fn track_to_fad(&self, track_num: u8) -> Option<u32> {
        self.tracks
            .iter()
            .find(|t| t.track_num == track_num)
            .map(|t| t.fad_start)
    }

    pub fn read_sector_fad(&mut self, fad: u32, buffer: &mut [u8; 2448]) -> Result<(), String> {
        buffer.fill(0);

        let track = self
            .tracks
            .iter()
            .find(|t| fad >= t.fad_start && fad <= t.fad_end)
            .cloned()
            .ok_or_else(|| format!("FAD {} out of disc bounds", fad))?;

        let offset_in_track = fad - track.fad_start;
        let chdlba = track.chd_frame_offset + track.pregap + offset_in_track;
        let byte_pos = (chdlba as usize) * 2448;
        let hunk_num = (byte_pos / self.hunk_num_bytes) as u32;
        let hunk_offset = byte_pos % self.hunk_num_bytes;

        if hunk_num != self.current_hunk_num {
            let mut hunk = self.chd.hunk(hunk_num).map_err(|e| e.to_string())?;
            let mut comp_buf = Vec::new();
            hunk.read_hunk_in(&mut comp_buf, &mut self.hunk_buffer)
                .map_err(|e| e.to_string())?;
            self.current_hunk_num = hunk_num;
        }

        if hunk_offset + 2448 > self.hunk_buffer.len() {
            return Err("Hunk offset overflow".to_string());
        }

        let hunk_slice = &self.hunk_buffer[hunk_offset..hunk_offset + 2448];

        match track.sector_size {
            2048 => {
                buffer[0..12].copy_from_slice(&SYNC_HDR);
                let (m, s, f) = Self::fad_to_msf(fad);
                buffer[12] = m;
                buffer[13] = s;
                buffer[14] = f;
                buffer[15] = 0x01; // Mode 1
                buffer[16..2064].copy_from_slice(&hunk_slice[0..2048]);
            }
            2352 => {
                buffer[0..2352].copy_from_slice(&hunk_slice[0..2352]);
            }
            _ => {
                buffer.copy_from_slice(hunk_slice);
            }
        }

        Ok(())
    }
}

#[allow(dead_code)]
struct RawTrackMeta {
    track_num: u32,
    track_type: String,
    subtype: String,
    frames: u32,
    pregap: u32,
    postgap: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_open_single_data_track_fixture() {
        let fixture_path = "tests/fixtures/single_data_track.chd";
        if !Path::new(fixture_path).exists() {
            return;
        }

        let mut cdrom =
            Cdrom::open_chd(fixture_path).expect("failed to open single_data_track.chd");
        assert_eq!(cdrom.tracks.len(), 1);
        let t1 = &cdrom.tracks[0];
        assert_eq!(t1.track_num, 1);
        assert_eq!(t1.ctl_addr, 0x41);
        assert_eq!(t1.fad_start, 150);
        assert_eq!(t1.fad_end, 150 + 32 - 1);
        assert_eq!(cdrom.lead_out_fad, 150 + 32);

        // Test TOC entries
        assert_eq!(cdrom.toc[0], 0x41000096);
        assert_eq!(cdrom.toc[1], 0xFFFFFFFF);
        assert_eq!(cdrom.toc[99], 0x41010000);
        assert_eq!(cdrom.toc[100], 0x41010000);
        assert_eq!(cdrom.toc[101], (0x41 << 24) | 182);

        // Test reading FAD 150 (IP.BIN)
        let mut buf = [0u8; 2448];
        cdrom
            .read_sector_fad(150, &mut buf)
            .expect("read FAD 150 failed");
        assert_eq!(&buf[0..12], &SYNC_HDR);
        assert_eq!(&buf[16..32], b"SEGA SEGASATURN ");
        assert_eq!(&buf[32..48], b"SEGA ENTERPRISES");

        // Test reading FAD 166 (ISO PVD)
        cdrom
            .read_sector_fad(166, &mut buf)
            .expect("read FAD 166 failed");
        assert_eq!(buf[16], 0x01); // PVD type
        assert_eq!(&buf[17..22], b"CD001");
    }

    #[test]
    fn test_open_data_plus_audio_fixture() {
        let fixture_path = "tests/fixtures/data_plus_audio.chd";
        if !Path::new(fixture_path).exists() {
            return;
        }

        let mut cdrom = Cdrom::open_chd(fixture_path).expect("failed to open data_plus_audio.chd");
        eprintln!("data_plus_audio tracks: {:?}", cdrom.tracks);
        assert_eq!(cdrom.tracks.len(), 2);
        assert_eq!(cdrom.tracks[0].track_num, 1);
        assert_eq!(cdrom.tracks[0].ctl_addr, 0x41);
        assert_eq!(cdrom.tracks[1].track_num, 2);
        assert_eq!(cdrom.tracks[1].ctl_addr, 0x01);
        assert_eq!(cdrom.tracks[1].is_audio, true);

        assert_eq!(cdrom.toc[0], 0x41000096);
        assert_eq!(cdrom.toc[1], (0x01 << 24) | 166);
        assert_eq!(cdrom.toc[99], 0x41010000);
        assert_eq!(cdrom.toc[100], 0x41020000);
        assert_eq!(cdrom.toc[101], (0x01 << 24) | 182);

        let mut buf = [0u8; 2448];
        cdrom
            .read_sector_fad(166, &mut buf)
            .expect("read FAD 166 audio sector failed");
    }

    #[test]
    fn test_open_mode2_form1_fixture() {
        let fixture_path = "tests/fixtures/mode2_form1.chd";
        if !Path::new(fixture_path).exists() {
            return;
        }

        let mut cdrom = Cdrom::open_chd(fixture_path).expect("failed to open mode2_form1.chd");
        assert_eq!(cdrom.tracks.len(), 1);
        assert_eq!(cdrom.tracks[0].track_num, 1);
        assert_eq!(cdrom.tracks[0].sector_size, 2352);

        let mut buf = [0u8; 2448];
        cdrom
            .read_sector_fad(150, &mut buf)
            .expect("read FAD 150 failed");
        assert_eq!(&buf[0..12], &SYNC_HDR);
        assert_eq!(buf[15], 0x02); // Mode 2
                                   // Subheader at 0x10..0x17
        assert_eq!(buf[16], 1); // file num
        assert_eq!(buf[17], 2); // chan num
        assert_eq!(buf[18], 0x08); // Form 1 submode
    }

    #[test]
    fn test_fad_msf_round_trip() {
        for fad in [150, 166, 4500, 10000, 150000] {
            let (m, s, f) = Cdrom::fad_to_msf(fad);
            let recovered = Cdrom::msf_to_fad(m, s, f);
            assert_eq!(fad, recovered);
        }
    }
}
