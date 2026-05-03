# B015 — Array length silently truncated `u64`→`u32` at codegen — silent miscompilation

## Original report

Surfaced by the soundness audit on 2026-05-03.

## The bug

Typeck stores array length as `u64` (`TyKind::Array(TyId, Option<u64>)`
at `src/typeck/ty.rs:52`). Codegen lowers via inkwell's
`array_type(u32)` and silently casts (`src/codegen/ty.rs:71-74`):

```rust
TyKind::Array(elem, Some(n)) => {
    let elem_ll = lower_ty(ctx, tcx, adt_ll, *elem);
    elem_ll.array_type(*n as u32).into()    // <-- silent truncation
}
```

There is no upper-bound check anywhere on `HirConst::Lit(u64)` length
values. So `[u8; 4_294_967_296]` (i.e. `2^32`) typechecks as
`Array(u8, Some(1<<32))` and codegen produces `[0 x u8]` — a
zero-length array.

The bounds check (`src/codegen/lower.rs:266-291`) reads its limit
from the *typeck* length, so indices in `[0, 2^32)` pass the bounds
check and walk past the end of the actual allocation. **Silent
miscompilation, not a panic.**

## Failing case (silent OOB write)

```rust
fn main() -> i32 {
    let mut a: [u8; 4294967296] = [0; 4294967296];   // typeck: 2^32 bytes; codegen: 0 bytes
    a[100] = 1;                                       // bounds check: 100 < 2^32 (ok); writes past end
    0
}
```

The `[0; 4294967296]` repeat-loop counter is `i64`
(`src/codegen/lower.rs:378-381`), so it iterates 2^32 times, writing
past the slot. Even without the repeat, a single `a[100] = 1` writes
past end.

## Severity

**Medium** — silent OOB write on an unusual but well-formed program.
The fact that the bounds check passes makes this dangerous: tools
and audits can't catch it by looking at runtime traps.

## Fix sketch

Reject array lengths that overflow inkwell's `array_type(u32)` at
typeck. Two options:

### Option A — at type construction

In `resolve_ty` for `Array(_, Some(n))`, check `n > u32::MAX as u64`
and emit `TypeError::ArrayLengthTooLarge { len: n, span }`.

### Option B — via a new obligation

Push `Obligation::ArrayLengthInRange` from the Array
type-construction site, discharged with the same check at finalize.
More uniform with the rest of typeck but heavier.

Recommend **A** — the check is a single comparison at a well-defined
call site, no new infrastructure.

Long-term: the const-generic spec (deferred) will replace
`Option<u64>` with a richer constant representation; this check
moves into the const-eval layer at that point.

## Related

- spec/09_ARRAY.md §"Array length representation".
- B019 (bounds-check counter hardcoded i64) is the *other* side of
  the array-length width assumption.
