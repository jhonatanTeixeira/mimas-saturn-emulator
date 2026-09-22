//! Composition root: instancia e liga todos os dispositivos. É o único módulo que conhece
//! os tipos concretos; CPU e JIT enxergam só `Sh2Bus`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::bus::{Shared, SystemBus};
use crate::cpu::address_space::Sh2AddressSpace;
use crate::cpu::sh2_bus::Tracer;
use crate::cpu::{Fault, Sh2Cpu};
use crate::devices::cd_block::CdBlock;
use crate::devices::ram::{Ram, Rom, SoundRam};
use crate::devices::scsp::Scsp;
use crate::devices::scu::*;
use crate::devices::smpc::Smpc;
use crate::devices::sound_cpu::SoundCpu;
use crate::devices::stub::{AccessLog, OpenBus, RegisterStub, SharedLog};
use crate::devices::vdp1::{Vdp1, Vdp1Area, Vdp1Port};
use crate::devices::vdp2::{Vdp2, Vdp2Area, Vdp2Port};
use crate::timing::{VideoEvent, VideoTiming};
use crate::video::{FrameSink, NullSink};

/// How many SH-2 cycles accumulate before the CD block's clock is fed. Under one scanline
/// (1820 cycles), so nothing that depends on video timing can tell the difference; see
/// `cd_cycle_carry` for why this batching is safe.
const CD_TICK_BATCH_CYCLES: u64 = 1024;

pub struct Saturn {
    pub cpu: Sh2Cpu,
    pub space: Sh2AddressSpace,
    pub scu: Rc<RefCell<Scu>>,
    pub smpc: Rc<RefCell<Smpc>>,
    pub cd: Rc<RefCell<CdBlock>>,
    /// The inserted disc, if any. See `insert_disc`.
    pub disc: Option<crate::devices::disc::DiscImage>,
    pub vdp1: Rc<RefCell<Vdp1>>,
    pub vdp2: Rc<RefCell<Vdp2>>,
    pub scsp_ram: Rc<RefCell<SoundRam>>,
    /// Off only for measurement: it is what tells how much of a frame the sound costs.
    pub sound_enabled: bool,
    pub scsp: Rc<RefCell<Scsp>>,
    pub sound_cpu: SoundCpu,
    /// Stereo samples produced by the SCSP at 44.1 kHz.
    pub audio: Vec<(i16, i16)>,
    pub timing: VideoTiming,
    pub stub_log: SharedLog,
    /// Contador de quadros visível a quem observa (ex.: verificador de traces).
    pub frame_cell: Rc<Cell<u32>>,
    pub sink: Box<dyn FrameSink>,
    events: Vec<VideoEvent>,
    /// Contagem de execuções por PC de entrada de bloco (só quando o profiler está ligado).
    pub profile: Option<std::collections::HashMap<u32, u64>>,
    /// SH-2 cycles not yet fed to the CD block's clock. Its only visible effect, `HIRQ_SCDQ`,
    /// fires once every 381,800 cycles (~1/75 s); ticking it every JIT block, often under a
    /// hundred cycles long, is a `RefCell` borrow and a call for no observable change most of
    /// the time. Batched to `CD_TICK_BATCH_CYCLES` — under one scanline (1820 cycles), and
    /// three orders of magnitude under the period it feeds — see `docs/current_status.md`.
    cd_cycle_carry: u64,
}

impl Saturn {
    pub fn new(bios: &[u8]) -> Self {
        let log: SharedLog = Rc::new(RefCell::new(AccessLog::default()));
        let mut bus = SystemBus::new();

        bus.map(
            "bios",
            0x0000_0000,
            0x0010_0000,
            0x8_0000,
            Box::new(Rom::new(bios, 0x8_0000)),
            false,
        );
        let (smpc_port, smpc) = Shared::new(Smpc::new());
        bus.map(
            "smpc",
            0x0010_0000,
            0x0008_0000,
            0x80,
            Box::new(smpc_port),
            false,
        );
        bus.map(
            "backup_ram",
            0x0018_0000,
            0x0008_0000,
            0x1_0000,
            Box::new(RegisterStub::new("backup_ram", 0x1_0000).with_log(log.clone())),
            false,
        );
        bus.map(
            "work_ram_low",
            0x0020_0000,
            0x0010_0000,
            0x10_0000,
            Box::new(Ram::new(0x10_0000)),
            true,
        );
        bus.map(
            "minit",
            0x0100_0000,
            0x0080_0000,
            0x1_0000,
            Box::new(OpenBus(0xFF)),
            false,
        );
        bus.map(
            "sinit",
            0x0180_0000,
            0x0080_0000,
            0x1_0000,
            Box::new(OpenBus(0xFF)),
            false,
        );
        bus.map(
            "abus_cs01",
            0x0200_0000,
            0x0300_0000,
            0x1_0000,
            Box::new(OpenBus(0xFF)),
            false,
        );
        let (cd_port, cd) = Shared::new(CdBlock::new());
        bus.map(
            "cd_block",
            0x0580_0000,
            0x0010_0000,
            0x10_0000,
            Box::new(cd_port),
            false,
        );
        let (scsp_ram_port, scsp_ram) = Shared::new(SoundRam::new(0x8_0000));
        bus.map(
            "scsp_ram",
            0x05A0_0000,
            0x0010_0000,
            0x8_0000,
            Box::new(scsp_ram_port),
            false,
        );
        let (scsp_port, scsp) = Shared::new(Scsp::new());
        bus.map(
            "scsp_regs",
            0x05B0_0000,
            0x0010_0000,
            0x1000,
            Box::new(scsp_port),
            false,
        );

        let vdp1 = Rc::new(RefCell::new(Vdp1::new()));
        bus.map(
            "vdp1_vram",
            0x05C0_0000,
            0x0008_0000,
            0x8_0000,
            Box::new(Vdp1Port {
                vdp: vdp1.clone(),
                area: Vdp1Area::Vram,
            }),
            false,
        );
        bus.map(
            "vdp1_fb",
            0x05C8_0000,
            0x0008_0000,
            0x4_0000,
            Box::new(Vdp1Port {
                vdp: vdp1.clone(),
                area: Vdp1Area::Framebuffer,
            }),
            false,
        );
        bus.map(
            "vdp1_regs",
            0x05D0_0000,
            0x0010_0000,
            0x20,
            Box::new(Vdp1Port {
                vdp: vdp1.clone(),
                area: Vdp1Area::Regs,
            }),
            false,
        );

        let vdp2 = Rc::new(RefCell::new(Vdp2::new()));
        bus.map(
            "vdp2_vram",
            0x05E0_0000,
            0x0010_0000,
            0x8_0000,
            Box::new(Vdp2Port {
                vdp: vdp2.clone(),
                area: Vdp2Area::Vram,
            }),
            false,
        );
        bus.map(
            "vdp2_cram",
            0x05F0_0000,
            0x0008_0000,
            0x1000,
            Box::new(Vdp2Port {
                vdp: vdp2.clone(),
                area: Vdp2Area::Cram,
            }),
            false,
        );
        bus.map(
            "vdp2_regs",
            0x05F8_0000,
            0x0004_0000,
            0x200,
            Box::new(Vdp2Port {
                vdp: vdp2.clone(),
                area: Vdp2Area::Regs,
            }),
            false,
        );

        let (scu_port, scu) = Shared::new(Scu::new());
        bus.map(
            "scu",
            0x05FE_0000,
            0x0001_0000,
            0x100,
            Box::new(scu_port),
            false,
        );
        bus.map(
            "work_ram_high",
            0x0600_0000,
            0x0200_0000,
            0x10_0000,
            Box::new(Ram::new(0x10_0000)),
            true,
        );

        let mut space = Sh2AddressSpace::new(bus);
        let cpu = Sh2Cpu::power_on(&mut space);
        Self {
            cpu,
            space,
            scu,
            smpc,
            cd,
            vdp1,
            vdp2,
            scsp_ram,
            sound_enabled: true,
            scsp,
            sound_cpu: SoundCpu::new(),
            audio: Vec::new(),
            timing: VideoTiming::new(),
            stub_log: log,
            frame_cell: Rc::new(Cell::new(0)),
            sink: Box::new(NullSink),
            events: Vec::new(),
            profile: None,
            disc: None,
            cd_cycle_carry: 0,
        }
    }

    pub fn frame(&self) -> u32 {
        self.timing.frame
    }

    /// Executa um bloco JIT e avança o resto do sistema pelos ciclos consumidos.
    pub fn step(&mut self, tracer: Option<&mut dyn Tracer>) -> Result<(), Fault> {
        self.space.onchip.now = self.cpu.st.cycles;
        if let Some(p) = self.profile.as_mut() {
            *p.entry(self.cpu.st.pc).or_insert(0) += 1;
        }
        let cycles = self.cpu.run_block(&mut self.space, tracer)?;
        self.advance(cycles);
        Ok(())
    }

    /// Runs the sound 68000 for the equivalent of the cycles the SH-2 just executed, and
    /// generates the SCSP samples in the same step. Sound never runs ahead of the SH-2.
    fn sound_step(&mut self, sh2_cycles: u64) {
        if !self.sound_enabled || !self.sound_cpu.running {
            return;
        }
        let cycles = {
            let mut ram = self.scsp_ram.borrow_mut();
            let mut scsp = self.scsp.borrow_mut();
            ram.m68k_side = true;
            let c = self.sound_cpu.advance(sh2_cycles, &mut ram, &mut scsp);
            ram.m68k_side = false;
            c
        };
        let mut ram = self.scsp_ram.borrow_mut();
        let mut scsp = self.scsp.borrow_mut();
        scsp.generate(cycles, ram.data_mut(), &mut self.audio);
    }

    fn advance(&mut self, cycles: u64) {
        self.cd_cycle_carry += cycles;
        if self.cd_cycle_carry >= CD_TICK_BATCH_CYCLES {
            self.cd.borrow_mut().tick(self.cd_cycle_carry);
            self.cd_cycle_carry = 0;
        }
        self.timing.advance(cycles, &mut self.events);
        // `mem::take` and drain, not `mem::take` and consume: consuming would hand back an
        // empty, zero-capacity Vec, and the next `timing.advance` would grow it from scratch
        // every single block. Draining keeps the allocation and just empties it.
        if !self.events.is_empty() {
            let mut events = std::mem::take(&mut self.events);
            for ev in events.drain(..) {
                self.on_video_event(ev);
            }
            self.events = events;
        }
        // One borrow of the SMPC for both checks, not two: on most blocks nothing here fires,
        // and re-borrowing a `RefCell` for that is pure overhead run millions of times.
        let (irq_pending, sound_on) = {
            let mut smpc = self.smpc.borrow_mut();
            let irq = std::mem::take(&mut smpc.irq_pending);
            (irq, smpc.sound_on.take())
        };
        if irq_pending {
            self.scu.borrow_mut().raise(IRQ_SMPC);
        }
        if let Some(on) = sound_on {
            let mut ram = self.scsp_ram.borrow_mut();
            let mut scsp = self.scsp.borrow_mut();
            if on {
                self.sound_cpu.power_on(&mut ram, &mut scsp);
            } else {
                self.sound_cpu.power_off();
            }
        }
        self.sound_step(cycles);
        self.service_vdp1();
        self.run_dmas();
        self.deliver_interrupt();
    }

    fn on_video_event(&mut self, ev: VideoEvent) {
        match ev {
            VideoEvent::LineStart(l) => {
                let mut v = self.vdp2.borrow_mut();
                v.hblank = false;
                v.line = l;
            }
            VideoEvent::HBlankIn(_) => {
                self.vdp2.borrow_mut().hblank = true;
                let mut s = self.scu.borrow_mut();
                s.raise(IRQ_HBLANK_IN);
                s.trigger_factor(DMA_FACTOR_HBLANK_IN);
            }
            VideoEvent::VBlankIn => {
                {
                    let mut v = self.vdp2.borrow_mut();
                    v.vblank = true;
                }
                {
                    let mut s = self.scu.borrow_mut();
                    s.raise(IRQ_VBLANK_IN);
                    s.trigger_factor(DMA_FACTOR_VBLANK_IN);
                }
                self.frame_cell.set(self.timing.frame);
                self.sink
                    .on_frame(self.timing.frame, &self.vdp1.borrow(), &self.vdp2.borrow());
            }
            VideoEvent::VBlankOut => {
                {
                    let mut v = self.vdp2.borrow_mut();
                    v.vblank = false;
                    v.frame = self.timing.frame;
                }
                let mut s = self.scu.borrow_mut();
                s.raise(IRQ_VBLANK_OUT);
                s.trigger_factor(DMA_FACTOR_VBLANK_OUT);
            }
        }
    }

    /// VDP1: desenho é instantâneo por enquanto (o rasterizador entra em `video`).
    fn service_vdp1(&mut self) {
        let mut v = self.vdp1.borrow_mut();
        if v.draw_requested {
            v.draw_requested = false;
            v.edsr |= 2;
            drop(v);
            let mut s = self.scu.borrow_mut();
            s.raise(IRQ_SPRITE_END);
            s.trigger_factor(DMA_FACTOR_SPRITE_END);
        }
    }

    fn run_dmas(&mut self) {
        let ready = self.scu.borrow_mut().take_ready_dma();
        for lvl in ready {
            self.scu.borrow_mut().run_dma(lvl, &mut self.space.sys);
        }
        self.scu
            .borrow_mut()
            .run_dsp_if_requested(&mut self.space.sys);
    }

    /// Opens the disc image at `cue` and keeps it. Nothing reads it yet: the CD Block is
    /// still the behavioural stub, and it will only be given the disc once it is rewritten
    /// from the hardware's documented command set.
    pub fn insert_disc(&mut self, cue: &std::path::Path) -> Result<(), String> {
        self.disc = Some(crate::devices::disc::DiscImage::open(cue)?);
        Ok(())
    }

    fn deliver_interrupt(&mut self) {
        let pending = self.scu.borrow().pending_irq();
        // The on-chip DMAC's transfer-end request competes with the SCU's: the higher level
        // wins, and the SCU's on a tie (it was here first).
        let dmac = self.space.onchip.dmac_irq();
        if let Some((level, vector)) = dmac
            && pending.is_none_or(|(l, _, _)| level > l)
            && self.cpu.try_interrupt(&mut self.space, level, vector)
        {
            return;
        }
        if let Some((level, vector, bit)) = pending {
            if self.cpu.try_interrupt(&mut self.space, level, vector) {
                self.scu.borrow_mut().acknowledge(bit);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Content does not matter here: these tests drive `advance()` directly and never run a
    /// JIT block, so the ROM only needs to be the size `Saturn::new` expects to map.
    fn saturn() -> Saturn {
        Saturn::new(&[0u8; 0x8_0000])
    }

    #[test]
    fn the_cd_clock_still_advances_despite_batching() {
        use crate::bus::MemoryDevice;
        let mut s = saturn();
        const HIRQ_OFF: u32 = 0x9_0008;
        const HIRQ_SCDQ: u16 = 0x0400;
        assert_eq!(s.cd.borrow_mut().read_word(HIRQ_OFF) & HIRQ_SCDQ, 0);
        // SCDQ's period is 381_800 cycles; drive well past it in steps far smaller than
        // CD_TICK_BATCH_CYCLES, to prove batching does not drop cycles on the floor.
        for _ in 0..(381_800 / 200 + 10) {
            s.advance(200);
        }
        assert_ne!(
            s.cd.borrow_mut().read_word(HIRQ_OFF) & HIRQ_SCDQ,
            0,
            "SCDQ must still fire even though the CD clock is only fed every few blocks"
        );
    }

    #[test]
    fn advancing_past_a_video_event_does_not_reset_the_event_queues_capacity() {
        let mut s = saturn();
        // One scanline (1820 cycles) crosses at least a LineStart event.
        s.advance(1820);
        let cap_after_first = s.events.capacity();
        assert!(
            cap_after_first > 0,
            "the first batch of video events must have allocated something"
        );
        s.advance(1820);
        assert_eq!(
            s.events.capacity(),
            cap_after_first,
            "the event queue must keep its allocation across steps, not restart at zero every \
             block — that was the whole point of draining instead of consuming it"
        );
    }

    #[test]
    fn a_full_frame_of_stepping_does_not_panic() {
        let mut s = saturn();
        // One frame is 263 lines * 1820 cycles; run a couple to exercise VBlank in and out,
        // the SMPC borrow consolidation, DMA and interrupt delivery along the way.
        for _ in 0..(263 * 1820 * 2 / 256 + 1) {
            s.advance(256);
        }
        assert!(
            s.frame() >= 1,
            "two frames' worth of cycles must have advanced the counter"
        );
    }

    #[test]
    fn a_disc_can_be_inserted_and_a_missing_one_is_an_error() {
        let mut s = saturn();
        assert!(
            s.insert_disc(std::path::Path::new("/nonexistent/x.cue"))
                .is_err()
        );
        assert!(s.disc.is_none());
        let dir = std::env::temp_dir().join(format!("mimas_machine_disc_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("d.bin"), vec![0u8; 2352 * 4]).unwrap();
        std::fs::write(
            dir.join("d.cue"),
            "FILE \"d.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n",
        )
        .unwrap();
        s.insert_disc(&dir.join("d.cue")).unwrap();
        assert_eq!(s.disc.as_ref().unwrap().tracks.len(), 1);
    }

    #[test]
    fn a_dmac_transfer_end_interrupt_is_delivered_at_the_level_ipra_gives_it() {
        use crate::cpu::sh2_bus::Sh2Bus;
        let mut s = saturn();
        s.cpu.st.sr = 0; // interrupts unmasked
        s.cpu.st.r[15] = 0x0600_0400;
        s.space.write32(0x0600_0000, 0x1234);
        s.space.write32(0xFFFF_FF80, 0x0600_0000);
        s.space.write32(0xFFFF_FF84, 0x0600_0100);
        s.space.write32(0xFFFF_FF88, 1);
        s.space.write32(0xFFFF_FFB0, 1);
        s.space.write16(0xFFFF_FEE2, 0x0600);
        s.space.write32(0xFFFF_FFA0, 0x48);
        s.space.write32(
            0xFFFF_FF8C,
            (1 << 14) | (1 << 12) | (2 << 10) | (1 << 9) | 4 | 1,
        );
        s.cpu.st.vbr = 0x0600_1000;
        s.space.write32(0x0600_1000 + 0x48 * 4, 0x0600_2000);
        s.deliver_interrupt();
        assert_eq!(s.cpu.st.pc, 0x0600_2000);
        assert_eq!(s.cpu.st.imask(), 6);
    }
}
