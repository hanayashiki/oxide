// `>>` lexes as `JointGt Gt`; the inner `Node<T>` close eats `JointGt`, the
// outer turbofish close eats `Gt`. Previously this required `Node<T> >`.
fn main() { let node = ox_alloc::<Node<T>>(); }
