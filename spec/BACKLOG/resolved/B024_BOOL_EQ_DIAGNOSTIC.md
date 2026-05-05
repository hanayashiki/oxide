# B024 — `==` / `!=` on bool rejected, with self-contradicting fix-it

## Original report

Surfaced 2026-05-06 building stage-1 typeck. Encountered while
porting a unify routine; cost ~30 min to bisect to the language
rule because the diagnostic surface is misleading.

## The bug

`a_bool != b_bool` produces:

```
[E0280] Error: expected an integer operand, found `bool`
   ╭─[ /tmp/repro.ox:2:8 ]
   │
 2 │     if a_is_some != b_is_some { return false; }
   │        ───────────┬──────────
   │                   ╰──────────── expected integer here
   │
   │ Help: compare booleans with `&&` / `||` / `!`, or convert via `b as i32`
───╯
```

Two problems with the user-facing surface:

1. The fix-it ("convert via `b as i32`") is **also rejected** —
   `bool as i32` fails per spec/12 cast rules. Following the help
   produces a second error.
2. `&&` / `||` / `!` aren't a substitute for equality. The natural
   write-up of "are these two flags both set or both unset" is
   `a == b`; the `&&`/`||` rewrite needs nested ifs or
   `(a && b) || (!a && !b)` boilerplate.

## Stage-1 sample site

`example-projects/oxide/typeck.ox:552-560` had to be rewritten from:

```rust
if a.kind == TYK_ARRAY() {
    if a.len_is_some != b.len_is_some { return false; }
    if a.len_is_some && a.len_val != b.len_val { return false; }
    return unify(c, a.elem, b.elem);
}
```

to:

```rust
if a.kind == TYK_ARRAY() {
    if a.len_is_some {
        if !b.len_is_some { return false; }
        if a.len_val != b.len_val { return false; }
    } else {
        if b.len_is_some { return false; }
    }
    return unify(c, a.elem, b.elem);
}
```

Three lines became seven, with the symmetric structure obscured.

## Severity

**Low-medium** — easily worked around once the rule is internalized,
but the help text actively misleads first-encounter users into a
second error.

## Fix options

Two reasonable directions; only one needs to land.

### Option A (preferred): allow `==` / `!=` on bool

Lift the spec/05 `Obligation::Integer` rule on `==`/`!=` to allow
`bool` operands. Codegen lowers to `icmp eq i1` / `icmp ne i1` —
LLVM has no special case. No design surface beyond "delete the
restriction"; the only argument for keeping the restriction was C-
ish strictness, but spec/05 already accepts `if cond` over bool, so
the boundary isn't motivated.

### Option B: fix the help text

Keep the rule, drop the broken fix-it. Replace with:

> Help: bool equality is not supported in v0. Use `&&`/`||`/`!` to
> express boolean logic; for "both same", write `(a && b) || (!a &&
> !b)` or branch explicitly.

Strictly worse than A but a cheap mitigation if the rule survives.

## Related

- spec/05 (`Obligation::Integer` arm in `unify_with`).
- spec/12 (the `bool as i32` rejection that the help text contradicts).
- B022 / B023 (separate; this one stands alone).

## Out of scope

- Comparison operators (`<`, `<=`, `>`, `>=`) on bool. Genuinely
  meaningless in v0; rejection is fine. This issue is about
  equality only.

## Resolution

Landed Option A — `==` / `!=` on bool now typecheck. Discharge of
the operand obligation gates on the *site*: `Bin(Eq | Ne)` admits
integer or `bool`; every other site (arith / bitwise / shift binary,
unary, compound assign) stays integer-only. Codegen was untouched —
`build_int_compare` with `IntPredicate::EQ` / `NE` lowers `i1`
operands to `icmp eq i1` directly.

Bundled the rename the obligation/site asked for: `IntegerSite` →
`PrimitiveSite`, `Obligation::Integer` → `Obligation::Primitive`,
`discharge_integer` → `discharge_primitive`. The error variant
`TypeError::NonIntegerOperand` (E0280) keeps its name and code —
it now only fires at integer-required sites, so the message text
("expected an integer operand") is still accurate at every emit
site.

Help text for the `(Bin(Eq | Ne), Bool)` arm is gone (admitted at
discharge, never reaches the renderer). Ordering ops on bool
(`<`/`<=`/`>`/`>=`) get a new dedicated hint:

> ordering comparisons aren't defined on `bool`; branch on the
> value or convert via `b as i32` if you really need an order

Note the original report's claim that `bool as i32` is rejected
per spec/12 was wrong — `BoolToInt` is admitted at
`src/typeck/check.rs:126,160` and exercised by
`tests/snapshots/typeck/acceptance_cast_bool_to_int.snap`. The
original help text was awkward (`b as i32` is a roundabout way to
spell equality), not contradictory.

### Files touched

- `src/typeck/error.rs` — `IntegerSite` → `PrimitiveSite`.
- `src/typeck/check/obligation.rs` — `Obligation::Integer` → `Primitive`.
- `src/typeck/check.rs` — push sites + `discharge_primitive` site-gate.
- `src/typeck/mod.rs` — re-export rename.
- `src/reporter/from_typeck.rs` — help-text split + `is_cmp` removal.
- `spec/05_TYPE_CHECKER.md`, `spec/07_POINTER.md` — terminology.
- `tests/snapshots/typeck/` — `error_cmp_bool` retired,
  `acceptance_bool_eq` / `acceptance_bool_ne` /
  `error_cmp_bool_lt` added.
- `tests/snapshots/codegen/` — `bool_eq_uses_icmp_eq_i1` added to
  prove the zero-codegen-change claim.
