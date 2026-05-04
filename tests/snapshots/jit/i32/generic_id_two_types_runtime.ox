fn id<T>(x: T) -> T { x }

fn add_via_id(a: i32, b: i32) -> i32 {
    id::<i32>(a) + id::<i32>(b)
}

fn main() -> i32 {
    add_via_id(40, 2)
}
