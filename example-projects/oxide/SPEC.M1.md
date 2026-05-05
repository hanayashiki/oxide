# M1 — Utility Crate

The bedrock layer. Every later milestone consumes these. Five
modules; no inter-dependencies beyond stdlib and `intrinsics.ox` /
`mem.ox`.

```text
example-projects/oxide/util/
├── vec.ox          ← Vec<T>
├── strbuf.ox       ← StrBuf = growable byte buffer (≈ String)
├── strmap.ox       ← StrMap<V> = linear-probed string→V table
├── arena.ox        ← Arena<T> = id-based generic arena
└── io.ox           ← print_str / eprint_str / write_all
```

## Language constraints driving the design

Oxide today is rich enough but two language facts dominate every
shape decision:

1. **No pointer arithmetic.** `p + n` is deferred per spec/07. The
   v0 indexable buffer type is `*mut [T]` / `*const [T]` — pointer
   to unsized array. `buf[i]` is the only way to reach element `i`
   of a heap allocation. Therefore **every backing store is typed
   `*mut [T]`, not `*mut T`**.
2. **`null` infers to `*mut α` where `α` must be Sized.** Unsized
   `α = [T]` is dicey at best. We sidestep entirely by either (a)
   initializing `cap = 0` with `data = ox_transmute::<usize, *mut [T]>(0)`
   for the empty case, or (b) eagerly allocating on `vec_new`. We
   pick **(a)** — zero-allocation `vec_new` matches Rust/C++ Vec
   semantics and avoids the "every empty Vec costs a `malloc`" pit.

`ox_transmute<usize, *mut [T]>(0)` is sound: both are pointer-width
on v0 targets; mono runs the size-equality check at instantiation.
The resulting `*mut [T]` value is a null pointer that is never
indexed (we always check `cap` before access).

## `vec.ox`

```rust
struct Vec<T> {
    data: *mut [T],
    len:  usize,
    cap:  usize,
}

fn vec_new<T>() -> Vec<T>;
fn vec_with_capacity<T>(cap: usize) -> Vec<T>;
fn vec_len<T>(v: *const Vec<T>) -> usize;
fn vec_capacity<T>(v: *const Vec<T>) -> usize;
fn vec_push<T>(v: *mut Vec<T>, x: T);
fn vec_pop<T>(v: *mut Vec<T>) -> T;            // panics on empty
fn vec_get<T>(v: *const Vec<T>, i: usize) -> T;
fn vec_set<T>(v: *mut Vec<T>, i: usize, x: T);
fn vec_data_mut<T>(v: *mut Vec<T>) -> *mut [T];
fn vec_data<T>(v: *const Vec<T>) -> *const [T];
fn vec_clear<T>(v: *mut Vec<T>);
fn vec_free<T>(v: *mut Vec<T>);                // frees `data`, sets cap=0
fn vec_reserve<T>(v: *mut Vec<T>, additional: usize);
```

**Growth policy:** doubling, starting at 4. When `cap == 0`,
`vec_reserve` allocates `max(4, additional)` elements.

**Out-of-bounds:** `vec_get` / `vec_set` / `vec_pop` print to stderr
and call `abort()` on misuse. No `Option<T>` (no enum payloads).
Acceptable: this is a compiler's internal data structure; out-of-
bounds is a compiler bug, not user input.

**Drop:** none. Containers leak on shutdown. Compiler is a short-
lived process; the OS reaps. `vec_free` exists for callers that
want to compact mid-run, not for hygiene.

### Why no `(T, bool)` return for `vec_pop`?

Tuple types don't exist (no tuple structs in v0 either). Could
encode as a struct, but `vec_pop` is internal — the abort-on-empty
contract is fine.

## `strbuf.ox`

A thin layer over `Vec<u8>`. Justifies its own file because the
operations we want differ from `Vec<u8>`'s element-pushing —
`strbuf_push_str`, `strbuf_push_int`, `strbuf_as_cstr` (NUL-
terminate in place and return `*const [u8]`), etc.

```rust
struct StrBuf {
    bytes: Vec<u8>,
}

fn strbuf_new() -> StrBuf;
fn strbuf_with_capacity(cap: usize) -> StrBuf;
fn strbuf_len(s: *const StrBuf) -> usize;
fn strbuf_push_byte(s: *mut StrBuf, b: u8);
fn strbuf_push_str(s: *mut StrBuf, src: *const [u8], len: usize);
fn strbuf_push_cstr(s: *mut StrBuf, src: *const [u8]);   // strlen-based
fn strbuf_push_i64(s: *mut StrBuf, n: i64);              // formats decimal
fn strbuf_push_u64(s: *mut StrBuf, n: u64);
fn strbuf_push_hex_u64(s: *mut StrBuf, n: u64);          // for IR @const naming
fn strbuf_clear(s: *mut StrBuf);
fn strbuf_free(s: *mut StrBuf);
fn strbuf_as_ptr(s: *const StrBuf) -> *const [u8];
fn strbuf_as_mut_ptr(s: *mut StrBuf) -> *mut [u8];
fn strbuf_as_cstr(s: *mut StrBuf) -> *const [u8];        // ensures trailing \0
fn strbuf_eq_bytes(s: *const StrBuf, b: *const [u8], n: usize) -> bool;
```

**`strbuf_as_cstr`** writes a sentinel `\0` at `len` (without
incrementing `len`), then returns the data pointer. The next
mutation invalidates the sentinel. Used for FFI calls that need
NUL-terminated input.

**Decimal formatting** lives in `strbuf` not `vec` because
formatting is byte-specific. Implementations write into a fixed
local `[u8; 20]` (max digits for `u64`) and append.

## `strmap.ox`

Linear-probed open-addressed hash table. Keys are owned byte
sequences; values are `T`. Used for symbol tables (string → `FnId`,
string → `HAdtId`, etc.).

```rust
struct StrMapEntry<V> {
    key_ptr: *mut [u8],   // owned; freed on map_free
    key_len: usize,
    hash:    u64,         // cached; 0 means empty slot
    value:   V,
}

struct StrMap<V> {
    entries: *mut [StrMapEntry<V>],
    cap:     usize,       // power of 2; mask = cap - 1
    len:     usize,
    tombs:   usize,       // count of tombstones
}

fn strmap_new<V>() -> StrMap<V>;
fn strmap_with_capacity<V>(cap: usize) -> StrMap<V>;
fn strmap_len<V>(m: *const StrMap<V>) -> usize;
fn strmap_insert<V>(m: *mut StrMap<V>, key: *const [u8], key_len: usize, value: V) -> bool;
                                                         // true if newly inserted
fn strmap_get<V>(m: *const StrMap<V>, key: *const [u8], key_len: usize, out: *mut V) -> bool;
                                                         // true ⇒ wrote *out
fn strmap_contains<V>(m: *const StrMap<V>, key: *const [u8], key_len: usize) -> bool;
fn strmap_remove<V>(m: *mut StrMap<V>, key: *const [u8], key_len: usize) -> bool;
fn strmap_free<V>(m: *mut StrMap<V>);
```

**Hash:** FNV-1a over the key bytes. Hard-coded — no traits, no
hasher abstraction. For the compiler's sole use case (interning
identifiers), FNV-1a is fine.

**Reserved hash 0:** if FNV-1a happens to produce 0 (vanishingly
rare for non-empty keys, but possible), we map it to 1 internally.
`hash == 0` is the "empty slot" sentinel.

**Tombstones:** `key_ptr = null, hash = 1` marks a removed slot.
We rebuild on `tombs >= cap / 2` to amortize.

**Load factor:** grow at 0.75. Cap is always a power of two.

**No `Option<V>` return on `get`:** out-pointer pattern, returns
`bool` for hit/miss, writes through the out-pointer on hit. Same
rationale as `vec_pop` — no enum-payloads.

### Memory ownership

`strmap_insert` **copies** the key bytes into a fresh allocation
owned by the map. `strmap_free` walks all live entries and frees
each `key_ptr`. Callers retain ownership of the value `V` in the
sense that `strmap_free` does not recurse into `V`s.

If `V` is itself heap-owning, the caller must walk the map first
and free those resources before `strmap_free` (we have no
generic `Drop`). The compiler's symbol tables key by string and
value to plain integer IDs (`FnId`, `HAdtId`), so this never
matters in practice.

## `arena.ox`

Thin layer over `Vec<T>`: hand out `usize` indices on push, look up
by index. Mirrors stage 0's `IndexVec<I, T>` minus the typed `I`
(no zero-sized phantoms in Oxide; we accept the looser typing).

```rust
struct Arena<T> {
    items: Vec<T>,
}

fn arena_new<T>() -> Arena<T>;
fn arena_alloc<T>(a: *mut Arena<T>, x: T) -> usize;
fn arena_get<T>(a: *const Arena<T>, id: usize) -> T;
fn arena_set<T>(a: *mut Arena<T>, id: usize, x: T);
fn arena_len<T>(a: *const Arena<T>) -> usize;
fn arena_free<T>(a: *mut Arena<T>);
```

**Typed wrapping at use sites.** Stage 0's `FnId`, `HAdtId`,
`HExprId` are zero-cost newtypes via `define_index_type!`. In
Oxide we wrap explicitly:

```rust
struct FnId   { idx: usize }
struct HAdtId { idx: usize }
```

Callers do `FnId { idx: arena_alloc(&mut fns, hir_fn) }`. This
costs an extra struct copy per allocation, but ADTs in Oxide are
trivially copyable and the cost is in the noise compared to the
arena ops themselves.

**Why generic-`Arena<T>` instead of typed-Arena-per-thing.** We
could write `FnArena`, `AdtArena`, `LocalArena`, etc., one struct
per item type. That avoids generics entirely. The cost is N copies
of the same growable-array logic, kept in sync by hand. Generic
`Arena<T>` is the cheaper option as long as monomorphization works
(it does — spec/16).

## `io.ox`

The minimum needed for stage 1 to emit IR + diagnostics.

```rust
extern "C" {
    fn write(fd: i32, buf: *const [u8], n: usize) -> isize;
    fn fputs(s: *const [u8], stream: *mut u8) -> i32;
    fn perror(s: *const [u8]);
    fn fflush(stream: *mut u8) -> i32;
}

fn io_print(buf: *const [u8], len: usize);          // fd=1
fn io_eprint(buf: *const [u8], len: usize);         // fd=2
fn io_print_strbuf(s: *const StrBuf);
fn io_eprint_strbuf(s: *const StrBuf);
fn io_print_cstr(s: *const [u8]);                   // strlen-then-write
fn io_eprint_cstr(s: *const [u8]);
fn io_flush();                                      // stdout
```

We write directly to fds via `write(2)`. Avoids dependency on
`stdout` global (which would need a separate FFI declaration with
`*mut FILE` semantics, and fputs's interaction with stdio
buffering).

For large IR output, the caller buffers into a `StrBuf` and dumps
via `io_print_strbuf` once — no incremental syscall pressure.

## Tests

Per project memory: snapshot-only, Jest-style auto-bless on first
run. Tests run via the **stage 0** compiler — they are ordinary
Oxide programs that exercise the utilities and print a stable
trace.

```text
example-projects/oxide/tests/
├── m1_vec.ox
├── m1_strbuf.ox
├── m1_strmap.ox
├── m1_arena.ox
├── m1_io.ox
└── snapshots/
    ├── m1_vec.ox.snap
    ├── ...
```

Each test is a `fn main() -> i32` that performs a fixed sequence
of ops and prints the final state. The `.snap` file is the
expected stdout. First run captures, subsequent runs `diff`.

Snapshot harness: a small bash script (`tests/run.sh`) runs each
`.ox` under the host compiler (`oxide --emit exe …`), captures
stdout, compares to `.snap`. Auto-blesses missing snapshots.

This is intentionally not the host's Cargo `tests/` machinery —
it's a separate harness because the things-under-test are Oxide
programs, not Rust modules.

## Acceptance for M1

All of the following must pass under stage 0:

1. `oxide example-projects/oxide/tests/m1_vec.ox` runs and matches
   `m1_vec.ox.snap`.
2. Same for `m1_strbuf`, `m1_strmap`, `m1_arena`, `m1_io`.
3. `tests/run.sh` exits 0 across the suite.

No stage 1 code yet. That's M2.

## Open / deferred

- **`Vec<T>` cloning.** Not needed by the compiler (we move via
  `*mut Vec<T>`). Skipped.
- **Iterator helpers.** No closures. Callers use index loops.
- **`strmap` iteration.** Needed eventually (debug-dump, codegen
  pass over symbol table). Add `strmap_for_each_entry` taking a
  `*mut u8` user-data + a function pointer once we commit to the
  function-pointer encoding. Defer to M2 prep — first place it's
  actually needed.
- **String interning.** A natural future addition (`Interner` =
  `StrMap<u32>` + `Vec<StrBuf>`). Not in M1; we call it out so the
  shape is anticipated.
