fn id<T>(x: T) -> T {
    x
}

fn main() -> i32 {
    id::<*mut *const i32>(null);
    0
}
