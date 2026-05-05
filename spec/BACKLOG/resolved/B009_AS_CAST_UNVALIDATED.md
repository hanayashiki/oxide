# B009 — `as` cast operator is entirely unvalidated (spec/12 unimplemented)

## Original report

Surfaced by the soundness audit on 2026-05-03.

## The bug

`infer_expr`'s Cast arm at `src/typeck/check.rs:688-691` discards the
inner expression's type and unconditionally returns the resolved
target type:

```rust
HirExprKind::Cast { expr: inner, ty } => {
    let _ = self.infer_expr(inf, inner);
    Self::resolve_ty(&mut self.tys, &mut inf.errors, &ty)
}
```

There is no `cast_kind` table, no `TypeError::InvalidCast` variant
(grep `src/typeck/error.rs`), and the only thing that "rejects"
anything today is the `as_prim(dst).expect("v0: cast target must
be a primitive")` assertion at `src/codegen/lower.rs:1417-1418` —
which panics rather than diagnosing.

spec/12_AS.md is the entire spec for cast validity. None of it is
enforced. The full table — primitive↔primitive (sign / width rules),
pointer↔pointer (mutability rules), pointer↔integer (forbidden in
v0), bool↔int — is unimplemented.

## Why this matters

spec/12 explicitly identifies `*const T as *mut T` as **the**
soundness boundary for v0 because the language has no `unsafe`
block. Today this cast typechecks and then ICEs codegen at
`as_prim(*mut T).expect(...)` — so the user-visible failure is
"compiler bug" rather than silent UB. **The moment codegen grows
the `(Ptr, Ptr) → no-op` arm prescribed in spec/12 §"Codegen
sketch", mut-laundering becomes silent UB with no diagnostic.**
Implement the validation now, before that arm lands.

## Failing cases (all currently typecheck)

```rust
fn launder(p: *const i32) -> *mut i32 { p as *mut i32 }    // soundness: mut-launder
fn from_int() -> *const i32 { 5 as *const i32 }             // arbitrary int → ptr
fn p2p(p: *const u8) -> *const i32 { p as *const i32 }      // reinterpret pointee
fn p2i(p: *const i32) -> i32 { p as i32 }                   // ptr → int (lossy on 64-bit)
struct S { x: i32 }
fn s2i(s: S) -> i32 { s as i32 }                            // adt → int
```

All compile to typeck-OK then codegen panic today; all become silent
miscompiles the moment the codegen no-op-ptr-cast arm lands.

## Severity

**High** — the only spec'd UB boundary in v0, currently masked only
by codegen panics. The order of operations matters: fix typeck
*before* codegen gets any more permissive.

## Fix sketch

Implement a `cast_kind` enum and `infer_cast` per the spec/12 table.
Add `TypeError::InvalidCast { src: TyId, dst: TyId, span }`. Allowed
casts in v0:

- Prim ↔ Prim where both are integer (any width / signedness combo).
- `bool → integer` (the reverse goes through `if`).
- `*mut T → *const T` (mut drop).
- `*const T → *mut T` is the spec'd soundness boundary — **must**
  be syntactically rejected in v0 (no unsafe).
- Same-pointee ptr ↔ ptr is OK if mut direction is OK.
- Cross-pointee ptr ↔ ptr: deferred (spec/12 §"Out of scope").
- Pointer ↔ integer: deferred (spec/12 §"Out of scope").

Codegen then dispatches on `cast_kind` rather than blindly poking
`into_int_value`.

## Related

- spec/12_AS.md (the entire spec is unimplemented).
- B011 (pointer comparison) and B012 (compound assigns on non-int)
  are the same shape — codegen blindly assumes "operand is integer,"
  which only holds if typeck filters everything else.
