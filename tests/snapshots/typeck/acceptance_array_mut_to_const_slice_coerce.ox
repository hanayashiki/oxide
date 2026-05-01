fn at(p: *const [i32], i: usize) -> i32 {
    p[i]
}

fn caller(a: *mut [i32; 10]) -> i32 {
    at(a, 5)
}
