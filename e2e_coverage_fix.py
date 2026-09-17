with open("e2e-tests/src/lib.rs", "r") as f:
    text = f.read()

import re
text = re.sub(r'let mut sys = SaturnSystem::new\(None\);.*?sys\.start\(1\.0\);', 'let mut sys = SaturnSystem::new();\n    sys.start();', text, flags=re.DOTALL)
text = text.replace("wr.bios.write().unwrap()[i] = b;", "sys.work_ram.bios.write().unwrap()[i] = b;")
text = text.replace("sync.request_shutdown();", "sys.sync.request_shutdown();\n    sys.wait_for_shutdown();")

with open("e2e-tests/src/lib.rs", "w") as f:
    f.write(text)
