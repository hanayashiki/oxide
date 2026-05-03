// B008: the Sized obligation discharge is recursive. A nested
// unsized inside a sized outer (`[[u8]; 3]` — outer `Some(3)`,
// inner `None`) used to slip past the shallow check (which only
// inspected the outer kind), causing a codegen ICE. The recursive
// `discharge_sized` walks the array element type and rejects the
// inner `[u8]`. See spec/BACKLOG/B008.

struct S { f: [[u8]; 3] }
fn main() -> i32 { 0 }
