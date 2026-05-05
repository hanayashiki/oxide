# B023 — `enum` with payloads and `match` would erase half of stage-1's IR plumbing

## Original report

Surfaced 2026-05-06 building stage-1. The single largest structural
cost across the bootstrap source. Reported by the implementer as
the #2 pain after const items, but the bigger lift to fix.

## The gap

Oxide reserves `KwEnum` and `KwMatch` (`src/lexer/token.rs:55,67`)
but neither is wired through. Today's `Adt` umbrella covers structs
only; there is no sum-type form and no pattern-match construct.

The bootstrap consequence: every IR node type — `Token`, `Expr`,
`HirExpr`, `Ty`, `Item`, `Type`, plus their pretty-printer and
lowerer — is a tagged struct carrying the **union** of all variant
payloads, with kind dispatch hand-rolled as if-else chains over u8
discriminants.

### Tagged-struct bloat

`example-projects/oxide/ast.ox:201-235` — `Expr`:

```rust
struct Expr {
    kind: u8,

    // Primitive payloads
    int_val:  u64,
    bool_val: bool,
    char_val: u32,

    // Interned name / string payload
    name: Ident,

    // Generic child slots — meaning per-variant
    e1: usize, e2: usize, e3: usize, e4: usize,
    e4_is_block: bool,

    // Type child
    t1: usize,

    // List payloads
    args:      Vec<usize>,
    type_args: Vec<usize>,
    sl_fields: Vec<StructLitField>,

    // Operator code (UnOp / BinOp / AssignOp / Mutability)
    op: u8,

    mutable: bool,

    span_start: usize,
    span_end:   usize,
}
```

Any given `Expr` variant uses 2–4 of these fields. The remaining
13–15 are zero-defaults. Same shape recurs in `HirExpr`
(`hir.ox:230-280`), `Ty` (`typeck.ox:107-150`), `Token`
(`lexer.ox:131-156`), and `Item` (`ast.ox:170-200`).

The `e1`/`e2`/`e3`/`e4`/`t1` slots have no self-documenting meaning
— comments at use sites are the only hint that "in `EX_FOR`, e1 is
init, e2 is cond, t1 is update, e3 is body."

### Dispatch chains

`example-projects/oxide/ast_pretty.ox:200-450` — pretty-printer for
`Expr`, ~250 lines of `if k == EX_INT_LIT() { … } if k == EX_PAREN()
{ … } if k == EX_UNARY() { … } …` over 28 expression kinds. Same
shape in `parser.ox` (parser dispatch on token kind), `hir_lower.ox`
(AST → HIR translation), `typeck.ox` (per-kind typing rules).

No exhaustiveness check. Adding a new variant silently misses arms
in pretty-printers and lowerers — caught only by snapshot diffs at
test time.

### Per-occurrence allocation cost

Every node carries empty `Vec<usize>` instances for unused list
slots. Each empty vec is 24 bytes (data ptr + len + cap) on 64-bit.
A typical `Expr` allocation costs ~200B; a Variant-encoded form
would average ~40B.

## Severity

**High** — biggest single structural cost in stage-1. Half of
`ast.ox` and `hir.ox` (~400 LoC combined) is structure-defining
boilerplate that the language could provide.

## Fix sketch

`enum` with C-style discriminant + variant payloads, plus `match`
with exhaustiveness:

```rust
enum HirExprKind {
    IntLit(u64),
    BoolLit(bool),
    Local(LocalId),
    Fn(FnId),
    Binary { op: BinOp, lhs: HExprId, rhs: HExprId },
    Call   { callee: HExprId, args: Vec<HExprId>, type_args: Vec<HirTy> },
    // ...
}

match e.kind {
    HirExprKind::IntLit(n) => …,
    HirExprKind::Binary { op, lhs, rhs } => …,
    HirExprKind::Call { callee, args, type_args } => …,
}
```

Implementation surface (large but bounded):

- **Lexer / parser.** `KwEnum` and `KwMatch` already lex; parser
  needs `EnumDecl`, `EnumVariant` (struct-shaped or tuple-shaped),
  `MatchExpr`, `Pattern`. New AST nodes; new HIR nodes
  (`HirAdtKind::Enum`, `HirExprKind::Match`).
- **Typeck.** Variant constructors as ADT-typed values; pattern
  type-checking; exhaustiveness analysis (a usefulness-walk, not a
  decision tree compiler).
- **Mono.** Enums with type params monomorphize identically to
  generic structs.
- **Codegen.** Tagged-union LLVM lowering: discriminant + max-
  payload-sized union body. Standard layout; nothing exotic.

Reasonable v0 simplifications:
- No struct-shaped variants (only `Foo(T1, T2)` tuple form). v0 of
  ADT (spec/08) used the same posture.
- No `match` arm guards (`if cond` after a pattern).
- No nested patterns. Patterns are one level deep (variant + bind
  per slot).
- Exhaustiveness is "every variant has a top-level arm or `_`."
  Real usefulness-walk lands later.

## Concrete payoff for stage-1

If B023 lands as proposed:

- `ast.ox` shrinks by ~150 LoC (drop `EX_*` tag fns, drop `e1..e4`
  slots, drop `op`/`mutable`/`is_place`/`name`/`int_val`/etc. unions).
- `hir.ox` shrinks similarly (~100 LoC).
- `ast_pretty.ox` becomes a single `match e.kind { … }` instead of a
  28-arm if-else chain.
- `parser.ox`'s `parse_prefix` becomes a `match` over `TokenKind`
  variants.
- Hir-lower's `lower_expr` becomes `match`.

Order-of-magnitude: stage-1 source drops by **~600 LoC** and gains
exhaustiveness checking on every dispatch.

## Related

- spec/08 (struct-only ADT today; enum is the natural extension).
- B022 (const items) — independent. Tag tables go away with B023;
  if only B022 lands, the tables are at least readable.
- B025 (no tuples) — variants would default to tuple-shaped, so
  this work and B025 share grammar surface.

## Out of scope

- Or-patterns (`Foo | Bar`).
- Range patterns.
- Guards.
- Pattern matching outside `match` (e.g. `if let`, destructuring
  `let`).
