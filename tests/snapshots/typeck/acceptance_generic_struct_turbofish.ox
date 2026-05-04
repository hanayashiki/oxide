struct Wrap<T> {
    v: T,
}

fn main() {
    let w = Wrap::<i32> { v: 0 };
}
