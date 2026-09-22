//! Disc image reader: a `.cue` sheet plus the raw `.bin` it names. Only what the CD Block
//! needs — a table of contents and 2352-byte raw sectors addressed by FAD (frame address; the
//! first user sector of a disc is FAD 150, the two-second pregap comes before it).
//!
//! Supported cue subset: one or more `FILE ... BINARY` lines, `TRACK n MODE1/2352 |
//! MODE2/2352 | AUDIO`, and `INDEX 00|01 mm:ss:ff`. Every track must be 2352 bytes per
//! sector, which is what a raw Saturn dump is.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub const RAW_SECTOR: usize = 2352;
/// FAD of the first sector of the disc's first track's INDEX 01 when the cue starts at 00:00:00.
pub const PREGAP_FADS: u32 = 150;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub number: u8,
    /// CONTROL nibble in the high half, ADR in the low half: 0x41 data, 0x01 audio.
    pub ctrl_adr: u8,
    /// FAD of INDEX 01, where the track's content starts.
    pub start_fad: u32,
    /// Index into `DiscImage::files` and the byte offset of `start_fad` in that file.
    file: usize,
    file_offset: u64,
    /// First FAD after the track (exclusive).
    pub end_fad: u32,
}

impl Track {
    pub fn is_data(&self) -> bool {
        self.ctrl_adr & 0x40 != 0
    }
}

pub struct DiscImage {
    files: Vec<File>,
    pub tracks: Vec<Track>,
    /// First FAD after the last track.
    pub leadout_fad: u32,
}

/// Parses `mm:ss:ff` into a frame count.
fn msf(s: &str) -> Option<u32> {
    let mut it = s.split(':').map(|p| p.trim().parse::<u32>().ok());
    let (m, sec, f) = (it.next()??, it.next()??, it.next()??);
    Some((m * 60 + sec) * 75 + f)
}

impl DiscImage {
    pub fn open(cue_path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(cue_path)
            .map_err(|e| format!("{}: {e}", cue_path.display()))?;
        let dir = cue_path.parent().unwrap_or_else(|| Path::new("."));
        let mut files: Vec<File> = Vec::new();
        let mut file_sectors: Vec<u32> = Vec::new();
        // (number, ctrl_adr, index01 frame within its file, file index)
        let mut raw: Vec<(u8, u8, u32, usize)> = Vec::new();
        for line in text.lines() {
            let mut w = line.split_whitespace();
            match w.next() {
                Some("FILE") => {
                    let name = line
                        .split('"')
                        .nth(1)
                        .ok_or_else(|| format!("bad FILE line: {line}"))?;
                    let f = File::open(dir.join(name)).map_err(|e| format!("{name}: {e}"))?;
                    let len = f.metadata().map_err(|e| e.to_string())?.len();
                    file_sectors.push((len / RAW_SECTOR as u64) as u32);
                    files.push(f);
                }
                Some("TRACK") => {
                    let n: u8 = w
                        .next()
                        .and_then(|s| s.parse().ok())
                        .ok_or_else(|| format!("bad TRACK line: {line}"))?;
                    let mode = w.next().unwrap_or("");
                    if mode != "AUDIO" && !mode.ends_with("/2352") {
                        return Err(format!(
                            "unsupported track mode {mode}: only 2352-byte tracks"
                        ));
                    }
                    let ctrl = if mode == "AUDIO" { 0x01 } else { 0x41 };
                    raw.push((n, ctrl, 0, files.len().saturating_sub(1)));
                }
                Some("INDEX") => {
                    let idx = w.next().unwrap_or("");
                    if idx == "01" {
                        let frame = w
                            .next()
                            .and_then(msf)
                            .ok_or_else(|| format!("bad INDEX line: {line}"))?;
                        if let Some(t) = raw.last_mut() {
                            t.2 = frame;
                        }
                    }
                }
                _ => {}
            }
        }
        if raw.is_empty() || files.is_empty() {
            return Err("cue sheet has no tracks".into());
        }
        // Files are laid out back to back on the disc, after the 150-frame pregap.
        let mut file_base = vec![PREGAP_FADS; files.len()];
        for i in 1..files.len() {
            file_base[i] = file_base[i - 1] + file_sectors[i - 1];
        }
        let mut tracks: Vec<Track> = raw
            .iter()
            .map(|&(number, ctrl_adr, frame, file)| Track {
                number,
                ctrl_adr,
                start_fad: file_base[file] + frame,
                file,
                file_offset: frame as u64 * RAW_SECTOR as u64,
                end_fad: 0,
            })
            .collect();
        let leadout_fad = file_base[files.len() - 1] + file_sectors[files.len() - 1];
        for i in 0..tracks.len() {
            tracks[i].end_fad = tracks.get(i + 1).map_or(leadout_fad, |t| t.start_fad);
        }
        Ok(Self {
            files,
            tracks,
            leadout_fad,
        })
    }

    pub fn track_at(&self, fad: u32) -> Option<&Track> {
        self.tracks
            .iter()
            .find(|t| fad >= t.start_fad && fad < t.end_fad)
    }

    /// Reads the raw 2352-byte sector at `fad`. Sectors in a track's pregap (before INDEX 01)
    /// and past the lead-out read as `None`.
    pub fn read_raw(&mut self, fad: u32) -> Option<[u8; RAW_SECTOR]> {
        let t = self.track_at(fad)?;
        let off = t.file_offset + (fad - t.start_fad) as u64 * RAW_SECTOR as u64;
        let file = t.file;
        let f = &mut self.files[file];
        let mut buf = [0u8; RAW_SECTOR];
        f.seek(SeekFrom::Start(off)).ok()?;
        f.read_exact(&mut buf).ok()?;
        Some(buf)
    }

    /// The 2048 bytes of user data of a Mode 1 sector, or the Mode 2 Form 1 payload.
    pub fn read_user(&mut self, fad: u32) -> Option<[u8; 2048]> {
        let raw = self.read_raw(fad)?;
        // Byte 15 is the sector mode; Mode 2 has an 8-byte subheader before the payload.
        let start = if raw[15] == 2 { 24 } else { 16 };
        let mut out = [0u8; 2048];
        out.copy_from_slice(&raw[start..start + 2048]);
        Some(out)
    }

    /// The 408-byte TOC the CD Block hands out: 99 track slots (`ctrl/adr`, FAD), then the
    /// first-track, last-track and lead-out points (A0, A1, A2).
    pub fn toc(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(102 * 4);
        for n in 1..=99u8 {
            match self.tracks.iter().find(|t| t.number == n) {
                Some(t) => out.extend_from_slice(&[
                    t.ctrl_adr,
                    (t.start_fad >> 16) as u8,
                    (t.start_fad >> 8) as u8,
                    t.start_fad as u8,
                ]),
                None => out.extend_from_slice(&[0xFF; 4]),
            }
        }
        let first = self.tracks.first().expect("checked in open");
        let last = self.tracks.last().expect("checked in open");
        out.extend_from_slice(&[first.ctrl_adr, first.number, 0, 0]);
        out.extend_from_slice(&[last.ctrl_adr, last.number, 0, 0]);
        out.extend_from_slice(&[
            last.ctrl_adr,
            (self.leadout_fad >> 16) as u8,
            (self.leadout_fad >> 8) as u8,
            self.leadout_fad as u8,
        ]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Writes a two-track disc (1 data + 1 audio sector run) to a temp dir and returns the
    /// cue path.
    fn fixture(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mimas_disc_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut bin = Vec::new();
        for s in 0..6u8 {
            let mut sec = vec![s; RAW_SECTOR];
            sec[15] = 1;
            bin.extend_from_slice(&sec);
        }
        std::fs::File::create(dir.join("t.bin"))
            .unwrap()
            .write_all(&bin)
            .unwrap();
        let cue = "FILE \"t.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n  \
                   TRACK 02 AUDIO\n    INDEX 00 00:00:04\n    INDEX 01 00:00:05\n";
        let p = dir.join("t.cue");
        std::fs::write(&p, cue).unwrap();
        p
    }

    #[test]
    fn tracks_start_after_the_pregap_and_the_leadout_follows_the_file() {
        let d = DiscImage::open(&fixture("toc")).unwrap();
        assert_eq!(d.tracks.len(), 2);
        assert_eq!(d.tracks[0].start_fad, 150);
        assert!(d.tracks[0].is_data());
        assert_eq!(d.tracks[1].start_fad, 155);
        assert!(!d.tracks[1].is_data());
        assert_eq!(d.leadout_fad, 156);
    }

    #[test]
    fn sectors_are_addressed_by_fad() {
        let mut d = DiscImage::open(&fixture("read")).unwrap();
        assert_eq!(d.read_raw(150).unwrap()[100], 0);
        assert_eq!(d.read_raw(153).unwrap()[100], 3);
        assert!(d.read_raw(149).is_none());
        assert!(d.read_raw(156).is_none());
    }

    #[test]
    fn toc_has_track_slots_and_the_three_special_points() {
        let d = DiscImage::open(&fixture("tocbytes")).unwrap();
        let toc = d.toc();
        assert_eq!(toc.len(), 408);
        assert_eq!(&toc[0..4], &[0x41, 0, 0, 150]);
        assert_eq!(&toc[4..8], &[0x01, 0, 0, 155]);
        assert_eq!(&toc[8..12], &[0xFF; 4]);
        assert_eq!(&toc[99 * 4..99 * 4 + 4], &[0x41, 1, 0, 0]);
        assert_eq!(&toc[100 * 4..100 * 4 + 4], &[0x01, 2, 0, 0]);
        assert_eq!(&toc[101 * 4..101 * 4 + 4], &[0x01, 0, 0, 156]);
    }

    #[test]
    fn user_data_skips_the_sync_and_header() {
        let mut d = DiscImage::open(&fixture("user")).unwrap();
        assert_eq!(d.read_user(151).unwrap()[0], 1);
    }
}
