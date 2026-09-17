import glob

test_files = glob.glob("saturn-core/tests/*.rs")

for f in test_files:
    with open(f, "r") as file:
        text = file.read()
    if "#![allow(" not in text:
        text = "#![allow(clippy::field_reassign_with_default)]\n" + text
        with open(f, "w") as file:
            file.write(text)
