fn takes_const(s: *const u8) -> i32 { 0 }
fn main(p: *mut u8) -> i32 { takes_const(p) }
