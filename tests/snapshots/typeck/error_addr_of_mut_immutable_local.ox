struct Counter { value: i32 }

extern "C" {
    fn reset(p: *mut Counter);
}

fn bad() {
    let c = Counter { value: 0 };
    reset(&mut c);
}
