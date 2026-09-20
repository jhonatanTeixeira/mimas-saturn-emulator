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
use crate::devices::scu::*;
use crate::devices::smpc::Smpc;
use crate::devices::stub::{AccessLog, OpenBus, RegisterStub, SharedLog};
use crate::devices::vdp1::{Vdp1, Vdp1Area, Vdp1Port};
use crate::devices::vdp2::{Vdp2, Vdp2Area, Vdp2Port};
use crate::timing::{VideoEvent, VideoTiming};
use crate::video::{FrameSink, NullSink};

pub struct Saturn {
    pub cpu: Sh2Cpu,
    pub space: Sh2AddressSpace,
    pub scu: Rc<RefCell<Scu>>,
    pub smpc: Rc<RefCell<Smpc>>,
    pub cd: Rc<RefCell<CdBlock>>,
    pub vdp1: Rc<RefCell<Vdp1>>,
    pub vdp2: Rc<RefCell<Vdp2>>,
    pub scsp_ram: Rc<RefCell<SoundRam>>,
    pub timing: VideoTiming,
    pub stub_log: SharedLog,
    /// Contador de quadros visível a quem observa (ex.: verificador de traces).
    pub frame_cell: Rc<Cell<u32>>,
    pub sink: Box<dyn FrameSink>,
    events: Vec<VideoEvent>,
    /// Contagem de execuções por PC de entrada de bloco (só quando o profiler está ligado).
    pub profile: Option<std::collections::HashMap<u32, u64>>,
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
        bus.map(
            "scsp_regs",
            0x05B0_0000,
            0x0010_0000,
            0x1_0000,
            Box::new(RegisterStub::new("scsp_regs", 0x1_0000).with_log(log.clone())),
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
            timing: VideoTiming::new(),
            stub_log: log,
            frame_cell: Rc::new(Cell::new(0)),
            sink: Box::new(NullSink),
            events: Vec::new(),
            profile: None,
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

    fn advance(&mut self, cycles: u64) {
        self.cd.borrow_mut().tick(cycles);
        self.timing.advance(cycles, &mut self.events);
        let events = std::mem::take(&mut self.events);
        for ev in events {
            self.on_video_event(ev);
        }
        if self.smpc.borrow().irq_pending {
            self.smpc.borrow_mut().irq_pending = false;
            self.scu.borrow_mut().raise(IRQ_SMPC);
        }
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

    fn deliver_interrupt(&mut self) {
        let pending = self.scu.borrow().pending_irq();
        if let Some((level, vector, bit)) = pending {
            if self.cpu.try_interrupt(&mut self.space, level, vector) {
                self.scu.borrow_mut().acknowledge(bit);
            }
        }
    }
}
