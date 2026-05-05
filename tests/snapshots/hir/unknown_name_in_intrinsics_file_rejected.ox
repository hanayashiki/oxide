/// intrinsics.ox
// File gate passes but `ox_unknown` is not in the allowlist; E0209 fires.
fn ox_unknown<T>(x: T) -> T;
/// main.ox
import "intrinsics.ox";
fn main() -> i32 { 0 }
