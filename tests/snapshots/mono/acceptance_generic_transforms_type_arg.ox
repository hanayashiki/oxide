fn make_ptr<T>(x: T) -> *mut T {
    null
}

fn consume_ptr<U>(p: *mut U) {
}

fn wrapper<V>(v: V) {
    let p = make_ptr(v);
    consume_ptr(p);
}

fn main() {
    wrapper(42);
}
