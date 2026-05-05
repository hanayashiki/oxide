// `*const [[i32]; 3]` is ill-formed: the *sized* outer array
// `[_; 3]` requires elements with known stride, but `[i32]` is
// unsized. The DST relaxation only applies one level deep — when
// the immediate pointee is unsized (`*const [T]`). When the
// pointee is a sized container, its components must be sized
// recursively. discharge_sized fires E0269 on the inner `[i32]`.
fn want(p: *const [[i32]; 3]) -> i32 {
    0
}
