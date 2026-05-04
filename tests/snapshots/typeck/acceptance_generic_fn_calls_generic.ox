fn outer<U>(x: U) -> U { inner(x) }
fn inner<T>(x: T) -> T { x }
