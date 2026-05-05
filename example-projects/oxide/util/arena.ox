// Arena<T> — id-based container.
//
// Hand out `usize` indices on push, look up by index. Mirrors stage
// 0's `IndexVec<I, T>` minus the typed `I` (Oxide has no zero-sized
// phantoms). Use-site convention: wrap the `usize` in a per-kind
// struct (`FnId { idx: usize }`, etc.) so the type system at least
// distinguishes "an id from arena A" from "an id from arena B".
//
// Internally a thin layer over `Vec<T>`: this gives us growth and
// indexing for free; arena adds nothing at runtime.

import "../util/vec.ox";

struct Arena<T> {
    items: Vec<T>,
}

fn arena_new<T>() -> Arena<T> {
    Arena::<T> { items: vec_new::<T>() }
}

fn arena_with_capacity<T>(cap: usize) -> Arena<T> {
    Arena::<T> { items: vec_with_capacity::<T>(cap) }
}

fn arena_len<T>(a: *const Arena<T>) -> usize {
    vec_len::<T>(&a.items)
}

fn arena_is_empty<T>(a: *const Arena<T>) -> bool {
    vec_is_empty::<T>(&a.items)
}

// Insert `x`; return its assigned id (= prior length).
fn arena_alloc<T>(a: *mut Arena<T>, x: T) -> usize {
    let id: usize = vec_len::<T>(&a.items);
    vec_push::<T>(&mut a.items, x);
    id
}

// Read by id (panics on OOB via vec_get).
fn arena_get<T>(a: *const Arena<T>, id: usize) -> T {
    vec_get::<T>(&a.items, id)
}

fn arena_set<T>(a: *mut Arena<T>, id: usize, x: T) {
    vec_set::<T>(&mut a.items, id, x);
}

// Pointer to the i-th slot. Useful for in-place mutation of large
// items without round-tripping through `arena_get` + `arena_set`.
fn arena_get_ptr<T>(a: *mut Arena<T>, id: usize) -> *mut T {
    let data: *mut [T] = vec_data_mut::<T>(&mut a.items);
    &mut data[id]
}

fn arena_free<T>(a: *mut Arena<T>) {
    vec_free::<T>(&mut a.items);
}
