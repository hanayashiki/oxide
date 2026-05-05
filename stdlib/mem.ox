// mem.ox — typed wrappers around the C memory allocator,
// plus a pointer-equality helper.
//
// Imported via `import "mem.ox";`. Provides:
//
//   ox_alloc<T>()                 → uninit *mut T
//   ox_alloc_zeroed<T>()          → zero-initialized *mut T
//   ox_dealloc<T>(p: *mut T)      → frees `p`
//   ox_realloc<T>(p, n)           → resizes `p` to hold `n` elements of T
//   ox_ptr_eq<T>(a, b)            → bit-level address compare
//
// The four allocator helpers use `ox_size_of::<T>()` to compute the
// byte count and `ox_transmute` to bridge the C side's `*mut u8` with
// the typed Oxide `*mut T`. `ox_ptr_eq` reuses the same `ox_transmute`
// machinery to ptr-cast both args to `usize` and integer-compare —
// see spec/07_POINTER.md §"Pointer equality (`ox_ptr_eq`)".
//
// Imports are non-transitive (spec/14_MODULES.md), so a user
// who writes `import "mem.ox";` gets only these names — `malloc`,
// `calloc`, `realloc`, `free`, `ox_size_of`, and `ox_transmute` stay
// encapsulated. See spec/17_LAYOUT.md §Bundled mem.ox.
//
// Soundness of `malloc`-backed allocation: every v0 type has
// `align_of <= 8` (primitives top out at 8 bytes, ADTs use max-field
// alignment, no `repr(align)`); `malloc` returns `>= max_align_t`
// (16 bytes on x86_64/aarch64); strictly sufficient. When extended
// alignment lands (SIMD, `repr(align(N > 16))`), this file should
// switch to `aligned_alloc(align_of::<T>(), size_of::<T>())`.

import "intrinsics.ox";   // ox_transmute, ox_size_of
import "stdlib.ox";       // malloc, calloc, realloc, free (C names)

fn ox_alloc<T>() -> *mut T {
    ox_transmute(malloc(ox_size_of::<T>()))
}

fn ox_alloc_zeroed<T>() -> *mut T {
    ox_transmute(calloc(1, ox_size_of::<T>()))
}

fn ox_dealloc<T>(p: *mut T) {
    free(ox_transmute(p))
}

fn ox_realloc<T>(p: *mut T, n: usize) -> *mut T {
    ox_transmute(realloc(ox_transmute(p), n * ox_size_of::<T>()))
}

// Pointer equality — bit-level address compare. See
// spec/07_POINTER.md §"Pointer equality (`ox_ptr_eq`)". Direct `==`
// / `!=` on raw pointers is rejected by typeck (E0279) so callers
// must make the address-vs-value intent explicit. Body lowers to two
// `ptrtoint` + one `icmp eq i64` after LLVM constant-folds the
// transmute round-trips.
fn ox_ptr_eq<T>(a: *const T, b: *const T) -> bool {
    let ai: usize = ox_transmute(a);
    let bi: usize = ox_transmute(b);
    ai == bi
}
