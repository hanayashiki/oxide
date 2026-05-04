fn id<T>(x: T) -> T {
    x
}

fn main() -> i32 {
    id::<*mut [*const i32; 5]>(null);
    0
}
