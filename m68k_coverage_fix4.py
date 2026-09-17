with open("saturn-core/src/m68k.rs", "r") as f:
    text = f.read()

text = text.replace("let mut cpu = M68k::new(work_ram.clone());\n        // Run through all opcodes", "let mut cpu = M68k::new(work_ram.clone());\n        cpu.running = true;\n        // Run through all opcodes")

with open("saturn-core/src/m68k.rs", "w") as f:
    f.write(text)
