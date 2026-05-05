# B026 — Typeck mistypes `if cond { return X; } expr` as `()` instead of expr's type

## Original report

Surfaced 2026-05-06 building stage-1. Hit ~5 times across stage-1
source; each occurrence required a bisect because the rendered
diagnostic has no span when the construct is inside an imported
file.

## The bug

A function whose body is `if cond { return X; }` followed by a tail
expression is rejected with a type-mismatch:

```rust
fn f() -> i32 {
    let c: i32 = 5;
    if c == 110 { return 10; }   // if-no-else, body diverges
    -1                            // tail; should make the block type i32
}
```

```
[E0250] Error: type mismatch: expected `i32`, found `()`
   ╭─[ /tmp/repro.ox:3:5 ]
   │
 3 │ ╭─▶     if c == 110 { return 10; }
 4 │ ├─▶     -1
   │ │
   │ ╰──────────── type mismatch here
───╯
```

Adding any intervening let or stmt makes it pass:

```rust
fn f() -> i32 {
    if c == 110 { return 10; }
    let _ = ();                   // the brace itself isn't enough; needs a stmt
    -1
}
```

Or replacing the tail with an explicit `return`:

```rust
fn f() -> i32 {
    if c == 110 { return 10; }
    return -1;                    // works
}
```

## Diagnosis (sketch)

Looks like the block-type computation drops the trailing tail
expression's type when the preceding statement is an `if`-without-
`else` whose body diverges. Either:

- The `if`-stmt absorbs the tail into its else-branch (parser
  ambiguity?); or
- The block is being typed as `()` because the if's body type
  (Never) is being lifted to the block before the tail is
  considered.

Reduces to: with no else branch, `if cond { return X; }` should
type as `()` (the missing-else default), the surrounding block
should evaluate the tail next, and the block's type should be the
tail's type. Currently the second step doesn't happen.

A second symptom — when the construct lives inside an imported
file, the rendered diagnostic has no span at all (just `E0250` on
its own line). That's a separate emit-pipeline issue but observed
together.

## Stage-1 workaround sites

`grep -n "workaround stage-0 bug"` in `example-projects/oxide/`:

- `lexer.ox:396` — `lookup_keyword` (255 tail → `return 255;`)
- `lexer.ox:519` — `read_escape` (-1 tail → `return -1;`)
- `lexer.ox:691` — `match2` (255 tail → `return 255;`)
- `lexer.ox:717` — `match1` (255 tail → `return 255;`)
- `typeck.ox:74`  — `prim_byte_width` (`return 0` → `return 0;`)
- `typeck.ox:89`  — `prim_name_lookup` (-1 tail → `return -1;`)

## Severity

**Medium** — easy workaround once you know the pattern (just
write `return X;` instead of `X` as tail), but the workaround
costs a comment ("// workaround stage-0 bug") at every site, the
diagnostic has no span when triggered cross-file, and the surface
behavior is mismatched with Rust semantics for the same construct.

## Fix sketch

Two parts:

1. **Block-type computation.** When a block's items end with an
   `if`-without-`else` (whose body diverges) followed by a tail
   expression, the block's type should be the tail expression's
   type. The current behavior treats the if as the block's value
   and ignores the tail.

   Bug likely lives in `Checker::infer_block` (`src/typeck/check`).
   Test fixture for the regression:

   ```rust
   fn f() -> i32 {
       if false { return 1; }
       2
   }
   ```

2. **Diagnostic span.** When E0250 fires from an imported file, the
   span renders empty. Audit the `from_typeck_error` reporter arm
   (`src/reporter/from_typeck.rs`) and check whether the `Span`
   carries `FileId(0)` or a valid one when the error originates in
   a non-root file. Likely a missing FileId propagation.

## Related

- spec/05 (block-type computation rules).
- spec/13_LOOPS.md (similar Never-absorption rules elsewhere — the
  `loop` body case works correctly, so the regression is specific
  to if-no-else).

## Status

Workarounds in place across stage-1 (~6 sites). Clean fix would
let those `return X;` revert to natural tail expressions and
remove the "workaround stage-0 bug" comments.
