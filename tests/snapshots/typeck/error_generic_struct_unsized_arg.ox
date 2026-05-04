struct Wrap<T> {
    v: *mut T,
}

fn main() {
    let w = Wrap::<[i32]> { v: null };
}
