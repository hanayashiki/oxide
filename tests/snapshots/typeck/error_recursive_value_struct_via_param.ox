struct B<T> {
    y: T,
}

struct A {
    x: B<A>,
}
