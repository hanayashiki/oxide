fn main() {
    let mut x: i32 = 0;
    let p: *mut i32 = &mut x;
    *p = 42;
}
