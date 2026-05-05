// `ox_transmute::<i32, i64>(...)` violates the per-instance size-equality
// gate (4 ≠ 8). Mono pushes E0276 at the call expression's span and
// includes the Src/Dst sizes in `note:` lines. The instance is still
// stamped (so codegen has a stable key for diagnostics) but the
// driver short-circuits before codegen runs.
import "intrinsics.ox";

fn main() -> i32 {
    let bad: i64 = ox_transmute::<i32, i64>(1);
    bad as i32
}
