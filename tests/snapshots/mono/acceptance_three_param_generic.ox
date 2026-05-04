fn choose<T, U, V>(x: T, y: U, z: V) -> V {
    z
}

fn main() -> i32 {
    let y: u8 = 2;
    choose::<i32, u8, i32>(1, y, 3)
}
