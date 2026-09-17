with open("saturn-core/src/scu.rs", "r") as f:
    text = f.read()

text = text.replace(
    '''        #[allow(clippy::type_complexity)]
        let cases: &[(&str, fn(&Scu), u8, u8, u32)] = &[''',
    '''        type TestCase<'a> = (&'a str, fn(&Scu), u8, u8, u32);
        let cases: &[TestCase] = &['''
)

with open("saturn-core/src/scu.rs", "w") as f:
    f.write(text)
