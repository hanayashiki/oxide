// Regression: `a * b` mid-expression still parses as binary `Mul`,
// not as `Mul` with a deref on either side. Pratt position
// disambiguates prefix `*` (level 13) from infix `*` (level 11).
fn f(a: i32, b: i32) -> i32 {
    a * b
}
