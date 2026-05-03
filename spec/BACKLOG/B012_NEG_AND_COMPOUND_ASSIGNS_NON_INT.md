# B012 — `Neg`/`BitNot` and compound assigns accept non-integer operands → codegen ICE

## Original report

Surfaced by the soundness audit on 2026-05-03.

## The bug

Two adjacent gaps in typeck — the codegen comment says "trust" and
codegen says "panic":

### Unary `Neg`/`BitNot` (`src/typeck/check.rs:905-908`)

```rust
match op {
    UnOp::Neg | UnOp::BitNot => t,  // numeric / integer (typeck v0 trusts; codegen checks)
    UnOp::Not => { /* equates t to bool */ }
    UnOp::Deref => { /* Ptr peel */ }
}
```

The operand type is returned as-is regardless of whether it's a
primitive integer, a pointer, an array, an Adt, or a unit. Codegen
`emit_unary` then panics in `into_int_value()`. The "codegen checks"
comment is wrong — codegen panics, it does not check.

### Compound assignment (`src/typeck/check.rs:998-1021`)

```rust
fn infer_assign(
    &mut self,
    inf: &mut Inferer,
    _op: AssignOp,             // <-- discarded
    target: HExprId,
    rhs: HExprId,
    span: &Span,
) -> TyId { /* ... unify::subtype(r, t, ...) ... */ }
```

The `AssignOp` discriminant is unused. `+=`, `-=`, `*=`, `&=`, etc.
typecheck whenever a plain `=` would. Codegen `emit_assign` for the
compound case extracts integer values from both sides → panic on
non-int.

## Failing cases (verified — codegen panic)

```rust
// Neg on pointer
fn bad1() -> i32 {
    let p: *const i32 = null;
    let _ = -p;
    0
}

// BitNot on pointer
fn bad2() -> i32 {
    let p: *const i32 = null;
    let _ = ~p;
    0
}

// Compound assign on pointer
fn bad3() {
    let mut p: *const i32 = null;
    let q: *const i32 = null;
    p += q;                                 // panic in into_int_value()
}

// Compound assign on array
fn bad4() {
    let mut a: [i32; 3] = [1, 2, 3];
    let b: [i32; 3] = [4, 5, 6];
    a += b;                                 // panic in into_int_value()
}
```

## Severity

**High** — codegen ICE on tiny programs that typecheck. Same family
as B009/B011 (typeck-doesn't-filter, codegen-assumes-integer).

## Fix sketch

Two minimal restrictions in typeck:

1. **`infer_unary`** for `Neg` and `BitNot`: require operand type
   to resolve to a primitive integer. `PrimTy::is_integer()` already
   exists in `src/typeck/ty.rs:98-100`. Add
   `TypeError::UnaryNonInteger { op, found, span }`.

2. **`infer_assign`**: dispatch on `op`. For
   `AssignOp::Plus | Minus | Mul | Div | Rem | And | Or | Xor | Shl | Shr`,
   require both target and rhs to be primitive integer (mirror the
   `infer_binary` arithmetic arm constraint). For `AssignOp::Eq`
   (plain `=`), keep current behavior.

Drop the "typeck v0 trusts; codegen checks" comment — it's
misleading.

## Related

- B009 (`as` cast), B011 (ptr compare): same family.
