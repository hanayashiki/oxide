// M1 strmap.ox snapshot test.

import "stdio.ox";
import "../util/strmap.ox";

fn main() -> i32 {
    let mut m: StrMap<i32> = strmap_new::<i32>();
    printf("empty: len=%zu cap=%zu\n", strmap_len(&m), strmap_capacity(&m));

    // --- inserts ----------------------------------------------------
    let was_new1: bool = strmap_insert::<i32>(&mut m, "alpha", 5, 1);
    let was_new2: bool = strmap_insert::<i32>(&mut m, "beta", 4, 2);
    let was_new3: bool = strmap_insert::<i32>(&mut m, "gamma", 5, 3);
    printf("insert new flags: %d %d %d  len=%zu cap=%zu\n",
           was_new1 as i32, was_new2 as i32, was_new3 as i32,
           strmap_len(&m), strmap_capacity(&m));

    // --- get hits ---------------------------------------------------
    let mut out: i32 = 0;
    let h1: bool = strmap_get::<i32>(&m, "alpha", 5, &mut out);
    printf("get alpha: hit=%d val=%d\n", h1 as i32, out);
    let h2: bool = strmap_get::<i32>(&m, "beta", 4, &mut out);
    printf("get beta: hit=%d val=%d\n", h2 as i32, out);
    let h3: bool = strmap_get::<i32>(&m, "gamma", 5, &mut out);
    printf("get gamma: hit=%d val=%d\n", h3 as i32, out);

    // --- get miss ---------------------------------------------------
    out = 0;
    let h4: bool = strmap_get::<i32>(&m, "delta", 5, &mut out);
    printf("get delta: hit=%d (out unchanged=%d)\n", h4 as i32, out);

    // --- contains ----------------------------------------------------
    printf("contains beta=%d gamma=%d zeta=%d\n",
           strmap_contains(&m, "beta", 4) as i32,
           strmap_contains(&m, "gamma", 5) as i32,
           strmap_contains(&m, "zeta", 4) as i32);

    // --- update ------------------------------------------------------
    let was_new4: bool = strmap_insert::<i32>(&mut m, "alpha", 5, 100);
    let mut v: i32 = 0;
    strmap_get::<i32>(&m, "alpha", 5, &mut v);
    printf("after update: was_new=%d v=%d len=%zu\n", was_new4 as i32, v, strmap_len(&m));

    // --- remove ------------------------------------------------------
    let removed: bool = strmap_remove::<i32>(&mut m, "beta", 4);
    let h5: bool = strmap_get::<i32>(&m, "beta", 4, &mut v);
    printf("after remove beta: removed=%d hit=%d len=%zu\n",
           removed as i32, h5 as i32, strmap_len(&m));

    // remove non-existent
    let removed2: bool = strmap_remove::<i32>(&mut m, "unknown", 7);
    printf("remove unknown: %d\n", removed2 as i32);

    // re-insert into a tombstone slot (same name as removed key)
    let was_new5: bool = strmap_insert::<i32>(&mut m, "beta", 4, 222);
    strmap_get::<i32>(&m, "beta", 4, &mut v);
    printf("re-insert beta: was_new=%d v=%d len=%zu\n",
           was_new5 as i32, v, strmap_len(&m));

    // --- bulk insert to force grows ---------------------------------
    let mut k: [u8; 8] = [0; 8];
    let mut i: usize = 0;
    while i < 50 {
        // key = "k_NN" where NN is two ASCII digits
        k[0] = 107;                                // 'k'
        k[1] = 95;                                 // '_'
        k[2] = ((i / 10) as u8) + 48;
        k[3] = ((i % 10) as u8) + 48;
        // strmap_insert copies the key bytes, so reusing `k` is safe.
        strmap_insert::<i32>(&mut m, &k, 4, i as i32);
        i = i + 1;
    }
    printf("after 50 inserts: len=%zu cap=%zu\n", strmap_len(&m), strmap_capacity(&m));

    // verify a few
    let mut found_count: usize = 0;
    let mut sum: i64 = 0;
    let mut j: usize = 0;
    while j < 50 {
        k[0] = 107; k[1] = 95;
        k[2] = ((j / 10) as u8) + 48;
        k[3] = ((j % 10) as u8) + 48;
        let mut got: i32 = -1;
        if strmap_get::<i32>(&m, &k, 4, &mut got) {
            found_count = found_count + 1;
            sum = sum + (got as i64);
        }
        j = j + 1;
    }
    printf("verify: found=%zu sum=%lld (expected 50, 1225)\n", found_count, sum);

    strmap_free::<i32>(&mut m);
    printf("after free: cap=%zu len=%zu\n", strmap_capacity(&m), strmap_len(&m));

    0
}
