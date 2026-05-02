fn write_through_const() {
    let x: i32 = 0;
    let p: *const i32 = &x;
    *p = 1;
}
