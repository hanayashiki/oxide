struct Pair<T, U> {
    l: T,
    r: U,
}

fn main() {
    let p = Pair::<i32> { l: 0, r: 0 };
}
