fn process<T>(p: *mut *const T) -> T {
    **p
}

fn main() {
    let p: *mut *const i32 = null;
    process(p);
}
