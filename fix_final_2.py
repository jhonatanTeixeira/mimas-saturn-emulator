import re

def rewrite(file):
    with open(file, "r") as f:
        text = f.read()

    # telemetry.rs
    text = re.sub(r'for i in 0\.\.8 \{\s*ns\[i\] = THREAD_IDLE_NS\[i\]\.swap\(0, Ordering::Relaxed\);\s*\}',
                  r'for (i, idle) in THREAD_IDLE_NS.iter().enumerate() {\n        ns[i] = idle.swap(0, Ordering::Relaxed);\n    }', text)
    
    # sh2.rs loop
    text = re.sub(r'for i in 0\.\.64 \{\s*expected_dst\[i\] = m68k\.ram\[src_addr as usize \+ i\];\s*\}',
                  r'for (i, val) in expected_dst.iter_mut().enumerate() {\n                    *val = m68k.ram[src_addr as usize + i];\n                }', text)
    
    # sh2.rs identical blocks
    text = re.sub(r'if sum > SAT_MAX \{\s*sum = if mul < 0 \{ SAT_MIN \} else \{ SAT_MAX \};\s*\} else if sum < SAT_MIN \{\s*sum = if mul < 0 \{ SAT_MIN \} else \{ SAT_MAX \};\s*\}',
                  r'if !(SAT_MIN..=SAT_MAX).contains(&sum) {\n                        sum = if mul < 0 { SAT_MIN } else { SAT_MAX };\n                    }', text)
    
    text = re.sub(r'if sum > SAT_MAX \{\s*self\.mach \|= 1;\s*self\.macl = if mul < 0 \{\s*SAT_MIN as u32\s*\} else \{\s*SAT_MAX as u32\s*\};\s*\} else if sum < SAT_MIN \{\s*self\.mach \|= 1;\s*self\.macl = if mul < 0 \{\s*SAT_MIN as u32\s*\} else \{\s*SAT_MAX as u32\s*\};\s*\}',
                  r'if !(SAT_MIN..=SAT_MAX).contains(&sum) {\n                        self.mach |= 1;\n                        self.macl = if mul < 0 {\n                            SAT_MIN as u32\n                        } else {\n                            SAT_MAX as u32\n                        };\n                    }', text)

    with open(file, "w") as f:
        f.write(text)

rewrite("saturn-core/src/telemetry.rs")
rewrite("saturn-core/src/sh2.rs")

