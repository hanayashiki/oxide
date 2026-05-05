// `"hello"` is `*const [u8; 6]`. Annotation `*const u8` has a
// non-array pointee — cross-kind shape mismatch at the inner Ptr
// position. discharge_subtype recurses through Ptr with
// pointee=true and fires E0250 on the inner Array-vs-Prim
// mismatch.
const X: *const u8 = "hello";
