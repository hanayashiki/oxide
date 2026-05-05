fn f() -> u32 {
    let x: u32 = 1;
    let p: *const u8 = null;
    x << p
}
