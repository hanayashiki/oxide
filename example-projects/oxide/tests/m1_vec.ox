// M1 vec.ox snapshot test.
//
// Exercises construction, push, get, set, len/capacity, pop, clear,
// reserve. Prints a stable trace to stdout that the snapshot harness
// captures.

import "stdio.ox";
import "../util/vec.ox";

fn main() -> i32 {
    // --- Empty vec ---------------------------------------------------
    let mut v: Vec<i32> = vec_new::<i32>();
    printf("empty: len=%zu cap=%zu\n", vec_len(&v), vec_capacity(&v));

    // --- Push triggers initial grow at cap=0 → 4 ---------------------
    vec_push(&mut v, 10);
    vec_push(&mut v, 20);
    vec_push(&mut v, 30);
    printf("after 3 pushes: len=%zu cap=%zu\n", vec_len(&v), vec_capacity(&v));

    // --- Read back ---------------------------------------------------
    printf("v[0]=%d v[1]=%d v[2]=%d\n",
           vec_get(&v, 0), vec_get(&v, 1), vec_get(&v, 2));

    // --- Cross-grow (push a 5th to force cap 4 → 8) ------------------
    vec_push(&mut v, 40);
    vec_push(&mut v, 50);
    printf("after 5 pushes: len=%zu cap=%zu v[4]=%d\n",
           vec_len(&v), vec_capacity(&v), vec_get(&v, 4));

    // --- Set at index ------------------------------------------------
    vec_set(&mut v, 2, 999);
    printf("after set v[2]=999: v[2]=%d\n", vec_get(&v, 2));

    // --- Pop ---------------------------------------------------------
    let popped: i32 = vec_pop(&mut v);
    printf("popped=%d len=%zu\n", popped, vec_len(&v));

    // --- Reserve well past cap ---------------------------------------
    vec_reserve(&mut v, 100);
    printf("after reserve(100): len=%zu cap>=%d\n",
           vec_len(&v), 100);

    // --- Clear -------------------------------------------------------
    vec_clear(&mut v);
    printf("after clear: len=%zu is_empty=%d\n",
           vec_len(&v), vec_is_empty(&v) as i32);

    // --- with_capacity(8) shape --------------------------------------
    let v2: Vec<i64> = vec_with_capacity::<i64>(8);
    printf("with_capacity(8): len=%zu cap=%zu\n",
           vec_len(&v2), vec_capacity(&v2));

    // --- Free --------------------------------------------------------
    vec_free(&mut v);
    printf("after free: cap=%zu\n", vec_capacity(&v));

    0
}
