struct Counter { value: i32 }

extern "C" {
    fn read_counter(p: *const Counter) -> i32;
    fn reset_counter(p: *mut Counter);
}

fn snapshot_then_reset() -> i32 {
    let mut c = Counter { value: 42 };
    let v = read_counter(&c);
    reset_counter(&mut c);
    v
}
