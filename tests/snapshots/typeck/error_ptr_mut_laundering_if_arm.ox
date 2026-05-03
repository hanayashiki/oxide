// B006: if-arm coalesce equates strictly on outer Ptr mut. Mixing
// a `*mut` and `*const` in the two arms is a type mismatch — same
// shape as the array-literal case, different surface. Without this
// rule, the if-expr would be typed `*mut [u8; 3]` (then-arm wins)
// and writing through it would SIGBUS on the `*const` value. See
// spec/BACKLOG/B006.

fn main() -> i32 {
    let mut buf: [u8; 3] = [104, 105, 0];
    let rw: *mut   [u8; 3] = &mut buf;
    let ro: *const [u8; 3] = "Hi";
    let p = if true { rw } else { ro };
    0
}
