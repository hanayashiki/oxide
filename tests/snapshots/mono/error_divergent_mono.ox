fn f<T>() {
    f::<*mut T>()
}

fn main() {
    f::<i32>();
}
