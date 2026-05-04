fn choose<T, U>(flag: i32, x: T, y: U) -> U {
    y
}

fn main() {
    choose(1, 7, 42 as u8);
}
