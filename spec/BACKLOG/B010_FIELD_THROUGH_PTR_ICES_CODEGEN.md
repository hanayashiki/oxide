# B010 — Field access through pointer base ICEs codegen (auto-deref mismatch)

## Original report

Surfaced by the soundness audit on 2026-05-03.

## The bug

typeck's `auto_deref_ptr` (`src/typeck/check.rs:507-519`) peels
arbitrary `Ptr` layers when the user writes `q.x` for `q: *mut P`.
`infer_field` calls it (`src/typeck/check.rs:822`) and returns the
field type cleanly. The result is recorded in `expr_tys[base]` *as
the original `Ptr(_, _)` type*, not as the peeled `Adt(_)`.

Codegen's lvalue and rvalue Field arms do not mirror the deref. They
both read `expr_tys[base]` and panic if it isn't `Adt(_)`:

```rust
// src/codegen/lower.rs:685-693 (lvalue path)
HirExprKind::Field { base, name } => {
    let base_ptr = self.lvalue(fx, base);
    let base_ty = self.ty_of(base);
    let aid = match self.typeck_results.tys().kind(base_ty) {
        TyKind::Adt(aid) => *aid,
        other => panic!("Field base lvalue: non-Adt type {:?}", other),
    };
    self.field_gep(base_ptr, base_ty, self.field_index(aid, &name))
}
```

```rust
// src/codegen/lower.rs:736-738 (rvalue path)
let aid = match self.typeck_results.tys().kind(base_ty) {
    TyKind::Adt(aid) => *aid,
    other => panic!("Field rvalue: non-Adt base type {:?}", other),
};
```

The user's only working idiom today is the explicit `(*q).x`. Existing
JIT tests use that form (`tests/snapshots/jit/i32/deref_field_roundtrip.ox`);
bare `q.x` was never exercised.

## Failing case (verified — codegen panic)

```rust
struct P { x: i32 }
fn read(q: *mut P) -> i32 { q.x }              // panic: Field rvalue: non-Adt base type Ptr(...)
fn write(q: *mut P) { q.x = 1; }                // panic: Field base lvalue: non-Adt type Ptr(...)
fn addr(q: *mut P) -> *mut i32 { &mut q.x }    // panic via lvalue path
fn main() -> i32 { 0 }
```

## Severity

**High** — codegen ICE on a single-line program that typechecks
cleanly. User-visible effect is "compiler bug" on natural
Rust-style code; the workaround `(*q).x` is undiscoverable.

## Fix sketch

Two viable approaches:

### Option A — HIR adjustment pass

When typeck's `infer_field` auto-derefs through N pointer layers,
insert N explicit `UnOp::Deref` nodes around the base in HIR.
Codegen then sees a clean `Adt(_)` base. Mirrors Rust's "adjustment"
model.

Pros: codegen stays simple; one canonical IR shape.
Cons: HIR grows a new mutation pass; expr_tys for the inserted
deref nodes need to be backfilled.

### Option B — auto-deref loop in codegen Field arms

Mirror `emit_index_place`'s existing auto-deref loop in both
`lvalue(Field)` and `emit_field`: peel `Ptr` layers via
load-pointer until the underlying Adt surfaces, then GEP/load.

Pros: localized to codegen; matches an existing pattern (indexing
through pointer-to-array already does this).
Cons: duplicated logic in two places (lvalue + rvalue); HIR shape
diverges from typeck's mental model.

Recommend **B** — matches the precedent already used for
`arr[i]` where `arr: *const [T; N]`.

## Related

- `emit_index_place` already does the auto-deref-through-pointer
  dance for indexing. Same shape.
- B009 (`as` cast unvalidated) — adjacent example of typeck and
  codegen disagreeing on what types reach the lowering layer.
