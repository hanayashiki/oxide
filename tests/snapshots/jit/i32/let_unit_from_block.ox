// `()`-typed local from an empty block. Same B005 path as
// let_unit_from_assign but with an explicit unit-producing init.
fn main() -> i32 {
    let _a = {};       // _a: ()
    42
}
