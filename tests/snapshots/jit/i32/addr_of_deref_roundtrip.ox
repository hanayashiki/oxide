fn main() -> i32 {
    let mut x: i32 = 5;
    let p: *mut i32 = &mut x;
    let q: *mut i32 = &mut *p;
    *q = 17;
    *q
}
