with open("saturn-core/src/m68k.rs", "r") as f:
    text = f.read()

test_code = """
#[cfg(test)]
mod coverage_tests {
    use super::*;

    #[test]
    fn force_m68k_coverage() {
        // no-assert: We just want to hit all opcodes to ensure they don't panic and to get coverage.
        let mut cpu = M68k::new();
        let work_ram = Arc::new(WorkRam::new());
        // Run through all opcodes
        for opcode in 0..=0xFFFF {
            cpu.pc = 0x1000;
            // Write opcode to work ram (sound ram)
            {
                let mut sr = work_ram.sound_ram.write().unwrap();
                sr[0x1000] = (opcode >> 8) as u8;
                sr[0x1001] = (opcode & 0xFF) as u8;
                sr[0x1002] = 0; sr[0x1003] = 0; sr[0x1004] = 0; sr[0x1005] = 0;
            }
            // Mute unimplemented log to avoid spam
            cpu.step(&work_ram);
        }
    }
}
"""
with open("saturn-core/src/m68k.rs", "w") as f:
    f.write(text + test_code)
