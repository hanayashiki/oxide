// `ox_alloc::<Node<T>>()` — the closing `>>` is `JointGt Gt` after the
// per-character `>` lex change. Inner `Node<T>` close eats `JointGt`,
// outer turbofish close eats `Gt`. Confirms the full lex→parse→HIR-lower
// path handles nested generic turbofish without forcing a space.
import "mem.ox";

struct Node<T> { v: T }

fn alloc_node<T>() -> *mut Node<T> {
    ox_alloc::<Node<T>>()
}

fn main() -> i32 {
    let p = alloc_node::<i32>();
    ox_dealloc::<Node<i32>>(p);
    0
}
