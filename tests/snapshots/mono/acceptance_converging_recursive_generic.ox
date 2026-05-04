fn id<T>(x: T) -> T { x }

fn wrapper<T>() {
    id::<*mut T>(null);
}

fn main() -> i32 {
    wrapper::<i32>();
    0
}