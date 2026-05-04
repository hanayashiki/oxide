fn f<T>() {
    f::<[T; 2]>()
}

fn main() {
    f::<i32>()
}
