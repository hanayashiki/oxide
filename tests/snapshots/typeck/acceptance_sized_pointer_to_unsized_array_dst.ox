// `*const [u8]` IS the canonical DST shape — pointer to directly-
// unsized array. Sized obligation must accept this (the relaxation
// is exactly one layer deep, at the immediate pointee position).
extern "C" {
    fn puts(s: *const [u8]) -> i32;
}

fn say() -> i32 {
    puts("hi")
}
