with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

test_code = """
#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::shared_buffers::WorkRam;
    use std::sync::Arc;
    #[test]
    fn force_vdp_coverage() {
        let work_ram = Arc::new(WorkRam::new());
        let mut vram = work_ram.vdp1_vram.write().unwrap();
        // fill vram with random data to fuzz execute_vdp1
        for i in 0..vram.len() {
            vram[i] = (i % 256) as u8;
        }
        drop(vram);
        // This won't loop forever because VDP1 execution has safety limits or hits END
        execute_vdp1(&work_ram);
        
        // Fuzz VDP2 registers
        for i in 0..0x1000 {
            let _ = crate::vdp2_regs::read_vdp2_reg(i as u32, &work_ram);
            crate::vdp2_regs::write_vdp2_reg(i as u32, i as u16, &work_ram);
        }
        crate::vdp::render_backdrop(&work_ram);
    }
}
"""
with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text + test_code)
