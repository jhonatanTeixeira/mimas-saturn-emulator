with open("e2e-tests/src/lib.rs", "r") as f:
    text = f.read()
    
# Clean up the extra } 
lines = text.split("\n")
if lines[-1].strip() == "}":
    # Wait, let's just make sure there is no extra } at the end if it's invalid.
    pass

import subprocess
with open("e2e-tests/src/lib.rs", "w") as f:
    f.write(text.replace("sys.shutdown();\n}\n}", "sys.shutdown();\n}"))
