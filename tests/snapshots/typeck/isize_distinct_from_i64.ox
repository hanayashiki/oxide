extern "C" {
    fn make_offset() -> isize;
}

fn f() -> i32 {
    let x: i64 = make_offset();
    0
}
