fn outer<U>(x: U) -> U { inner(x) }
fn inner<T>(x: T) -> T { x }

fn main() -> i32 {
    outer::<i32>(1)
}
