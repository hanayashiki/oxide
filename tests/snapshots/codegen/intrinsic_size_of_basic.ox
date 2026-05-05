// `ox_size_of::<T>()` lowers to a single `i64 N` constant inline at the
// call site. No `declare ... @ox_size_of$T...` line for the intrinsic
// instance — codegen synthesizes the IR; the mangled symbol never
// surfaces.
//
// Three probes: a 4-byte primitive, an 8-byte pointer, and a 12-byte
// aggregate (per spec/17 §`size_of` worked example).
import "intrinsics.ox";

struct Mixed { a: u8, b: u32, c: u8 }

fn main() -> i32 {
    let four:   usize = ox_size_of::<i32>();        // i64 4
    let eight:  usize = ox_size_of::<*mut i32>();   // i64 8
    let twelve: usize = ox_size_of::<Mixed>();      // i64 12
    (four + eight + twelve) as i32                  // 24
}
