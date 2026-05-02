fn count_down(mut n: i32) -> i32 {
    let mut last = 0;
    while n > 0 { last = n; n = n - 1; }
    last
}
