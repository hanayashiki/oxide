# B005 — Modeling `()` and `!` in codegen

## Status
Closed. The `Operand` abstraction landed in
`src/codegen/lower.rs`; `lower_ty(())` returns LLVM `{}`; `emit_let`
no longer panics on `()`-typed locals; the divergence contract is
codified at `emit_expr`'s docstring.

## What landed

### The `Operand` enum

```rust
enum Operand<'ctx> {
    Value(BasicValueEnum<'ctx>),    // SSA value: Int, Bool, Ptr, Struct
    Place(PointerValue<'ctx>),       // memory-backed; type alongside
    Unit,                            // zero-sized canonical value of `()`
}
```

`emit_expr` returns `Option<Operand<'ctx>>`. The `None` channel is
reserved for **divergence** — `is_terminated()` is true iff `emit_expr`
returned `None` for that call. `()` is never `None`; it's `Unit`.

### Three universal helpers

`store_into`, `load_value`, `spill_to_place_fresh` absorb the per-site
dispatch that used to live as repeated `is_sized_array(ty)` forks at
every consumer. The `is_sized_array` check still appears for choosing
between SSA-Value and Place form at *production* sites (`Local` /
`Field` / `Index` rvalue, fn-param init, byval call ABI), but every
*consumer* now goes through one of the helpers.

### `lower_ty` total for value types

`Unit` lowers to `{}` (LLVM zero-sized empty struct, sole inhabitant
`{} undef`). `Never` keeps panicking by design — `!`-typed expressions
terminate the BB before any consumer reaches `lower_ty`. `emit_let`
special-cases `Never` (skips the alloca, evaluates the init for its
BB-terminating side effect) since `lower_ty(Never)` is the only non-
divergence-friendly entry point.

## How the three modeling questions were answered

### Q1 — Does `()` have a value form?

**Answered yes (Position B).** `Operand::Unit` is the canonical form;
`{} undef` is its SSA representation; `lower_ty(()) = {}` makes the
type available everywhere. The previous "no SSA value of `()` ever
exists" invariant is gone; `is_void_ret` short-circuits remain only as
*IR-quality optimizations* (skip the no-op alloca for void-typed if /
loop result slots), never as load-bearing invariants.

### Q2 — How does `!` differ from `()` in codegen?

**Distinct by construction.** `()` is `Operand::Unit`; `!` is `None`.
Sites that emit `!`-typed expressions terminate the BB
(`Return`/`Break`/`Continue`/no-cond no-break `Loop`); consumers that
receive `None` from `emit_expr` know the BB is already terminated and
short-circuit. `lower_ty(!)` panics by design — `!`-typed expressions
never reach a "give me a slot" site because the BB termination makes
the consumer dead before it asks.

### Q3 — Where does the divergence "absorption" happen?

**Codified at `emit_expr`'s docstring:**

> Calling `emit_expr` may terminate the BB and return `None` if the
> sub-expression diverged. Every call site MUST either propagate
> `None` (typically via `?`) or document why typeck guarantees the
> operand cannot be `!`-typed at this site.

Audit pass converted the latent-bug `.expect("…produced no value")`
sites that were reachable via `!`-typed operands (`emit_assign` rhs,
`emit_if`/`emit_loop` cond, `emit_short_circuit` lhs/rhs, `emit_cast`
operand, `emit_call` args, `emit_index_place` base/idx) to `?`. The
remaining `.expect` sites (fn-body return, struct/array-literal
elements) are left as a follow-up audit.

## Reproducer status (closed cases)

| Shape | Status | Test |
|---|---|---|
| `let _a = b = 7;` (`_a: ()`) | Works | `tests/snapshots/jit/i32/let_unit_from_assign.ox` |
| `let _a = {};` (`_a: ()`) | Works | `tests/snapshots/jit/i32/let_unit_from_block.ox` |
| `x = loop {};` (divergent rhs) | Works | `tests/snapshots/jit/i32/assign_from_return.ox` |
| `let _a = loop {};` (`_a: !` infer) | Blocked on typeck | — |

The last case (`!`-fallback for unannotated locals) requires typeck
to infer `_a: !` from a divergent init — currently it produces an
`{error}` type. That's a typeck issue (probably wants Rust-style
"never-type fallback to `()`"), not a codegen one. The codegen path
for it is in place: `emit_let` special-cases Never and skips the
alloca. When typeck grows the fallback rule, the test can be added
with no codegen change.

## Known limitation (not currently planned)

`Local`/`Field`/`Index` rvalue arms still default to Value form
(loading eagerly) and special-case Place only for sized arrays. The
consequence: `arr_of_arrays[i]` and `arr_of_structs[i]` are still
broken — `emit_index_rvalue` always loads, which materializes nested
aggregates as Value and breaks the place-form invariant. Fixing this
would mean defaulting `Local`/`Field`/`Index` rvalues to Place and
only loading on consumer demand. Not in scope here; revisit if a real
use case appears.

## Cross-references

- `src/codegen/lower.rs` — `Operand` enum, `store_into` /
  `load_value` / `spill_to_place_fresh` helpers, divergence contract
  on `emit_expr`'s docstring, `emit_let` Never-special-case.
- `src/codegen/ty.rs` — `lower_ty(()) = {}`; `lower_ty(!) = panic` with
  tightened ICE message.
- `tests/snapshots/jit/i32/let_unit_from_assign.ox` and
  `let_unit_from_block.ox` and `assign_from_return.ox` — the closure
  reproducers.
