// StrBuf — growable byte buffer with formatting helpers.
//
// Logical wrapper over `Vec<u8>`. Distinct file because the
// operations we want differ from `Vec<u8>`'s element-pushing:
// formatting (`push_i64`, `push_u64`, `push_hex_u64`), C-string
// interop (`push_cstr`, `as_cstr`), bulk byte append.
//
// All number formatters write into a fixed local scratch array and
// append digit-by-digit; no recursive allocation, no reliance on
// `snprintf`.

import "stdlib.ox";       // abort
import "stdio.ox";        // puts
import "string.ox";       // strlen
import "intrinsics.ox";   // ox_transmute, ox_size_of
import "../util/vec.ox";

struct StrBuf {
    bytes: Vec<u8>,
}

// --- Construction ---------------------------------------------------

fn strbuf_new() -> StrBuf {
    StrBuf { bytes: vec_new::<u8>() }
}

fn strbuf_with_capacity(cap: usize) -> StrBuf {
    StrBuf { bytes: vec_with_capacity::<u8>(cap) }
}

// --- Inspection -----------------------------------------------------

fn strbuf_len(s: *const StrBuf) -> usize {
    vec_len::<u8>(&s.bytes)
}

fn strbuf_capacity(s: *const StrBuf) -> usize {
    vec_capacity::<u8>(&s.bytes)
}

fn strbuf_is_empty(s: *const StrBuf) -> bool {
    vec_is_empty::<u8>(&s.bytes)
}

fn strbuf_byte_at(s: *const StrBuf, i: usize) -> u8 {
    vec_get::<u8>(&s.bytes, i)
}

// --- Mutation -------------------------------------------------------

fn strbuf_push_byte(s: *mut StrBuf, b: u8) {
    vec_push::<u8>(&mut s.bytes, b);
}

// Append `len` bytes from `src` (interpreted as `*const [u8]`).
fn strbuf_push_str(s: *mut StrBuf, src: *const [u8], len: usize) {
    vec_reserve::<u8>(&mut s.bytes, len);
    let mut i: usize = 0;
    while i < len {
        vec_push::<u8>(&mut s.bytes, src[i]);
        i = i + 1;
    }
}

// Append a NUL-terminated C string. Length is `strlen(src)`.
fn strbuf_push_cstr(s: *mut StrBuf, src: *const [u8]) {
    let n: usize = strlen(src);
    strbuf_push_str(s, src, n);
}

// Append the decimal representation of `n` (signed).
//
// INT64_MIN is handled via wrapping unsigned negation so the
// negation step doesn't overflow.
fn strbuf_push_i64(s: *mut StrBuf, n: i64) {
    if n < 0 {
        strbuf_push_byte(s, 45);                     // '-'
        let nu: u64 = n as u64;
        let neg: u64 = (0 as u64) - nu;              // wrapping; correct for INT64_MIN
        strbuf_push_u64(s, neg);
    } else {
        strbuf_push_u64(s, n as u64);
    }
}

// Append the decimal representation of `n` (unsigned).
fn strbuf_push_u64(s: *mut StrBuf, n: u64) {
    if n == 0 {
        strbuf_push_byte(s, 48);                     // '0'
        return;
    }
    // u64::MAX = 18446744073709551615 → 20 digits
    let mut digits: [u8; 20] = [0; 20];
    let mut count: usize = 0;
    let mut x: u64 = n;
    while x > 0 {
        digits[count] = ((x % 10) as u8) + 48;
        x = x / 10;
        count = count + 1;
    }
    let mut i: usize = 0;
    while i < count {
        strbuf_push_byte(s, digits[count - 1 - i]);
        i = i + 1;
    }
}

// Append the lowercase-hex representation of `n` (no `0x` prefix).
fn strbuf_push_hex_u64(s: *mut StrBuf, n: u64) {
    if n == 0 {
        strbuf_push_byte(s, 48);                     // '0'
        return;
    }
    // u64::MAX = 0xFFFFFFFFFFFFFFFF → 16 hex digits
    let mut digits: [u8; 16] = [0; 16];
    let mut count: usize = 0;
    let mut x: u64 = n;
    while x > 0 {
        let d: u64 = x % 16;
        let c: u8 = if d < 10 {
            (d as u8) + 48                           // '0'-'9'
        } else {
            (d as u8) - 10 + 97                      // 'a'-'f'
        };
        digits[count] = c;
        x = x / 16;
        count = count + 1;
    }
    let mut i: usize = 0;
    while i < count {
        strbuf_push_byte(s, digits[count - 1 - i]);
        i = i + 1;
    }
}

fn strbuf_clear(s: *mut StrBuf) {
    vec_clear::<u8>(&mut s.bytes);
}

fn strbuf_free(s: *mut StrBuf) {
    vec_free::<u8>(&mut s.bytes);
}

// --- Pointer access -------------------------------------------------

fn strbuf_as_ptr(s: *const StrBuf) -> *const [u8] {
    vec_data::<u8>(&s.bytes)
}

fn strbuf_as_mut_ptr(s: *mut StrBuf) -> *mut [u8] {
    vec_data_mut::<u8>(&mut s.bytes)
}

// Ensure the byte at `len` is `\0` (without incrementing `len`) and
// return the data pointer. The next mutation invalidates the
// sentinel. Used for FFI calls expecting NUL-terminated input.
fn strbuf_as_cstr(s: *mut StrBuf) -> *const [u8] {
    let len: usize = strbuf_len(s);
    if len == vec_capacity::<u8>(&s.bytes) {
        vec_reserve::<u8>(&mut s.bytes, 1);
    }
    // Reach the slot at index `len` directly through `data` (the
    // capacity guarantees the slot exists; we don't bump `len`).
    let data: *mut [u8] = vec_data_mut::<u8>(&mut s.bytes);
    data[len] = 0;
    ox_transmute::<*mut [u8], *const [u8]>(data)
}

// --- Comparison -----------------------------------------------------

// True iff `s`'s bytes equal the first `n` bytes of `b`. `n` must
// match `s.len`.
fn strbuf_eq_bytes(s: *const StrBuf, b: *const [u8], n: usize) -> bool {
    if strbuf_len(s) != n {
        return false;
    }
    let data: *const [u8] = strbuf_as_ptr(s);
    let mut i: usize = 0;
    while i < n {
        if data[i] != b[i] {
            return false;
        }
        i = i + 1;
    }
    true
}
