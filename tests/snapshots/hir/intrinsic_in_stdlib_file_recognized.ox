/// intrinsics.ox
fn ox_transmute<Src, Dst>(x: Src) -> Dst;
fn ox_size_of<T>() -> usize;
/// main.ox
import "intrinsics.ox";
fn main() -> i32 {
    let n: i32 = ox_transmute::<u32, i32>(0);
    n
}
