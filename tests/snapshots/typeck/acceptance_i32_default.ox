fn default() {
    let mut n = 1;
    let a = n;
    let b = a + 2;
    let c = if true { 3 } else { b };
    let _ = c;
}
