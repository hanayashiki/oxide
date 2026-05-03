fn main() -> i32 {
    let x: i32 = 0;
    let a = &x;
    let b = &a;
    let c = &b;
    let d = &c;
    let _ = d;
    0
}
