struct LinkedList<T> {
    value: T,
    next: *mut LinkedList<T>,
}

fn main() {
    let p: *mut LinkedList<i32> = null;
}
