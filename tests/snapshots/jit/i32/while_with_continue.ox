fn main() -> i32 {
    let mut s = 0;
    let mut i = 0;
    while i < 10 {
        i = i + 1;
        if i == 5 { continue; }
        s = s + i;
    }
    s
}
