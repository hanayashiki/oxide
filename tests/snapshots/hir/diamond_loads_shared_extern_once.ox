/// main.ox
import "./a.ox";
import "./b.ox";
fn main() -> i32 { 0 }
/// a.ox
import "./util.ox";
fn from_a() -> i32 { helper() }
/// b.ox
import "./util.ox";
fn from_b() -> i32 { helper() }
/// util.ox
fn helper() -> i32 { 42 }
