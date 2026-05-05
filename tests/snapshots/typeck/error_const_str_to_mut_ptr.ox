// `"hello"` is `*const [u8; 6]`. Assigning to `*mut [u8; 6]` would
// reverse the directional rule (`*const → *mut`), which is illegal.
// discharge_subtype should fire E0257 PointerMutabilityMismatch.
const X: *mut [u8; 6] = "hello";
