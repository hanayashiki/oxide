# B004 — Comma operator (`a, b`)

## Status
Open. Spotted while writing `13_LOOPS.md`. Not blocking the loops
spec; a real ergonomic gap that surfaces most visibly there.

## The gap

C's classic `for (i = 0, j = n; i < j; i++, j--) { ... }` doesn't
translate to Oxide. Our `for`-header allows exactly one expression
per slot:

```
ForExpr ::= 'for' Expr? ';' Expr? ';' Expr? Block
```

So `for ; ...; i = i + 1, j = j - 1 { ... }` is a parse error — the
`,` between two expressions has no grammar production.

This is **not** a deficiency of the `for` design. It's that Oxide
doesn't have the C-style comma operator (`expr , expr` evaluates
both, yields the second's value, type of the second). Once we add
the operator, `for`'s update slot accepts multi-statement updates
for free, with no change to the loop spec.

## Workarounds today

The patterns that motivate multi-update in C have direct rewrites:

```rust
// C:    for (i = 0; i < n; i++, j--) { ... }
// Oxide:
let mut i = 0;
for ; i < n; i = i + 1 {
    j = j - 1;          // secondary update at end of body
    ...
}
```

The "secondary update at end of body" rewrite has one wrinkle:
`continue` in the body skips the secondary update, since `continue`
branches to the for's `update_bb`, not the back-edge of the body
block. In C with `i++, j--` in the update slot, both fire on every
`continue`. In our workaround, only `i = i + 1` fires.

That's the real ergonomic loss — not the syntax cost, but the
control-flow asymmetry between primary (in update slot, fires on
`continue`) and secondary (in body, skipped by `continue`).

## Scope of the future spec

Rust does not have a comma operator. So we can't claim "subset of
Rust" for this. Two design directions:

### A. Real comma operator

```
Expr ::= ... | Expr ',' Expr
```

Pratt-level binary, lowest precedence (lower than `=`), left-
associative. Type of `a, b` is type of `b`; `a` is evaluated for
side effect (type must coerce to `()` / `!` / let-it-be — TBD).

Pros: matches C's mental model directly, drops into the for-update
slot without grammar surgery.

Cons: opens up "use comma everywhere" patterns Rust deliberately
avoided. Function call argument lists already use `,` — the
expression-level comma would have to thread through "no-comma-here"
flags at call sites the same way `if`/`while` thread "no-struct-lit-
here" through cond positions. Real grammar complexity.

### B. Multi-expression update slot only

Allow `,`-separated expressions specifically in the for's
update slot (and maybe init), without introducing a
general-purpose expression-level comma operator.

```
ForExpr ::= 'for' (ForInit (',' ForInit)*)? ';' Expr? ';' (Expr (',' Expr)*)? Block
```

Pros: solves the real ergonomic problem with no leakage. No call-
site disambiguation needed.

Cons: feels ad-hoc. C users see `,` and expect it to be the comma
operator everywhere. Documenting "the for-header has its own
`,`-list grammar that doesn't match expression-level `,`" is a
small footnote but a footnote.

**Recommendation when the time comes:** B. Cleaner, more contained,
solves 95% of the use cases. A is a bigger commitment that pulls in
parser disambiguation everywhere `,` appears.

## When to land

After we have at least one real-world Oxide program that hits the
"continue should re-run secondary update" issue. Until then the
body-rewrite is fine and the spec stays simpler.
