with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

import re
match = re.search(r'let mut tex_x_acc = 0;\n\s+for x in lx\.\.=rx \{', text)
if match:
    insert = """
            if current_shape == 2 {
                // println!("distorted x={} y={} off={} tex_x={} tex_y={} pixel={:04X} lx={} rx={}", x, y, off, tex_x, tex_y, current_pixel, lx, rx);
            }
    """
    # actually I can just add a println inside the `else { current_pixel = cmd.cmdcolr; }` ? No, inside the drawing part.
    
    # Let's just find `putpixel(x, y, ...)`
    match2 = re.search(r'let fb_idx = \(y_actual \* fb_width \+ x\) \* 2;', text)
    if match2:
        text = text[:match2.start()] + 'if current_shape == 2 { println!("drawing distorted x={} y={} pixel={:04X}", x, y_actual, current_pixel); }\n            ' + text[match2.start():]

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

