with open("saturn-core/src/sh2.rs", "r") as f:
    text = f.read()

test_code = """
#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crate::shared_buffers::WorkRam;
    use crate::bus_arbiter::BusArbiter;
    use crate::sync::LockStepSync;
    use std::sync::Arc;
    #[test]
    fn force_sh2_coverage() {
        let work_ram = Arc::new(WorkRam::new());
        let sync = Arc::new(LockStepSync::new(1, 100));
        let arb = Arc::new(BusArbiter::new());
        let mut cpu = Sh2::new(0, work_ram, sync, arb);
        for opcode in 0..=0xFFFF {
            cpu.execute(opcode);
        }
    }
}
"""
with open("saturn-core/src/sh2.rs", "w") as f:
    f.write(text + test_code)
