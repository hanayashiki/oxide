# Mutability

This spec consolidates Oxide's mutability rules. Pieces of the model live in
`05_TYPE_CHECKER.md`, `07_POINTER.md`, and `10_ADDRESS_OF.md`; this document
is the single normative source.

## 1. Theoretical model

`mut` is a **binding-level place capability**, not part of the type. A binding
owns storage; the capability says whether that storage may be re-written or
have `&mut` taken to it. The capability does **not** travel with values:

```rust
let mut x = 1;
let y = x;     // y is a fresh, immutable binding; x's `mut` did not transfer
```

Mutability *is* part of the type system **only for pointer types** (`*mut T`
vs `*const T`), because a pointer is a value that carries its access
capability through assignments and across function boundaries.

Slogan: **mutability lives where the storage lives.**

- Bindings own storage → mutability annotates the binding.
- Pointers refer to storage through values → mutability is in the pointer type.

This matches Rust. Oxide does not have `mut T` as a type, only `*mut T` /
`*const T`.

## 2. Surface syntax

```
LetItem ::= 'let' 'mut'? Ident (':' Type)? ('=' Expr)? ';'
Param   ::= 'mut'? Ident ':' Type
```

The default (no keyword) is **immutable**. There is no `mut` modifier on
struct fields — per-field mutability is not a concept; writing through a
field requires the *owner* be mutable (matches Rust).

## 3. Place expressions

Assignment and `&mut` require a **place** expression. A place is one of:

- `Local(lid)` — a let-binding or function parameter.
- `Field { base, .. }` — projects to a field of a place.
- `Index { base, .. }` — projects to an index of a place.
- *(Future)* `Unary { Deref, .. }` — once arrays / pointer deref land.

Place validity is decided at HIR-lowering. Non-place targets of assignment
or `&` are rejected there with `InvalidAssignTarget` / `AddrOfNonPlace`.

## 4. The place-mutability walk

Given a place expression, its mutability comes from the **root binding**,
walked along the access path:

- `Local(lid)` → `hir.locals[lid].mutable`
- `Field { base, .. }` / `Index { base, .. }` → recurse into `base`
- non-place expressions → `None` (suppressed; HIR already filed an error)

Implementation: `place_mutability` in `src/typeck/check.rs`.

Consequence: writing through a struct field requires the owner be `mut`.
There is no separate per-field mutability annotation.

## 5. Enforcement

| Construct       | Rule                                              | Error code / payload |
|-----------------|---------------------------------------------------|----------------------|
| `target = rhs`  | `place_mutability(target)` must be `Some(Mut)`    | E0263 `MutateImmutable { op: Assign }` |
| `&mut expr`     | `place_mutability(expr)` must be `Some(Mut)`      | E0263 `MutateImmutable { op: BorrowMut }` |
| `&expr`         | (no mutability constraint)                        | — |

`place_mutability == None` means the operand was already rejected at
HIR-lower (non-place); typeck stays silent to avoid stacked diagnostics.

Implementation: `infer_assign` and `infer_addr_of` in `src/typeck/check.rs`.

### Examples

```rust
fn ok() -> i32 {
    let mut x = 1;
    x = 5;          // OK: x is mut
    x
}

fn bad() -> i32 {
    let x = 1;
    x = 5;          // E0263: cannot assign to an immutable place
    x
}

fn ok_param(mut x: i32) -> i32 {
    x = 5;          // OK: param declared mut
    x
}

fn bad_param(x: i32) -> i32 {
    x = 5;          // E0263
    x
}

fn bad_borrow_mut() -> i32 {
    let x = 1;
    let p = &mut x; // E0263 (BorrowMut)
    *p
}
```

## 6. Pointer mutability and coercion

Pointer type: `*const T` or `*mut T`. Mutability is part of the type
(`TyKind::Ptr(TyId, Mutability)`).

Two-step model:

- **Inference (loose unification)**: `*mut T ~ *const T` are unified
  shape-only — `mut_le` ignores the mutability bit at the inference step.
  This avoids spurious type-variable failures during inference.
- **Coercion (strict)**: at the actual assignment / argument-passing
  boundary, only `*mut T → *const T` is permitted, and **only at the outer
  layer**. Inner layers must match exactly:
  - `*mut u8` → `*const u8` — allowed (outer layer).
  - `*mut *mut u8` ↛ `*mut *const u8` — rejected (inner mismatch).
  - `*const T → *mut T` — rejected everywhere.

Error code on the coercion path: E0257 (pointer mutability mismatch).

Implementation: `mut_le`, `check_ptr_outer_compat`, `check_ptr_inner_eq`
in `src/typeck/check.rs`.

### Why the loose-unification step is load-bearing

Inference needs to unify two pointer types when their pointee is still a
type variable. If the unifier respected mutability strictly, a literal
`null` (typed as `*const _`) could not unify with a `*mut T` slot through
inference. Soundness is recovered at the coercion boundary, which is
strict. **Future changes to `mut_le` must preserve the strict boundary**;
weakening it would let `*const T` flow into `*mut T` and silently mutate
read-only memory.

## 7. Soundness

Under the current rules, no path mutates an immutable place:

1. Direct `x = ...` blocked by E0263.
2. `&mut x` of an immutable `x` blocked by E0263.
3. `&x` produces `*const T`; this cannot coerce to `*mut T`; cannot write
   through it.
4. Field/index inherits root mutability; no laundering through projections.
5. StrLit (`*const [u8; N]` per `07_POINTER.md` §4) is not a place:
   `&"hello"` is rejected with `AddrOfNonPlace` (E0208). The outer
   `*const` also makes any indexed write through the literal
   (`"hi"[0] = 1`) error as `MutateImmutable`. No path to mutate a
   literal.

The structural caveat is the loose-unification step in §6 — the strict
boundary check is what keeps the model sound.

## 8. Codegen

Mutability is a **typeck-only** concept. Codegen unconditionally allocas
locals (and parameters) and emits load/store; LLVM opaque pointers do not
carry the const/mut distinction. See `src/codegen/lower.rs` and the v0
note in `07_POINTER.md` §Codegen.

A `mut` parameter is lowered identically to an immutable one: the param
slot is the alloca; the entry block stores the incoming value; subsequent
reads/writes are loads/stores. The `mut` flag only changes whether typeck
*permits* those writes.

## 9. Out of scope (today)

- **Per-field mutability.** Matches Rust; not adding.
- **`Unary { Deref, .. }` as a place producer in `place_mutability`.**
  Deferred with the array work (`09_ARRAY.md`); the `_ => None` arm in
  the walk is the placeholder.
- **Mutability inference / "unused mut" lints.** Not in v0.
- **Reference types `&T` / `&mut T` as type-level constructs.** We only
  have raw pointers (`*const T` / `*mut T`).
