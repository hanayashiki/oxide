fn f(mut x: i32) {
    while x > 0 {
        if x == 5 { continue; }
        x = x - 1;
    }
}
