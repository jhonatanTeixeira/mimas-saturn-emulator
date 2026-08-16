use crate::cdrom::Cdrom;
use crate::scu::Scu;
use std::sync::Arc;

// HIRQ bit flags (16-bit)
pub const HIRQ_CMOK: u16 = 0x0001; // Command execution complete
pub const HIRQ_DRDY: u16 = 0x0002; // Data ready
pub const HIRQ_CSCT: u16 = 0x0004; // Sector stored
pub const HIRQ_BFUL: u16 = 0x0008; // Buffer full
pub const HIRQ_PEND: u16 = 0x0010; // Play end
pub const HIRQ_DCHG: u16 = 0x0020; // Disc change
pub const HIRQ_ESEL: u16 = 0x0040; // Selector execution complete
pub const HIRQ_EHST: u16 = 0x0080; // Host data transfer complete / error
pub const HIRQ_ECPY: u16 = 0x0100; // Sector copy/move complete
pub const HIRQ_EFLS: u16 = 0x0200; // File system execution complete
pub const HIRQ_SCDQ: u16 = 0x0400; // Subcode Q update complete
pub const HIRQ_MPED: u16 = 0x0800; // MPEG execution complete
pub const HIRQ_MPCM: u16 = 0x1000; // MPEG command complete
pub const HIRQ_MPST: u16 = 0x2000; // MPEG status update

// Status byte values
pub const CDB_STAT_BUSY: u8 = 0x00;
pub const CDB_STAT_PAUSE: u8 = 0x01;
pub const CDB_STAT_STANDBY: u8 = 0x02;
pub const CDB_STAT_PLAY: u8 = 0x03;
pub const CDB_STAT_SEEK: u8 = 0x04;
pub const CDB_STAT_SCAN: u8 = 0x05;
pub const CDB_STAT_OPEN: u8 = 0x06;
pub const CDB_STAT_NODISC: u8 = 0x07;
pub const CDB_STAT_RETRY: u8 = 0x08;
pub const CDB_STAT_ERROR: u8 = 0x09;
pub const CDB_STAT_FATAL: u8 = 0x0A;
pub const CDB_STAT_PERI: u8 = 0x20;
pub const CDB_STAT_TRNS: u8 = 0x40;
pub const CDB_STAT_WAIT: u8 = 0x80;
pub const CDB_STAT_REJECT: u8 = 0xFF;

pub const MAX_BLOCKS: usize = 200;
pub const MAX_PARTITIONS: usize = 24;
pub const MAX_FILTERS: usize = 24;

#[derive(Debug, Clone)]
pub struct SectorBlock {
    pub data: [u8; 2352],
    pub size: u32,
    pub fad: u32,
    pub fn_: u8,
    pub cn: u8,
    pub sm: u8,
    pub ci: u8,
    pub used: bool,
}

impl Default for SectorBlock {
    fn default() -> Self {
        Self {
            data: [0; 2352],
            size: 0,
            fad: 0,
            fn_: 0,
            cn: 0,
            sm: 0,
            ci: 0,
            used: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Partition {
    pub blocks: Vec<u8>,
    pub size: u32,
    pub numblocks: u32,
    pub getsectsize: u32,
    pub putsectsize: u32,
}

impl Default for Partition {
    fn default() -> Self {
        Self {
            blocks: Vec::new(),
            size: 0,
            numblocks: 0,
            getsectsize: 2048,
            putsectsize: 2048,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Filter {
    pub fad_start: u32,
    pub fad_range: u32,
    pub mode: u8,
    pub chan: u8,
    pub smmask: u8,
    pub smval: u8,
    pub cimask: u8,
    pub cival: u8,
    pub condtrue_partition: u8,
    pub condfalse_partition: u8,
}

impl Default for Filter {
    fn default() -> Self {
        Self {
            fad_start: 0,
            fad_range: 0,
            mode: 0,
            chan: 0,
            smmask: 0,
            smval: 0,
            cimask: 0,
            cival: 0,
            condtrue_partition: 0xFF,
            condfalse_partition: 0xFF,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct FileInfoRecord {
    pub lba: u32,
    pub size: u32,
    pub interleavegapsize: u8,
    pub fileunitsize: u8,
    pub flags: u8,
    pub fid: u32,
}

#[derive(Debug, Clone, Default)]
pub struct CdIpBin {
    pub system: String,
    pub company: String,
    pub itemnum: String,
    pub version: String,
    pub date: String,
    pub cdinfo: String,
    pub region: String,
    pub peripheral: String,
    pub gamename: String,
    pub ipsize: u32,
    pub msh2stack: u32,
    pub ssh2stack: u32,
    pub firstprogaddr: u32,
    pub firstprogsize: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataTransferType {
    Idle,
    GetSector,
    GetDeleteSector,
    PutSector,
}

impl Default for DataTransferType {
    fn default() -> Self {
        Self::Idle
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfoTransferType {
    Idle,
    Toc,
    SingleFile,
    AllFiles,
    SubQ,
    SubRw,
}

impl Default for InfoTransferType {
    fn default() -> Self {
        Self::Idle
    }
}

pub struct Cs2 {
    pub hirq: u16,
    pub hirqmask: u16,
    pub cr1: u16,
    pub cr2: u16,
    pub cr3: u16,
    pub cr4: u16,
    pub mpegrgb: u16,

    // Drive status
    pub status: u8,
    pub fad: u32,
    pub options: u8,
    pub repcnt: u8,
    pub ctrladdr: u8,
    pub track: u8,
    pub index: u8,

    // Command scheduling
    pub command_pending: bool,
    pub vblank_pending: bool,

    // Timers for free-running state machine
    pub status_cycles_us: i32,
    pub periodic_cycles_us: i32,
    pub isdiskchanged: bool,
    pub isbufferfull: bool,
    pub isonesectorstored: bool,
    pub isaudio: bool,
    pub satauth: u8,
    pub mpgauth: u8,

    // Sector buffers, partitions, filters
    pub block: Box<[SectorBlock; MAX_BLOCKS]>,
    pub blockfreespace: u32,
    pub partition: [Partition; MAX_PARTITIONS],
    pub filter: [Filter; MAX_FILTERS],
    pub outconcddev: u8,
    pub outconmpeg: u8,
    pub outconhost: u8,

    // Workblock for sector staging (2448 bytes)
    pub workblock: Box<[u8; 2448]>,

    // Data transfer FIFO state
    pub datatranstype: DataTransferType,
    pub datatranspartition: u8,
    pub datatransoffset: u32,
    pub datatranssectoffset: u32,
    pub datatranssectsize: u32,
    pub datatransblockindex: usize,
    pub datatranstargetbytes: u32,
    pub datatransbytesread: u32,
    pub put_block_idx: Option<u8>,
    pub put_offset: usize,

    // Info port transfer state
    pub infotranstype: InfoTransferType,
    pub transfercount: u32,
    pub cdwnum: u32,
    pub trans_buffer: Vec<u8>,

    // Playback state
    pub play_start_fad: u32,
    pub play_end_fad: u32,
    pub play_mode: u8,
    pub play_type: u8,
    pub scan_direction: i8,
    pub subcode_q: [u8; 10],
    pub subcode_rw: [u8; 24],

    // Filesystem state
    pub curdirsect: u32,
    pub curdirsize: u32,
    pub curdirfidoffset: u32,
    pub fileinfo: Vec<FileInfoRecord>,
    pub cdip: Option<CdIpBin>,

    // MPEG state
    pub mpegaudiostatus: u8,
    pub mpegvideostatus: u8,
    pub pictureinfo: u8,
    pub vcounter: u16,
    pub actionstatus: u8,
    pub mpegintmask: u32,

    // Disc backend & SCU
    pub disc: Option<Cdrom>,
    pub scu: Option<Arc<Scu>>,
}

impl Cs2 {
    pub fn new() -> Self {
        let mut cs2 = Self {
            hirq: 0xFFFF,
            hirqmask: 0x0000,
            cr1: 0x0043, // ASCII '\0C'
            cr2: 0x4442, // ASCII 'DB'
            cr3: 0x4C4F, // ASCII 'LO'
            cr4: 0x434B, // ASCII 'CK'
            mpegrgb: 0,

            status: CDB_STAT_PAUSE,
            fad: 150,
            options: 0,
            repcnt: 0,
            ctrladdr: 0x41,
            track: 1,
            index: 1,

            command_pending: false,
            vblank_pending: false,

            status_cycles_us: 0,
            periodic_cycles_us: 0,
            isdiskchanged: false,
            isbufferfull: false,
            isonesectorstored: false,
            isaudio: false,
            satauth: 0,
            mpgauth: 0,

            block: {
                let vec: Vec<SectorBlock> =
                    (0..MAX_BLOCKS).map(|_| SectorBlock::default()).collect();
                vec.into_boxed_slice().try_into().unwrap()
            },
            blockfreespace: MAX_BLOCKS as u32,
            partition: std::array::from_fn(|_| Partition::default()),
            filter: std::array::from_fn(|_| Filter::default()),
            outconcddev: 0,
            outconmpeg: 0,
            outconhost: 0,

            workblock: vec![0u8; 2448].into_boxed_slice().try_into().unwrap(),

            datatranstype: DataTransferType::Idle,
            datatranspartition: 0,
            datatransoffset: 0,
            datatranssectoffset: 0,
            datatranssectsize: 2048,
            datatransblockindex: 0,
            datatranstargetbytes: 0,
            datatransbytesread: 0,
            put_block_idx: None,
            put_offset: 0,

            infotranstype: InfoTransferType::Idle,
            transfercount: 0,
            cdwnum: 0,
            trans_buffer: Vec::new(),

            play_start_fad: 150,
            play_end_fad: 150,
            play_mode: 0,
            play_type: 0,
            scan_direction: 1,
            subcode_q: [0; 10],
            subcode_rw: [0; 24],

            curdirsect: 0,
            curdirsize: 0,
            curdirfidoffset: 0,
            fileinfo: Vec::new(),
            cdip: None,

            mpegaudiostatus: 0,
            mpegvideostatus: 0,
            pictureinfo: 0,
            vcounter: 0,
            actionstatus: 0,
            mpegintmask: 0,

            disc: None,
            scu: None,
        };
        cs2.reset_system();
        cs2
    }

    pub fn set_scu(&mut self, scu: Arc<Scu>) {
        self.scu = Some(scu);
    }

    pub fn reset_system(&mut self) {
        self.hirq = 0xFFFF;
        self.hirqmask = 0x0000;
        self.cr1 = 0x0043;
        self.cr2 = 0x4442;
        self.cr3 = 0x4C4F;
        self.cr4 = 0x434B;
        self.mpegrgb = 0;

        self.status = if self.disc.is_some() {
            CDB_STAT_PAUSE
        } else {
            CDB_STAT_NODISC
        };
        self.fad = 150;
        self.options = 0;
        self.repcnt = 0;
        self.ctrladdr = 0x41;
        self.track = 1;
        self.index = 1;

        self.command_pending = false;
        self.vblank_pending = false;
        self.status_cycles_us = 0;
        self.periodic_cycles_us = 0;
        self.isdiskchanged = false;
        self.isbufferfull = false;
        self.isonesectorstored = false;
        self.isaudio = false;
        self.satauth = 0;
        self.mpgauth = 0;

        self.free_all_blocks();
        for p in &mut self.partition {
            *p = Partition::default();
        }
        for f in &mut self.filter {
            *f = Filter::default();
        }
        self.outconcddev = 0;
        self.outconmpeg = 0;
        self.outconhost = 0;

        self.datatranstype = DataTransferType::Idle;
        self.infotranstype = InfoTransferType::Idle;
        self.transfercount = 0;
        self.cdwnum = 0;
        self.trans_buffer.clear();
    }

    pub fn free_all_blocks(&mut self) {
        for b in self.block.iter_mut() {
            b.used = false;
            b.size = 0;
        }
        self.blockfreespace = MAX_BLOCKS as u32;
        for p in &mut self.partition {
            p.blocks.clear();
            p.size = 0;
            p.numblocks = 0;
        }
    }

    pub fn allocate_block(&mut self) -> Option<u8> {
        if self.blockfreespace == 0 {
            self.isbufferfull = true;
            self.raise_irq(HIRQ_BFUL);
            return None;
        }
        for (i, b) in self.block.iter_mut().enumerate() {
            if !b.used {
                b.used = true;
                self.blockfreespace = self.blockfreespace.saturating_sub(1);
                return Some(i as u8);
            }
        }
        None
    }

    pub fn free_block(&mut self, block_idx: u8) {
        let idx = block_idx as usize;
        if idx < MAX_BLOCKS && self.block[idx].used {
            self.block[idx].used = false;
            self.block[idx].size = 0;
            self.blockfreespace = (self.blockfreespace + 1).min(MAX_BLOCKS as u32);
        }
    }

    pub fn raise_irq(&mut self, bits: u16) {
        self.hirq |= bits;
        self.check_external_irq();
    }

    pub fn check_external_irq(&self) {
        if (self.hirq & self.hirqmask) != 0 {
            if let Some(ref scu) = self.scu {
                scu.external(0);
            }
        }
    }

    pub fn load_disc(&mut self, path: &str) -> Result<(), String> {
        let cdrom = Cdrom::open_chd(path)?;
        self.disc = Some(cdrom);
        self.isdiskchanged = true;
        self.status = CDB_STAT_PAUSE;
        self.fad = 150;
        self.track = 1;
        self.index = 1;
        self.setup_default_play_stats();
        self.raise_irq(HIRQ_DCHG);
        Ok(())
    }

    pub fn setup_default_play_stats(&mut self) {
        if let Some(ref disc) = self.disc {
            if let Some(t) = disc.tracks.first() {
                self.ctrladdr = t.ctl_addr;
                self.track = t.track_num;
                self.fad = t.fad_start;
            }
        }
    }

    pub fn do_cd_report(&mut self) {
        self.cr1 = ((self.status as u16) << 8) | (self.options as u16);
        self.cr2 = ((self.repcnt as u16) << 8) | (self.ctrladdr as u16);
        self.cr3 = ((self.track as u16) << 8) | (self.index as u16);
        self.cr4 = (self.fad >> 8) as u16;
    }

    pub fn do_cd_report_with_fad(&mut self, fad: u32) {
        self.cr1 = ((self.status as u16) << 8) | (self.options as u16);
        self.cr2 = ((self.repcnt as u16) << 8) | (self.ctrladdr as u16);
        self.cr3 = ((self.track as u16) << 8) | (self.index as u16);
        self.cr4 = (fad >> 8) as u16;
    }

    pub fn do_mpeg_report(&mut self) {
        self.cr1 = ((self.status as u16) << 8) | (self.actionstatus as u16);
        self.cr2 = self.vcounter;
        self.cr3 = ((self.pictureinfo as u16) << 8) | (self.mpegaudiostatus as u16);
        self.cr4 = self.mpegvideostatus as u16;
    }

    pub fn has_work(&self) -> bool {
        self.command_pending || self.vblank_pending
    }

    pub fn exec_vblank(&mut self) {
        self.exec(16667);
    }

    pub fn exec(&mut self, elapsed_us: i32) {
        if self.command_pending {
            self.execute_command();
            self.command_pending = false;
        }

        if elapsed_us <= 0 {
            return;
        }

        // 2. 3 Hz backend status poll (333,333 µs). Real hardware carries
        // the remainder forward (`Cs2Exec`, `cs2.c:1083`:
        // `_statuscycles -= _statustiming`) rather than resetting to 0 --
        // matched here to avoid the same systematic drift already fixed for
        // `Sh2`'s own line-cycle accounting (`sh2.rs`'s `pending_line_cycles`
        // doc comment).
        self.status_cycles_us += elapsed_us;
        if self.status_cycles_us >= 333_333 {
            self.status_cycles_us -= 333_333;
            if self.disc.is_none() && self.status != CDB_STAT_OPEN {
                self.status = CDB_STAT_NODISC;
            }
        }

        // 3. Periodic report & sector engine.
        // Periods: 60Hz = 16667 µs (pause/idle), 75Hz = 13333 µs (1x), 150Hz = 6667 µs (2x).
        // Real hardware (`Cs2Exec`, `cs2.c:1109-1111`) only fires this once
        // per call and carries the remainder (`_periodiccycles -=
        // _periodictiming`) -- but it's called from the main loop's own
        // fine-grained instruction-batch tick, far more often than Core 7's
        // `exec_vblank()`, which delivers a whole real V-Blank period's
        // worth of elapsed time (16667 µs) in one call. A single check at
        // that coarser granularity would silently drop 2x-speed sector
        // reads to ~40% of their real rate (16667 µs / 6667 µs ≈ 2.5
        // periods owed per call, only 1 serviced). Looping here reproduces
        // the reference's own carry-the-remainder arithmetic exactly, just
        // possibly more than once per call -- the observable throughput
        // matches what fine-grained calls would have produced, which is
        // what actually matters, not the call-count shape of the C code.
        self.periodic_cycles_us += elapsed_us;
        loop {
            let period = if self.status == CDB_STAT_PLAY {
                if self.isaudio {
                    13_333 // 75Hz (1x audio)
                } else {
                    6_667 // 150Hz (2x data)
                }
            } else {
                16_667 // 60Hz idle/pause
            };
            if self.periodic_cycles_us < period {
                break;
            }
            self.periodic_cycles_us -= period;
            self.step_periodic();
        }
    }

    fn step_periodic(&mut self) {
        // Periodic report
        self.status |= CDB_STAT_PERI;
        self.do_cd_report();
        self.raise_irq(HIRQ_SCDQ);

        // Update Subcode Q
        let (m, s, f) = Cdrom::fad_to_msf(self.fad);
        let track_start_fad = self
            .disc
            .as_ref()
            .and_then(|d| d.track_to_fad(self.track))
            .unwrap_or(150);
        let rel_fad = self.fad.saturating_sub(track_start_fad);
        let (rm, rs, rf) = Cdrom::fad_to_msf(rel_fad);

        self.subcode_q[0] = self.ctrladdr;
        self.subcode_q[1] = Self::to_bcd(self.track);
        self.subcode_q[2] = Self::to_bcd(self.index);
        self.subcode_q[3] = Self::to_bcd(rm);
        self.subcode_q[4] = Self::to_bcd(rs);
        self.subcode_q[5] = Self::to_bcd(rf);
        self.subcode_q[6] = 0;
        self.subcode_q[7] = Self::to_bcd(m);
        self.subcode_q[8] = Self::to_bcd(s);
        self.subcode_q[9] = Self::to_bcd(f);

        let base_stat = self.status & 0x0F;
        if base_stat == CDB_STAT_PLAY {
            self.step_playback();
        } else if base_stat == CDB_STAT_SEEK {
            if self.fad >= self.play_start_fad {
                self.status = (self.status & !0x0F) | CDB_STAT_PAUSE;
            } else {
                self.fad += 1;
            }
        }
    }

    fn step_playback(&mut self) {
        if self.disc.is_none() {
            self.status = CDB_STAT_NODISC;
            return;
        }

        let disc = self.disc.as_mut().unwrap();
        if let Some(t) = disc.fad_to_track(self.fad) {
            self.track = t;
            if let Some(track_obj) = disc.tracks.iter().find(|tr| tr.track_num == t) {
                self.ctrladdr = track_obj.ctl_addr;
                self.isaudio = track_obj.is_audio;
            }
        }

        // Read sector
        let mut raw = [0u8; 2448];
        if disc.read_sector_fad(self.fad, &mut raw).is_ok() {
            self.workblock.copy_from_slice(&raw);

            if !self.isaudio {
                // Data sector: filter and store
                self.process_filtered_sector();
                self.isonesectorstored = true;
                self.raise_irq(HIRQ_CSCT);
            }
        }

        // Advance FAD
        self.fad += 1;

        if self.fad >= self.play_end_fad {
            if self.repcnt > 0 && self.repcnt != 0xF {
                self.repcnt -= 1;
                self.fad = self.play_start_fad;
            } else if self.repcnt == 0xF {
                // Infinite repeat
                self.fad = self.play_start_fad;
            } else {
                // End of play
                self.status = CDB_STAT_PAUSE;
                self.raise_irq(HIRQ_PEND);
            }
        }
    }

    fn process_filtered_sector(&mut self) {
        let filter_idx = (self.outconcddev & 0x1F) as usize;
        if filter_idx >= MAX_FILTERS {
            return;
        }

        let dest_part_idx = self.filter[filter_idx].condtrue_partition as usize;
        if dest_part_idx >= MAX_PARTITIONS {
            return;
        }

        if let Some(blk_idx) = self.allocate_block() {
            let blk = &mut self.block[blk_idx as usize];
            blk.data.copy_from_slice(&self.workblock[16..2368]);
            blk.size = 2048;
            blk.fad = self.fad;
            blk.fn_ = self.workblock[16];
            blk.cn = self.workblock[17];
            blk.sm = self.workblock[18];
            blk.ci = self.workblock[19];

            let part = &mut self.partition[dest_part_idx];
            part.blocks.push(blk_idx);
            part.numblocks = part.blocks.len() as u32;
            part.size += blk.size;
        }
    }

    pub fn to_bcd(val: u8) -> u8 {
        ((val / 10) << 4) | (val % 10)
    }

    pub fn from_bcd(val: u8) -> u8 {
        (val >> 4) * 10 + (val & 0x0F)
    }

    // Command Dispatcher
    pub fn execute_command(&mut self) {
        // Real hardware clears CMOK the instant a new command actually
        // starts executing (`Cs2Execute`, `cs2.c:1289`) -- autonomously, not
        // something the host driver has to do. Every individual command
        // handler below re-raises it on completion via `raise_irq`.
        self.hirq &= !HIRQ_CMOK;
        let cmd = (self.cr1 >> 8) as u8;
        match cmd {
            // Phase 2 Commands
            0x00 => self.cmd_get_cd_status(),
            0x01 => self.cmd_get_hw_info(),
            0x02 => self.cmd_get_toc(),
            0x03 => self.cmd_get_session_info(),
            0x04 => self.cmd_init_cd_system(),
            0x05 => self.cmd_open_tray(),
            0x06 => self.cmd_end_data_transfer(),
            0xE0 => self.cmd_auth_device(),
            0xE1 => self.cmd_is_auth(),

            // Phase 4 Commands: Partition / Filter / Buffer
            0x30 => self.cmd_set_filter_range(),
            0x31 => self.cmd_get_filter_subh_cond(),
            0x32 => self.cmd_set_filter_subh_cond(),
            0x40 => self.cmd_set_filter_mode(),
            0x41 => self.cmd_set_filter_conn(),
            0x42 => self.cmd_get_filter_conn(),
            0x43 => self.cmd_reset_selector(),
            0x44 => self.cmd_set_filter_subh_cond(),
            0x45 => self.cmd_get_filter_subh_cond(),
            0x46 => self.cmd_set_filter_subh_cond(),
            0x47 => self.cmd_get_filter_subh_cond(),
            0x48 => self.cmd_reset_selector(),
            0x50 => self.cmd_get_buffer_size(),
            0x51 => self.cmd_get_sector_number(),
            0x52 => self.cmd_calculate_actual_size(),
            0x53 => self.cmd_get_actual_size(),
            0x54 => self.cmd_get_sector_info(),
            0x60 => self.cmd_set_sector_length(),
            0x61 => self.cmd_get_sector_data(),
            0x62 => self.cmd_delete_sector_data(),
            0x63 => self.cmd_get_then_delete_sector_data(),
            0x64 => self.cmd_put_sector_data(),
            0x65 => self.cmd_copy_sector_data(),
            0x66 => self.cmd_move_sector_data(),
            0x67 => self.cmd_get_copy_error(),

            // Phase 5 Commands: Playback / Seek / Scan / Subcode
            0x10 => self.cmd_play_disc(),
            0x11 => self.cmd_seek_disc(),
            0x12 => self.cmd_scan_disc(),
            0x20 => self.cmd_get_subcode_q_rw(),

            // Phase 6 Commands: Filesystem
            0x70 => self.cmd_change_directory(),
            0x71 => self.cmd_read_directory(),
            0x72 => self.cmd_get_filesystem_scope(),
            0x73 => self.cmd_get_file_info(),
            0x74 => self.cmd_read_file(),
            0x75 => self.cmd_abort_file(),

            // Phase 7 Commands: MPEG & Search
            0x55 => self.cmd_exec_fad_search(),
            0x56 => self.cmd_get_fad_search_results(),
            0x90 => self.cmd_mpeg_get_status(),
            0x91 => self.cmd_mpeg_get_interrupt(),
            0x92 => self.cmd_mpeg_set_interrupt_mask(),
            0x93 => self.cmd_mpeg_init(),
            0x94 => self.cmd_mpeg_set_mode(),
            0x95 => self.cmd_mpeg_play(),
            0x96 => self.cmd_mpeg_set_decoding_method(),
            0x9A => self.cmd_mpeg_set_connection(),
            0x9B => self.cmd_mpeg_get_connection(),
            0x9D => self.cmd_mpeg_set_stream(),
            0x9E => self.cmd_mpeg_get_stream(),
            0xA0..=0xA4 => self.cmd_mpeg_stubs(),
            0xAF => self.cmd_mpeg_set_lsi(),
            0xE2 => self.cmd_get_mpeg_rom(),

            // Undispatched opcodes hang per §10 [QUIRK 6]
            _ => {
                eprintln!("[CS2] Unimplemented opcode {:#04X} requested", cmd);
            }
        }
    }

    // ==========================================
    // Phase 2 Command Handlers
    // ==========================================
    fn cmd_get_cd_status(&mut self) {
        self.do_cd_report();
        self.raise_irq(HIRQ_CMOK);
    }

    fn cmd_get_hw_info(&mut self) {
        let base_stat = self.status & 0x0F;
        if base_stat != CDB_STAT_OPEN && base_stat != CDB_STAT_NODISC {
            self.isdiskchanged = false;
        }

        self.cr1 = (self.status as u16) << 8;
        self.cr2 = 0x0201; // CD-ROM driver version / flags (mpeg card exists)
        self.cr3 = if self.mpgauth != 0 { 0x0001 } else { 0x0000 };
        self.cr4 = 0x0400; // 4x speed
        self.raise_irq(HIRQ_CMOK);
    }

    fn cmd_get_toc(&mut self) {
        self.infotranstype = InfoTransferType::Toc;
        self.transfercount = 0;
        self.cdwnum = 0x66; // 102 words

        self.trans_buffer.clear();
        let toc = if let Some(ref d) = self.disc {
            d.toc
        } else {
            [0xFFFFFFFF; 102]
        };

        for val in &toc {
            self.trans_buffer.extend_from_slice(&val.to_be_bytes());
        }

        self.do_cd_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_DRDY);
    }

    fn cmd_get_session_info(&mut self) {
        let session_num = (self.cr1 & 0xFF) as u8;
        let total_sessions = self.disc.as_ref().map(|d| d.total_sessions).unwrap_or(1);

        if session_num == 0 {
            self.cr1 = ((self.status as u16) << 8) | (total_sessions as u16);
            self.cr2 = 0;
            self.cr3 = 0;
            self.cr4 = 0;
        } else {
            self.cr1 = (self.status as u16) << 8;
            self.cr2 = 0;
            self.cr3 = 0;
            self.cr4 = 150; // Session 1 start FAD
        }
        self.raise_irq(HIRQ_CMOK);
    }

    fn cmd_init_cd_system(&mut self) {
        let init_flag = (self.cr1 & 0xFF) as u8;
        let standby = ((self.cr2 >> 8) & 0xFF) as u8;
        self.repcnt = 0;

        if init_flag & 1 != 0 {
            self.free_all_blocks();
        }

        if standby != 0 {
            self.status = CDB_STAT_STANDBY;
        } else if self.disc.is_some() {
            self.status = CDB_STAT_PAUSE;
        }

        self.do_cd_report();

        let irq_bits = if self.isdiskchanged {
            self.isdiskchanged = false;
            HIRQ_CMOK | HIRQ_ESEL | HIRQ_DCHG
        } else {
            HIRQ_CMOK | HIRQ_ESEL
        };
        self.raise_irq(irq_bits);
    }

    fn cmd_open_tray(&mut self) {
        self.status = CDB_STAT_OPEN;
        self.do_cd_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_DCHG);
    }

    fn cmd_end_data_transfer(&mut self) {
        self.infotranstype = InfoTransferType::Idle;
        self.datatranstype = DataTransferType::Idle;
        self.do_cd_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_EHST);
    }

    fn cmd_auth_device(&mut self) {
        self.satauth = 1;
        self.mpgauth = 1;
        self.cr1 = (self.status as u16) << 8;
        self.cr2 = 0;
        self.cr3 = 0;
        self.cr4 = 0;
        self.raise_irq(HIRQ_CMOK | HIRQ_CSCT | HIRQ_EFLS);
    }

    fn cmd_is_auth(&mut self) {
        self.cr1 = if self.satauth != 0 {
            (self.status as u16) << 8
        } else {
            0x0000
        };
        self.cr2 = 0;
        self.cr3 = 0;
        self.cr4 = 0;
        self.raise_irq(HIRQ_CMOK);
    }

    // ==========================================
    // Phase 4 Command Handlers (Partition/Filter/Buffer)
    // ==========================================
    fn cmd_set_filter_range(&mut self) {
        let filter_num = ((self.cr1 & 0xFF) as usize) & 0x1F;
        if filter_num < MAX_FILTERS {
            let fad_start = (((self.cr2 & 0xFF) as u32) << 16) | (self.cr3 as u32);
            let fad_range = (self.cr4 as u32) | (((self.cr2 >> 8) as u32) << 16);
            self.filter[filter_num].fad_start = fad_start;
            self.filter[filter_num].fad_range = fad_range;
        }
        self.do_cd_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_ESEL);
    }

    fn cmd_get_filter_subh_cond(&mut self) {
        let filter_num = ((self.cr1 & 0xFF) as usize) & 0x1F;
        if filter_num < MAX_FILTERS {
            let f = &self.filter[filter_num];
            self.cr1 = ((self.status as u16) << 8) | (f.chan as u16);
            self.cr2 = ((f.smmask as u16) << 8) | (f.cimask as u16);
            self.cr3 = ((f.smval as u16) << 8) | (f.cival as u16);
            self.cr4 = 0;
        } else {
            self.cr1 = 0xFF00;
        }
        self.raise_irq(HIRQ_CMOK | HIRQ_ESEL);
    }

    fn cmd_set_filter_subh_cond(&mut self) {
        let filter_num = ((self.cr1 & 0xFF) as usize) & 0x1F;
        if filter_num < MAX_FILTERS {
            self.filter[filter_num].chan = (self.cr1 & 0xFF) as u8;
            self.filter[filter_num].smmask = (self.cr2 >> 8) as u8;
            self.filter[filter_num].cimask = (self.cr2 & 0xFF) as u8;
            self.filter[filter_num].smval = (self.cr3 >> 8) as u8;
            self.filter[filter_num].cival = (self.cr3 & 0xFF) as u8;
        }
        self.do_cd_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_ESEL);
    }

    fn cmd_set_filter_mode(&mut self) {
        let filter_num = ((self.cr1 & 0xFF) as usize) & 0x1F;
        if filter_num < MAX_FILTERS {
            self.filter[filter_num].mode = (self.cr2 >> 8) as u8;
        }
        self.do_cd_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_ESEL);
    }

    fn cmd_set_filter_conn(&mut self) {
        let filter_num = ((self.cr1 & 0xFF) as usize) & 0x1F;
        if filter_num < MAX_FILTERS {
            let true_part = (self.cr2 >> 8) as u8;
            let false_part = (self.cr2 & 0xFF) as u8;
            self.filter[filter_num].condtrue_partition = true_part;
            self.filter[filter_num].condfalse_partition = false_part;
        }
        self.do_cd_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_ESEL);
    }

    fn cmd_get_filter_conn(&mut self) {
        let filter_num = ((self.cr1 & 0xFF) as usize) & 0x1F;
        if filter_num < MAX_FILTERS {
            let f = &self.filter[filter_num];
            self.cr1 = (self.status as u16) << 8;
            self.cr2 = ((f.condtrue_partition as u16) << 8) | (f.condfalse_partition as u16);
            self.cr3 = 0;
            self.cr4 = 0;
        } else {
            self.cr1 = 0xFF00;
        }
        self.raise_irq(HIRQ_CMOK | HIRQ_ESEL);
    }

    fn cmd_reset_selector(&mut self) {
        let flags = (self.cr1 & 0xFF) as u8;
        let part_idx = (self.cr2 >> 8) as usize;

        if flags & 1 != 0 {
            // Reset all filters
            for f in &mut self.filter {
                *f = Filter::default();
            }
        }
        if flags & 2 != 0 {
            // Reset partition buffers and restore blockfreespace
            if part_idx < MAX_PARTITIONS {
                let to_free = std::mem::take(&mut self.partition[part_idx].blocks);
                for b_idx in to_free {
                    self.free_block(b_idx);
                }
                self.partition[part_idx].size = 0;
                self.partition[part_idx].numblocks = 0;
            }
        }
        if flags & 4 != 0 {
            // Reset output connectors
            self.outconcddev = 0;
            self.outconmpeg = 0;
            self.outconhost = 0;
        }

        self.do_cd_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_ESEL);
    }

    fn cmd_get_buffer_size(&mut self) {
        self.cr1 = (self.status as u16) << 8;
        self.cr2 = (self.blockfreespace as u16).min(MAX_BLOCKS as u16);
        self.cr3 = (MAX_PARTITIONS as u16) << 8;
        self.cr4 = MAX_BLOCKS as u16;
        self.raise_irq(HIRQ_CMOK);
    }

    fn cmd_get_sector_number(&mut self) {
        let part_idx = ((self.cr2 >> 8) as usize) & 0x1F;
        if part_idx < MAX_PARTITIONS {
            let num = self.partition[part_idx].numblocks as u16;
            self.cr1 = (self.status as u16) << 8;
            self.cr2 = 0;
            self.cr3 = 0;
            self.cr4 = num;
        } else {
            self.cr1 = 0xFF00;
        }
        self.raise_irq(HIRQ_CMOK);
    }

    fn cmd_calculate_actual_size(&mut self) {
        let part_idx = ((self.cr2 >> 8) as usize) & 0x1F;
        if part_idx < MAX_PARTITIONS {
            let mut total_bytes = 0u32;
            let sectsize = self.partition[part_idx].getsectsize;
            for &blk_idx in &self.partition[part_idx].blocks {
                let blk = &self.block[blk_idx as usize];
                total_bytes += blk.size.min(sectsize);
            }
            self.partition[part_idx].size = total_bytes;
        }
        self.do_cd_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_ESEL);
    }

    fn cmd_get_actual_size(&mut self) {
        let part_idx = ((self.cr2 >> 8) as usize) & 0x1F;
        if part_idx < MAX_PARTITIONS {
            let size = self.partition[part_idx].size;
            self.cr1 = (self.status as u16) << 8;
            self.cr2 = (size >> 16) as u16;
            self.cr3 = (size & 0xFFFF) as u16;
            self.cr4 = 0;
        } else {
            self.cr1 = 0xFF00;
        }
        self.raise_irq(HIRQ_CMOK);
    }

    fn cmd_get_sector_info(&mut self) {
        let part_idx = ((self.cr2 >> 8) as usize) & 0x1F;
        let sect_idx = (self.cr2 & 0xFF) as usize;

        if part_idx < MAX_PARTITIONS && sect_idx < self.partition[part_idx].blocks.len() {
            let blk_idx = self.partition[part_idx].blocks[sect_idx] as usize;
            let blk = &self.block[blk_idx];
            self.cr1 = (self.status as u16) << 8;
            self.cr2 = (blk.fad >> 16) as u16;
            self.cr3 = (blk.fad & 0xFFFF) as u16;
            self.cr4 = ((blk.fn_ as u16) << 8) | (blk.cn as u16);
        } else {
            self.cr1 = 0xFF00;
        }
        self.raise_irq(HIRQ_CMOK);
    }

    fn cmd_set_sector_length(&mut self) {
        let get_len = (self.cr1 & 0xFF) as u8;
        let put_len = ((self.cr2 >> 8) & 0xFF) as u8;

        let decode_len = |code: u8| match code {
            0 => 2048,
            1 => 2336,
            2 => 2352,
            _ => 2048,
        };

        let gsize = decode_len(get_len);
        let psize = decode_len(put_len);

        for p in &mut self.partition {
            p.getsectsize = gsize;
            p.putsectsize = psize;
        }

        self.do_cd_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_ESEL);
    }

    fn cmd_get_sector_data(&mut self) {
        self.setup_get_sector_data(DataTransferType::GetSector);
    }

    fn cmd_delete_sector_data(&mut self) {
        let part_idx = ((self.cr2 >> 8) as usize) & 0x1F;
        let num_sectors = (self.cr4 as usize).min(200);

        if part_idx < MAX_PARTITIONS {
            let part = &mut self.partition[part_idx];
            let del_count = num_sectors.min(part.blocks.len());
            let mut freed_blocks = Vec::with_capacity(del_count);
            for _ in 0..del_count {
                if !part.blocks.is_empty() {
                    freed_blocks.push(part.blocks.remove(0));
                }
            }
            part.numblocks = part.blocks.len() as u32;
            for b in freed_blocks {
                self.free_block(b);
            }
        }
        self.do_cd_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_EHST);
    }

    fn cmd_get_then_delete_sector_data(&mut self) {
        self.setup_get_sector_data(DataTransferType::GetDeleteSector);
    }

    fn setup_get_sector_data(&mut self, del_mode: DataTransferType) {
        let part_idx = ((self.cr2 >> 8) as usize) & 0x1F;
        let sect_offset = (self.cr2 & 0xFF) as u32;
        let num_sectors = self.cr4 as u32;

        if part_idx < MAX_PARTITIONS && !self.partition[part_idx].blocks.is_empty() {
            self.datatranstype = del_mode;
            self.datatranspartition = part_idx as u8;
            self.datatransoffset = 0;
            self.datatranssectoffset = 0;
            self.datatranssectsize = self.partition[part_idx].getsectsize;
            self.datatransblockindex =
                (sect_offset as usize).min(self.partition[part_idx].blocks.len());
            self.datatranstargetbytes = num_sectors * self.datatranssectsize;
            self.datatransbytesread = 0;

            self.do_cd_report();
            self.raise_irq(HIRQ_CMOK | HIRQ_DRDY | HIRQ_EHST);
        } else {
            self.cr1 = 0xFF00;
            self.raise_irq(HIRQ_CMOK);
        }
    }

    fn cmd_put_sector_data(&mut self) {
        let part_idx = ((self.cr2 >> 8) as usize) & 0x1F;
        let num_sectors = self.cr4 as u32;

        if part_idx < MAX_PARTITIONS {
            self.datatranstype = DataTransferType::PutSector;
            self.datatranspartition = part_idx as u8;
            self.datatransoffset = 0;
            self.datatranssectsize = self.partition[part_idx].putsectsize;
            self.datatranstargetbytes = num_sectors * self.datatranssectsize;
            self.put_block_idx = self.allocate_block();
            self.put_offset = 0;

            self.do_cd_report();
            self.raise_irq(HIRQ_CMOK | HIRQ_DRDY);
        } else {
            self.cr1 = 0xFF00;
            self.raise_irq(HIRQ_CMOK);
        }
    }

    fn cmd_copy_sector_data(&mut self) {
        self.do_copy_move_sector(false);
    }

    fn cmd_move_sector_data(&mut self) {
        self.do_copy_move_sector(true);
    }

    fn do_copy_move_sector(&mut self, is_move: bool) {
        let src_part = ((self.cr2 >> 8) as usize) & 0x1F;
        let dst_part = (self.cr1 & 0x1F) as usize;
        let num_sectors = self.cr4 as usize;

        if src_part < MAX_PARTITIONS && dst_part < MAX_PARTITIONS {
            let count = if num_sectors == 0xFFFF {
                self.partition[src_part].blocks.len()
            } else {
                num_sectors.min(self.partition[src_part].blocks.len())
            };

            for _ in 0..count {
                if !self.partition[src_part].blocks.is_empty() {
                    let b = if is_move {
                        self.partition[src_part].blocks.remove(0)
                    } else {
                        let orig_b = self.partition[src_part].blocks[0];
                        if let Some(new_b) = self.allocate_block() {
                            let src_data = self.block[orig_b as usize].clone();
                            self.block[new_b as usize] = src_data;
                            new_b
                        } else {
                            break;
                        }
                    };
                    self.partition[dst_part].blocks.push(b);
                }
            }
            self.partition[src_part].numblocks = self.partition[src_part].blocks.len() as u32;
            self.partition[dst_part].numblocks = self.partition[dst_part].blocks.len() as u32;
        }

        self.do_cd_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_ECPY);
    }

    fn cmd_get_copy_error(&mut self) {
        self.cr1 = (self.status as u16) << 8;
        self.cr2 = 0;
        self.cr3 = 0;
        self.cr4 = 0;
        self.raise_irq(HIRQ_CMOK);
    }

    // ==========================================
    // Phase 5 Command Handlers (Playback & Subcode)
    // ==========================================
    fn cmd_play_disc(&mut self) {
        let start_pos_type = ((self.cr1 >> 8) & 0xFF) as u8;
        let mut start_fad = (((self.cr1 & 0xFF) as u32) << 16) | (self.cr2 as u32);
        let mut end_fad = (((self.cr3 & 0xFF) as u32) << 16) | (self.cr4 as u32);
        let play_mode = ((self.cr3 >> 8) & 0xFF) as u8;

        if start_pos_type != 0 && self.disc.is_some() {
            // Track start
            let track_num = (start_fad & 0xFF) as u8;
            start_fad = self
                .disc
                .as_ref()
                .unwrap()
                .track_to_fad(track_num)
                .unwrap_or(150);
        }
        if end_fad == 0 && self.disc.is_some() {
            end_fad = self.disc.as_ref().unwrap().lead_out_fad;
        }

        self.play_start_fad = start_fad;
        self.play_end_fad = end_fad;
        self.fad = start_fad;
        self.play_mode = play_mode;
        self.status = CDB_STAT_PLAY;

        self.do_cd_report();
        self.raise_irq(HIRQ_CMOK);
    }

    fn cmd_seek_disc(&mut self) {
        let pos_type = ((self.cr1 >> 8) & 0xFF) as u8;
        let mut target_fad = (((self.cr1 & 0xFF) as u32) << 16) | (self.cr2 as u32);

        if pos_type != 0 && self.disc.is_some() {
            let track_num = (target_fad & 0xFF) as u8;
            target_fad = self
                .disc
                .as_ref()
                .unwrap()
                .track_to_fad(track_num)
                .unwrap_or(150);
        }

        self.fad = target_fad;
        self.status = CDB_STAT_PAUSE;
        self.do_cd_report();
        self.raise_irq(HIRQ_CMOK);
    }

    fn cmd_scan_disc(&mut self) {
        let dir = (self.cr1 & 0xFF) as u8;
        self.scan_direction = if dir == 0 { 1 } else { -1 };
        self.status = CDB_STAT_SCAN;
        self.do_cd_report();
        self.raise_irq(HIRQ_CMOK);
    }

    fn cmd_get_subcode_q_rw(&mut self) {
        let req_type = (self.cr1 & 0xFF) as u8;
        if req_type == 0 {
            // Subcode Q: 5 words / 10 bytes
            self.infotranstype = InfoTransferType::SubQ;
            self.transfercount = 0;
            self.cdwnum = 5;
            self.trans_buffer = self.subcode_q.to_vec();
        } else {
            // Subcode RW: 12 words / 24 bytes
            self.infotranstype = InfoTransferType::SubRw;
            self.transfercount = 0;
            self.cdwnum = 12;
            self.trans_buffer = self.subcode_rw.to_vec();
        }
        self.do_cd_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_DRDY);
    }

    // ==========================================
    // Phase 6 Command Handlers (Filesystem)
    // ==========================================
    fn cmd_change_directory(&mut self) {
        let fid = (((self.cr3 & 0xFF) as u32) << 16) | (self.cr4 as u32);
        self.read_filesystem_directory(fid);
        self.do_cd_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_EFLS);
    }

    fn cmd_read_directory(&mut self) {
        let fid = (((self.cr3 & 0xFF) as u32) << 16) | (self.cr4 as u32);
        self.read_filesystem_directory(fid);
        self.do_cd_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_EFLS);
    }

    fn cmd_get_filesystem_scope(&mut self) {
        self.cr1 = (self.status as u16) << 8;
        self.cr2 = (self.fileinfo.len() as u16).min(256);
        self.cr3 = (self.curdirfidoffset as u16) << 8;
        self.cr4 = (self.curdirsize / 2048) as u16;
        self.raise_irq(HIRQ_CMOK | HIRQ_EFLS);
    }

    fn cmd_get_file_info(&mut self) {
        let fid = (((self.cr3 & 0xFF) as u32) << 16) | (self.cr4 as u32);
        if (fid as usize) < self.fileinfo.len() {
            let f = &self.fileinfo[fid as usize];
            self.infotranstype = InfoTransferType::SingleFile;
            self.transfercount = 0;
            self.cdwnum = 6; // 6 words = 12 bytes
            self.trans_buffer.clear();
            self.trans_buffer.extend_from_slice(&f.lba.to_be_bytes());
            self.trans_buffer.extend_from_slice(&f.size.to_be_bytes());
            self.trans_buffer.push(f.interleavegapsize);
            self.trans_buffer.push(f.fileunitsize);
            self.trans_buffer.push((f.fid & 0xFF) as u8);
            self.trans_buffer.push(f.flags);

            self.do_cd_report();
            self.raise_irq(HIRQ_CMOK | HIRQ_DRDY);
        } else {
            self.cr1 = 0xFF00;
            self.raise_irq(HIRQ_CMOK);
        }
    }

    fn cmd_read_file(&mut self) {
        let fid = (((self.cr3 & 0xFF) as u32) << 16) | (self.cr4 as u32);
        let filter_idx = (self.outconcddev & 0x1F) as usize;
        if (fid as usize) < self.fileinfo.len() && filter_idx < MAX_FILTERS {
            let f = &self.fileinfo[fid as usize];
            self.filter[filter_idx].fad_start = f.lba;
            self.filter[filter_idx].fad_range = (f.size + 2047) / 2048;
            self.play_start_fad = f.lba;
            self.play_end_fad = f.lba + self.filter[filter_idx].fad_range;
            self.fad = f.lba;
            self.status = CDB_STAT_PLAY;
            self.do_cd_report();
            self.raise_irq(HIRQ_CMOK);
        } else {
            self.cr1 = 0xFF00;
            self.raise_irq(HIRQ_CMOK);
        }
    }

    fn cmd_abort_file(&mut self) {
        self.status = CDB_STAT_PAUSE;
        self.do_cd_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_EFLS);
    }

    pub fn read_filesystem_directory(&mut self, fid: u32) {
        if self.disc.is_none() {
            return;
        }

        let root_fad = if fid == 0 || self.curdirsect == 0 {
            // Read PVD at FAD 166
            let mut pvd_buf = [0u8; 2448];
            if self
                .disc
                .as_mut()
                .unwrap()
                .read_sector_fad(166, &mut pvd_buf)
                .is_ok()
            {
                // Root directory extent LBA at offset 0x9C + 2 (LE) / + 6 (BE)
                let root_lba = u32::from_be_bytes([
                    pvd_buf[16 + 156 + 6],
                    pvd_buf[16 + 156 + 7],
                    pvd_buf[16 + 156 + 8],
                    pvd_buf[16 + 156 + 9],
                ]);
                let root_size = u32::from_be_bytes([
                    pvd_buf[16 + 156 + 14],
                    pvd_buf[16 + 156 + 15],
                    pvd_buf[16 + 156 + 16],
                    pvd_buf[16 + 156 + 17],
                ]);
                self.curdirsect = 150 + root_lba;
                self.curdirsize = root_size;
                self.curdirsect
            } else {
                168
            }
        } else if (fid as usize) < self.fileinfo.len() {
            let f = &self.fileinfo[fid as usize];
            self.curdirsect = f.lba;
            self.curdirsize = f.size;
            f.lba
        } else {
            self.curdirsect
        };

        // Read directory sector
        let mut dir_buf = [0u8; 2448];
        if self
            .disc
            .as_mut()
            .unwrap()
            .read_sector_fad(root_fad, &mut dir_buf)
            .is_ok()
        {
            self.fileinfo.clear();
            let mut offset = 16usize; // Start after 16 bytes sync header
            let limit = 16 + 2048;
            let mut id = 0u32;

            while offset < limit {
                let rec_len = dir_buf[offset] as usize;
                if rec_len == 0 {
                    break;
                }
                if offset + rec_len > limit {
                    break;
                }

                let ext_lba = u32::from_be_bytes([
                    dir_buf[offset + 6],
                    dir_buf[offset + 7],
                    dir_buf[offset + 8],
                    dir_buf[offset + 9],
                ]);
                let data_len = u32::from_be_bytes([
                    dir_buf[offset + 14],
                    dir_buf[offset + 15],
                    dir_buf[offset + 16],
                    dir_buf[offset + 17],
                ]);
                let flags = dir_buf[offset + 25];
                let fileunitsize = dir_buf[offset + 26];
                let interleavegapsize = dir_buf[offset + 27];

                self.fileinfo.push(FileInfoRecord {
                    lba: 150 + ext_lba,
                    size: data_len,
                    interleavegapsize,
                    fileunitsize,
                    flags,
                    fid: id,
                });
                id += 1;
                offset += rec_len;
            }
        }
    }

    pub fn get_ip(&mut self, autoregion: bool) -> Option<CdIpBin> {
        if self.disc.is_none() {
            return None;
        }

        let mut buf = [0u8; 2448];
        if self
            .disc
            .as_mut()
            .unwrap()
            .read_sector_fad(150, &mut buf)
            .is_err()
        {
            return None;
        }

        if &buf[16..32] != b"SEGA SEGASATURN " {
            return None;
        }

        let clean_str = |b: &[u8]| {
            String::from_utf8_lossy(b)
                .trim_matches(|c: char| c == '\0' || c.is_whitespace())
                .to_string()
        };

        let system = clean_str(&buf[16..32]);
        let company = clean_str(&buf[32..48]);
        let itemnum = clean_str(&buf[48..58]);
        let version = clean_str(&buf[58..64]);
        let date_raw = clean_str(&buf[64..72]);
        let date = if date_raw.len() == 8 {
            format!(
                "{}/{}/{}",
                &date_raw[6..8],
                &date_raw[4..6],
                &date_raw[0..4]
            )
        } else {
            date_raw
        };
        let cdinfo = clean_str(&buf[72..80]);
        let region = clean_str(&buf[80..90]);
        let peripheral = clean_str(&buf[96..112]);
        let gamename = clean_str(&buf[112..224]);

        let ipsize = u32::from_be_bytes([
            buf[0xE0 + 16],
            buf[0xE1 + 16],
            buf[0xE2 + 16],
            buf[0xE3 + 16],
        ]);
        let msh2stack = u32::from_be_bytes([
            buf[0xE8 + 16],
            buf[0xE9 + 16],
            buf[0xEA + 16],
            buf[0xEB + 16],
        ]);
        let ssh2stack = u32::from_be_bytes([
            buf[0xEC + 16],
            buf[0xED + 16],
            buf[0xEE + 16],
            buf[0xEF + 16],
        ]);
        let firstprogaddr = u32::from_be_bytes([
            buf[0xF0 + 16],
            buf[0xF1 + 16],
            buf[0xF2 + 16],
            buf[0xF3 + 16],
        ]);
        let firstprogsize = u32::from_be_bytes([
            buf[0xF4 + 16],
            buf[0xF5 + 16],
            buf[0xF6 + 16],
            buf[0xF7 + 16],
        ]);

        if autoregion && !region.is_empty() {
            let region_char = region.chars().next().unwrap();
            let _code = match region_char {
                'J' => 1,
                'T' => 2,
                'U' => 4,
                'B' => 5,
                'K' => 6,
                'A' => 0xA,
                'E' => 0xC,
                'L' => 0xD,
                _ => 0,
            };
        }

        let ip = CdIpBin {
            system,
            company,
            itemnum,
            version,
            date,
            cdinfo,
            region,
            peripheral,
            gamename,
            ipsize,
            msh2stack,
            ssh2stack,
            firstprogaddr,
            firstprogsize,
        };
        self.cdip = Some(ip.clone());
        Some(ip)
    }

    // ==========================================
    // Phase 7 Command Handlers (MPEG & Search)
    // ==========================================
    fn cmd_exec_fad_search(&mut self) {
        self.do_cd_report();
        self.raise_irq(HIRQ_CMOK);
    }

    fn cmd_get_fad_search_results(&mut self) {
        self.do_cd_report();
        self.raise_irq(HIRQ_CMOK);
    }

    fn cmd_mpeg_get_status(&mut self) {
        self.do_mpeg_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_MPCM);
    }

    fn cmd_mpeg_get_interrupt(&mut self) {
        let int_val = 0u32 & self.mpegintmask;
        self.cr1 = ((self.status as u16) << 8) | ((int_val >> 16) as u16 & 0xFF);
        self.cr2 = (int_val & 0xFFFF) as u16;
        self.cr3 = 0;
        self.cr4 = 0;
        self.raise_irq(HIRQ_CMOK | HIRQ_MPCM);
    }

    fn cmd_mpeg_set_interrupt_mask(&mut self) {
        self.mpegintmask = (((self.cr1 & 0xFF) as u32) << 16) | (self.cr2 as u32);
        self.do_mpeg_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_MPCM);
    }

    fn cmd_mpeg_init(&mut self) {
        self.cr1 = if self.mpgauth != 0 {
            (self.status as u16) << 8
        } else {
            0xFF00
        };
        self.cr2 = 0;
        self.cr3 = 0;
        self.cr4 = 0;
        self.raise_irq(HIRQ_CMOK | HIRQ_MPED | HIRQ_MPST | HIRQ_MPCM);
    }

    fn cmd_mpeg_set_mode(&mut self) {
        self.do_mpeg_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_MPCM);
    }

    fn cmd_mpeg_play(&mut self) {
        self.do_mpeg_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_MPCM);
    }

    fn cmd_mpeg_set_decoding_method(&mut self) {
        self.do_mpeg_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_MPCM);
    }

    fn cmd_mpeg_set_connection(&mut self) {
        self.do_mpeg_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_MPCM);
    }

    fn cmd_mpeg_get_connection(&mut self) {
        self.do_mpeg_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_MPCM);
    }

    fn cmd_mpeg_set_stream(&mut self) {
        self.do_mpeg_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_MPCM);
    }

    fn cmd_mpeg_get_stream(&mut self) {
        self.do_mpeg_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_MPCM);
    }

    fn cmd_mpeg_stubs(&mut self) {
        self.do_mpeg_report();
        self.raise_irq(HIRQ_CMOK | HIRQ_MPCM);
    }

    fn cmd_mpeg_set_lsi(&mut self) {
        self.raise_irq(HIRQ_CMOK | HIRQ_MPCM);
    }

    fn cmd_get_mpeg_rom(&mut self) {
        // Deliberate deviation #19: Reject arbitrary host file reading
        self.cr1 = 0xFF00;
        self.cr2 = 0;
        self.cr3 = 0;
        self.cr4 = 0;
        self.raise_irq(HIRQ_CMOK | HIRQ_MPED);
    }

    // ==========================================
    // Register Read & Write Port Implementations
    // ==========================================
    pub fn read_word(&mut self, offset: usize) -> u16 {
        let off = offset & 0xFFFFF;
        match off {
            0x90008 | 0x9000A => self.hirq,
            0x9000C | 0x9000E => self.hirqmask,
            0x90018 | 0x9001A => self.cr1,
            0x9001C | 0x9001E => self.cr2,
            0x90020 | 0x90022 => self.cr3,
            0x90024 | 0x90026 => {
                // Reading CR4 clears command_pending
                self.command_pending = false;
                self.cr4
            }
            0x90028 | 0x9002A => self.mpegrgb,
            0x98000 => self.read_info_port(),
            _ => 0,
        }
    }

    pub fn write_word(&mut self, offset: usize, val: u16) {
        let off = offset & 0xFFFFF;
        match off {
            0x90008 | 0x9000A => {
                // HIRQ write ANDs
                self.hirq &= val;
                self.check_external_irq();
            }
            0x9000C | 0x9000E => {
                self.hirqmask = val;
                self.check_external_irq();
            }
            0x90018 | 0x9001A => {
                self.cr1 = val;
                self.status &= !CDB_STAT_PERI;
                self.command_pending = true;
            }
            0x9001C | 0x9001E => {
                self.cr2 = val;
            }
            0x90020 | 0x90022 => {
                self.cr3 = val;
            }
            0x90024 | 0x90026 => {
                self.cr4 = val;
                self.command_pending = true;
            }
            0x90028 | 0x9002A => {
                self.mpegrgb = val;
            }
            _ => {}
        }
    }

    pub fn read_long(&mut self, offset: usize) -> u32 {
        let off = offset & 0xFFFFF;
        if off == 0x18000 {
            self.read_data_fifo_long()
        } else {
            let w = self.read_word(off);
            ((w as u32) << 16) | (w as u32)
        }
    }

    pub fn write_long(&mut self, offset: usize, val: u32) {
        let off = offset & 0xFFFFF;
        if off == 0x18000 {
            self.write_data_fifo_long(val);
        } else {
            self.write_word(off, (val >> 16) as u16);
        }
    }

    pub fn read_info_port(&mut self) -> u16 {
        if self.infotranstype == InfoTransferType::Idle || self.trans_buffer.is_empty() {
            return 0;
        }

        let byte_pos = (self.transfercount as usize) * 2;
        if byte_pos + 1 < self.trans_buffer.len() {
            let b0 = self.trans_buffer[byte_pos] as u16;
            let b1 = self.trans_buffer[byte_pos + 1] as u16;
            let word = (b0 << 8) | b1;
            self.transfercount += 1;
            if self.transfercount >= self.cdwnum {
                self.infotranstype = InfoTransferType::Idle;
            }
            word
        } else {
            self.infotranstype = InfoTransferType::Idle;
            0
        }
    }

    pub fn read_data_fifo_long(&mut self) -> u32 {
        if self.datatranstype == DataTransferType::Idle
            || (self.datatranspartition as usize) >= MAX_PARTITIONS
        {
            return 0;
        }

        let part_idx = self.datatranspartition as usize;
        let part = &mut self.partition[part_idx];

        if self.datatransblockindex >= part.blocks.len() {
            self.datatranstype = DataTransferType::Idle;
            self.raise_irq(HIRQ_EHST);
            return 0;
        }

        let blk_idx = part.blocks[self.datatransblockindex] as usize;
        let blk = &self.block[blk_idx];

        let offset = self.datatranssectoffset as usize;
        let b0 = blk.data.get(offset).copied().unwrap_or(0) as u32;
        let b1 = blk.data.get(offset + 1).copied().unwrap_or(0) as u32;
        let b2 = blk.data.get(offset + 2).copied().unwrap_or(0) as u32;
        let b3 = blk.data.get(offset + 3).copied().unwrap_or(0) as u32;
        let val = (b0 << 24) | (b1 << 16) | (b2 << 8) | b3;

        self.datatranssectoffset += 4;
        self.datatransoffset += 4;
        self.datatransbytesread += 4;

        let mut freed_opt = None;
        if self.datatranssectoffset >= self.datatranssectsize {
            self.datatranssectoffset = 0;
            if self.datatranstype == DataTransferType::GetDeleteSector {
                // GETDELSECTOR: delete sector
                let freed = part.blocks.remove(self.datatransblockindex);
                freed_opt = Some(freed);
                part.numblocks = part.blocks.len() as u32;
            } else {
                self.datatransblockindex += 1;
            }
        }

        let is_done = self.datatransoffset >= self.datatranstargetbytes
            || self.datatransblockindex >= part.blocks.len();

        if let Some(freed) = freed_opt {
            self.free_block(freed);
        }

        if is_done {
            self.datatranstype = DataTransferType::Idle;
            self.raise_irq(HIRQ_EHST);
        }

        val
    }

    pub fn write_data_fifo_long(&mut self, val: u32) {
        if self.datatranstype != DataTransferType::PutSector {
            return;
        }

        if let Some(blk_idx) = self.put_block_idx {
            let blk = &mut self.block[blk_idx as usize];
            let bytes = val.to_be_bytes();
            for &b in &bytes {
                if self.put_offset < 2352 {
                    blk.data[self.put_offset] = b;
                    self.put_offset += 1;
                }
            }

            self.datatransoffset += 4;
            if self.put_offset >= (self.datatranssectsize as usize) {
                blk.size = self.datatranssectsize;
                let part_idx = self.datatranspartition as usize;
                self.partition[part_idx].blocks.push(blk_idx);
                self.partition[part_idx].numblocks = self.partition[part_idx].blocks.len() as u32;
                self.partition[part_idx].size += blk.size;
                self.put_offset = 0;

                if self.datatransoffset < self.datatranstargetbytes {
                    self.put_block_idx = self.allocate_block();
                } else {
                    self.put_block_idx = None;
                    self.datatranstype = DataTransferType::Idle;
                    self.raise_irq(HIRQ_EHST);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cs2_reset_state() {
        let cs2 = Cs2::new();
        assert_eq!(cs2.hirq, 0xFFFF);
        assert_eq!(cs2.hirqmask, 0x0000);
        assert_eq!(cs2.cr1, 0x0043);
        assert_eq!(cs2.cr2, 0x4442);
        assert_eq!(cs2.cr3, 0x4C4F);
        assert_eq!(cs2.cr4, 0x434B);
        assert_eq!(cs2.blockfreespace, 200);
    }

    #[test]
    fn test_cs2_handshake_cr4_write_exec() {
        let mut cs2 = Cs2::new();
        cs2.write_word(0x90008, !HIRQ_CMOK);
        cs2.write_word(0x90018, 0x0100); // 0x01 Get HW Info
        cs2.write_word(0x90024, 0x0000); // CR4 write triggers command

        assert_eq!(cs2.command_pending, true);
        assert_eq!(cs2.hirq & HIRQ_CMOK, 0);

        cs2.exec(60); // 60 µs elapsed

        assert_eq!(cs2.command_pending, false);
        assert_eq!(cs2.hirq & HIRQ_CMOK, HIRQ_CMOK);
        assert_eq!(cs2.cr1, 0x0700);
        assert_eq!(cs2.cr2, 0x0201);
        assert_eq!(cs2.cr3, 0x0000);
        assert_eq!(cs2.cr4, 0x0400);
    }

    #[test]
    fn test_phase1_register_decoding_and_access_widths() {
        let mut cs2 = Cs2::new();

        // 1. Initial CR registers match ASCII "CDBLOCK"
        assert_eq!(cs2.read_word(0x90018), 0x0043); // '\0C'
        assert_eq!(cs2.read_word(0x9001C), 0x4442); // 'DB'
        assert_eq!(cs2.read_word(0x90020), 0x4C4F); // 'LO'
        assert_eq!(cs2.read_word(0x90024), 0x434B); // 'CK'

        // 2. 32-bit register reads duplicate 16-bit word across high and low halves
        let long_cr2 = cs2.read_long(0x9001C);
        assert_eq!(long_cr2, 0x44424442);

        // 3. HIRQ write clears bits via bitwise AND
        cs2.hirq = 0xFFFF;
        cs2.write_word(0x90008, 0xFFFE); // clear bit 0
        assert_eq!(cs2.read_word(0x90008), 0xFFFE);
        cs2.write_word(0x90008, 0x0000); // clear all
        assert_eq!(cs2.read_word(0x90008), 0x0000);

        // 4. HIRQMASK read/write
        cs2.write_word(0x9000C, 0x0505);
        assert_eq!(cs2.read_word(0x9000C), 0x0505);

        // 5. MPEGRGB read/write
        cs2.write_word(0x90028, 0x1234);
        assert_eq!(cs2.read_word(0x90028), 0x1234);
    }

    #[test]
    fn test_phase2_command_handshake_and_get_hardware_info() {
        let mut cs2 = Cs2::new();

        // 1. Command 0x00: Get Status
        cs2.write_word(0x90018, 0x0000);
        cs2.write_word(0x9001C, 0x0000);
        cs2.write_word(0x90020, 0x0000);
        cs2.write_word(0x90024, 0x0000);
        assert!(cs2.command_pending);
        cs2.exec(60);

        assert!(!cs2.command_pending);
        assert_eq!(cs2.hirq & HIRQ_CMOK, HIRQ_CMOK);
        assert_eq!(cs2.cr1 >> 8, CDB_STAT_NODISC as u16);

        // 2. Command 0x01: Get Hardware Info
        cs2.write_word(0x90018, 0x0100);
        cs2.write_word(0x90024, 0x0000);
        cs2.exec(60);

        assert_eq!(cs2.cr1 >> 8, CDB_STAT_NODISC as u16);
        assert_eq!(cs2.cr1 & 0xFF, 0x00);
        assert_eq!(cs2.cr2, 0x0201);
        assert_eq!(cs2.cr3, 0x0000);
        assert_eq!(cs2.cr4, 0x0400);

        // 3. Command 0x06: Reset / Initialize
        cs2.write_word(0x90018, 0x0600);
        cs2.write_word(0x90024, 0x0000);
        cs2.exec(60);
        assert_eq!(cs2.hirq & (HIRQ_CMOK | HIRQ_ESEL), HIRQ_CMOK | HIRQ_ESEL);
    }

    #[test]
    fn test_phase2_get_toc_mode_0() {
        let mut cs2 = Cs2::new();
        let fixture_path = "tests/fixtures/single_data_track.chd";
        if !std::path::Path::new(fixture_path).exists() {
            return;
        }
        cs2.load_disc(fixture_path).unwrap();

        cs2.write_word(0x90018, 0x0200); // 0x02 Get TOC
        cs2.write_word(0x90024, 0x0000);
        cs2.exec(300);

        assert_eq!(cs2.hirq & HIRQ_DRDY, HIRQ_DRDY);
        assert_eq!(cs2.cdwnum, 102);

        // Read first 2 words (Track 1 TOC)
        let w0 = cs2.read_word(0x98000);
        let w1 = cs2.read_word(0x98000);
        let toc0 = ((w0 as u32) << 16) | (w1 as u32);
        assert_eq!(toc0, 0x41000096);
    }

    #[test]
    fn test_phase3_periodic_sector_engine_and_scdq() {
        let mut cs2 = Cs2::new();
        let fixture_path = "tests/fixtures/single_data_track.chd";
        if !std::path::Path::new(fixture_path).exists() {
            return;
        }
        cs2.load_disc(fixture_path).unwrap();

        // Start playback on track 1
        cs2.write_word(0x90018, 0x1001); // 0x10 Play Disc, Track 1
        cs2.write_word(0x9001C, 0x0000);
        cs2.write_word(0x90020, 0x0000);
        cs2.write_word(0x90024, 0x0000);
        cs2.exec(60);

        assert_eq!(cs2.status, CDB_STAT_PLAY);

        // Advance emulation by ~100ms (multiple sector ticks at 75 Hz / 13.3ms)
        cs2.exec(100_000);

        // Verify FAD has advanced beyond starting 150
        assert!(cs2.fad > 150, "FAD should advance during playback");
        assert_eq!(cs2.hirq & HIRQ_SCDQ, HIRQ_SCDQ, "SCDQ should be raised");
    }

    #[test]
    fn test_phase4_partitions_filters_and_data_fifo() {
        let mut cs2 = Cs2::new();

        // 1. Get Buffer Size (0x50)
        cs2.write_word(0x90018, 0x5000);
        cs2.write_word(0x90024, 0x0000);
        cs2.exec(60);
        assert_eq!(cs2.cr2, 200); // 200 free blocks
        assert_eq!(cs2.cr4, 200); // 200 max blocks

        // 2. Put Sector Data (0x64) to Partition 2
        cs2.write_word(0x90018, 0x6400);
        cs2.write_word(0x9001C, 0x0200); // Partition 2
        cs2.write_word(0x90024, 0x0001); // 1 sector
        cs2.exec(60);
        assert_eq!(cs2.hirq & HIRQ_DRDY, HIRQ_DRDY);

        // Write 512 32-bit words (2048 bytes) into Data FIFO
        for i in 0..512u32 {
            cs2.write_long(0x18000, 0x10000000 | i);
        }

        // 3. Get Sector Number (0x51) for Partition 2
        cs2.write_word(0x90018, 0x5100);
        cs2.write_word(0x9001C, 0x0200); // Partition 2
        cs2.write_word(0x90024, 0x0000);
        cs2.exec(60);
        assert_eq!(cs2.cr4, 1, "Partition 2 should contain 1 sector block");

        // 4. Calculate Actual Size (0x52)
        cs2.write_word(0x90018, 0x5200);
        cs2.write_word(0x9001C, 0x0200);
        cs2.write_word(0x90024, 0x0000);
        cs2.exec(60);
        assert_eq!(cs2.partition[2].size, 2048);

        // 5. Get Sector Data (0x61) from Partition 2
        cs2.write_word(0x90018, 0x6100);
        cs2.write_word(0x9001C, 0x0200); // Partition 2
        cs2.write_word(0x90024, 0x0001); // 1 sector
        cs2.exec(60);

        let first_long = cs2.read_long(0x18000);
        assert_eq!(first_long, 0x10000000);

        // 6. Copy Sector Data (0x65) from Partition 2 to Partition 3
        cs2.write_word(0x90018, 0x6503); // Dst: Partition 3
        cs2.write_word(0x9001C, 0x0200); // Src: Partition 2
        cs2.write_word(0x90024, 0x0001); // 1 sector
        cs2.exec(60);
        assert_eq!(cs2.partition[3].numblocks, 1);

        // 7. Delete Sector Data (0x62) on Partition 2
        cs2.write_word(0x90018, 0x6200);
        cs2.write_word(0x9001C, 0x0200);
        cs2.write_word(0x90024, 0x0001);
        cs2.exec(60);
        assert_eq!(cs2.partition[2].numblocks, 0);

        // 8. Reset Selector (0x43)
        cs2.write_word(0x90018, 0x4307); // Reset all (filters, partitions, connectors)
        cs2.write_word(0x9001C, 0x0300);
        cs2.write_word(0x90024, 0x0000);
        cs2.exec(60);
        assert_eq!(cs2.partition[3].numblocks, 0);
    }

    #[test]
    fn test_phase5_playback_seek_scan_and_subcode() {
        let mut cs2 = Cs2::new();
        let fixture_path = "tests/fixtures/data_plus_audio.chd";
        if !std::path::Path::new(fixture_path).exists() {
            return;
        }
        cs2.load_disc(fixture_path).unwrap();

        // 1. Seek (0x11)
        cs2.write_word(0x90018, 0x1101); // Seek to Track 2
        cs2.write_word(0x9001C, 0x0002);
        cs2.write_word(0x90024, 0x0000);
        cs2.exec(60);
        assert_eq!(cs2.status, CDB_STAT_PAUSE);

        // 2. Scan (0x12)
        cs2.write_word(0x90018, 0x1200); // Forward scan
        cs2.write_word(0x90024, 0x0000);
        cs2.exec(60);
        assert_eq!(cs2.status, CDB_STAT_SCAN);

        // 3. Get Subcode Q (0x20)
        cs2.write_word(0x90018, 0x2000); // Type 0: Subcode Q
        cs2.write_word(0x90024, 0x0000);
        cs2.exec(60);
        assert_eq!(cs2.hirq & HIRQ_DRDY, HIRQ_DRDY);
        assert_eq!(cs2.cdwnum, 5); // 5 words = 10 bytes

        for _ in 0..5 {
            let _w = cs2.read_word(0x98000);
        }
    }

    #[test]
    fn test_phase6_filesystem_and_ip_bin() {
        let mut cs2 = Cs2::new();
        let fixture_path = "tests/fixtures/single_data_track.chd";
        if !std::path::Path::new(fixture_path).exists() {
            return;
        }
        cs2.load_disc(fixture_path).unwrap();

        // 1. Parse IP.BIN
        let ip = cs2.get_ip(true).expect("Failed to parse IP.BIN");
        assert_eq!(ip.system, "SEGA SEGASATURN");
        assert_eq!(ip.company, "SEGA ENTERPRISES");
        assert!(ip.gamename.starts_with("MIMAS TEST DISC"));

        // 2. Read Filesystem Directory (0x71)
        cs2.write_word(0x90018, 0x7100);
        cs2.write_word(0x90020, 0x0000);
        cs2.write_word(0x90024, 0x0000);
        cs2.exec(60);
        assert_eq!(cs2.hirq & HIRQ_EFLS, HIRQ_EFLS);

        // 3. Get Filesystem Scope (0x72)
        cs2.write_word(0x90018, 0x7200);
        cs2.write_word(0x90024, 0x0000);
        cs2.exec(60);
        assert!(cs2.cr2 > 0, "Directory should contain file records");

        // 4. Get File Info (0x73) for FID 0
        cs2.write_word(0x90018, 0x7300);
        cs2.write_word(0x90020, 0x0000);
        cs2.write_word(0x90024, 0x0000); // FID 0
        cs2.exec(60);
        assert_eq!(cs2.cdwnum, 6); // 6 words = 12 bytes
    }

    #[test]
    fn test_phase7_mpeg_stubs_and_fad_search() {
        let mut cs2 = Cs2::new();

        // 1. FAD Search (0x04)
        cs2.write_word(0x90018, 0x0400);
        cs2.write_word(0x90024, 0x0000);
        cs2.exec(60);
        assert_eq!(cs2.hirq & HIRQ_CMOK, HIRQ_CMOK);

        // 2. MPEG Get Status (0x90)
        cs2.write_word(0x90018, 0x9000);
        cs2.write_word(0x90024, 0x0000);
        cs2.exec(60);
        assert_eq!(cs2.hirq & HIRQ_MPCM, HIRQ_MPCM);

        // 3. MPEG Set Interrupt Mask (0x92)
        cs2.write_word(0x90018, 0x9212);
        cs2.write_word(0x9001C, 0x3456);
        cs2.write_word(0x90024, 0x0000);
        cs2.exec(60);
        assert_eq!(cs2.mpegintmask, 0x00123456);

        // 4. MPEG Get Interrupt (0x91)
        cs2.write_word(0x90018, 0x9100);
        cs2.write_word(0x90024, 0x0000);
        cs2.exec(60);
        assert_eq!(cs2.hirq & HIRQ_MPCM, HIRQ_MPCM);

        // 5. Get MPEG ROM (0xE2) - deliberate rejection
        cs2.write_word(0x90018, 0xE200);
        cs2.write_word(0x90024, 0x0000);
        cs2.exec(60);
        assert_eq!(cs2.cr1 >> 8, 0xFF);
        assert_eq!(cs2.hirq & HIRQ_MPED, HIRQ_MPED);
    }

    // ---- Code review regressions: CMOK clear timing, periodic catch-up ----

    #[test]
    fn execute_command_clears_cmok_at_dispatch_not_at_cr4_write_regression() {
        // Real hardware clears CMOK inside `Cs2Execute` the instant a
        // command actually starts running (`cs2.c:1289`), not when CR4 is
        // written (`cs2.c:415-418` -- CR4's write handler never touches
        // HIRQ at all).
        let mut cs2 = Cs2::new();
        cs2.hirq = HIRQ_CMOK; // simulate a stale, still-set flag from a prior command

        cs2.write_word(0x90018, 0xFE00); // opcode 0xFE: genuinely undispatched
        cs2.write_word(0x90024, 0x0000); // CR4 write: only schedules the command
        assert_eq!(
            cs2.hirq & HIRQ_CMOK,
            HIRQ_CMOK,
            "CR4 write alone must not touch HIRQ -- CMOK stays whatever it was"
        );

        cs2.exec(60); // dispatches execute_command()

        // Section 10 QUIRK 6: an undispatched opcode "hangs" -- no handler
        // ever raises CMOK again, so the autonomous clear-on-dispatch is the
        // only thing observable here.
        assert_eq!(
            cs2.hirq & HIRQ_CMOK,
            0,
            "an unimplemented opcode must leave CMOK cleared (it never completes), \
             not stale-set from whatever ran before it"
        );
    }

    #[test]
    fn periodic_engine_catches_up_multiple_periods_in_one_call_regression() {
        // Core 7 delivers a whole real V-Blank period's worth of elapsed
        // time (16_667 µs) in a single `exec()` call (`exec_vblank`),
        // unlike real hardware's `Cs2Exec`, which is fed tiny increments
        // many times more often from the main loop's own instruction-batch
        // tick (`yabause.c:802`). A single, non-looping check (matching the
        // reference's own `if`, `cs2.c:1109`) would silently drop periods
        // owed beyond the first at that coarser call granularity. Values
        // below are hand-derived, not read back from the implementation.
        let mut cs2 = Cs2::new();
        cs2.status = CDB_STAT_SEEK;
        cs2.fad = 150;
        cs2.play_start_fad = 200; // far enough that 2 ticks won't reach it

        cs2.exec(40_000); // 2 * 16_667 = 33_334; remainder = 40_000 - 33_334 = 6_666

        assert_eq!(
            cs2.fad, 152,
            "SEEK's periodic step must have fired exactly twice (40_000 / 16_667 = 2)"
        );
        assert_eq!(
            cs2.periodic_cycles_us, 6_666,
            "the remainder past the 2nd period must be carried forward, not dropped"
        );
    }
}
