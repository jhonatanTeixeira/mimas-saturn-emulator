#!/usr/bin/env python3
"""
make_test_chd.py - Deterministic test CHD generator for Mimas Saturn emulator.

Generates 3 test fixtures using `chdman createcd`:
1. single_data_track.chd: 1 MODE1/2048 track, 32 sectors.
   - Sector 0 (LBA 0 / FAD 150): IP.BIN header ("SEGA SEGASATURN", company, date, region, gamename, firstprogaddr, etc.)
   - Sector 16 (LBA 16 / FAD 166): Minimal ISO-9660 PVD with Root Directory Record.
   - Sector 18 (LBA 18 / FAD 168): Directory sector with sample files ("0.BIN", "TEST.DAT").
   - Other sectors: Known pseudo-random LCG pattern.
2. data_plus_audio.chd: Track 1 MODE1/2048 (16 sectors), Track 2 AUDIO/2352 (16 sectors with sine wave).
3. mode2_form1.chd: 1 MODE2_RAW/2352 track (16 sectors) with CD-XA subheaders (fn/cn/sm/ci).

Along with each fixture, emits a `.expected.json` recording the track table, TOC, and metadata.
"""

import os
import sys
import struct
import json
import subprocess
import tempfile
import math

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(SCRIPT_DIR)
SATURN_CORE_FIXTURES = os.path.join(REPO_ROOT, "saturn-core", "tests", "fixtures")
E2E_FIXTURES = os.path.join(REPO_ROOT, "e2e-tests", "fixtures")

def lcg_bytes(seed: int, count: int) -> bytes:
    state = seed
    out = bytearray(count)
    for i in range(count):
        state = (state * 1103515245 + 12345) & 0x7FFFFFFF
        out[i] = (state >> 16) & 0xFF
    return bytes(out)

def create_ip_bin() -> bytes:
    sector = bytearray(2048)
    # 0x00: "SEGA SEGASATURN "
    sector[0x00:0x10] = b"SEGA SEGASATURN "
    # 0x10: "SEGA ENTERPRISES"
    sector[0x10:0x20] = b"SEGA ENTERPRISES"
    # 0x20: "GS-9000   "
    sector[0x20:0x2A] = b"GS-9000   "
    # 0x2A: "V1.000"
    sector[0x2A:0x30] = b"V1.000"
    # 0x30..0x37: Date "19950715" -> reformatted as "15/07/1995"
    sector[0x30:0x38] = b"19950715"
    # 0x38: "CD-1/1  "
    sector[0x38:0x40] = b"CD-1/1  "
    # 0x40: "JUE     " (Region flags)
    sector[0x40:0x4A] = b"JUE       "
    # 0x50: Peripheral "JT4             "
    sector[0x50:0x60] = b"JT4             "
    # 0x60: Game name (up to 112 bytes)
    gamename = b"MIMAS TEST DISC 1 - SINGLE DATA TRACK"
    sector[0x60:0x60+len(gamename)] = gamename
    # 0xE0: IP size (u32 BE)
    struct.pack_into(">I", sector, 0xE0, 0x00002000)
    # 0xE8: Master SH2 stack
    struct.pack_into(">I", sector, 0xE8, 0x06004000)
    # 0xEC: Slave SH2 stack
    struct.pack_into(">I", sector, 0xEC, 0x06002000)
    # 0xF0: First program address
    struct.pack_into(">I", sector, 0xF0, 0x06004000)
    # 0xF4: First program size
    struct.pack_into(">I", sector, 0xF4, 0x00010000)

    # Fill payload in rest of sector
    sector[0x100:] = lcg_bytes(0x12345678, 2048 - 0x100)
    return bytes(sector)

def create_iso_pvd(root_dir_lba: int, root_dir_size: int) -> bytes:
    sector = bytearray(2048)
    sector[0x00] = 0x01  # Primary Volume Descriptor
    sector[0x01:0x06] = b"CD001"
    sector[0x06] = 0x01  # Version
    # Volume ID (offset 40, length 32)
    sector[40:40+15] = b"MIMAS_TEST_DISC"
    # Volume space size (offset 80, 8 bytes: LE u32, BE u32)
    struct.pack_into("<I", sector, 80, 32)
    struct.pack_into(">I", sector, 84, 32)
    # Logical block size (offset 128, 4 bytes: LE u16, BE u16)
    struct.pack_into("<H", sector, 128, 2048)
    struct.pack_into(">H", sector, 130, 2048)
    
    # Root Directory Record at offset 0x9C (156)
    # Length: 34 bytes
    root_rec = bytearray(34)
    root_rec[0] = 34  # Length of Directory Record
    root_rec[1] = 0   # Extended Attribute Record Length
    # Location of extent (LBA): LE u32, BE u32 (8 bytes at offset 2)
    struct.pack_into("<I", root_rec, 2, root_dir_lba)
    struct.pack_into(">I", root_rec, 6, root_dir_lba)
    # Data length: LE u32, BE u32 (8 bytes at offset 10)
    struct.pack_into("<I", root_rec, 10, root_dir_size)
    struct.pack_into(">I", root_rec, 14, root_dir_size)
    # Recording date/time (7 bytes at offset 18)
    root_rec[18:25] = bytes([95, 7, 15, 12, 0, 0, 0])
    # File flags (offset 25): 0x02 = Directory
    root_rec[25] = 0x02
    # File unit size (26), Interleave gap size (27)
    root_rec[26] = 0
    root_rec[27] = 0
    # Volume sequence number (28, 4 bytes: LE u16, BE u16)
    struct.pack_into("<H", root_rec, 28, 1)
    struct.pack_into(">H", root_rec, 30, 1)
    # Length of file identifier (32)
    root_rec[32] = 1
    # File identifier (33): 0x00 for root
    root_rec[33] = 0x00

    sector[156:156+34] = root_rec
    return bytes(sector)

def create_directory_sector() -> bytes:
    sector = bytearray(2048)
    offset = 0

    # 1. Current directory record "." (length 34, name 0x00)
    dot_rec = bytearray(34)
    dot_rec[0] = 34
    struct.pack_into("<I", dot_rec, 2, 18)
    struct.pack_into(">I", dot_rec, 6, 18)
    struct.pack_into("<I", dot_rec, 10, 2048)
    struct.pack_into(">I", dot_rec, 14, 2048)
    dot_rec[25] = 0x02 # Directory
    dot_rec[32] = 1
    dot_rec[33] = 0x00
    sector[offset:offset+34] = dot_rec
    offset += 34

    # 2. Parent directory record ".." (length 34, name 0x01)
    dotdot_rec = bytearray(34)
    dotdot_rec[0] = 34
    struct.pack_into("<I", dotdot_rec, 2, 18)
    struct.pack_into(">I", dotdot_rec, 6, 18)
    struct.pack_into("<I", dotdot_rec, 10, 2048)
    struct.pack_into(">I", dotdot_rec, 14, 2048)
    dotdot_rec[25] = 0x02 # Directory
    dotdot_rec[32] = 1
    dotdot_rec[33] = 0x01
    sector[offset:offset+34] = dotdot_rec
    offset += 34

    # 3. File "0.BIN;1" (LBA 20, size 4096 bytes)
    name1 = b"0.BIN;1"
    reclen1 = 33 + len(name1)
    if reclen1 % 2 != 0:
        reclen1 += 1
    f1 = bytearray(reclen1)
    f1[0] = reclen1
    struct.pack_into("<I", f1, 2, 20)
    struct.pack_into(">I", f1, 6, 20)
    struct.pack_into("<I", f1, 10, 4096)
    struct.pack_into(">I", f1, 14, 4096)
    f1[25] = 0x00 # Normal file
    f1[32] = len(name1)
    f1[33:33+len(name1)] = name1
    sector[offset:offset+reclen1] = f1
    offset += reclen1

    # 4. File "TEST.DAT;1" (LBA 22, size 2048 bytes)
    name2 = b"TEST.DAT;1"
    reclen2 = 33 + len(name2)
    if reclen2 % 2 != 0:
        reclen2 += 1
    f2 = bytearray(reclen2)
    f2[0] = reclen2
    struct.pack_into("<I", f2, 2, 22)
    struct.pack_into(">I", f2, 6, 22)
    struct.pack_into("<I", f2, 10, 2048)
    struct.pack_into(">I", f2, 14, 2048)
    f2[25] = 0x00 # Normal file
    f2[32] = len(name2)
    f2[33:33+len(name2)] = name2
    sector[offset:offset+reclen2] = f2
    offset += reclen2

    return bytes(sector)

def make_fixture_1(target_dir: str):
    """Fixture 1: single_data_track.chd (32 sectors, MODE1/2048)"""
    bin_path = os.path.join(target_dir, "single_data_track.bin")
    cue_path = os.path.join(target_dir, "single_data_track.cue")
    chd_path = os.path.join(target_dir, "single_data_track.chd")
    json_path = os.path.join(target_dir, "single_data_track.expected.json")

    num_sectors = 32
    with open(bin_path, "wb") as f:
        for lba in range(num_sectors):
            if lba == 0:
                f.write(create_ip_bin())
            elif lba == 16:
                f.write(create_iso_pvd(root_dir_lba=18, root_dir_size=2048))
            elif lba == 18:
                f.write(create_directory_sector())
            else:
                f.write(lcg_bytes(0x1000 + lba * 100, 2048))

    with open(cue_path, "w") as f:
        f.write('FILE "single_data_track.bin" BINARY\n')
        f.write('  TRACK 01 MODE1/2048\n')
        f.write('    INDEX 01 00:00:00\n')

    # Run chdman createcd
    cmd = ["chdman", "createcd", "-i", cue_path, "-o", chd_path, "-f"]
    subprocess.run(cmd, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)

    # Clean up intermediate .bin and .cue
    os.remove(bin_path)
    os.remove(cue_path)

    # Derive expected TOC
    toc = [0xFFFFFFFF] * 102
    toc[0] = (0x41 << 24) | 150 # 0x41000096
    toc[99] = (0x41 << 24) | 0x010000 # 0x41010000 (First track 1)
    toc[100] = (0x41 << 24) | (1 << 16) # 0x41010000 (Last track 1)
    lead_out_fad = 150 + num_sectors # 182 = 0x0000B6
    toc[101] = (0x41 << 24) | lead_out_fad # 0x410000B6

    expected = {
        "num_tracks": 1,
        "tracks": [
            {
                "track_num": 1,
                "ctl_addr": 0x41,
                "sector_size": 2048,
                "fad_start": 150,
                "fad_end": 150 + num_sectors - 1,
                "frames": num_sectors
            }
        ],
        "lead_out_fad": lead_out_fad,
        "toc": [f"0x{val:08X}" for val in toc],
        "ip_bin": {
            "system": "SEGA SEGASATURN",
            "company": "SEGA ENTERPRISES",
            "itemnum": "GS-9000",
            "version": "V1.000",
            "date": "15/07/1995",
            "region": "JUE",
            "gamename": "MIMAS TEST DISC 1 - SINGLE DATA TRACK",
            "firstprogaddr": "0x06004000",
            "firstprogsize": "0x00010000"
        }
    }
    with open(json_path, "w") as f:
        json.dump(expected, f, indent=2)

def make_fixture_2(target_dir: str):
    """Fixture 2: data_plus_audio.chd (Track 1: MODE1/2048 (16 secs), Track 2: AUDIO/2352 (16 secs))"""
    t1_bin = os.path.join(target_dir, "data_plus_audio_t1.bin")
    t2_bin = os.path.join(target_dir, "data_plus_audio_t2.bin")
    cue_path = os.path.join(target_dir, "data_plus_audio.cue")
    chd_path = os.path.join(target_dir, "data_plus_audio.chd")
    json_path = os.path.join(target_dir, "data_plus_audio.expected.json")

    t1_sectors = 16
    t2_sectors = 16
    with open(t1_bin, "wb") as f:
        for lba in range(t1_sectors):
            if lba == 0:
                f.write(create_ip_bin())
            else:
                f.write(lcg_bytes(0x2000 + lba * 50, 2048))

    with open(t2_bin, "wb") as f:
        for s in range(t2_sectors):
            audio_sec = bytearray(2352)
            num_samples = 2352 // 4 # 588 stereo samples per sector
            for i in range(num_samples):
                sample_idx = s * num_samples + i
                val = int(32767.0 * math.sin(2.0 * math.pi * 440.0 * sample_idx / 44100.0))
                val = max(-32768, min(32767, val))
                struct.pack_into("<hh", audio_sec, i * 4, val, val)
            f.write(audio_sec)

    with open(cue_path, "w") as f:
        f.write('FILE "data_plus_audio_t1.bin" BINARY\n')
        f.write('  TRACK 01 MODE1/2048\n')
        f.write('    INDEX 01 00:00:00\n')
        f.write('FILE "data_plus_audio_t2.bin" BINARY\n')
        f.write('  TRACK 02 AUDIO\n')
        f.write('    INDEX 01 00:00:00\n')

    cmd = ["chdman", "createcd", "-i", cue_path, "-o", chd_path, "-f"]
    subprocess.run(cmd, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)

    os.remove(t1_bin)
    os.remove(t2_bin)
    os.remove(cue_path)

    toc = [0xFFFFFFFF] * 102
    toc[0] = (0x41 << 24) | 150 # Track 1: FAD 150
    # Track 2: FAD 150 + 16 + 150(if pregap? in cue without pregap, FAD = 150 + 16 = 166)
    t2_fad_start = 150 + t1_sectors # 166
    toc[1] = (0x01 << 24) | t2_fad_start
    toc[99] = (0x41 << 24) | 0x010000 # First track 1
    toc[100] = (0x41 << 24) | (2 << 16) # Last track 2
    lead_out_fad = t2_fad_start + t2_sectors # 182
    toc[101] = (0x01 << 24) | lead_out_fad

    expected = {
        "num_tracks": 2,
        "tracks": [
            {
                "track_num": 1,
                "ctl_addr": 0x41,
                "sector_size": 2048,
                "fad_start": 150,
                "fad_end": 150 + t1_sectors - 1,
                "frames": t1_sectors
            },
            {
                "track_num": 2,
                "ctl_addr": 0x01,
                "sector_size": 2352,
                "fad_start": t2_fad_start,
                "fad_end": t2_fad_start + t2_sectors - 1,
                "frames": t2_sectors
            }
        ],
        "lead_out_fad": lead_out_fad,
        "toc": [f"0x{val:08X}" for val in toc]
    }
    with open(json_path, "w") as f:
        json.dump(expected, f, indent=2)

def make_fixture_3(target_dir: str):
    """Fixture 3: mode2_form1.chd (1 MODE2_RAW/2352 track with real CD-XA subheaders)"""
    bin_path = os.path.join(target_dir, "mode2_form1.bin")
    cue_path = os.path.join(target_dir, "mode2_form1.cue")
    chd_path = os.path.join(target_dir, "mode2_form1.chd")
    json_path = os.path.join(target_dir, "mode2_form1.expected.json")

    num_sectors = 16
    sync_hdr = bytes([0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00])

    with open(bin_path, "wb") as f:
        for lba in range(num_sectors):
            sec = bytearray(2352)
            # Sync header (12 bytes)
            sec[0:12] = sync_hdr
            # Header: MIN, SEC, FRAME, MODE (Mode 2 = 0x02)
            fad = 150 + lba
            frame = fad % 75
            sec_idx = (fad // 75) % 60
            min_idx = fad // (75 * 60)
            sec[12] = min_idx
            sec[13] = sec_idx
            sec[14] = frame
            sec[15] = 0x02 # Mode 2

            # Subheader at 0x10..0x17: file num (1), channel num (2), submode (0x08 for Form 1 data or 0x28 for Form 2), coding info (0)
            # Form 1: submode bit 5 is 0 (e.g. 0x08)
            # Form 2: submode bit 5 is 1 (e.g. 0x28)
            fn = 1
            cn = 2
            sm = 0x28 if (lba % 2 == 1) else 0x08
            ci = 0
            subhdr = bytes([fn, cn, sm, ci, fn, cn, sm, ci])
            sec[16:24] = subhdr

            # Payload (2048 bytes for Form 1 at 0x18..0x818, or 2324 bytes for Form 2 at 0x18..0x92C)
            payload = lcg_bytes(0x3000 + lba * 77, 2352 - 24)
            sec[24:] = payload
            f.write(sec)

    with open(cue_path, "w") as f:
        f.write('FILE "mode2_form1.bin" BINARY\n')
        f.write('  TRACK 01 MODE2/2352\n')
        f.write('    INDEX 01 00:00:00\n')

    cmd = ["chdman", "createcd", "-i", cue_path, "-o", chd_path, "-f"]
    subprocess.run(cmd, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)

    os.remove(bin_path)
    os.remove(cue_path)

    toc = [0xFFFFFFFF] * 102
    toc[0] = (0x41 << 24) | 150
    toc[99] = (0x41 << 24) | 0x010000
    toc[100] = (0x41 << 24) | (1 << 16)
    lead_out_fad = 150 + num_sectors
    toc[101] = (0x41 << 24) | lead_out_fad

    expected = {
        "num_tracks": 1,
        "tracks": [
            {
                "track_num": 1,
                "ctl_addr": 0x41,
                "sector_size": 2352,
                "fad_start": 150,
                "fad_end": 150 + num_sectors - 1,
                "frames": num_sectors
            }
        ],
        "lead_out_fad": lead_out_fad,
        "toc": [f"0x{val:08X}" for val in toc]
    }
    with open(json_path, "w") as f:
        json.dump(expected, f, indent=2)

def main():
    os.makedirs(SATURN_CORE_FIXTURES, exist_ok=True)
    os.makedirs(E2E_FIXTURES, exist_ok=True)

    print("Generating CHD fixtures in saturn-core/tests/fixtures/...")
    make_fixture_1(SATURN_CORE_FIXTURES)
    make_fixture_2(SATURN_CORE_FIXTURES)
    make_fixture_3(SATURN_CORE_FIXTURES)

    print("Copying/generating CHD fixtures in e2e-tests/fixtures/...")
    make_fixture_1(E2E_FIXTURES)
    make_fixture_2(E2E_FIXTURES)
    make_fixture_3(E2E_FIXTURES)

    print("Done! Fixtures generated successfully.")

if __name__ == "__main__":
    main()
