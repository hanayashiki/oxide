fn f<T>(x: T) -> T {
    g(x)
}

fn g<U>(y: U) -> U {
    h::<*mut U>(null);
    y
}

fn h<V>(z: V) -> V {
    z
}

fn main() -> i32 {
    f(42)
}
