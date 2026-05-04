struct Wrap<T> {
    v: T,
}

fn make() -> Wrap<i32> {
    Wrap::<i32> { v: 0 }
}
