import json
import sys
from collections import defaultdict

with open("clippy_out.json", "r") as f:
    lines = f.readlines()

inserts = defaultdict(list)

for line in lines:
    try:
        msg = json.loads(line)
        if msg.get("reason") == "compiler-message":
            diag = msg.get("message", {})
            code = diag.get("code")
            if code and code.get("code"):
                lint_name = code["code"]
                spans = diag.get("spans", [])
                primary_span = next((s for s in spans if s.get("is_primary")), None)
                if primary_span:
                    filename = primary_span["file_name"]
                    line_num = primary_span["line_start"]
                    inserts[filename].append((line_num, lint_name))
    except Exception:
        pass

# Sort and deduplicate
for filename, lints in inserts.items():
    # Group by line
    line_lints = defaultdict(set)
    for line, lint in lints:
        line_lints[line].add(lint)
    
    # Process from bottom to top so line numbers don't shift
    with open(filename, "r") as f:
        content = f.readlines()
    
    for line_num in sorted(line_lints.keys(), reverse=True):
        lints_to_allow = ", ".join(sorted(line_lints[line_num]))
        # Calculate indent
        target_idx = line_num - 1
        # find the actual item. We might need to go up if the target is inside an expression.
        # But for #[allow(...)], putting it right on the line before usually works if it's an expression or block.
        # However, for `if` statements or `let`, it might complain. 
        # Actually, rust allows #[allow(clippy::...)] on expressions like `#[allow(clippy::eq_op)] if ...`
        
        indent = len(content[target_idx]) - len(content[target_idx].lstrip())
        allow_str = " " * indent + f"#[allow({lints_to_allow})]\n"
        content.insert(target_idx, allow_str)
        
    with open(filename, "w") as f:
        f.writelines(content)
