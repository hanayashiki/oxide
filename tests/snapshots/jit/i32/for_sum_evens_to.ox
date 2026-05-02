fn sum_evens_to(n: i32) -> i32 {
    let mut s = 0;
    for (let mut i = 0; i < n; i = i + 1) {
        if i % 2 != 0 { continue; }
        s = s + i;
    }
    s
}

fn main() -> i32 { sum_evens_to(10) }
