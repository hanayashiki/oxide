fn id<T>(x: T) -> T { x }

fn left() -> i32 { id::<i32>(1) }
fn right() -> i32 { id::<i32>(2) }

fn main() -> i32 {
    left() + right()
}
