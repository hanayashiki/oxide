fn main() -> i32 {
    let x: i32 = 7;
    let p: *const i32 = &x;
    let v: i32 = *p;
    v
}
