// Two distinct fn-refs flow into separate locals; each gets its own
// Infer set, resolved independently by use-site. Mono produces two
// distinct instances (`id__$i32` and `id__$bool`).
fn id<T>(x: T) -> T { x }

fn main() -> i32 {
    let a = id;
    let b = id;
    let _ = a(1);
    let _ = b(true);
    0
}
