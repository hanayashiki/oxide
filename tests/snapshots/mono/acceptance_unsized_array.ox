fn eat<T>(x: T) {}

fn main() -> i32 {
    let unsized: *const [i32] = null;

    eat(unsized);

    0
}

