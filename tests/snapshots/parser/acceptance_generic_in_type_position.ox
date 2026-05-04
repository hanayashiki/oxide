struct LinkedList<T> {
    value: T,
    next: *mut LinkedList<T>,
}
