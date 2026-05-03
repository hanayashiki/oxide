# B018 — No definite-assignment analysis (`let x: T;` then read of `x` is UB)

## Original report

Surfaced by the soundness audit on 2026-05-03.

## The bug

`let x: T;` (no initializer) is accepted by typeck
(`src/typeck/check.rs:1353-1381`):

```rust
if let Some(init_id) = init {
    let init_ty = self.infer_expr(inf, init_id);
    let init_span = self.hir.exprs[init_id].span.clone();
    unify::subtype(self, inf, init_ty, local_ty, init_span);
}
```

When `init` is `None`, no further check fires. There is no
definite-assignment pass anywhere in `src/hir/` or `src/typeck/`
(verified by `grep -rn "definite\|init_check\|UseBeforeAssign"`).

Codegen then emits an `alloca` for the slot and, on a subsequent
read, a plain `load` — which yields LLVM `undef` if no `store` has
happened on the path. Reading `undef` is well-defined at the IR
level (a poison-like value), but passing it as a function argument
or using it in a branch is UB by the program's source-level
semantics.

## Failing case

```rust
fn use_int(x: i32) -> i32 { x + 1 }

fn main() -> i32 {
    let x: i32;                                 // declared, never assigned
    use_int(x)                                  // codegen: load undef; UB on the source side
}
```

Typeck accepts because `local_ty` is set from the annotation
(`let x: i32`) without needing init to constrain it. Codegen alloca
+ load. LLVM IR is well-formed; program semantics are not.

## Severity

**Medium** — UB on program reads of uninitialized locals, but only
triggered by an explicit `let x: T;` (no init). Probably rare in
user code but trivial to write.

## Fix sketch

Two routes:

### Option A — reject `let` without init in typeck

Simplest: in the let-binding arm, require `init.is_some()` and emit
`TypeError::LetWithoutInit { span }`. Forces every binding to have
an initializer. Clean, but more restrictive than the language might
want long-term (e.g.
`let x: i32; if cond { x = 1; } else { x = 2; }` is a useful
pattern).

### Option B — definite-assignment pass over HIR

Standard forward dataflow: each local starts `Uninit`. Every
assignment to a place rooted at a local sets that local's bit.
Reading a local in the `Uninit` state fires
`TypeError::UseOfUninitialized { local, span }`. Joins take the AND
across predecessor blocks. Loop back-edges fix-point.

Recommend **A** for v0 (matches the spec's "C-ish" simplicity) and
revisit when the language wants the `let x;` then-branch idiom.

## Related

- spec/05_TYPE_CHECKER.md does not currently mention
  definite-assignment; should be added under §"Obligations" or as a
  standalone subsection alongside the fix.
- B016 (`Never` accepted as let type) is adjacent — both are "let
  allows things it shouldn't."

## Out of scope

- Borrow-checker-style analysis (move tracking, partial moves, etc.).
  Out of v0.
