// Intrinsics synthesize values rather than calling a function — a
// pointer to one is meaningless. spec/19_FN_PTR.md follow-up: E0281
// (`IntrinsicAsValue`) gates intrinsic-as-value while ordinary
// generic fns may flow through value position.
import "intrinsics.ox";

fn main() -> i32 {
    let _ = ox_size_of;
    0
}
