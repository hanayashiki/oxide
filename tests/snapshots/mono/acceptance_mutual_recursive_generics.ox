fn even<T>(n: i32, x: T) -> T {
    if n != 0 {
        odd(n, x)
    } else {
        x
    }
}

fn odd<U>(m: i32, y: U) -> U {
    if m != 0 {
        even(m, y)
    } else {
        y
    }
}

fn main() -> i32 {
    even(0, 5)
}
