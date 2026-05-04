fn id<T>(x: T) -> T {
    x
}

fn main() -> i32 {
    let a = id(42);
    let b = id(7 as u8);
    a + (b as i32)
}
