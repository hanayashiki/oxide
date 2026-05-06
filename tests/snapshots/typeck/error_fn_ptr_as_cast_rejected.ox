// Wrong-direction Fn-Fn cast: `fn(*mut i32) -> i32` is NOT a subtype
// of `fn(*const i32) -> i32` (would require `*const → *mut` on the
// param side). Discharge fires PointerMutabilityMismatch via the
// subtype routing per spec/19_FN_PTR.md §5.
fn writer(p: *mut i32) -> i32 { 0 }

fn main() -> i32 {
    let f: fn(*mut i32) -> i32 = writer;
    let g = f as fn(*const i32) -> i32;
    0
}
