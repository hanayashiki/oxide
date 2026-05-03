extern "C" {
    fn print_int(x: i32) -> i32;
}
fn main() -> i32 { print_int(42); 0 }
