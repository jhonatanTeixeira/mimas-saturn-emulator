with open("saturn-core/src/telemetry.rs", "r") as f:
    text = f.read()
text = text.replace(
    "for i in 0..8 {\n        let idle_ns = THREAD_IDLE_NS[i].load(Ordering::Relaxed);",
    "for (i, idle) in THREAD_IDLE_NS.iter().enumerate() {\n        let idle_ns = idle.load(Ordering::Relaxed);"
)
with open("saturn-core/src/telemetry.rs", "w") as f:
    f.write(text)

with open("saturn-core/src/sh2.rs", "r") as f:
    text = f.read()
text = text.replace(
    """                    for i in 0..64 {
                        let actual = cpu.read_byte(dst_base + i as u32);
                        assert_eq!(
                            actual, expected_dst[i],""",
    """                    for (i, &expected) in expected_dst.iter().enumerate() {
                        let actual = cpu.read_byte(dst_base + i as u32);
                        assert_eq!(
                            actual, expected,"""
)
with open("saturn-core/src/sh2.rs", "w") as f:
    f.write(text)

