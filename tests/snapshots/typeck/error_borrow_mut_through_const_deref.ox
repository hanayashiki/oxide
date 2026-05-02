fn borrow_mut_through_const() {
    let x: i32 = 0;
    let p: *const i32 = &x;
    let _q: *mut i32 = &mut *p;
}
