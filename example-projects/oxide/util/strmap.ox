// StrMap<V> — open-addressed string-keyed hash table.
//
// Linear probing, FNV-1a hash, doubling growth at 0.75 load.
// Tombstones for delete; rebuild at-cap when tombs > entries.
//
// Keys are owned: `strmap_insert` copies the key bytes into a
// fresh allocation. `strmap_free` walks live entries and frees
// each `key_ptr`.
//
// Values: `V` is treated as opaque bits; if `V` itself heap-owns,
// the caller must walk the map and reclaim those resources before
// `strmap_free` (Oxide has no `Drop`).
//
// Hash 0 is reserved as the "empty slot" sentinel; FNV-1a outputs
// of 0 are mapped to 1 inside `strmap_hash`. Hash 1 marks a
// tombstone (key_ptr is null, value is junk).
//
// Storage shape: `entries: *mut [StrMapEntry<V>]`, sized exactly
// `cap` slots, allocated via `calloc` so all slots start with
// `hash == 0` (empty).

import "stdlib.ox";        // calloc, malloc, free, abort
import "stdio.ox";         // puts
import "intrinsics.ox";    // ox_transmute, ox_size_of

struct StrMapEntry<V> {
    key_ptr: *mut [u8],
    key_len: usize,
    hash:    u64,
    value:   V,
}

struct StrMap<V> {
    entries: *mut [StrMapEntry<V>],
    cap:     usize,
    len:     usize,
    tombs:   usize,
}

// --- Construction ---------------------------------------------------

fn strmap_new<V>() -> StrMap<V> {
    StrMap::<V> {
        entries: ox_transmute::<usize, *mut [StrMapEntry<V>]>(0),
        cap:     0,
        len:     0,
        tombs:   0,
    }
}

fn strmap_with_capacity<V>(initial: usize) -> StrMap<V> {
    let mut m: StrMap<V> = strmap_new::<V>();
    let cap_pow2: usize = strmap_next_pow2(if initial < 8 { 8 } else { initial });
    strmap_grow_to::<V>(&mut m, cap_pow2);
    m
}

// --- Inspection -----------------------------------------------------

fn strmap_len<V>(m: *const StrMap<V>) -> usize { m.len }
fn strmap_capacity<V>(m: *const StrMap<V>) -> usize { m.cap }
fn strmap_is_empty<V>(m: *const StrMap<V>) -> bool { m.len == 0 }

fn strmap_contains<V>(m: *const StrMap<V>, key: *const [u8], key_len: usize) -> bool {
    if m.cap == 0 {
        return false;
    }
    let hash: u64 = strmap_hash(key, key_len);
    let mask: usize = m.cap - 1;
    let mut bucket: usize = (hash as usize) & mask;
    loop {
        let h: u64 = m.entries[bucket].hash;
        if h == 0 {
            return false;
        }
        if h == hash && strmap_entry_key_eq::<V>(m, bucket, key, key_len) {
            return true;
        }
        bucket = (bucket + 1) & mask;
    }
}

// Read into `*out` if present. Returns true on hit.
fn strmap_get<V>(m: *const StrMap<V>, key: *const [u8], key_len: usize, out: *mut V) -> bool {
    if m.cap == 0 {
        return false;
    }
    let hash: u64 = strmap_hash(key, key_len);
    let mask: usize = m.cap - 1;
    let mut bucket: usize = (hash as usize) & mask;
    loop {
        let h: u64 = m.entries[bucket].hash;
        if h == 0 {
            return false;
        }
        if h == hash && strmap_entry_key_eq::<V>(m, bucket, key, key_len) {
            *out = m.entries[bucket].value;
            return true;
        }
        bucket = (bucket + 1) & mask;
    }
}

// --- Mutation -------------------------------------------------------

// Returns true iff this was a new insert (false = overwrite).
fn strmap_insert<V>(m: *mut StrMap<V>, key: *const [u8], key_len: usize, value: V) -> bool {
    // Eager grow so the probe always finds an empty slot.
    if m.cap == 0 {
        strmap_grow_to::<V>(m, 16);
    } else if (m.len + m.tombs + 1) * 4 >= m.cap * 3 {
        let target: usize = if m.tombs > m.len { m.cap } else { m.cap * 2 };
        strmap_grow_to::<V>(m, target);
    }

    let hash: u64 = strmap_hash(key, key_len);
    let mask: usize = m.cap - 1;
    let mut bucket: usize = (hash as usize) & mask;
    let mut first_tomb: usize = m.cap;     // sentinel = "none seen"
    loop {
        let h: u64 = m.entries[bucket].hash;
        if h == 0 {
            // Empty slot reached without a match — insert here, or
            // at the first tombstone we passed.
            let target: usize = if first_tomb < m.cap { first_tomb } else { bucket };
            let key_copy: *mut [u8] = strmap_copy_key(key, key_len);
            m.entries[target].key_ptr = key_copy;
            m.entries[target].key_len = key_len;
            m.entries[target].hash    = hash;
            m.entries[target].value   = value;
            if first_tomb < m.cap {
                m.tombs = m.tombs - 1;
            }
            m.len = m.len + 1;
            return true;
        }
        if h == 1 {
            if first_tomb == m.cap {
                first_tomb = bucket;
            }
        } else if h == hash && strmap_entry_key_eq::<V>(
            ox_transmute::<*mut StrMap<V>, *const StrMap<V>>(m),
            bucket, key, key_len)
        {
            // Match — overwrite value, leave key alone.
            m.entries[bucket].value = value;
            return false;
        }
        bucket = (bucket + 1) & mask;
    }
}

fn strmap_remove<V>(m: *mut StrMap<V>, key: *const [u8], key_len: usize) -> bool {
    if m.cap == 0 {
        return false;
    }
    let hash: u64 = strmap_hash(key, key_len);
    let mask: usize = m.cap - 1;
    let mut bucket: usize = (hash as usize) & mask;
    loop {
        let h: u64 = m.entries[bucket].hash;
        if h == 0 {
            return false;
        }
        if h == hash && strmap_entry_key_eq::<V>(
            ox_transmute::<*mut StrMap<V>, *const StrMap<V>>(m),
            bucket, key, key_len)
        {
            // Free the owned key, mark slot as tombstone.
            let raw: *mut u8 = ox_transmute::<*mut [u8], *mut u8>(m.entries[bucket].key_ptr);
            free(raw);
            m.entries[bucket].key_ptr = ox_transmute::<usize, *mut [u8]>(0);
            m.entries[bucket].key_len = 0;
            m.entries[bucket].hash    = 1;
            m.len   = m.len - 1;
            m.tombs = m.tombs + 1;
            return true;
        }
        bucket = (bucket + 1) & mask;
    }
}

// Frees all keys plus the entries array. Does not touch values.
fn strmap_free<V>(m: *mut StrMap<V>) {
    if m.cap == 0 {
        return;
    }
    let mut i: usize = 0;
    while i < m.cap {
        let h: u64 = m.entries[i].hash;
        if h != 0 && h != 1 {
            let raw: *mut u8 = ox_transmute::<*mut [u8], *mut u8>(m.entries[i].key_ptr);
            free(raw);
        }
        i = i + 1;
    }
    let raw_entries: *mut u8 =
        ox_transmute::<*mut [StrMapEntry<V>], *mut u8>(m.entries);
    free(raw_entries);
    m.entries = ox_transmute::<usize, *mut [StrMapEntry<V>]>(0);
    m.cap   = 0;
    m.len   = 0;
    m.tombs = 0;
}

// --- Internal -------------------------------------------------------

// FNV-1a 64-bit. Output 0 is remapped to 1 to keep 0 reserved as
// the "empty slot" sentinel.
fn strmap_hash(key: *const [u8], key_len: usize) -> u64 {
    let mut h: u64 = 14695981039346656037;     // FNV offset basis
    let mut i: usize = 0;
    while i < key_len {
        h = h ^ (key[i] as u64);
        h = h * 1099511628211;                  // FNV prime
        i = i + 1;
    }
    if h == 0 { 1 } else { h }
}

fn strmap_next_pow2(n: usize) -> usize {
    let mut p: usize = 1;
    while p < n {
        p = p * 2;
    }
    p
}

// Allocate `key_len` bytes and copy `key` into them.
fn strmap_copy_key(key: *const [u8], key_len: usize) -> *mut [u8] {
    let raw: *mut u8 = malloc(if key_len == 0 { 1 } else { key_len });
    if ox_transmute::<*mut u8, usize>(raw) == 0 {
        strmap_die("strmap: out of memory copying key");
    }
    let typed: *mut [u8] = ox_transmute::<*mut u8, *mut [u8]>(raw);
    let mut i: usize = 0;
    while i < key_len {
        typed[i] = key[i];
        i = i + 1;
    }
    typed
}

fn strmap_entry_key_eq<V>(m: *const StrMap<V>, bucket: usize,
                          key: *const [u8], key_len: usize) -> bool {
    let entry_len: usize = m.entries[bucket].key_len;
    if entry_len != key_len {
        return false;
    }
    let entry_key: *mut [u8] = m.entries[bucket].key_ptr;
    let mut i: usize = 0;
    while i < key_len {
        if entry_key[i] != key[i] {
            return false;
        }
        i = i + 1;
    }
    true
}

// Reallocate `m`'s entries to `new_cap` (power of two), rehashing
// every live entry. Old key allocations are reused (we move the
// pointer; we don't copy the bytes).
fn strmap_grow_to<V>(m: *mut StrMap<V>, new_cap: usize) {
    let old_entries: *mut [StrMapEntry<V>] = m.entries;
    let old_cap: usize = m.cap;
    let raw: *mut u8 = calloc(new_cap, ox_size_of::<StrMapEntry<V>>());
    if ox_transmute::<*mut u8, usize>(raw) == 0 {
        strmap_die("strmap: out of memory in grow_to");
    }
    m.entries = ox_transmute::<*mut u8, *mut [StrMapEntry<V>]>(raw);
    m.cap   = new_cap;
    m.len   = 0;
    m.tombs = 0;

    let mask: usize = new_cap - 1;
    let mut i: usize = 0;
    while i < old_cap {
        let h: u64 = old_entries[i].hash;
        if h != 0 && h != 1 {
            // Move (key, len, hash, value) into the new table.
            let mut bucket: usize = (h as usize) & mask;
            while m.entries[bucket].hash != 0 {
                bucket = (bucket + 1) & mask;
            }
            m.entries[bucket].key_ptr = old_entries[i].key_ptr;
            m.entries[bucket].key_len = old_entries[i].key_len;
            m.entries[bucket].hash    = h;
            m.entries[bucket].value   = old_entries[i].value;
            m.len = m.len + 1;
        }
        i = i + 1;
    }

    if old_cap > 0 {
        let raw_old: *mut u8 =
            ox_transmute::<*mut [StrMapEntry<V>], *mut u8>(old_entries);
        free(raw_old);
    }
}

fn strmap_die(msg: *const [u8]) {
    puts(msg);
    abort();
}
