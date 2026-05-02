// Whole-array assignment via `=`. Worked via `let`-init (memcpy)
// but `=` previously called `.into_int_value()` on the rhs
// PointerValue and panicked. Now both paths go through
// `emit_store_into_slot` and pick memcpy for sized arrays.
fn main() -> i32 {
    let mut arr: [i32; 3] = [0; 3];
    let other:   [i32; 3] = [10, 20, 30];
    arr = other;
    arr[0] + arr[1] + arr[2]   // 60
}
