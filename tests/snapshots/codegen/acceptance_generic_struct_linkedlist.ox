struct LinkedList<T> {
    value: T,
    next: *mut LinkedList<T>,
}

fn main() -> i32 {
    let head = LinkedList::<i32> { value: 7, next: null };
    head.value
}
