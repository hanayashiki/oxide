// B005 reproducer: assignment evaluates to `()`, so the let binding
// has type `()`. Pre-fix: emit_let panicked with "void type for local a".
// After fix: lower_ty(()) returns LLVM `{}`, the alloca is dead and
// gets DCE'd; the assign side-effect on `b` survives.
fn main() -> i32 {
    let mut b = 0;
    let _a = b = 7;   // _a: ()
    b                  // 7
}
