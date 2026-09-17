with open("saturn-core/src/sh2.rs", "r") as f:
    text = f.read()

text = text.replace("let mut cpu = Sh2::new(0, work_ram, sync, arb);", "let mut cpu = Sh2::new(false, arb, work_ram);")

with open("saturn-core/src/sh2.rs", "w") as f:
    f.write(text)
