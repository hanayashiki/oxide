// Vec<T> — growable contiguous buffer.
//
// Backing store is `*mut [T]` (pointer to unsized array) because v0
// Oxide has no pointer arithmetic: `*mut [T]` is the only typed
// pointer that supports `buf[i]` (flat element-stride GEP per
// spec/09).
//
// Empty `Vec<T>` has `cap = 0` and `data` set to a null `*mut [T]`
// produced via `ox_transmute::<usize, *mut [T]>(0)`. We never index
// `data` while `cap == 0`. `realloc(NULL, n)` is well-defined as
// `malloc(n)`, so growth from empty works without a special case.
//
// No Drop. Containers leak unless explicitly `vec_free`'d. The
// compiler is short-lived; the OS reaps. `vec_free` exists for
// callers who want to reclaim mid-run.
//
// Panic paths emit one-line diagnostics via `puts` (stdout) — Oxide
// has no `extern "C" { static stderr; }` yet, so a clean stderr path
// is unavailable. Acceptable: every panic path is a compiler bug,
// not user input.

import "stdlib.ox";        // malloc, realloc, free, abort
import "stdio.ox";         // puts
import "intrinsics.ox";    // ox_transmute, ox_size_of

struct Vec<T> {
    data: *mut [T],
    len:  usize,
    cap:  usize,
}

// --- Construction ---------------------------------------------------

fn vec_new<T>() -> Vec<T> {
    Vec::<T> {
        data: ox_transmute::<usize, *mut [T]>(0),
        len:  0,
        cap:  0,
    }
}

fn vec_with_capacity<T>(cap: usize) -> Vec<T> {
    if cap == 0 {
        return vec_new::<T>();
    }
    let bytes: usize = cap * ox_size_of::<T>();
    let raw: *mut u8 = malloc(bytes);
    if ox_transmute::<*mut u8, usize>(raw) == 0 {
        vec_die("vec: out of memory in vec_with_capacity");
    }
    Vec::<T> {
        data: ox_transmute::<*mut u8, *mut [T]>(raw),
        len:  0,
        cap:  cap,
    }
}

// --- Inspection -----------------------------------------------------

fn vec_len<T>(v: *const Vec<T>) -> usize { v.len }
fn vec_capacity<T>(v: *const Vec<T>) -> usize { v.cap }
fn vec_is_empty<T>(v: *const Vec<T>) -> bool { v.len == 0 }

fn vec_data<T>(v: *const Vec<T>) -> *const [T] {
    ox_transmute::<*mut [T], *const [T]>(v.data)
}

fn vec_data_mut<T>(v: *mut Vec<T>) -> *mut [T] { v.data }

// --- Mutation -------------------------------------------------------

fn vec_push<T>(v: *mut Vec<T>, x: T) {
    if v.len == v.cap {
        vec_grow::<T>(v, v.len + 1);
    }
    v.data[v.len] = x;
    v.len = v.len + 1;
}

fn vec_pop<T>(v: *mut Vec<T>) -> T {
    if v.len == 0 {
        vec_die("vec: pop from empty");
    }
    v.len = v.len - 1;
    v.data[v.len]
}

fn vec_get<T>(v: *const Vec<T>, i: usize) -> T {
    if i >= v.len {
        vec_die("vec: vec_get: index out of bounds");
    }
    v.data[i]
}

fn vec_set<T>(v: *mut Vec<T>, i: usize, x: T) {
    if i >= v.len {
        vec_die("vec: vec_set: index out of bounds");
    }
    v.data[i] = x;
}

fn vec_clear<T>(v: *mut Vec<T>) {
    v.len = 0;
}

fn vec_reserve<T>(v: *mut Vec<T>, additional: usize) {
    let needed: usize = v.len + additional;
    if needed > v.cap {
        vec_grow::<T>(v, needed);
    }
}

fn vec_free<T>(v: *mut Vec<T>) {
    if v.cap > 0 {
        let raw: *mut u8 = ox_transmute::<*mut [T], *mut u8>(v.data);
        free(raw);
    }
    v.data = ox_transmute::<usize, *mut [T]>(0);
    v.len  = 0;
    v.cap  = 0;
}

// --- Internal -------------------------------------------------------

// Grow `v` so its capacity is at least `min_cap`. Doubling policy
// with a floor of 4. Allocates if `v.cap == 0` (realloc(NULL, n) ==
// malloc(n) per ISO C).
fn vec_grow<T>(v: *mut Vec<T>, min_cap: usize) {
    let mut new_cap: usize = if v.cap == 0 { 4 } else { v.cap * 2 };
    while new_cap < min_cap {
        new_cap = new_cap * 2;
    }
    let bytes: usize = new_cap * ox_size_of::<T>();
    let raw_old: *mut u8 = ox_transmute::<*mut [T], *mut u8>(v.data);
    let raw_new: *mut u8 = realloc(raw_old, bytes);
    if ox_transmute::<*mut u8, usize>(raw_new) == 0 {
        vec_die("vec: out of memory in vec_grow");
    }
    v.data = ox_transmute::<*mut u8, *mut [T]>(raw_new);
    v.cap  = new_cap;
}

// Print a one-line diagnostic and abort. `puts` writes to stdout
// (no stderr path available in stage 0 yet — see top-of-file
// comment); acceptable for compiler-internal panic.
fn vec_die(msg: *const [u8]) {
    puts(msg);
    abort();
}
