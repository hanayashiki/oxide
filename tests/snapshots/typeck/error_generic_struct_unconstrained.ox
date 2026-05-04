struct Wrap<T> {
    v: *mut T,
}

fn main() {
    let w = Wrap { v: null };
}
