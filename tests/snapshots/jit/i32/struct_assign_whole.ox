// Whole-struct assignment via `=`. Pre-fix: emit_assign called
// `.into_int_value()` on the rhs StructValue and panicked.
struct S { a: i32, b: i32 }

fn main() -> i32 {
    let mut s = S { a: 1, b: 2 };
    s = S { a: 9, b: 8 };
    s.a + s.b   // 17
}
