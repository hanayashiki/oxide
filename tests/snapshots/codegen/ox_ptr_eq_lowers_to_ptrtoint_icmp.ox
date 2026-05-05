import "mem.ox";

fn f(p: *const u8, q: *const u8) -> bool {
    ox_ptr_eq(p, q)
}
