with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

lines = text.split("\n")
for i, line in enumerate(lines):
    if "current_shape ==" in line or "current_shape == 1" in line:
        for j in range(i, min(i+80, len(lines))):
            print(lines[j])
        break
