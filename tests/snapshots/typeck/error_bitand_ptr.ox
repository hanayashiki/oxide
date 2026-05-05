fn f() {
    let p: *const u8 = null;
    let q: *const u8 = null;
    let _: *const u8 = p & q;
}
