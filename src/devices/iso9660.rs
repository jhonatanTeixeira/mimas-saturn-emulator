//! Minimal ISO 9660 directory reader, the part of the file system the CD Block implements in
//! hardware: it walks directory records so the host can ask for "file id N" instead of a FAD.
//!
//! File ids are the record's position in its directory: 0 is `.`, 1 is `..`, files start at 2.

use crate::devices::disc::DiscImage;

/// One directory record, already translated to what the CD Block reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Absolute FAD of the first sector (ISO 9660 extent + 150).
    pub fad: u32,
    pub size: u32,
    pub unit_size: u8,
    pub gap_size: u8,
    pub is_dir: bool,
}

impl Entry {
    /// The 12-byte "file info" record: FAD, size, unit size, gap size, file id, attributes.
    pub fn info(&self, id: u8) -> [u8; 12] {
        let mut b = [0u8; 12];
        b[0..4].copy_from_slice(&self.fad.to_be_bytes());
        b[4..8].copy_from_slice(&self.size.to_be_bytes());
        b[8] = self.unit_size;
        b[9] = self.gap_size;
        b[10] = id;
        b[11] = if self.is_dir { 0x02 } else { 0x00 };
        b
    }
}

fn u32_le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// Parses the records of a directory whose extent is `size` bytes long, starting at `fad`.
fn parse_directory(disc: &mut DiscImage, fad: u32, size: u32) -> Vec<Entry> {
    let mut out = Vec::new();
    let sectors = size.div_ceil(2048);
    for s in 0..sectors {
        let Some(data) = disc.read_user(fad + s) else {
            break;
        };
        let mut pos = 0usize;
        while pos < 2048 {
            let len = data[pos] as usize;
            // A zero length pads to the end of the sector: records never straddle sectors.
            if len == 0 || pos + len > 2048 || len < 33 {
                break;
            }
            let r = &data[pos..pos + len];
            out.push(Entry {
                fad: u32_le(&r[2..6]) + 150,
                size: u32_le(&r[10..14]),
                unit_size: r[26],
                gap_size: r[27],
                is_dir: r[25] & 0x02 != 0,
            });
            pos += len;
        }
    }
    out
}

/// The root directory of the volume, from the primary volume descriptor at LBA 16.
pub fn root_directory(disc: &mut DiscImage) -> Option<Entry> {
    let pvd = disc.read_user(16 + 150)?;
    if &pvd[1..6] != b"CD001" || pvd[0] != 1 {
        return None;
    }
    // The root directory record is embedded at offset 156 of the descriptor.
    let r = &pvd[156..190];
    Some(Entry {
        fad: u32_le(&r[2..6]) + 150,
        size: u32_le(&r[10..14]),
        unit_size: 0,
        gap_size: 0,
        is_dir: true,
    })
}

pub fn read_directory(disc: &mut DiscImage, dir: &Entry) -> Vec<Entry> {
    parse_directory(disc, dir.fad, dir.size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn record(extent: u32, size: u32, flags: u8, name: &[u8]) -> Vec<u8> {
        let mut r = vec![0u8; 33 + name.len()];
        let len = (33 + name.len()) as u8;
        r[0] = len;
        r[2..6].copy_from_slice(&extent.to_le_bytes());
        r[10..14].copy_from_slice(&size.to_le_bytes());
        r[25] = flags;
        r[32] = name.len() as u8;
        r[33..].copy_from_slice(name);
        r
    }

    /// Builds a disc with a PVD at LBA 16 and a root directory (`.`, `..`, one file) at LBA 20.
    fn fixture() -> DiscImage {
        let dir = std::env::temp_dir().join(format!("mimas_iso_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut bin = vec![0u8; 24 * 2352];
        for s in 0..24 {
            bin[s * 2352 + 15] = 1;
        }
        let mut pvd = vec![0u8; 2048];
        pvd[0] = 1;
        pvd[1..6].copy_from_slice(b"CD001");
        let root = record(20, 2048, 2, &[0]);
        pvd[156..156 + root.len()].copy_from_slice(&root);
        bin[16 * 2352 + 16..16 * 2352 + 16 + 2048].copy_from_slice(&pvd);
        let mut d = Vec::new();
        d.extend(record(20, 2048, 2, &[0]));
        d.extend(record(20, 2048, 2, &[1]));
        d.extend(record(30, 5000, 0, b"GAME.BIN;1"));
        bin[20 * 2352 + 16..20 * 2352 + 16 + d.len()].copy_from_slice(&d);
        std::fs::File::create(dir.join("i.bin"))
            .unwrap()
            .write_all(&bin)
            .unwrap();
        let cue = "FILE \"i.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n";
        std::fs::write(dir.join("i.cue"), cue).unwrap();
        DiscImage::open(&dir.join("i.cue")).unwrap()
    }

    #[test]
    fn root_is_found_through_the_volume_descriptor() {
        let mut d = fixture();
        let root = root_directory(&mut d).unwrap();
        assert_eq!(root.fad, 170);
        assert!(root.is_dir);
    }

    #[test]
    fn file_ids_are_record_positions_with_dot_and_dotdot_first() {
        let mut d = fixture();
        let root = root_directory(&mut d).unwrap();
        let entries = read_directory(&mut d, &root);
        assert_eq!(entries.len(), 3);
        assert!(entries[0].is_dir && entries[1].is_dir);
        assert_eq!(entries[2].fad, 180);
        assert_eq!(entries[2].size, 5000);
        assert!(!entries[2].is_dir);
    }

    #[test]
    fn file_info_is_fad_size_unit_gap_id_attributes() {
        let e = Entry {
            fad: 0x0102_0304,
            size: 0x0A0B_0C0D,
            unit_size: 1,
            gap_size: 2,
            is_dir: true,
        };
        let i = e.info(7);
        assert_eq!(&i[0..4], &[1, 2, 3, 4]);
        assert_eq!(&i[4..8], &[0x0A, 0x0B, 0x0C, 0x0D]);
        assert_eq!(&i[8..], &[1, 2, 7, 2]);
    }
}
