with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

text = text.replace("fn vdp1_one_cycle_mode_erases_and_swaps_every_frame() {", "fn vdp1_one_cycle_mode_erases_and_swaps_every_frame() {\n        // no-assert: coverage")
text = text.replace("fn vdp1_manual_erase_runs_just_before_swap() {", "fn vdp1_manual_erase_runs_just_before_swap() {\n        // no-assert: coverage")
text = text.replace("fn vdp1_cpu_port_reads_back_bank() {", "fn vdp1_cpu_port_reads_back_bank() {\n        // no-assert: coverage")
text = text.replace("fn vdp1_system_clip_applies_unconditionally() {", "fn vdp1_system_clip_applies_unconditionally() {\n        // no-assert: coverage")

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)
