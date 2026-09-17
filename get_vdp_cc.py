with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

lines = text.split("\n")
for i, line in enumerate(lines):
    if "match colour_calc {" in line:
        for j in range(i, min(i+40, len(lines))):
            print(lines[j])
        break
