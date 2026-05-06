// Generic fn-ref with no use site: Infer ?T0 stays unbound at
// finalize → E0256 CannotInfer. (Same diagnostic the user gets for
// any other under-determined Infer.)
fn id<T>(x: T) -> T { x }

fn main() -> i32 {
    let _ = id;
    0
}
