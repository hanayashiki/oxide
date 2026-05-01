extern "C" {
    fn make_size() -> usize;
    fn use_size(n: usize);
}

fn f() {
    let x: usize = make_size();
    use_size(x);
}
