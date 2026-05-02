fn write_through_const_with_mut_local() {
    let mut x: i32 = 0;
    let p: *const i32 = &x;
    *p = 1;
}
