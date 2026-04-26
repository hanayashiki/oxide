fn f(x: i32) -> i32 {
    1 + { let y = x; y }
}
