fn f(s: *const *const u8) -> i32 { 0 }
fn main(p: *const *mut u8) -> i32 { f(p) }
