// `let f = id; f(42)` lowers to a pointer to the `id__$i32` instance,
// then an indirect call. LLVM mem2reg + early-cse will collapse the
// alloca/load/indirect-call sequence to a direct call in optimized
// builds; the unoptimized IR keeps the indirect form so we can see
// the lowering shape.
fn id<T>(x: T) -> T { x }

fn main() -> i32 {
    let f = id;
    f(42)
}
