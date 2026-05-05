// M1 arena.ox snapshot test.

import "stdio.ox";
import "../util/arena.ox";

struct Node {
    label: i32,
    parent: usize,
    weight: i64,
}

fn main() -> i32 {
    let mut a: Arena<Node> = arena_new::<Node>();
    printf("empty: len=%zu is_empty=%d\n", arena_len(&a), arena_is_empty(&a) as i32);

    // --- alloc ------------------------------------------------------
    let id0: usize = arena_alloc::<Node>(&mut a, Node { label: 100, parent: 0, weight: 7 });
    let id1: usize = arena_alloc::<Node>(&mut a, Node { label: 200, parent: id0, weight: 14 });
    let id2: usize = arena_alloc::<Node>(&mut a, Node { label: 300, parent: id1, weight: 21 });
    printf("ids: %zu %zu %zu  len=%zu\n", id0, id1, id2, arena_len(&a));

    // --- get --------------------------------------------------------
    let n0: Node = arena_get(&a, id0);
    let n1: Node = arena_get(&a, id1);
    let n2: Node = arena_get(&a, id2);
    printf("n0 label=%d parent=%zu weight=%lld\n", n0.label, n0.parent, n0.weight);
    printf("n1 label=%d parent=%zu weight=%lld\n", n1.label, n1.parent, n1.weight);
    printf("n2 label=%d parent=%zu weight=%lld\n", n2.label, n2.parent, n2.weight);

    // --- set --------------------------------------------------------
    arena_set::<Node>(&mut a, id1, Node { label: 999, parent: 99, weight: 42 });
    let n1b: Node = arena_get(&a, id1);
    printf("after set id1: label=%d parent=%zu weight=%lld\n",
           n1b.label, n1b.parent, n1b.weight);

    // --- get_ptr (in-place mutation) --------------------------------
    let p: *mut Node = arena_get_ptr::<Node>(&mut a, id2);
    p.weight = 12345;
    let n2b: Node = arena_get(&a, id2);
    printf("after ptr write id2.weight: %lld\n", n2b.weight);

    // --- bulk allocate ---------------------------------------------
    let mut i: usize = 0;
    while i < 100 {
        arena_alloc::<Node>(&mut a, Node { label: i as i32, parent: 0, weight: i as i64 });
        i = i + 1;
    }
    printf("after bulk: len=%zu\n", arena_len(&a));

    // verify a sample
    let mid: Node = arena_get(&a, 50);
    printf("sample[50]: label=%d weight=%lld\n", mid.label, mid.weight);

    arena_free::<Node>(&mut a);
    printf("after free: len=%zu\n", arena_len(&a));

    0
}
