// B006 cousin: directional `*const → *mut` violation buried inside
// an Array. The eager subtype walk is loose-mut at every Ptr layer
// during inference; the recursive `discharge_subtype` (Array arm)
// walks into the element type and applies the directional check on
// the buried Ptr. Without recursive discharge, this silently slips
// through (top-level Array-Array, not Ptr-Ptr — old shallow
// discharge_coerce returned without checking).

fn main() -> i32 {
    let mut a: i32 = 1;
    let mut b: i32 = 2;
    let mut c: i32 = 3;
    let ca: *const i32 = &a;
    let cb: *const i32 = &b;
    let cc: *const i32 = &c;
    // Array of `*const i32`. Try to flow into a `[*mut i32; 3]`
    // slot — outer Array shape matches; the buried `*const → *mut`
    // is the violation.
    let arr_of_const: [*const i32; 3] = [ca, cb, cc];
    let arr_of_mut: [*mut i32; 3] = arr_of_const;
    0
}
