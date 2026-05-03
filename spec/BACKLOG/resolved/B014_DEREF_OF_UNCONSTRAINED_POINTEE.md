# B014 — Deref of unconstrained pointee silently produces `Error` → codegen ICE

## Original report

Surfaced by the soundness audit on 2026-05-03.

## The bug

`null` literal types as `Ptr(α, Mut)` where α is a fresh Infer var
(`src/typeck/check.rs:628-638`). When the user writes `*null` in a
position that doesn't pin α (e.g., a discarded statement, an
unconstrained `let _ = *null;`), the Deref arm at
`src/typeck/check.rs:915-953` does NOT fire `CannotInfer` for the
pointee:

```rust
TyKind::Ptr(pointee, _) => {
    let span = self.hir.exprs[inner].span.clone();
    inf.obligations.push(Obligation::Sized {
        ty: pointee,
        pos: SizedPos::Deref,
        span,
    });
    pointee                          // <-- pointee may be Infer(α)
}
```

The existing `CannotInfer` arm at line 940-944 only fires when the
*operand of Deref* itself resolves to `Infer(_)` (e.g. `let x; *x;`),
not when the pointee is. So `*null` returns the unbound α as the
expression type.

At fn finalize, α has no constraint and defaults to `Error`. Then
`discharge_sized` for the queued obligation no-ops on `Error` (Error
is the absorbing type by design). The expression's recorded type in
`expr_tys` is `Error`. Codegen `lower_ty(Error)` panics
(`src/codegen/ty.rs:78-80`).

## Failing case (verified — codegen panic)

```rust
fn main() -> i32 {
    let _y = *null;       // y: Error; codegen: panic at lower_ty(Error)
    0
}
```

Same shape: any Deref on a pointer whose pointee is an unbound Infer
var at finalize. The `_` binding is what prevents the let-binding's
own type-flow from constraining α back to a concrete type.

## Severity

**Medium** — silent acceptance at typeck, ICE at codegen. The user
gets "compiler bug" rather than a structured diagnostic.

## Fix sketch

In the Deref arm, after resolving the pointee, check whether the
pointee is `Infer(_)`. If so, fire `CannotInfer { span }` immediately
rather than queuing a Sized obligation that will silently no-op when
the var defaults to `Error`. Alternatively, extend `discharge_sized`
to fire `CannotInfer` (or a fresh `CannotInferPointee` variant) when
it sees `Error` at `SizedPos::Deref` — but the Deref-site check has
the better span.

## Related

- B009 (`as` cast unvalidated) is the broader pattern: typeck assumes
  "if it's a Ptr at all I'm fine," then the pointee details bite at
  codegen.
- The existing `CannotInfer` arm at line 940-944 handles the
  *operand* unbound case correctly; this is the same idea applied
  one level deeper to the pointee.
