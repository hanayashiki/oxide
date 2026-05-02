# B005 — Modeling `()` and `!` in codegen

## Status
Open. Spotted while fixing `emit_assign` for non-int aggregates (the
`pipes[0] = Pipe { ... }` ICE in the flappy example). The fix landed
narrowly (`emit_assign` aggregate dispatch + shared
`emit_store_into_slot` helper); the void-typed-local gap is left for
this entry. Today `emit_let` still panics with *"void type for local
…"* on `let a = b = 1;` — the immediate user-visible symptom.

## The gap

Today's *implicit* invariant: **no SSA value of type `()` or `!` ever
exists.** Every codegen consumer that could produce or consume a
void-typed value keys off `is_void_ret(ty)` and short-circuits:

- `lower_fn_type` (codegen/ty.rs:121-125) — void return ⇒ LLVM `void`.
- `emit_fn` epilogue (codegen/lower.rs:365) — skip return value.
- `emit_if` result slot (lower.rs:1239) — skip alloca.
- `emit_loop` result slot (lower.rs:1349) — skip alloca.
- `emit_return` (lower.rs:1478) — skip operand emit.
- `emit_let` — **panic** if local is void-typed.

The panic at `emit_let` is the load-bearing tell that the invariant is
incomplete: `()` and `!` *can* surface in a value position via
`let a = b = 1;`, `let a = loop {};`, `let a = return;`, etc. — Rust
accepts all of these, we don't.

A narrow option (typeck-side diagnostic that rejects the `let` at
parse-resolve time) dodges the question instead of answering it.
Other surface shapes will keep hitting the same gap:

- `b = return;` — `emit_assign`'s `.expect("assign rhs produced no
  value")` panics because `emit_return` returns `None`. (Listed as
  "Out of scope" in the `emit_assign` aggregate fix.)
- `if cond { return } else { panic!() }` in any value position would
  hit similar shape questions if patterns like `panic!()` ever land.

## Three modeling questions to settle

B005 is fundamentally a design exercise, not an implementation list.
The right shape of "fix void-typed locals" depends on three open
questions:

### Q1: Does `()` have a value form?

Two coherent positions:

**Position A — keep "() is no SSA value."** Reads of a `()`-typed
expression produce `None`. Void-typed locals are virtual: no alloca,
no entry in `fx.locals`, reads return `None`. Every consumer
(let-init, fn-arg, if-arms, assign rhs, return) handles the `None`
propagation explicitly. The invariant stays simple; the per-site
discipline is wide.

**Position B — give `()` a zero-sized canonical value.** Probably an
LLVM `{}` (empty struct), zero bytes, conceptually a `Const`. Then
`lower_ty(())` succeeds, `alloca {}` / `load {}` / `store {} %v, ptr`
all just work, no per-consumer special-casing. One central change.
Cost: every existing `is_void_ret` short-circuit is now arguably
wrong — it's skipping work the new model would do uniformly. The
short-circuits become optional optimizations, not invariants.

The choice has knock-on consequences for ABI (does `()` cross
`extern "C"` as zero bytes or as nothing at all?), pretty-print, and
diagnostic wording.

### Q2: How does `!` differ from `()` in codegen?

`is_void_ret` collapses them today. They aren't actually the same:
`!` carries the **divergence invariant** — any expression of type `!`
terminates the basic block, and no successor reads from it. So `!`
doesn't need a value representation regardless of what we pick for
`()` — every `!`-typed local is bind-only-in-name, every `!`-typed
operand is unreachable past its production site.

B005 should make the `!`-vs-`()` distinction explicit at codegen. A
candidate split:

- **`()`**: subject to Q1's answer. Has (or doesn't have) a value form
  uniformly.
- **`!`**: never has a value form. Sites that emit `!`-typed
  expressions terminate the block (already the case for `Return`,
  `Break`, `Continue`, `Loop` with no `cond` and no `break`).
  Consumers of `!`-typed operands must check `is_terminated()` and
  short-circuit — never try to materialize a value.

Today both invariants are implicit and ad-hoc per site.

### Q3: Where does the divergence "absorption" happen?

Typeck handles the type-level part: `!` coerces into anything via the
existing `coerce` rule, and `infer_block` short-circuits on a
divergent tail. The codegen-level interaction with the
`is_terminated()` short-circuit is implicit — it lives as defensive
checks scattered through `emit_*` sites, with no explicit contract.

The `b = return;` ICE is a direct symptom: `emit_assign` doesn't
expect a `None` from `emit_expr(rhs)` because the operand-divergence
case never reached design attention. The fix is small (handle
`None`); the lesson is bigger (the contract should be specified once,
not rediscovered per consumer).

B005 should specify: *"any consumer that calls `emit_expr` on a
sub-expression must either (a) handle `None` as 'the operand
diverged, terminate this consumer's emission too,' or (b) prove via
typeck invariants that `None` is impossible at this site."* Then
audit every `emit_expr` call site against that rule.

## Workarounds today

User-side: write the assignment as a statement and bind separately:

```rust
b = 1;
let a = b;        // a: i32
```

instead of:

```rust
let a = b = 1;    // a: () — currently ICEs in emit_let
```

For divergent inits (`let a = loop {};`, `let a = return;`,
`let a = panic!();`), there's no workaround — the binding has no
useful purpose anyway since no read can execute, but the panic still
fires before the user can write the obvious "just don't bind it"
rewrite.

## Scope of the future spec

A real B005 spec would:

1. Pick a position for Q1 (probably Position A — minimum disruption,
   matches today's invariants — but that's the design call).
2. Specify the `!`-vs-`()` distinction per Q2.
3. Document the divergence-absorption contract per Q3 and audit all
   `emit_expr` call sites against it.
4. Replace `emit_let`'s panic with the chosen model. `let a = loop
   {};` should likely become accepted (the local is dead, but not
   malformed). `let a = b = 1;` likewise.
5. Add codegen tests for the void-typed-local round-trip in let,
   fn-arg, if-arm-tail, assign-rhs positions.

## Cross-references

- `emit_let` panic at `src/codegen/lower.rs` — the immediate
  user-visible symptom; B005 replaces with the chosen model.
- `emit_assign`'s `.expect("assign rhs produced no value")` — same
  underlying gap, different surface (`b = return;` would crash).
  Symptom of Q3.
- `is_void_ret` (`src/codegen/ty.rs:129-131`) — the predicate every
  consumer keys off today; B005 may eliminate or refine it.
