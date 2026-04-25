fn takes_mut(s: *mut u8) -> i32 { 0 }
fn main(p: *const u8) -> i32 { takes_mut(p) }
