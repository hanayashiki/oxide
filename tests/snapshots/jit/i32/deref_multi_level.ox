fn main() -> i32 {
    let mut x: i32 = 0;
    let mut p: *mut i32 = &mut x;
    let pp: *mut *mut i32 = &mut p;
    **pp = 99;
    **pp
}
