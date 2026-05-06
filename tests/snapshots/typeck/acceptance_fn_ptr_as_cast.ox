// Fn-Fn `as` cast routes through subtype rules per spec/19_FN_PTR.md §5.
// `fn(*const i32) -> i32` is a subtype of `fn(*mut i32) -> i32` (param
// contravariance), so this cast is accepted.
fn read_only(p: *const i32) -> i32 { 0 }

fn main() -> i32 {
    let f: fn(*const i32) -> i32 = read_only;
    let g = f as fn(*mut i32) -> i32;
    0
}
