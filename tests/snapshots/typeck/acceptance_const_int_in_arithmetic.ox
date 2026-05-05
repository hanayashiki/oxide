// Const int referenced in arithmetic — verifies the Const(cid)
// expression typechecks via const_tys[cid] and flows through the
// Binary infer arm cleanly.
const SIZE: i32 = 10;

fn double() -> i32 {
    SIZE * 2
}
