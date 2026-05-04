struct Wrap<T> {
    v: T,
}

fn deref<T>(p: *mut Wrap<T>) -> T {
    (*p).v
}

fn main() -> i32 {
    let mut w = Wrap::<i32> { v: 42 };
    deref(&mut w)
}
