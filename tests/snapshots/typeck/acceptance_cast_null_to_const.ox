// `null as *const u8` directly is rejected because `null`'s α
// pointee is unknown at the cast site (option A — `as` requires
// the source type to be fully known; see spec/12_AS.md). The
// idiom is to bind through a typed slot, then cast trivially:
fn f() -> *const u8 {
    let p: *const u8 = null;
    p as *const u8
}
