fn id<T>(x: T) -> T { x }

fn main() -> i32 {
    id::<i32>(1);
    id::<i32>(2);
    0
}
