extern "C" {
    fn print_int(x: u32) -> u32;
}

fn fib(n: u32) -> u32 {
    if n <= 1 {
        1
    } else {
        fib(n - 1) + fib(n - 2)
    }
}

fn main() -> i32 {
    print_int(fib(12));

    0
}
