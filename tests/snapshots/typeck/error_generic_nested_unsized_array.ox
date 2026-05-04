fn wrap<T>() -> *mut T {
    null
}

fn main() {
    let p: *mut *mut [i32] = wrap();
}
