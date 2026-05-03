// B006: array-literal element coalesce now equates strictly on
// outer Ptr mut, so a `*mut` and `*const` can't silently coalesce
// into the more-permissive of the two arm orderings. Without this
// rule, `arr[1]` would be typed `*mut [u8; 3]` despite the runtime
// pointer being a read-only `.rodata` pointer — writing through it
// SIGBUSes on macOS. See spec/BACKLOG/B006.

fn main() -> i32 {
    let mut buf: [u8; 3] = [104, 105, 0];
    let rw: *mut   [u8; 3] = &mut buf;
    let ro: *const [u8; 3] = "Hi";
    let arr = [rw, ro];
    0
}
