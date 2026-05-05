// `ox_transmute<S, [u8; 8]>` exercises the alloca-store-load fallback —
// neither side is a primitive or pointer, so the dispatch table's
// catch-all arm fires. Verifies:
//   - The alloca lands in the `allocas:` entry block (mem2reg/SROA
//     visibility).
//   - The slot's alignment is `max(align(Src), align(Dst)) = 4` —
//     `S = { a: i32, b: i32 }` is 4-byte-aligned, `[u8; 8]` is 1-byte.
//   - The store and load both inherit the slot's alignment (no
//     pessimistic `align 1` on the load side).
import "intrinsics.ox";

struct S { a: i32, b: i32 }

fn main() -> i32 {
    let s: S = S { a: 1, b: 2 };
    let bytes: [u8; 8] = ox_transmute::<S, [u8; 8]>(s);
    bytes[0] as i32
}
