with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

import re
text = text.replace(
    "let bytes = write_pixel.to_be_bytes();",
    "if current_shape == 4 { println!(\"putpixel x={} y={} current_pixel={:04X} write_pixel={:04X}\", x, y, current_pixel, write_pixel); }\n            let bytes = write_pixel.to_be_bytes();"
)

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

