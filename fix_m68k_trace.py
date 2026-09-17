with open("saturn-core/src/m68k.rs", "r") as f:
    text = f.read()

text = text.replace(
    '''static TRACE_RING: std::sync::Mutex<Vec<(u32, u16, u32, u32, u32)>> =
    std::sync::Mutex::new(Vec::new());''',
    '''type TraceEntry = (u32, u16, u32, u32, u32);
static TRACE_RING: std::sync::Mutex<Vec<TraceEntry>> =
    std::sync::Mutex::new(Vec::new());'''
)

with open("saturn-core/src/m68k.rs", "w") as f:
    f.write(text)
