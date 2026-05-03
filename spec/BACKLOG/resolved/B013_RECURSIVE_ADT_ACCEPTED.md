# B013 — Direct recursive ADT (`struct A { x: A }`) accepted; LLVM struct of infinite size

## Original report

Surfaced by the soundness audit on 2026-05-03. spec/08_ADT.md
§"TBD-T2" already documents the gap; this entry tracks the fix.

## The bug

There is no cycle check in `src/typeck/check/decl.rs` after phase 0.5
builds the field types. Each field's `Sized` obligation only checks
the field's outer kind — `discharge_sized` deliberately does **not**
descend into `Adt` (`src/typeck/check/unify.rs:470-482`), per its
own design comment "each field carries its own decl-phase Sized
obligation."

So `struct A { x: A }` resolves cleanly to `Adt(0)` with
`fields = [FieldDef { ty: Adt(0) }]`, every Sized obligation passes
(the field's outer kind is `Adt`, which discharge accepts), and
codegen's `prepare_adt_types` (`src/codegen/ty.rs:31-48`) calls
`set_body([Adt(0).as_basic_type_enum()])` on the struct itself —
producing an LLVM type with infinite size. Behavior is between
"LLVM verifier rejects" and "downstream tooling crashes" depending
on the LLVM version.

A `Ptr` layer breaks the cycle (the pointer is sized, the pointee
is not entered structurally). That's the spec'd condition for legal
recursion.

## Failing cases

```rust
// Direct self-reference
struct A { x: A }
fn main() -> i32 { 0 }
```

```rust
// Indirect cycle, same shape
struct A { b: B }
struct B { a: A }
fn main() -> i32 { 0 }
```

```rust
// Cycle through sized array
struct A { xs: [A; 3] }
fn main() -> i32 { 0 }
```

## Severity

**Medium** — codegen ICE / LLVM-rejected on a tiny program; not
silent UB but a quality-of-implementation gap. spec/08 TBD-T2
already calls it out.

## Fix sketch

After phase 0.5 in `decl::resolve_decls`, run an SCC / cycle walk
over the field-type dependency graph:

- Edge `(adt_a → adt_b)` exists iff `adt_a` has a field of
  structural type that contains `adt_b` *without* crossing a `Ptr`
  layer.
- For sized arrays `[T; N]`, the edge points through `T`.
- For `Ptr(T, _)`, no edge — the pointer breaks the cycle.

Any non-trivial SCC is a hard error:
`TypeError::InfiniteSize { adts: Vec<AdtId>, span }`. Tarjan's or
Kosaraju's algorithm; the graph size is at most `|adts|` so cost is
trivial.

## Related

- spec/08_ADT.md §"TBD-T2 — Recursive struct rejection".
- B008 closed the array-recursion side of Sized; this is the
  Adt-recursion side.

## Out of scope

- Mutually recursive types via `Box<T>` or other indirection types
  — not relevant in v0 (no `Box`); `Ptr` is the only legal break.
