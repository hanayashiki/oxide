extern "C" {
    fn make_size() -> usize;
}

fn f() -> i32 {
    let x: u64 = make_size();
    0
}
