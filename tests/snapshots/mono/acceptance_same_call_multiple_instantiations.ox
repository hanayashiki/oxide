fn id<T>(x: T) -> T {
    x
}

fn f(x: i32) -> i32 {
    id(x)
}

fn g(y: u8) -> u8 {
    id(y)
}

fn main() -> i32 {
    f(1) as i32
}
