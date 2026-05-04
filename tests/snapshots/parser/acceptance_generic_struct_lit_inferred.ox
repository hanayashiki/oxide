struct Wrap<T> {
    v: T,
}

fn make() -> Wrap<i32> {
    Wrap { v: 0 }
}
