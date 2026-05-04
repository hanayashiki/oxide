fn deref<T>(p: *mut T) -> T {
    *p
}

fn main() {
    let p: *mut i32 = null;
    deref(p);
}
