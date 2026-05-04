fn id<T>(x: T) -> T { x }
fn unsized_p() -> *const [i32] { null }
fn main() { id(*(unsized_p())); }
