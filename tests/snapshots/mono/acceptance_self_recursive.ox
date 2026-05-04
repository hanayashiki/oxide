fn rec<T>(x: T) -> T { rec(x) }

fn main() -> i32 {
    rec::<i32>(1);
    0
}
