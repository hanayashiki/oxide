# B002 — Typeck diagnostic UX problems

## Status
Open — collected from snapshot review. Each item is independent;
fix in any order. None are soundness bugs.

## U1 — `if`-branch mismatch: span on else, message describes then's type as "found"

`error_if_branches_must_unify.snap`:

```
fn f() -> i32 { if true { 1 } else { false } }
```

Diagnostic:

```
[E0250] Error: type mismatch: expected `bool`, found `i32`
 1 │ fn f() -> i32 { if true { 1 } else { false } }
   │                                    ────┬────
   │                                        ╰────── type mismatch here
```

The caret underlines `{ false }`. `false` *is* a `bool`. Reading
"found `i32`" while looking at `false` is jarring — the user has
to mentally swap "expected" and "found".

**Cause.** `infer_if` (check.rs:767) calls
`unify(then_ty, else_ty, span=else_span)`. So `found = then_ty`,
`expected = else_ty`, but the *span* belongs to else. The span and
the "found" identity disagree.

**Options.**
- Swap arguments: `unify(else_ty, then_ty, else_span)`. Message
  becomes "expected `i32`, found `bool`" pointing at `false` —
  natural reading.
- Better: introduce a dedicated `IfArmsMismatch { then_ty, else_ty,
  then_span, else_span }` diagnostic. "if branches have
  incompatible types: then is `i32`, else is `bool`" with both
  spans labeled. Scales to `else if` chains.

The same orientation bug recurs in any other site that calls
`unify` with span belonging to one operand and the *other* operand
as `found`. Worth a grep audit.

## U2 — Missing-`;` reported as bare type mismatch with no fix-it

`error_missing_semi_in_middle_of_block_e0250.snap`:

```
fn f() -> i32 { 1 + 2  3 }
```

```
[E0250] Error: type mismatch: expected `()`, found `i32`
 1 │ fn f() -> i32 { 1 + 2  3 }
   │                 ──┬──
   │                   ╰──── type mismatch here
```

Technically correct (mid-block expressions must coerce to `()`),
but the user's mental model is "function returns `i32`" — being
told `1 + 2` should be `()` is bewildering.

**Fix.** Detect this specific shape (mid-block item, `has_semi ==
false`, item type ≠ `()`) in `infer_block` and emit a more
specific diagnostic with a fix-it suggestion:

> Help: add `;` to discard this expression's value, or remove the
> trailing expression(s) that follow.

Same wording problem in:
- `error_non_unit_call_with_semi_at_end_for_unit_fn.snap` —
  `fn f() -> i32 { g(); }` reports "expected `i32`, found `()`".
  Could add: "the trailing `;` discards the value of `g()`;
  remove it to return the value."
- `error_semicolon_after_expr_discards_value.snap` —
  `fn f() -> u32 { 1; }` same shape, same suggested help.

## U3 — Pointer mismatch messages strip outer layers, hiding the real shape

`error_pointer_pointee_shape_mismatch_e0250.snap`:

```
fn f(s: *const u8) -> i32 { 0 }
fn main(p: *const i32) -> i32 { f(p) }
```

```
[E0250] Error: type mismatch: expected `u8`, found `i32`
```

The user's actual types are `*const u8` vs `*const i32`. Message
strips the `*const` layer. For one level it's recoverable; for
nested pointers it's painful.

`error_inner_mutability_mismatch_e0257.snap` shows the worse
case:

```
fn f(s: *const *const u8) -> i32 { 0 }
fn main(p: *const *mut u8) -> i32 { f(p) }
```

```
[E0257] Error: pointer mutability mismatch: expected `*const u8`, found `*mut u8`
```

User has to manually re-add the outer `*const` to figure out which
layer differs. With three levels of indirection this would be
nearly unparseable.

**Fix.** When recursing into pointer pointees inside `unify` /
`check_ptr_inner_eq`, capture the *full original* `expected` and
`found` types passed in by the caller, and use those in the
emitted diagnostic. Add a "differs at depth N" hint pointing at
the inner mismatch:

> expected `*const *const u8`, found `*const *mut u8`
> (inner `*const u8` ≠ inner `*mut u8` at depth 1)

The existing `Help:` line is already good — keep it.

## U4 — Trailing-mismatch errors caret the entire fn body

Three snapshots span the whole `{ ... }` instead of the offending
tail:

- `error_return_then_trailing_string.snap`:
  `fn f() -> i32 { return 1; "a" }` — caret covers
  `{ return 1; "a" }`, real culprit is `"a"`.
- `error_divergent_subblock_does_not_silence_trailing_mismatch.snap`:
  `fn shit() -> i32 { { return 1 } "a" }` — same pattern, same
  body-wide caret.
- `error_semicolon_after_expr_discards_value.snap`:
  `fn f() -> u32 { 1; }` — body-wide caret.

**Fix.** When the block-value-type fails to coerce against the
declared return, span the *tail item* (`block.items.last()`'s
`expr` span) rather than the block. The fn-return check sits
above `infer_block`; either thread the tail span back from
`infer_block` or recompute it at the use site.

## U5 (low) — No `unreachable_code` warning after `Return`

`acceptance_never_unifies_with_anything.ox`:

```rust
fn f() -> i32 { let b: i32 = return 1; b }
```

Silently accepted. `b` is dead code. Rust warns
`unreachable_code` here. Not urgent — gate behind a future
warnings pass when one exists, alongside other "accepted but
suspicious" patterns.

## Coverage gaps worth a snapshot (low)

- `if`/`else if` chain unification where the divergent arm is in
  the middle — currently no test pins down the behavior.
- Pointer mutability check on `Return` (`fn f() -> *mut u8 { p }`
  where `p: *const u8`) — `coerce` is called, but no snapshot
  documents the resulting diagnostic.
- Three-level pointer mismatch — to lock in U3's improved
  rendering once it lands.
