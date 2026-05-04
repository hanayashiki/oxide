fn build<T>(t: T) -> *mut T {
    null
}

fn main() {
    let p: *mut i32 = build(42);
}
