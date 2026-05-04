struct Pair<T, U> {
    l: T,
    r: U,
}

fn main() -> i32 {
    let p = Pair::<i32, u8> { l: 1, r: 2 as u8 };
    let q = Pair::<u8, i32> { l: 3 as u8, r: 4 };
    p.l + q.r
}
