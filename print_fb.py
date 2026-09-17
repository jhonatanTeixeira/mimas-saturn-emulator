with open("saturn-core/src/vdp.rs", "r") as f:
    text = f.read()

import re

def insert_print(name):
    global text
    match = re.search(r'fn ' + name + r'\(\) \{.*?(?=#\[test\]|\n})', text, flags=re.DOTALL)
    if not match: return
    body = match.group(0)
    
    # insert a print loop before assert identical
    print_code = """
        let mut drew_anything = false;
        for (i, val) in fb2.iter().enumerate() {
            if *val != 0 {
                println!("fb2[{}] = {}", i, val);
                drew_anything = true;
                if i > 1000 { break; }
            }
        }
        if !drew_anything {
            println!("fb2 is COMPLETELY EMPTY");
        }
    """
    body = body.replace('// assert identical', print_code + '\n        // assert identical')
    
    text = text[:match.start()] + body + text[match.end():]

insert_print("vdp1_polygon_and_distorted_sprite_have_identical_geometry")

with open("saturn-core/src/vdp.rs", "w") as f:
    f.write(text)

