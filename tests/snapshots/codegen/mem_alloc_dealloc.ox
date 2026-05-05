// `ox_alloc<T>` lowers to `call malloc(ox_size_of::<T>())` plus an
// `ox_transmute` from `*mut u8` to `*mut T` (a no-op for opaque LLVM
// pointers). `ox_dealloc<T>(p)` mirrors as a `call free(p)` after
// transmuting back. Verifies:
//   - The malloc call carries the precomputed size constant
//     (`i64 4` here, from `ox_size_of::<i32>()`).
//   - No `declare` for any `ox_*` intrinsic — the transmute punning is
//     synthesized inline.
//   - `mem.ox`'s `ox_alloc` / `ox_dealloc` themselves DO emit
//     declarations (regular generic fns, instantiated for `T = i32`).
import "mem.ox";

fn main() -> i32 {
    let p: *mut i32 = ox_alloc::<i32>();
    *p = 7;
    let v: i32 = *p;
    ox_dealloc::<i32>(p);
    v
}
