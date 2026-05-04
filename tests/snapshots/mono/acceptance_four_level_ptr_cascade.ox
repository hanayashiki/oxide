fn level_a<T>() {
    level_b::<*mut T>()
}

fn level_b<T>() {
    level_c::<*mut T>()
}

fn level_c<T>() {
    level_d::<*mut T>()
}

fn level_d<T>() {}

fn main() {
    level_a::<i32>()
}
