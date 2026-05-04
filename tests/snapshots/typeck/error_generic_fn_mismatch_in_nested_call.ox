fn outer<T>(x: T, y: i32) -> T {
    inner(y)
}

fn inner<U>(u: U) -> U {
    u
}

fn main() {
    outer(42, 7);
}
