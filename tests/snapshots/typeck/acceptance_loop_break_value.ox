fn first_match() -> i32 {
    let mut i = 0;
    loop {
        if i == 7 { break i * 2; }
        i = i + 1;
    }
}
