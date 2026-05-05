// Smoke test for stdlib/mem.ox — typed wrappers around the C allocator.
//
// Build + run:
//   cargo run --bin oxide -- example-projects/layout_mem/main.ox
//   echo $?     # expect 0

import "mem.ox";

fn main() -> i32 {
    // Alloc one i32, write through the pointer, read it back, free.
    let p: *mut i32 = ox_alloc::<i32>();
    *p = 42;
    let v: i32 = *p;
    ox_dealloc::<i32>(p);
    if v == 42 { 0 } else { 1 }
}
