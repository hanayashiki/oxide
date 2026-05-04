fn root<T>(x: T) -> T {
    path_a::<T>(x);
    path_b::<T>(x);
    x
}

fn path_a<T>(x: T) -> T {
    leaf::<*mut T>();
    x
}

fn path_b<T>(x: T) -> T {
    leaf::<*mut T>();
    x
}

fn leaf<T>() {}

fn main() -> i32 {
    root::<i32>(0)
}
