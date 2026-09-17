import subprocess
import re
import sys

def fix_errors():
    res = subprocess.run(["cargo", "check", "-p", "saturn-core"], capture_output=True, text=True)
    lines = res.stderr.splitlines()
    fixes = {}
    
    current_file = None
    current_line = None
    current_col = None
    
    for line in lines:
        m = re.search(r'--> saturn-core/src/(vdp2?\.rs):(\d+):(\d+)', line)
        if m:
            current_file = "saturn-core/src/" + m.group(1)
            current_line = int(m.group(2))
            current_col = int(m.group(3))
        
        if "cannot find value `" in line and current_file:
            var_name = re.search(r"cannot find value `(\w+)`", line).group(1)
            if current_file not in fixes:
                fixes[current_file] = []
            fixes[current_file].append((current_line, current_col, var_name))
            current_file = None
            
        if "no field `vram_8mbit` on type `Vdp2State`" in line and current_file:
            fixes[current_file].append((current_line, current_col, "vram_8mbit"))
            current_file = None

    if not fixes:
        print("No more errors!")
        return False
        
    for file, items in fixes.items():
        with open(file, "r") as f:
            lines_text = f.read().splitlines()
        
        # apply fixes from bottom to top so columns don't shift
        items.sort(key=lambda x: (x[0], x[1]), reverse=True)
        for line_num, col, var in items:
            idx = line_num - 1
            if var == "vram_8mbit" and file.endswith("vdp.rs"):
                lines_text[idx] = lines_text[idx].replace("state.vram_8mbit", "vram_8mbit")
            else:
                l = lines_text[idx]
                lines_text[idx] = l[:col-1] + "cfg." + l[col-1:]
        
        with open(file, "w") as f:
            f.write("\n".join(lines_text) + "\n")
    return True

while fix_errors():
    pass
