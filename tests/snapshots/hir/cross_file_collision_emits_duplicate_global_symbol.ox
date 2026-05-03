/// main.ox
import "./a.ox";
import "./b.ox";
fn main() -> i32 { 0 }
/// a.ox
fn helper() -> i32 { 1 }
struct Shared { x: i32 }
/// b.ox
fn helper() -> i32 { 2 }
struct Shared { y: i32 }
