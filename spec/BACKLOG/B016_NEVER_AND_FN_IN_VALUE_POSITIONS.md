# B016 — `Never` and `Fn` types accepted in value positions → codegen ICE

## Original report

Surfaced by the soundness audit on 2026-05-03.

## The bug

The Sized obligation in `src/typeck/check/unify.rs:470-482` only
screens for unsized arrays (`Array(_, None)` and nested). It does
NOT reject other non-value-type kinds:

- `TyKind::Never` is the bottom type — uninhabited — so it's not a
  valid value type for a fn-param, struct field, let-binding, or
  fn-return-by-value-as-storage. The typeck arena even lets `"never"`
  flow in via the type syntax (`from_prim_name` at
  `src/typeck/ty.rs:241-258`).
- `TyKind::Fn(_, _, _)` is currently constructed for function items
  (`HirExprKind::Fn(fid)` arm at `src/typeck/check.rs:640-643`), but
  spec/15 §"Out of scope" defers fn-as-value pending a fn-pointer
  spec.

Both flow to codegen:

- `lower_ty(Never)` panics with `"!-typed expressions terminate the
  BB before any consumer asks for a slot"` (`src/codegen/ty.rs:66-69`).
  False — the consumer is a param/field whose typeck never rejected it.
- `lower_ty(Fn)` panics with `"use lower_fn_type"` (`src/codegen/ty.rs:70`).
  Also false at the call site — `lower_fn_type` is only for function
  *signatures*, not for `Fn`-typed values.

## Failing cases (verified — codegen panic)

```rust
// Never as fn-param
fn foo(x: never) -> i32 { 0 }                      // panic at lower_ty(Never)
fn main() -> i32 { 0 }
```

```rust
// Never as let-binding type
fn main() -> i32 {
    let _x: never;                                  // panic at lower_ty(Never)
    0
}
```

```rust
// Fn as let-binding (fn-pointer-as-value)
fn helper() -> i32 { 42 }
fn main() -> i32 {
    let f = helper;                                 // f: Fn([], i32, false); panic at lower_ty(Fn)
    0
}
```

## Severity

**Medium** — codegen ICE on programs that should be rejected at
typeck with a clean diagnostic.

## Fix sketch

Extend the value-type obligation (the existing `Sized` check or a
sibling) to reject `Never` and `Fn` at value positions. Concretely,
in `discharge_sized`:

```rust
match self.tys.kind(resolved) {
    TyKind::Array(_, None) => /* existing E0269 */,
    TyKind::Array(elem, Some(_)) => /* recurse */,
    TyKind::Never => self.errors.push(TypeError::NeverAsValue { pos, span }),
    TyKind::Fn(..) => self.errors.push(TypeError::FnAsValue { pos, span }),
    _ => {}
}
```

The two new error variants share the existing `SizedPos` (LetBinding
/ FnParam / FnRet / FieldDef / Deref) so the diagnostic locations
come for free.

Optionally drop `"never"` from `from_prim_name` to give the user a
"type not found" error at the syntactic level. Up to taste.

## Related

- spec/15_VARIADIC.md §"Out of scope" (fn pointers deferred).
- spec/06_LLVM_CODEGEN.md (the `Never` panic message conflates "after
  a divergent expr" with "as a value type" — message can be improved
  alongside the fix).

## Out of scope

- Implementing fn pointers as a real value type. Wait for the
  fn-pointer spec.
