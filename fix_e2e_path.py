with open("e2e-tests/src/lib.rs", "r") as f:
    text = f.read()

text = text.replace('"../target/debug/saturn-frontend-native",', '"../target/debug/saturn-frontend-native",\n            "../target/release/saturn-frontend-native",')
text = text.replace('"../../target/debug/saturn-frontend-native",', '"../../target/debug/saturn-frontend-native",\n            "../../target/release/saturn-frontend-native",')
text = text.replace('"target/debug/saturn-frontend-native",', '"target/debug/saturn-frontend-native",\n            "target/release/saturn-frontend-native",')

with open("e2e-tests/src/lib.rs", "w") as f:
    f.write(text)
