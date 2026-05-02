fn main() -> i32 {
    let mut total = 0;
    let mut i = 0;
    while i < 3 {
        let mut j = 0;
        loop {
            if j == 2 { break; }
            total = total + 1;
            j = j + 1;
        }
        i = i + 1;
    }
    total
}
