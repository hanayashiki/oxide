struct Wrap<T> {
    v: T,
}

fn main() -> i32 {
    let w = Wrap { v: 7 };
    w.v
}
