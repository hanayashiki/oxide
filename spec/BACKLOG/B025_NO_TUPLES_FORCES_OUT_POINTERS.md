# B025 — No tuple types forces the out-pointer pattern in stage-1

## Original report

Surfaced 2026-05-06 building stage-1. Lower-ranked pain than B022
or B023; reported for completeness because the workaround is ugly
enough to mention.

## The gap

Oxide has no tuple types. Multi-return is unavailable; functions
that want to return two values either:

1. Take an out-pointer arg and return `bool`/the primary value
   (the "C-ish" shape).
2. Wrap the pair in a one-off named struct.

Stage-1 picked (1) because struct-of-the-week is itself noise.

## Concrete sites

### Hash-map probe lookup

`example-projects/oxide/util/strmap.ox:113-122`:

```rust
fn strmap_get<V>(m: *const StrMap<V>, key: *const [u8], key_len: usize, out: *mut V) -> bool {
    if m.cap == 0 { return false; }
    // ...
    if h == hash && strmap_entry_key_eq::<V>(m, bucket, key, key_len) {
        *out = m.entries[bucket].value;
        return true;
    }
    // ...
}
```

The natural shape is `fn strmap_get<V>(...) -> Option<V>` or `fn
strmap_get<V>(...) -> (bool, V)`. Without `Option` (B023) and
without tuples, every call site needs a stack-local of type V,
takes its address, then conditionally reads it back.

### Scope-stack lookup

`example-projects/oxide/hir_lower.ox:200-218`:

```rust
fn lookup_value(l: *const Lowerer, off: usize, len: usize, out_id: *mut usize) -> u8 {
    // returns kind via the function and id via the out-pointer; kind = 255 = miss
}
```

Same shape: kind comes back as the return value, id through an
out-pointer. Two pieces of information, two channels.

### Probe disambiguation in hash table insert

`example-projects/oxide/util/strmap.ox:159-189` — probe loop wants
to return `(target_bucket, was_match)` to signal "insert here vs.
overwrite here." The current code inlines the loop and uses a
sentinel `first_tomb == m.cap` to encode "no tombstone seen." Would
be a tuple if tuples existed.

## Severity

**Low** — workable everywhere, just inelegant. The out-pointer
pattern propagates through call sites; ~10 sites in stage-1.

## Fix sketch

Tuple types as anonymous structural products:

```text
TupleType ::= '(' Type (',' Type)+ ','? ')'        # 2+ elements
Tuple1    ::= '(' Type ',' ')'                     # singleton — Rust-compatible
TupleExpr ::= '(' Expr (',' Expr)+ ','? ')'
TupleIdx  ::= Expr '.' IntLit                      # `t.0`, `t.1`
```

- Anonymous, structural. `(i32, bool)` and `(i32, bool)` are the
  same type whatever the source location.
- No destructuring `let (a, b) = …` in v0 — read via `t.0` / `t.1`.
  Destructuring slots in alongside `match` (B023).
- LLVM lowering: same as a struct with implicit field names
  `0`, `1`, … . Reuses the struct-by-value plumbing.
- Unit `()` already exists; v0 tuples start at 2-arity. (Singleton
  `(T,)` reserved syntactically for forward compat with patterns.)

## Concrete payoff

Each out-pointer pattern collapses:

```rust
// Before
fn strmap_get<V>(m, k, klen, out: *mut V) -> bool { … }

let mut tmp: V = …;
let ok = strmap_get::<V>(&map, key, len, &mut tmp);
if ok { use(tmp); }

// After
fn strmap_get<V>(m, k, klen) -> (bool, V) { … }

let (ok, val) = strmap_get::<V>(&map, key, len);
if ok { use(val); }
```

Stage-1 saves ~5–10 LoC per call site (declare local, take
address) plus removes the "what if I forget to check the bool"
foot-gun.

## Related

- B023 (enum + match) — `Option<T>` is the better answer for many
  of these sites; tuples cover the cases where both elements are
  always meaningful (e.g. `(target_bucket, was_match)` in probe).
  The two features are complementary.

## Out of scope

- Named tuple fields (would just be a record struct).
- Pattern destructuring `let (a, b) = t`. Lands with B023.
- Tuple structs `struct Pair(i32, bool)`. Independent feature.
