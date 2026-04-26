# B001 — Never type unifies in both directions (soundness hole)

## Status
**Resolved.** Fixed by routing the "Never absorbs" rule through `coerce`
only and keeping `unify` as pure symmetric Hindley-Milner. Snapshots
updated; new guard test added.

## The bug

`src/typeck/check.rs:344` (before fix):

```rust
(TyKind::Never, _) | (_, TyKind::Never) => {}
```

`unify` was documented `(found, expected)`. The `Never` arm above
accepted in **both directions**, so any type silently unified with
`!` regardless of which side it sat on.

### Failing case

```rust
fn g() -> never { return 1 }
```

- `Return(val=1)` calls `coerce(v_ty=i32, cur_ret=Never)`.
- `coerce` delegated to `unify(i32, Never)`.
- Matched `(_, TyKind::Never) => {}` → silently OK.
- Function typechecked. The signature lied.

Two snapshot tests (`acceptance_never_returning_call_as_tail.ox` and
`acceptance_never_returning_call_with_semi_makes_block_divergent.ox`)
enshrined this — both contained `fn g() -> never { return 1 }` in
their setup.

### Why it mattered

The typechecker already trusted the `Never` contract for control-flow
reasoning elsewhere — without re-checking via CFG:

- `infer_block` infers "block diverges, value = `!`" purely from the
  trailing expression's type.
- `expect_unit` lets `Never` through as if it were `()`.
- `join_never` picks the non-`!` arm of an `if`.

If a value-producing expression could be typed `!`, all three would
fire on a false premise. Codegen layers that exploit divergence
(omitting unreachable code after a Never call, etc.) would inherit
the lie.

## Resolution — Option B (refined)

`unify` is **symmetric HM**. The "`!` flows into any context" rule
is a directional one and lives only in `coerce`. The two arguments
to `unify` are algebraically interchangeable; the `found` /
`expected` parameter names are kept solely as a presentation
artifact for the rendered diagnostic.

### Changes to `src/typeck/check.rs`

1. **`unify`** — strip the bidirectional Never arm, leaving only
   `(Never, Never) → ok`. Anything else against `Never` is now a
   mismatch. Doc comment rewritten to spell out the symmetry.

   ```rust
   (TyKind::Error, _) | (_, TyKind::Error) => {}
   (TyKind::Never, TyKind::Never) => {}
   // (Never, _) | (_, Never) — gone
   ```

2. **`coerce`** — gain the Never-absorbs rule. Only `actual = Never`
   is rescued. The reverse (`expected = Never`, `actual = T`) falls
   through to `unify` and errors, which is exactly the
   `fn x() -> never { 0 }` case.

   ```rust
   fn coerce(&mut self, inf, actual, expected, span) {
       if matches!(self.tys.kind(self.resolve(inf, actual)), TyKind::Never) {
           return;
       }
       self.unify(inf, actual, expected, span.clone());
       self.check_ptr_outer_compat(inf, actual, expected, span);
   }
   ```

3. **`unify_arms`** (new) — `if` branch unification is symmetric
   *join*, not coercion, but shares the Never-absorbs spirit. If
   either arm is `Never`, skip unify; `join_never` picks the
   surviving non-divergent arm. This keeps
   `if c { return 1 } else { 0 }` typing as `i32`. Replaced the two
   `self.unify(then_ty, else_ty, span)` calls in `infer_if` with
   `self.unify_arms(...)`.

4. **`bind_infer_checked`** — drop `TyKind::Never` from the
   "int-flagged var may bind to this" allow-list. Once `unify` is
   pure HM, `unify(Infer(α), Never)` would otherwise bind `α := Never`
   and silently re-type integer literals as `!` — exactly what
   surfaces inside `return 1` for a `-> never` fn. Stripping it
   makes that bind a real type error and falls back to the int
   default (`i32`), so the literal in `return 1` correctly types as
   `i32` and the diagnostic span lands on the literal.

   ```rust
   TyKind::Infer(_) | TyKind::Error => true,   // was: ... | TyKind::Never
   ```

### Audit of `unify` / `coerce` call sites

All sites that need the "Never can flow into context T" semantics
already routed through `coerce` (verified):

| Site | Layer |
|---|---|
| fn body vs declared return (`check_fn`) | `coerce` ✓ |
| `Return(val)` body | `coerce` ✓ |
| `infer_let` init vs annotated/inferred type | `coerce` ✓ |
| `infer_assign` rhs vs lhs | `coerce` ✓ |
| `infer_call` arg vs param | `coerce` ✓ |
| `infer_if` arm-unification | `unify_arms` (new helper) |
| Other `unify` sites (cond-vs-bool, binary operands, unary) | `unify` — degenerate Never-input cases not covered by snapshots; accepted regression |

### Spec update

`spec/05_TYPE_CHECKER.md` § Unification rules rewritten to call
`unify` symmetric and forbid Never-against-non-Never. New
§ Coercion rules section documents where the Never-absorbs and
pointer-mutability subtype rules live.

### Snapshots

Rewrote two `.ox` files so their `g` body is genuinely divergent
(can't use `loop`/`panic` yet — recursion does the job):

- `acceptance_never_returning_call_as_tail.ox`:
  `fn g() -> never { g() } fn f() -> i32 { g() }`
- `acceptance_never_returning_call_with_semi_makes_block_divergent.ox`:
  `fn g() -> never { g() } fn f() -> i32 { g(); }`

Both still cover their original purpose (block-divergence in `f`).

Added the canonical guard:

- `error_return_value_in_never_fn_e0250.ox`:
  `fn impossible() -> never { 0 }` — now correctly errors with
  `expected '!', found 'i32'` pointing at the `0`.

All other 28 typeck snapshots stayed green untouched.
