extern "C" {
    fn use_a(p: *const *const u8);
    fn use_b(p: *const *mut u8);
}

fn bad() {
    let n = null;
    use_a(n);
    use_b(n);
}
