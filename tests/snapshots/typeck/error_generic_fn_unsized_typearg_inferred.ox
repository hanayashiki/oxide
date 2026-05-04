fn marker<T>() -> *mut T { null }
fn main() { let p: *mut [i32] = marker(); }
