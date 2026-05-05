// Const string-typed value passed to a fn taking `*const [u8]`.
// Exercises discharge_subtype's length-erasure rule under Ptr at
// a body-phase call site (not just decl-phase).
extern "C" { fn puts(s: *const [u8]) -> i32; }

const HELLO: *const [u8; 6] = "hello";

fn say() -> i32 {
    puts(HELLO)
}
