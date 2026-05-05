// `null as *const u8` directly fails because the cast does not
// propagate the target pointee back into null's fresh inference
// var. User gets E0256 (cannot infer) plus E0274 (the cast).
// Idiom: bind through a typed slot first.
fn f() -> *const u8 {
    null as *const u8
}
