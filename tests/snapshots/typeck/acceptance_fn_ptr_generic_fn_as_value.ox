// spec/19_FN_PTR.md §F1 lift: a generic fn can be referenced as a
// value. The Infer minted at the fn-ref site gets pinned by the use
// site (`f(42)` constrains it to `i32`).
fn id<T>(x: T) -> T { x }

fn main() -> i32 {
    let f = id;
    f(42)
}
