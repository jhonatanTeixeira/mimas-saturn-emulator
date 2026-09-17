with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

import re
match = re.search(r'let total = len_left\.max\(len_right\);', text)
if match:
    insert = """
    let total = len_left.max(len_right);
    if current_shape == 2 {
        println!("distorted tl={:?} tr={:?} bl={:?} br={?} len_left={} len_right={} total={}", tl, tr, bl, br, len_left, len_right, total);
        println!("char_width={} char_height={} char_base={:X}", char_width, char_height, char_base);
    }
    """
    text = text[:match.start()] + insert + text[match.end():]

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

