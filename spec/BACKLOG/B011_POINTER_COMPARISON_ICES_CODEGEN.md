# B011 — Pointer comparison ICEs codegen via `into_int_value()`

## Original report

Surfaced by the soundness audit on 2026-05-03.

## The bug

typeck's `infer_binary` for comparison operators just `equate`s
lhs/rhs and returns `bool` (`src/typeck/check.rs:982-986`):

```rust
BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
    unify::equate(self, inf, lt, rt, span.clone());
    bool_ty
}
```

There is no constraint that the operand be a primitive integer,
bool, or anything else codegen can handle. Two same-typed `*const T`
operands trivially equate (same TyId on both sides, no walk).

Codegen's `emit_binary` unconditionally extracts an `IntValue` from
each operand (`src/codegen/lower.rs:1061-1062`):

```rust
let l = self.load_value(l_op, lt, "load").into_int_value();   // panics on PointerValue
let r = self.load_value(r_op, rt, "load").into_int_value();
```

`into_int_value()` is an inkwell helper that panics on non-int
`BasicValueEnum`. Pointer operands hit it immediately.

`null` itself triggers the same shape — its type is `Ptr(α, Mut)`
per `src/typeck/check.rs:628-638` — so `null == null` and `p == null`
ICE codegen identically.

## Failing case (verified — codegen panic)

```rust
fn main() -> i32 {
    let p: *const i32 = null;
    if p == null { 1 } else { 0 }       // panic in into_int_value()
}
```

Same shape for `<`, `<=`, `>`, `>=`, `!=` on any pointer pair.

## Severity

**High** — codegen ICE on a one-liner. spec/07 already documents
"no `==` on pointers in v0; add later via a dedicated small spec",
but the deferral is not enforced anywhere.

## Fix sketch

Two choices:

### Option A — reject in typeck (per spec/07)

In `infer_binary`'s comparison arm, require the equated type to be
a primitive integer (or `bool` for Eq/Ne only). Emit
`TypeError::CannotCompare { ty, span }`. Reject pointer comparisons
until the dedicated pointer-comparison spec lands.

### Option B — implement via integer comparison

`build_int_compare` actually accepts pointer operands at the inkwell
level (LLVM `icmp eq`/`ne` is defined on pointers). Dispatch on
operand kind in `emit_binary` and generate the right `icmp` directly,
without the `into_int_value()` round-trip.

Recommend **A** for v0 (matches the spec deferral and keeps the
codegen surface small). Revisit when the pointer-comparison spec
lands.

## Related

- spec/07_POINTER.md §"Out of scope" defers pointer equality.
- B009 (`as` cast unvalidated) and B012 (compound assigns on non-int)
  are the same family — typeck didn't filter, codegen blindly
  assumed integer.
