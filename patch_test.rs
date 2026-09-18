#[test]
fn sound_ram_test() {
    let mut cpu = make_cpu();
    cpu.work_ram.mem4b.store(false, std::sync::atomic::Ordering::Relaxed);
    cpu.write_long(0x25A00000, 0x12345678);
    assert_eq!(cpu.read_long(0x25A00000), 0x12345678);
}
