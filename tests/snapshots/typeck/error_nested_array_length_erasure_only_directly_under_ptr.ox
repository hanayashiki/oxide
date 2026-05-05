// Length erasure `[T; N] → [T]` is gated to *directly* under a Ptr.
// `*const [[i32; 2]; 3]` should NOT coerce to `*const [[i32]; 3]`:
// the outer Array layer separates the inner `[i32; 2]` from the
// Ptr, so the inner-array erasure isn't permitted by spec/09.
fn want(p: *const [[i32]; 3]) -> i32 { 0 }

fn caller(q: *const [[i32; 2]; 3]) -> i32 {
    want(q)
}
