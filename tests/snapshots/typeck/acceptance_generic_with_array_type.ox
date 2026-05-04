fn size_of<T>() -> usize {
    0
}

fn main() {
    let n = size_of::<[i32; 4]>();
}
