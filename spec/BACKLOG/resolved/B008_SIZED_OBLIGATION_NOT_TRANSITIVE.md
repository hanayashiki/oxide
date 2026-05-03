# B008 — `Sized` obligation only checks the outer kind (codegen ICE)

## Status
**Resolved 2026-05-03.** Closed by `discharge_sized` walking
`Array(_, Some(_))` element types recursively (Option A from this
doc). Stops at `Ptr` (the pointer is sized; pointee can be unsized).
Test:
`tests/snapshots/typeck/error_unsized_nested_in_sized_array.ox`.

## Original report

Surfaced by the soundness audit on 2026-05-03.

## The bug

`discharge_obligation` for `Obligation::Sized`
(`src/typeck/check.rs:369-378`) tests only the outer `TyKind` of the
type:

```rust
if let TyKind::Array(_, None) = self.tys.kind(resolved) {
    self.errors.push(TypeError::UnsizedArrayAsValue { pos, span });
}
```

A nested unsized layer — e.g. `Array(Array(u8, None), Some(3))` —
passes this check (outer is `Array(_, Some(3))`), since the inner
`Array(_, None)` is not inspected. Codegen later panics on
`lower_ty(Array(_, None))` (`src/codegen/ty.rs:71-73`):

```rust
TyKind::Array(_, None) => {
    unreachable!("Array(_, None) is not a value type; typeck E0269 should have rejected")
}
```

Same risk for nested unsized through:

- ADT field types of nested-unsized shape.
- Function parameter / return type at internal calls.
- Deref of `*const [[u8]; 3]` at value position.

## Failing case (verified — codegen ICE)

```rust
struct S { f: [[u8]; 3] }
fn main() -> i32 { 0 }
```

```text
thread 'main' panicked at src/codegen/ty.rs:76:13:
internal error: entered unreachable code:
Array(_, None) is not a value type; typeck E0269 should have rejected
```

The outer `Array(_, Some(3))` field type passes the field's Sized
obligation; the inner `[u8]` is never checked.

## Severity

Compiler crash (ICE) on input that should have been rejected by
typeck. Always serious to fix — `unreachable!` in production paths
on user input is a quality-of-implementation bug.

## Fix sketch

Two viable approaches; the second is simpler.

### Option A — recursive Sized check at discharge

Walk the type structurally inside the discharge handler:

```rust
fn check_sized_recursively(&mut self, ty: TyId, pos: SizedPos, span: Span) {
    let resolved = self.resolve_fully(/* ... */, ty);
    match self.tys.kind(resolved) {
        TyKind::Array(_, None) => {
            self.errors.push(TypeError::UnsizedArrayAsValue { pos, span });
        }
        TyKind::Array(elem, Some(_)) => {
            self.check_sized_recursively(*elem, pos, span);
        }
        // Other type kinds bottom out as sized in v0; if/when DST
        // generics land, this list grows.
        _ => {}
    }
}
```

Pros: localized to one place (the discharge site). Cons: doesn't
catch the failure at the type-construction site, so the diagnostic
points at the let / param / return rather than the type literal.

### Option B — enqueue Sized on inner array element at type construction

When `resolve_ty` builds an `Array(elem, Some(_))` type, also enqueue
a Sized obligation on `elem`. Same for ADT field types. The existing
discharge handler then catches it.

Pros: caught earlier; better span (the inner type's span is available
at `resolve_ty` time). Cons: more places to plumb.

Recommend **Option A** for v0 simplicity; revisit if/when generics
land and the recursive walk becomes the natural shape anyway.

## Related

- The outer-only check is already documented at `src/typeck/check.rs:369-378`.
  Updating that comment is part of the fix.
- spec/09 §"Sized obligation" should mention the recursive check as
  part of the fix.

## Out of scope

- Generics-era `Sized` trait. The recursive check above is sufficient
  until generics land; at that point `Sized` becomes a real trait
  with `T: Sized` bounds and the predicate becomes structural via
  trait resolution.
