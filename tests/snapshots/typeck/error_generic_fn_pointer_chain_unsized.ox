fn wrapper<T>() -> *mut T {
    null
}

fn main() {
    let p: *mut [i32] = wrapper();
}
