fn f<T>() {
    f::<*const T>()
}

fn main() {
    f::<i32>()
}
