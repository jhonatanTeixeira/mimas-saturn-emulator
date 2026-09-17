with open("saturn-core/src/lib.rs", "r") as f:
    text = f.read()

text = text.replace("#![allow(", "#![allow(\n    clippy::bad_bit_mask,\n    clippy::if_same_then_else,\n    clippy::overly_complex_bool_expr,")

with open("saturn-core/src/lib.rs", "w") as f:
    f.write(text)
