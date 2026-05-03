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

## Update 2026-05-04 — extended symptoms from docs walkthrough audit

`docs/src/01_walkthrough.md:155-163` claimed `(*ptr_to_p).x = 5;` was
the same as `ptr_to_p.x = 5;` (auto-deref). That's the design intent
typeck encodes via `auto_deref_ptr`, but the bug surfaces in two
different ways depending on how the pointer is bound:

### Variant 1 — concrete pointer type → codegen ICE (covered by original report)

```rust
struct Point { x: i32, y: i32 }

// Form A: fn parameter
fn read(q: *mut Point) -> i32 { q.x }            // ICE @ lower.rs:738

// Form B: let-binding with explicit type annotation
fn main() -> i32 {
    let mut p = Point { x: 1, y: 2 };
    let ptr: *mut Point = &mut p;                // explicit Ptr
    ptr.x = 5;                                   // ICE @ lower.rs:690
    ptr.x                                        // ICE @ lower.rs:738
}
```

Both forms panic in codegen with `Field {base lvalue,rvalue}: non-Adt
base type Ptr(...)`.

### Variant 2 — inferred pointer type → typeck E0256 (new finding)

```rust
fn main() -> i32 {
    let mut p = Point { x: 1, y: 2 };
    let ptr = &mut p;            // inferred — type is an unresolved Infer var
    ptr.x = 5;                   // E0256 "could not infer a type" + E0263
    ptr.x                        // E0256
}
```

Adding `: *mut Point` on the `let` rescues us into Variant 1 (codegen
ICE). Without the annotation, `&mut <place>` produces a type that
`auto_deref_ptr` apparently doesn't peel — the field-base type stays
ambiguous and typeck bails with E0256 before HIR reaches codegen. So
the bug *also* manifests as a confusing typeck error, not just a
codegen panic.

This means a fix for the codegen side is **necessary but not sufficient**
— the typeck inference for `let ptr = &mut p; ptr.x` also needs to
resolve `&mut p` to `*mut Point` before `auto_deref_ptr` can do its
job. Probably a small change in how addr-of's result type interacts
with the inferer when the pointee is a struct.

### Doc fallout

`docs/src/01_walkthrough.md:155-163` was rewritten 2026-05-04 to say
auto-deref is **not** supported and to require explicit `(*ptr).x`
syntax. Once B010 is fixed, the docs paragraph reverts to the
original "auto-deref works" form. Mark this issue as gating the doc
revert.

### Severity bump

**Critical for UX** — combines a misleading typeck diagnostic
("ambiguous type" when the user wrote a perfectly natural
`ptr.x = 5`) with a codegen ICE on the slightly-more-explicit form.
The two failure modes look unrelated to a user, multiplying the
debugging cost.
