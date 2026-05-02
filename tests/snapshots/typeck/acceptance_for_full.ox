fn sum_to(n: i32) -> i32 {
    let mut s = 0;
    for (let mut i = 0; i < n; i = i + 1) { s = s + i; }
    s
}
