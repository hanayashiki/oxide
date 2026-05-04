fn size_of<T>() -> usize {
    0
}

fn main() -> usize {
    size_of::<[i32; 10]>()
}
