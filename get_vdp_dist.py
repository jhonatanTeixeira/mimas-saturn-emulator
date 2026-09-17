with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

lines = text.split("\n")
for i, line in enumerate(lines):
    if "current_shape == 2" in line or "current_shape == 3" in line:
        for j in range(i, min(i+40, len(lines))):
            print(lines[j])
        break
