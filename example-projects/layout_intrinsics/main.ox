// Walk-through for spec/17_LAYOUT.md — exercises both intrinsics
// (`ox_size_of`, `ox_transmute`) and the typed allocator wrappers in
// `stdlib/mem.ox` (`ox_alloc`, `ox_dealloc`).
//
// Build + run:
//   cargo run --bin oxide -- example-projects/layout_intrinsics/main.ox
//   echo $?     # expect 0
//
// Eyeball the IR. Intrinsics emit no `declare` lines: `ox_size_of`
// becomes an `i64` constant inline, `ox_transmute` becomes a bitcast
// (or a no-op for opaque-ptr→opaque-ptr), and the mem.ox wrappers
// resolve to direct `call malloc` / `call free` against libc:
//   cargo run --bin oxide -- example-projects/layout_intrinsics/main.ox --emit ir

import "intrinsics.ox";    // ox_size_of, ox_transmute (compiler intrinsics)
import "mem.ox";           // ox_alloc, ox_dealloc, ox_alloc_zeroed, ox_realloc

fn main() -> i32 {
    // -- ox_size_of ----------------------------------------------------
    // Returns the byte size as a `usize` (= i64 in v0). Mono precomputes
    // the value; codegen emits a single `i64 N` constant at the call
    // site, no helper call.
    let four:  usize = ox_size_of::<i32>();
    let eight: usize = ox_size_of::<*mut i32>();

    // -- ox_transmute --------------------------------------------------
    // Same-width int reinterpret. `u32` and `i32` both lower to LLVM
    // `i32`, so the emitted bitcast is a no-op at the IR level —
    // codegen still emits the bitcast for shape uniformity.
    let n: i32 = ox_transmute::<u32, i32>(0x42);

    // -- mem.ox roundtrip ----------------------------------------------
    // ox_alloc<T> wraps `malloc(ox_size_of::<T>())` + an ox_transmute
    // from `*mut u8` to `*mut T`. The dereference and write go through
    // the typed pointer — no further casts needed at the use site.
    // ox_dealloc<T> hands `p` back to libc `free` via ox_transmute.
    let p: *mut i32 = ox_alloc::<i32>();
    *p = 7;
    let stored: i32 = *p;
    ox_dealloc::<i32>(p);

    // Return 0 only when every probe matches; non-zero exit otherwise.
    if four == 4 && eight == 8 && n == 0x42 && stored == 7 {
        0
    } else {
        1
    }
}
