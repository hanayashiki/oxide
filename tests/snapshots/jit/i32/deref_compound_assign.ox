fn main() -> i32 {
    let mut x: i32 = 41;
    let p: *mut i32 = &mut x;
    *p += 1;
    *p
}
