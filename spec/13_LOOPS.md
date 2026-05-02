# Loops (`while`, `loop`, C-style `for`) and `break` / `continue`

## Requirements

Oxide today has no looping construct — recursion is the only iteration
mechanism. `spec/10_ADDRESS_OF.md` even calls this out ("the still-
missing `while`"). The lexer already recognises `while`, `for`,
`break`, `continue` as keywords, but none of them reach the parser.

This spec wires three loop forms through the full pipeline (parser →
HIR → typeck → codegen) and adds the divergent `break expr?` /
`continue` operators. With this in, `for`-style imperative algorithms
become writable directly:

```rust
fn sum_to(n: i32) -> i32 {
    let mut s = 0;
    for (let mut i = 0; i < n; i = i + 1) { s = s + i; }
    s
}
```

## Design overview — three AST forms, one HIR form

The AST keeps `while` / `loop` / `for` as **three distinct expression
kinds** so parser snapshots round-trip cleanly and the source's shape
is preserved through pretty-printing.

HIR collapses all three into a **single `Loop` node** (named after
the most generic loop concept, not after any specific surface
keyword — see "Naming" note below):

```rust
HirExprKind::Loop {
    init:   Option<HExprId>,   // empty for While / Loop
    cond:   Option<HExprId>,   // empty for Loop and `for (;;) { ... }`
    update: Option<HExprId>,   // empty for While / Loop
    body:   HBlockId,
    has_break: bool,
    source:  LoopSource,       // While | Loop | For — diagnostics & pretty-print only
}
```

The lowering is structural: each AST form just sets the optional
slots and a `source` tag.

```text
while cond { body }            ─▶  Loop{init: ∅, cond: Some(c), update: ∅, body, source: While}
loop      { body }             ─▶  Loop{init: ∅, cond: ∅,       update: ∅, body, source: Loop}
for (i;c;u) { body }           ─▶  Loop{init,    cond,          update,    body, source: For}
```

**Naming.** `HirExprKind::Loop` shadows the surface-AST `ExprKind::Loop`
in name only — they live in different namespaces. AST's `Loop` is the
narrow keyword form `loop { }`; HIR's `Loop` is the unified node that
covers all three surface forms. We picked `Loop` over `For` for the
unified node because "Loop" reads as "any loop", while "For" carries
a connotation of the C-style three-slot header that misleads readers
into thinking the node is for the for-keyword only. The `source` tag
(`LoopSource::Loop`) preserves the keyword-level distinction where it
matters.

The runtime model under this unification is one CFG shape — the
classic C-style `for` skeleton — with optional pieces:

```text
preheader:  init?         (skip if init is ∅)
header:     cond?         (use constant true if cond is ∅)
body:       body          (back-edge to update)
update:     update?       (continue target; skip if update is ∅)
end:        break target
```

`while` is "init/update absent"; `loop` is "all three header slots
absent"; `for` is "all three present (or any subset)." All three
forms collapse cleanly into the same skeleton.

### Why collapse in HIR but not in AST

- **AST stays split** because the parser-snapshot tests need to assert
  on the user's chosen form. `while x { y }` and `for (; x;) { y }`
  produce equivalent runtime behaviour but they're different surface
  programs and the parser is the layer that records that distinction.
- **HIR collapses** because typeck and codegen both want the same
  unified CFG shape and the same set of semantic rules. Carrying
  three near-identical kinds through three layers is duplication
  without benefit.
- **The `source` tag** is kept on the HIR node so HIR pretty-printing
  can reconstruct "While" / "Loop" / "For" labels and diagnostics can
  flavour their wording when they want to. It does **not** drive
  semantic rules — see "Typing rule is structural, not source-driven"
  below.

### Typing rule is structural, not source-driven

The `Loop` node's value type is decided by the **structure** of its
header, not by the `source` tag:

```text
type_of_loop(cond, has_break, break_expr_types):
    if cond.is_some():           unit                    // exits via cond → ()
    else if !has_break:          never                   // truly infinite → !
    else:                        type_of_break_exprs     // break-driven exit → derived
```

Reading the rule: **a loop with a `cond` can fall out the bottom
without `break`, so its expression value is `()`**. A loop without a
`cond` can only exit via `break`, so its value is whatever `break`
carries (or `!` if no `break` ever fires).

Concretely:

| Form | Lowered shape | Typing |
|---|---|---|
| `while c { ... }` | `Loop{cond: Some(c), …}` | `()` |
| `for (i;c;u) { ... }` | `Loop{cond: Some(c), …}` | `()` |
| `for (;c;) { ... }` | `Loop{cond: Some(c), …}` | `()` |
| `loop { ... }` (no break) | `Loop{cond: ∅, has_break: false}` | `!` |
| `loop { ... }` (break;) | `Loop{cond: ∅, has_break: true}` | `()` |
| `loop { break v; }` | `Loop{cond: ∅, has_break: true}` | type of `v` |
| `for (;;) { ... }` (no break) | `Loop{cond: ∅, has_break: false}` | `!` |
| `for (;;) { ... }` (break v;) | `Loop{cond: ∅, has_break: true}` | type of `v` |

Note the last two rows: `for (;;) {}` and `loop {}` produce identical
HIR up to the `source` tag, and they type identically. That's
deliberate — they have the same CFG and the same runtime behaviour,
and the user's syntactic choice between them shouldn't change the
type of the expression.

`source` could have driven the rule instead (e.g. "While/For ⇒ ()
always; Loop ⇒ break-derived"), which would force `for (;;) {}` to
type as `()` despite being an infinite loop. We pick the structural
rule because it matches user intuition: an infinite loop is
divergent regardless of which keyword spelled it.

## Subset-of-Rust constraint

`while cond { body }` and `loop { body }` are exact subsets of Rust.
The C-style `for (init; cond; update) { body }` is **not** in Rust;
it is the one deliberate non-subset addition. Rust's iterator-style
`for pat in iter { body }` is blocked on traits and stays out of
scope (see "Out of scope" below).

`break expr?` and `continue` (no operand) are exact subsets of Rust
once you drop labels. Rust spells `break` with an optional label and
optional expression (`break 'outer val;`); we drop the label half.

## Acceptance

```rust
// while
fn count_down(mut n: i32) -> i32 {
    let mut last = 0;
    while n > 0 { last = n; n = n - 1; }
    last
}

// loop with break value (loop expression types as i32)
fn first_match() -> i32 {
    let mut i = 0;
    loop {
        if i == 7 { break i * 2; }
        i = i + 1;
    }
}

// C-style for, with continue
fn sum_evens_to(n: i32) -> i32 {
    let mut s = 0;
    for (let mut i = 0; i < n; i = i + 1) {
        if i % 2 != 0 { continue; }
        s = s + i;
    }
    s
}

// loop {} with no break — types as `!` (divergent), absorbs into any
// return type. Used here to satisfy `-> i32` without a tail expression.
fn spin() -> i32 { loop {} }
```

These programs parse, typecheck, and JIT-execute to the expected
values (`count_down(3) == 1`, `first_match() == 14`, `sum_evens_to(10)
== 20`).

The negative cases:

```rust
// E0263 BreakOutsideLoop
fn bad() -> i32 { break; 0 }

// E0264 ContinueOutsideLoop
fn bad2() -> i32 { continue; 0 }

// TypeMismatch — break value forced into a `()`-typed loop
fn bad3() {
    while true { break 5; }
}

// TypeMismatch — break values disagree
fn bad4() -> i32 {
    loop { break 5; break true; }
}
```

## Position in the pipeline

```
Source ─▶ tokens ─▶ AST ─▶ HIR ─▶ typeck ─▶ codegen
                       ╰── all four layers gain new arms ──╯
```

Tokens already exist (`KwWhile`, `KwFor`, `KwBreak`, `KwContinue` —
`src/lexer/token.rs`). Lexer is unchanged.

## AST changes (`src/parser/`)

### New expression kinds

```rust
pub enum ExprKind {
    ...
    While {
        cond: ExprId,
        body: BlockId,
    },
    Loop {
        body: BlockId,
    },
    /// C-style `for ( init? ; cond? ; update? ) block`. All three
    /// header slots are optional; `for (;;) { ... }` is the infinite-
    /// loop spelling. The parens around the header are mandatory —
    /// they delimit header from body unambiguously, fixing the
    /// update→body parse ambiguity that the parenless form has
    /// (see "Why parens around the `for` header" below).
    /// `init` may be a `let`-form (parsed as `ExprKind::Let`) or any
    /// other expression; the `let` is tried first because the keyword
    /// disambiguates.
    For {
        init: Option<ExprId>,
        cond: Option<ExprId>,
        update: Option<ExprId>,
        body: BlockId,
    },
    /// `break expr?` — type `!`. The named field `expr` carries the
    /// value the loop expression evaluates to: typeck coerces
    /// `expr`'s type (or `()` if `None`) into the innermost loop's
    /// result-type slot. Struct-variant shape (rather than
    /// `Break(Option<_>)` like `Return`) because the operand has a
    /// load-bearing typing role we want named explicitly at the AST.
    Break {
        expr: Option<ExprId>,
    },
    /// `continue` — no operand in v0 (no labels, so no need for one).
    Continue,
}
```

### Grammar

```
WhileExpr    ::= 'while' Expr Block
LoopExpr     ::= 'loop'  Block
ForExpr      ::= 'for' '(' ForInit? ';' Expr? ';' Expr? ')' Block
ForInit      ::= LetNoSemi | Expr
LetNoSemi    ::= 'let' 'mut'? Ident (':' Type)? ('=' Expr)?
BreakExpr    ::= 'break' Expr?
ContinueExpr ::= 'continue'
```

`While` / `Loop` / `For` slot into the **atom** alternation alongside
`if_expr` (`src/parser/parse/syntax.rs`'s `expr_parser`). They're
expressions, not block items, so they nest anywhere an expression
can.

`Break` / `Continue` live at the same top level as `return_form` (a
choice rung above the Pratt builder), so they can occupy any
expression slot — `let _ = break 5;` is a valid (if useless) shape,
just as `let _ = return 1;` is.

### `for`-header parsing

The header sits inside `(` `)` and contains three optional slots
separated by two mandatory `;`s. Parens delimit header from body —
see "Why parens around the `for` header" below for why they're
mandatory.

Examples:

```
for (;;) { ... }                                     // infinite
for (let mut i = 0; i < 10; i = i + 1) { ... }       // full
for (let mut i = 0;;) { ... }                        // init only
for (; i < 10;) { ... }                              // cond only
for (;; i = i + 1) { ... }                           // update only
```

### Why parens around the `for` header

The parenless grammar `for init?; cond?; update? block` is ambiguous
at the update→body boundary:

```
for ;; { body }
//   ^^^^^^^^ — is this update=Block(body), or update=∅ + body=block?
```

In the chumsky parser, the update slot's expression matches eagerly
through the atom alternation — and `block_expr_parser` is in that
alternation, so `{ body }` matches as a Block expression and the
update slot commits to it, leaving no body for the loop to consume.

`if`/`while` don't have this issue because their cond is followed by
exactly one block (the body); if cond's expr eats `{ x }` and there's
no body block, the parse correctly fails. `for` has the asymmetry
that update is followed by the body but is itself optional, so there
is no separator to disambiguate.

We resolve this by mandating parens. The `)` delimits the header,
the body block starts unambiguously after it, and chumsky doesn't
need to backtrack. C-style. Trivial parser.

The alternative — a "no-block-at-top mode" for the update slot's
expression parser — preserves the parenless syntax at the cost of
an alternate expression parser variant. We pick parens for size and
familiarity.

### Cond struct-literal ambiguity

`while Foo { ... } { ... }` would parse the `Foo { ... }` as a struct
literal, leaving the trailing `{ ... }` as the body. This matches
`if`'s known deviation (a struct literal in cond position) and inherits
the same TBD from `08_ADT.md` — eventually a "no-struct-lit-at-top"
mode for cond / for-cond / for-update parsing.

### What the AST does *not* add

- Labels (`'name: loop { ... }`). All `break` / `continue` target the
  innermost enclosing loop.
- `for pat in iter { ... }` (iterator-style for). Needs traits.
- `while let pat = expr { ... }`. Needs patterns.

## HIR changes (`src/hir/`)

### One unified loop kind

All three AST forms collapse to a single `Loop` node, plus the two
divergent flow operators:

```rust
pub enum HirExprKind {
    ...
    /// Unified loop. Covers all three surface forms (`while` / `loop` /
    /// C-style `for`). Each header slot is populated only when the
    /// surface form supplied it; see the lowering table above.
    Loop {
        /// Empty for `while` / `loop`; populated for C-style `for`.
        init:   Option<HExprId>,
        /// Empty for `loop` and `for (;;) { ... }`; populated otherwise.
        /// `None` is "no condition test"; codegen treats it as "always
        /// true".
        cond:   Option<HExprId>,
        /// Empty for `while` / `loop`; populated for C-style `for`.
        update: Option<HExprId>,
        body:   HBlockId,
        /// Set by the lowerer iff a `break` inside `body` targets this
        /// `Loop`. Lets typeck decide divergent loops without walking
        /// the body itself.
        has_break: bool,
        /// Diagnostic / pretty-print only. Records which AST keyword
        /// the user wrote. Does **not** drive any semantic rule —
        /// typing keys on `cond.is_some()`, codegen keys on the
        /// optional slots being `Some`/`None`. See "Typing rule is
        /// structural, not source-driven" above.
        source: LoopSource,
    },
    /// `break e?`. The operand (`expr`) carries the value the
    /// enclosing loop expression evaluates to. Named-field form
    /// matches the AST shape; typeck reads `expr` and coerces its
    /// type into the innermost loop's result-type slot. `None` is
    /// "no operand" (acts as `()` for typing purposes — see the
    /// `infer_break` rule below).
    Break {
        expr: Option<HExprId>,
    },
    Continue,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LoopSource { While, Loop, For }
```

We **don't** carry a `LoopId` on HIR nodes. `break` / `continue`
target the innermost enclosing loop unconditionally (no labels),
which both typeck and codegen track via a per-fn loop stack
maintained during their own walks. Storing the target on the HIR node
would be redundant.

### Place rule

None of the new kinds are place expressions. `compute_is_place`
falls through its catch-all `_ => false` arm. No code change.

### Lowering

`Lowerer` gains:

```rust
loop_stack: Vec<LoopFrame>,

struct LoopFrame {
    /// Set by `Break { .. }` lowering when this frame is the innermost.
    has_break: bool,
}
```

A single helper folds all three AST forms into the unified
`HirExprKind::Loop` node. The shape of the lowering is the same; only
which slots get populated and the `source` tag differ.

```text
lower_loop(init?, cond?, update?, body, source):
    push fresh block scope                       // for-init's `let` lives only inside
    init'   = init.map(lower_expr)
    cond'   = cond.map(lower_expr)
    update' = update.map(lower_expr)
    push LoopFrame{has_break: false}
    body' = lower_block(body)
    frame = pop loop frame
    pop block scope
    HirExprKind::Loop {
        init: init', cond: cond', update: update', body: body',
        has_break: frame.has_break,
        source,
    }

ast::While { cond, body } -> lower_loop(None, Some(cond), None, body, LoopSource::While)
ast::Loop  { body }       -> lower_loop(None, None,       None, body, LoopSource::Loop)
ast::For   { i, c, u, b } -> lower_loop(i,    c,          u,    b,    LoopSource::For)

ast::Break { expr }       -> expr' = expr.map(lower_expr)
                             if loop_stack is empty: HirError::BreakOutsideLoop
                             else: loop_stack.top.has_break = true
                             HirExprKind::Break { expr: expr' }

ast::Continue             -> if loop_stack is empty: HirError::ContinueOutsideLoop
                             HirExprKind::Continue
```

The block scope is pushed unconditionally even for `while` / `loop`,
because pushing an empty scope is free and it keeps the lowering
helper uniform. (No `let` ever lands in that scope unless the AST
shape was a C-style `for` with an init.)

`has_break` is set on **every** `Loop` node, regardless of source.
Typeck uses it together with `cond.is_some()` to decide the loop's
type via the structural rule (see "Typing rule is structural" above).

### New `HirError` variants

```rust
pub enum HirError {
    ...
    BreakOutsideLoop    { span: Span },     // E0263
    ContinueOutsideLoop { span: Span },     // E0264
}
```

Both spans point at the `break` / `continue` keyword's span (the
expression's outer span, captured by the lowerer).

`HirError::span()` gains arms for the two new variants.
`reporter/from_hir.rs` gains the matching render arms.

### Scope of the `Loop` node

All four pieces of a `Loop` — `init`, `cond`, `update`, `body` —
share a **single lexical scope**: the for-scope, pushed by the
lowerer around the whole construct and popped on exit. The body's
own block contributes a *nested* scope (via `lower_block`) for
bindings introduced inside the body proper.

```text
{ for-scope ─────────────────────────────────────┐
    init     ← may introduce bindings (let)      │
    cond     ← sees init's bindings              │
    update   ← sees init's bindings              │
    { body-scope ────────────────────────┐       │
        body items                       │       │
        let j = ...   (j visible only    │       │
                       inside body-scope)│       │
    } ────────────────────────────────────┘       │
} ─────────────────────────────────────────────────┘
```

Two patterns fall out cleanly:

**Loop-private counter** — `init` is a `let`, the binding lives only
inside the for:

```rust
for (let mut i = 0; i < n; i = i + 1) { ... }
// `i` is gone here
```

**Counter shared with the outer scope** — `init` is an assignment
(or any non-let expression); no new binding is introduced, so the
outer binding is what's read/written:

```rust
let mut i = 0;
for (i = 0; i < n; i = i + 1) { ... }
// `i` is still alive here, holding its final value
```

This is the load-bearing reason the `init` slot accepts any
expression (not just `let`): the user picks scoping by picking the
form. Restricting `init` to `let` would lose this capability.

For `while` and `loop` (no `init` slot at all), the for-scope is
still pushed but does no work — it's a no-op wrapper around `body`.
Cheap, and it keeps the lowering helper uniform across all three
surface forms.

This two-block structure (for-scope wrapping body-scope) matches
C99 §6.8.5/5 verbatim: "An iteration statement is a block whose
scope is a strict subset of the scope of its enclosing block. The
loop body is also a block whose scope is a strict subset of the
scope of the iteration statement." The strict-subset relationship
is what lets the body shadow init's bindings:

```rust
for (let mut i = 0; i < 3; i = i + 1) {
    let i = 99;        // fresh `i` in body-scope, shadows the for-scope `i`
    // body reads i == 99
}
// cond/update still operated on the for-scope `i` each iteration
```

This works automatically because `lower_block(body)` pushes its own
scope on top of the for-scope; the body's `let i = 99` introduces a
new local there, and name lookups inside the body hit it before
reaching the outer for-scope binding. No special-case rule needed.

## Typeck changes (`src/typeck/`)

### `Inferer` gains a loop-target stack

```rust
struct Inferer {
    ...
    /// Stack of "expected type of the loop expression," one frame per
    /// enclosing loop. Pushed when we enter a loop body; popped on
    /// exit. Read by `Break { .. }` to decide what to coerce its `expr`
    /// against. No labels means the innermost frame is always the
    /// right target.
    loop_tys: Vec<TyId>,
}
```

### Type rules — one unified `Loop` arm, one `Break`, one `Continue`

```text
infer_loop(init, cond, update, body, has_break, span):
    if let Some(i) = init   { infer_expr(i); }   // result discarded
    if let Some(c) = cond   {
        ct = infer_expr(c)
        unify(ct, bool, c.span)                  // cond must be bool
    }
    if let Some(u) = update { infer_expr(u); }

    // Structural typing rule:
    //   cond.is_some()  ⇒ unit              (loop can fall out the bottom)
    //   cond.is_none() & !has_break ⇒ never (truly infinite)
    //   cond.is_none() &  has_break ⇒ fresh infer var (break-driven exit)
    target =
        if cond.is_some()      { unit }
        else if has_break      { fresh_infer_var }
        else                   { never }

    push loop_tys ← target
    body_ty = infer_block(body)
    pop loop_tys
    expect_unit(body_ty, body.span)              // body must be ()/!/error
    target

infer_break(expr, span):
    expr_ty = match expr {
        Some(e) => infer_expr(e),
        None    => unit,
    }
    target = loop_tys.last()
        .expect("HIR enforced: break inside a loop")
    coerce(expr_ty, target, expr_or_break_span)
    never

infer_continue(span):
    never
```

### Why a single `coerce` covers every loop shape

The `Break` arm calls `coerce(expr_ty, target, span)` and the
`target` value already encodes the loop's typing regime:

- **cond-bearing loop (`target = unit`)**: `break;` → coerce `() → ()`,
  trivial. `break 5;` → coerce `i32 → ()`, errors with the standard
  `TypeMismatch`. No special-case branch needed for "break with a
  value in a `while`."
- **infinite-loop, no break in body (`target = never`)**: unreachable
  from a `Break` — `has_break` was `false` at HIR-lower so the body
  contains no `Break` to invoke this arm.
- **infinite-loop with break (`target = fresh_infer`)**: first
  `break;` binds the infer var to `()`; first `break 5;` binds it to
  `i32`; subsequent breaks unify against the binding (so
  `break 5; break true;` errors on the second break).

`Break` itself produces `!` so it flows vacuously into any context —
same `Never`-absorbing rule that already handles `Return`.

### No reachability analysis — coherent with `infer_block`

The structural rule ("`has_break` decides whether the loop has a
break-exit path") is computed **purely from HIR structure**, not from
runtime reachability. Concretely: typeck does not look at what
control flow the body actually exhibits; it just looks at whether a
`Break` HIR node appears anywhere inside the loop's frame.

This matters for cases where a `break` is syntactically present but
runtime-unreachable. The canonical pair:

```rust
loop { return 1; break; }       // typeck'd as ()
loop { return 1; break 2; }     // typeck'd as i32
```

In both, the `break` is dominated by a `return` and would never
execute. We type them anyway:

- HIR lowering walks the body and sees `Break { .. }`, sets
  `has_break = true` on the enclosing loop frame regardless of any
  preceding divergent expressions.
- Typeck visits the `Break` arm, coerces its operand into the loop's
  fresh target var, and produces `!` for the `Break` itself. The
  coerce binds the loop type.

This is **coherent with `infer_block`'s existing behavior**:
`infer_block` does not skip items after a divergent one. Given
`{ return 1; let x = 5; }`, the `let x = 5` is still lowered, still
typed (binding `x: i32`), and still contributes to inference — it
just doesn't get a chance to *run*. The block's *value* uses the
trailing `is_never` short-circuit (the last item's type wins iff
it's `!` or has no semi), but every item is still visited and typed.

We make the same call here: `Break` after `Return` is typed normally,
contributes to the loop's target type, and the loop ends up `()` /
`T` accordingly. The alternative — "a `Break` after a divergent
statement should be ignored for typing purposes" — would require:

1. A reachability pass over the HIR to identify dominated `Break`s.
2. A new "this `Break` doesn't count" flag, or post-hoc filtering of
   loop-stack contributions.
3. Order-sensitive typing (`loop { break; return 1; }` and
   `loop { return 1; break; }` would type differently despite being
   semantically equivalent at the type level).

We skip all of that. Reachability stays an LLVM-side concern (dead
code gets pruned by the optimizer; the `is_terminated()` short-
circuit in codegen handles emission). This also matches Rust, which
types the `break` and emits an `unreachable_code` lint instead of
changing the loop's type.

### `expect_unit` for body

`while`/`for` / `loop` bodies are evaluated for side effect. The body
**block**'s tail must be `()` — exactly the same constraint
`if`-without-`else` enforces on its then-arm. We use the existing
`expect_unit` (one-way; doesn't bind infer vars) so that:

- `while c { 5 }` → tail `5` types `i32`, `expect_unit` errors.
- `while c { return 1; }` → tail `return 1` types `!`, passes.
- `while c { let mut x = 0; x }` → tail `x` types `i32`, errors.

`init` and `update` slots of `for` go through `infer_expr` only — we
do **not** `expect_unit` them. They're typically a `Let` (always
`()`), an assignment (always `()`), or a call/expression with
side-effects whose value is discarded. The block-level
`;`-enforcement spirit doesn't quite apply since these aren't block
items; we accept any type for them and rely on the user to write
side-effecting forms (matching how Rust treats the equivalent
`for_each`-via-loop pattern).

### Errors

No new typeck errors. The existing `TypeError::TypeMismatch` (E0250)
covers:

- `while`/`for` cond not `bool`
- `break val` operand-vs-target type mismatch (in any loop kind)

`BreakOutsideLoop` / `ContinueOutsideLoop` live at HIR (E0263,
E0264).

## Codegen (`src/codegen/`)

### One unified IR skeleton

Because HIR carries a single `Loop` node with optional pieces, codegen
needs only **one** emit function. The IR skeleton is the C-style
`for` shape, with three optional pieces:

```text
preheader:                    ; emitted iff init is Some
  <init>
  br label %header
header:                       ; emitted iff cond is Some
  %c = <cond>                 ; or this whole block is elided
  br i1 %c, label %body, label %end
body:
  <body>                      ; back-edge → update
  br label %update            ; (or %body when update is None and we collapse)
update:                       ; emitted iff update is Some
  <update>
  br label %header            ; (or %body if cond is None)
end:                          ; break target
```

Block-elision rules (compute lazily, not all at once):

- `init.is_none()` ⇒ no `preheader`; the predecessor block branches
  straight into `header` (or `body` if cond is also None).
- `cond.is_none()` ⇒ no `header`; `update`'s back-edge goes directly
  to `body`. The post-loop `end:` block has no predecessors except
  `break` sites.
- `update.is_none()` ⇒ no `update`; the back-edge from `body` goes
  directly to `header` (or `body` itself if cond is also None — i.e.
  pure `loop { ... }`).

`continue_bb` is whichever of `update` / `header` / `body` is the
"top of the next iteration" given the elisions:

| Form | continue_bb | break_bb |
|---|---|---|
| `while`        (cond=Some, init/update=None)   | `header` | `end` |
| `loop`         (all None)                      | `body`   | `end` |
| C-style `for`  (any combination)               | `update` if Some else `header` if Some else `body` | `end` |

`break_bb` is always `end`. The single rule: continue jumps to the
"step that re-tests the loop's exit condition," which is `update` if
the user wrote one, else `header`, else (no header either) the top
of the body.

Result-slot rule: allocate via `alloca_in_entry` iff the `Loop`'s
typeck'd type is a value type (non-`()`, non-`!`). Concretely this
only fires when `cond.is_none() && has_break` and at least one
`break` carries a value. `break v` stores into the slot before
`br end`; the post-loop emit loads it as the expression's value.

If `cond.is_none() && !has_break`, the `end:` block has no
predecessors. Use the existing `merge_bb_has_no_preds` helper
(`emit_if` pattern) to terminate it with `unreachable` and return
`None`.

### `FnCodegenContext` gains a single `LoopTargets` shape

```rust
struct FnCodegenContext<'ctx> {
    ...
    /// Pushed when we start emitting a `Loop` body, popped on exit. The
    /// innermost frame is the `break`/`continue` target.
    loop_targets: Vec<LoopTargets<'ctx>>,
}

struct LoopTargets<'ctx> {
    break_bb:    BasicBlock<'ctx>,
    continue_bb: BasicBlock<'ctx>,
    /// `Some` only when the loop's typeck'd type is a value type
    /// (occurs only for `cond.is_none() && has_break` with at least
    /// one valued break). `break expr` stores `expr`'s value here
    /// before branching to `break_bb`. Mirrors the `result_slot`
    /// pattern in `emit_if`.
    result_slot: Option<PointerValue<'ctx>>,
}
```

### `emit_break` / `emit_continue`

```rust
fn emit_break(&self, fx: &mut FnCodegenContext<'ctx>, expr: Option<HExprId>) {
    let target = *fx.loop_targets.last().expect("HIR ensured break is inside a loop");
    if let Some(eid) = expr {
        let v = self.emit_expr(fx, eid);
        if !self.is_terminated() {
            if let (Some(slot), Some(val)) = (target.result_slot, v) {
                self.builder.build_store(slot, val).unwrap();
            }
            self.builder.build_unconditional_branch(target.break_bb).unwrap();
        }
    } else {
        self.builder.build_unconditional_branch(target.break_bb).unwrap();
    }
}

fn emit_continue(&self, fx: &mut FnCodegenContext<'ctx>) {
    let target = *fx.loop_targets.last().expect("HIR ensured continue is inside a loop");
    self.builder.build_unconditional_branch(target.continue_bb).unwrap();
}
```

Both terminate the current basic block. The existing `is_terminated()`
short-circuit at the top of `emit_expr` / `emit_block` already
handles dead-code skipping after `break`/`continue`, same as it does
for `return`.

### Worked LLVM IR — `loop { break v; }`

For:

```rust
fn first_match() -> i32 {
    let mut i = 0;
    loop {
        if i == 7 { break i * 2; }
        i = i + 1;
    }
}
```

```llvm
allocas:
  %i.0.slot      = alloca i32, align 4
  %loop.slot     = alloca i32, align 4         ; loop's result slot
  br label %body

body:                                           ; preds = %allocas
  store i32 0, ptr %i.0.slot, align 4
  br label %loop.body

loop.body:                                      ; preds = %body, %if.end
  %i.cur = load i32, ptr %i.0.slot, align 4
  %eq    = icmp eq i32 %i.cur, 7
  br i1 %eq, label %if.then, label %if.end

if.then:                                        ; preds = %loop.body
  %i.cur1 = load i32, ptr %i.0.slot, align 4
  %mul    = mul i32 %i.cur1, 2
  store i32 %mul, ptr %loop.slot, align 4
  br label %loop.end

if.end:                                         ; preds = %loop.body
  %i.cur2 = load i32, ptr %i.0.slot, align 4
  %add    = add i32 %i.cur2, 1
  store i32 %add, ptr %i.0.slot, align 4
  br label %loop.body

loop.end:                                       ; preds = %if.then
  %loop.val = load i32, ptr %loop.slot, align 4
  ret i32 %loop.val
}
```

Block names are inkwell-suffix-disambiguated in real output; the
shape is what matters.

## Worked example — `for` summing with `continue`

```rust
fn sum_evens_to(n: i32) -> i32 {
    let mut s = 0;
    for (let mut i = 0; i < n; i = i + 1) {
        if i % 2 != 0 { continue; }
        s = s + i;
    }
    s
}
```

After lowering + typeck (spans elided):

```text
sum_evens_to body:
  Let s = 0                                        : ()
  Loop source: For, has_break: false               : ()
      init   = Let i = 0                           : ()
      cond   = i < n                               : bool
      update = Assign(i, +, 1)                     : ()
      body   =
          If(cond= i % 2 != 0,
             then= { Continue }                    : !
             else= None)                           : ()
          Assign(s, +, i)                          : ()
  s                                                : i32
```

The `Loop` node carries `source: LoopSource::For` (this came from a
C-style `for`) and `has_break: false` (no `break` reaches this loop's
frame; the `Continue` doesn't count). Typeck's structural rule sees
`cond.is_some()` and types the loop as `()`.

Codegen produces the `header / body / update / end` shape; `Continue`
inside the `if.then` branches to `update` (not `header` — matching C).
The body's tail item `Assign(s, +, i)` is unreachable in iterations
where `continue` fires; its basic block has no predecessor on those
edges, but the surrounding fall-through still produces a valid CFG.

### Contrast — `loop { break v }` after lowering

The same `Loop` HIR node, different optional slots:

```text
first_match body:
  Let i = 0                                        : ()
  Loop source: Loop, has_break: true               : i32
      init   = ∅
      cond   = ∅
      update = ∅
      body   =
          If(cond= i == 7,
             then= { Break { expr: Some(i * 2) } } : !
             else= None)                           : ()
          Assign(i, +, 1)                          : ()
```

Typeck's structural rule sees `cond.is_none() && has_break` and
allocates a fresh infer var for the loop's value type. The
`Break { expr: Some(i * 2) }`'s `expr` type (`i32`) coerces into that var,
binding the loop expression to `i32`. The `source: Loop` tag has no
effect on typing; it's there only for HIR pretty-print and any
diagnostic that wants to render "loop" in its message.

## Out of scope (this round)

- Iterator `for pat in iter { ... }` — blocked on traits.
- Labels (`'outer: loop { ... break 'outer val; }`). Without labels,
  `break` and `continue` always target the innermost loop, which
  covers every realistic single-fn use case we have today.
- `while let pat = expr { ... }` — needs patterns.
- Cond struct-literal disambiguation (`while Foo { ... } { ... }`) —
  inherits `if`'s TBD from `08_ADT.md`.
- Loop over an iterator-shaped value (`for x in 0..10`). Until we
  have ranges and `IntoIterator`, this is exactly what the C-style
  `for` is for.

## Errors summary

| Code | Variant | Layer |
|---|---|---|
| E0263 | `HirError::BreakOutsideLoop` | HIR |
| E0264 | `HirError::ContinueOutsideLoop` | HIR |

Existing `TypeError::TypeMismatch` (E0250) covers cond-not-bool and
break-value-mismatch — no new typeck error variant needed.

## What this unblocks

Once loops land, the realistic Oxide program surface widens
substantially. Imperative algorithms (sums, counters, table lookups,
state machines) become writable directly instead of via recursion-
under-tail-call-pressure. Combined with the existing array work
(`09_ARRAY.md`), the language is now expressive enough to write a
self-contained mini-interpreter, sort routine, or buffer scan loop
without C glue.
