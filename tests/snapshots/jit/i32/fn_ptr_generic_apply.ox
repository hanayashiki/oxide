// End-to-end: a generic fn referenced as a value, then called.
// `let f = id;` lifts via spec/19 §F1 (fresh Infer per generic param,
// recorded on the fn-ref eid). `f(42)` pins ?T0 = i32. Mono cascades
// through walk_expr's bare-Fn arm to instantiate `id__$i32`. Codegen
// emits `ptr @id__$i32` and an indirect call. Result: 42.
fn id<T>(x: T) -> T { x }

fn main() -> i32 {
    let f = id;
    f(42)
}
