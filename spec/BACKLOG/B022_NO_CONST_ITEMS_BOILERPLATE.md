# B022 — Lack of `const` items forces ~150 fake-constant fns in stage-1

## Original report

Surfaced 2026-05-06 building stage-1 (the Oxide-in-Oxide bootstrap
compiler at `example-projects/oxide/`). Reported as the highest-
frequency pain by the bootstrap implementer.

## The gap

Oxide reserves `KwConst` (`src/lexer/token.rs:48`) but has no
const-item form. Stage-1 needs ~10 small enum-shaped tag tables
(token kinds, AST kinds, HIR kinds, operator codes, error codes,
intrinsic ids, owner discriminators, mutability flags, etc.).
Without const items, every entry becomes a zero-arg fn:

```rust
fn TK_KW_FN()       -> u8 { 10 }
fn TK_KW_LET()      -> u8 { 11 }
fn TK_KW_MUT()      -> u8 { 12 }
// … 100+ more
```

Use sites carry the call syntax forever:

```rust
if k == TK_KW_FN()     { return parse_fn_item(p, false); }
if k == TK_KW_STRUCT() { return parse_struct_item(p); }
if k == TK_KW_EXTERN() { return parse_extern_block(p); }
```

vs. the natural form:

```rust
if k == TK_KW_FN     { return parse_fn_item(p, false); }
```

## Concrete sites

By table, in stage-1 source:

| File | Lines | Count |
|---|---|---|
| `example-projects/oxide/lexer.ox`     | 32–130 | ~110 fns (token + lex-error tags) |
| `example-projects/oxide/ast.ox`       | 23–130 | ~50 fns (item / expr / type / op tags) |
| `example-projects/oxide/hir.ox`       | 20–115 | ~30 fns |
| `example-projects/oxide/typeck.ox`    | 31–90  | ~30 fns |
| `example-projects/oxide/hir_lower.ox` | 30–60  | ~20 fns |

Conservative aggregate: **~600 LoC** of pure boilerplate, plus the
parens at every use site (~thousands of additional `()` characters
threaded through dispatch chains).

## Severity

**Medium-high** — by itself a paper-cut category, but applied at the
density a compiler-shaped program demands it dominates the line
count and degrades readability of every dispatch site. Bootstrap
implementer ranked it the #1 friction point ahead of structural
issues like enum payloads.

## Fix sketch

Minimum-viable v0:

```text
ConstItem ::= "const" Ident ":" Type "=" LiteralExpr ";"
```

- Top-level only.
- RHS restricted to a single literal token (`IntLit`, `BoolLit`,
  `CharLit`, `StrLit`). No expressions, no other consts. Same
  posture as the array-length slot in spec/09.
- Type annotation mandatory (no inference). The annotation is the
  literal's type; the parser already rejects literal kinds that
  don't fit a primitive slot.
- HIR: new `HirItem::Const { name, ty, value }`.
- Typeck: nothing new; the literal types as `ty`, and the value is
  inlined wherever the const name is referenced.
- Codegen: emit as a private `@const.<name> = constant <ty> <value>`
  in the LLVM module, or — for primitives small enough — inline at
  every use site. Either is correct.

Future expansion to const-eval (`const N: usize = M + 1`) is its
own spec; staying with literals is the smallest forward-compatible
cut and matches how spec/09 staged const expressions in array
lengths.

## Related

- spec/09 (length slot already takes a literal-only "const") — the
  same one-arm match shape applies here.
- B023 (enum with payload) — independent but compounds with this:
  with enums, the const-tag tables go away entirely. With const
  items but without enums, the tables are at least readable.

## Out of scope

- Const-expression evaluation.
- Const generics.
- `const fn` items.
