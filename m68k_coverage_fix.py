with open("saturn-core/src/m68k.rs", "r") as f:
    text = f.read()

text = text.replace("let mut cpu = M68k::new();", "let mut cpu = M68k::new(work_ram.clone());")
text = text.replace("let work_ram = Arc::new(WorkRam::new());", "let work_ram = Arc::new(WorkRam::new());\n        let mut cpu = M68k::new(work_ram.clone());")
text = text.replace("let mut cpu = M68k::new(work_ram.clone());\n        let work_ram = Arc::new(WorkRam::new());\n        let mut cpu = M68k::new(work_ram.clone());", "let work_ram = Arc::new(WorkRam::new());\n        let mut cpu = M68k::new(work_ram.clone());")
text = text.replace("cpu.step(&work_ram);", "cpu.step();")

with open("saturn-core/src/m68k.rs", "w") as f:
    f.write(text)
