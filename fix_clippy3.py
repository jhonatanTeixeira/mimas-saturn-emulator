with open("saturn-core/src/lib.rs", "r") as f:
    text = f.read()

text = text.replace("#![allow(", "#![allow(\n    clippy::unnecessary_unwrap,")

with open("saturn-core/src/lib.rs", "w") as f:
    f.write(text)
