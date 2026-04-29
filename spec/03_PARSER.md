# Parser Spec

The parser turns the lexer's `Vec<Token>` into a typed AST. It is recoverable:
the public API always returns a `Module` (possibly with empty arenas) plus a
list of `ParseError`s. There is no "failed to parse" case.

The parser is built on `chumsky`. We get Pratt operator precedence, `Rich`
errors, and bracket-balanced recovery from the library; we add a small
`ModuleBuilder` parser state so that combinators emit *arena indices* rather
than owned tree nodes.

## Goals

- Recoverable: a syntax error inside one function does not prevent parsing the
  next function. Errors are collected, the parser keeps moving.
- Spans on every node, reusing `lexer::Span` (byte + LSP). No parser-local
  span type.
- Stable identity for every major node, so future passes (typeck, IR
  lowering, LSP) can hang side-tables off it.
- Smallest viable target program: `fn add(a: i32, b: i32) { a + b }`.

## v0 scope

- **Items**: `fn` only.
- **Statements**: `let [mut] name [: Ty] [= expr];`, expression statement,
  `return [expr];`, `if/else` as a statement, block as a statement.
- **Expressions**: int / bool / char / str literals, identifier,
  parenthesized, unary (`- ! ~`), binary (arithmetic, comparison, logical,
  bitwise, shift), `as` cast, call, indexing, field access, assignment,
  `if/else` as expression, block as expression. Blocks support a **trailing
  tail expression**, so `{ a + b }` evaluates to `a + b`.
- **Types**: named only (`Ident`, e.g. `i32`, `bool`, `void`).

Anything outside this list is in *Out of scope (v0)* below.

## AST

The AST is **arena-allocated** using `index_vec::IndexVec` with typed-index
newtypes. The `Module` owns one arena per major node kind; tree edges are
typed indices (`ExprId`, `BlockId`, …) instead of `Box<T>`. Every node still
carries `span: Span`, but the typed index *is* the node's identity.

Why arenas-with-indices:

- Cheap `Copy + Eq + Hash` handles for cross-references.
- Dense side-tables for typeck (`IndexVec<ExprId, Ty>`) — no hashing.
- Natural cross-module handles `(ModuleId, ExprId)` when modules land.
- Settles "how do later passes address AST nodes" without needing a separate
  HIR for v0.

Cost: every traversal needs `&Module` in scope to dereference IDs. Acceptable
for v0; the storage shape is what production compilers (rustc, rust-analyzer)
converge on anyway.

```rust
use index_vec::IndexVec;

index_vec::define_index_type! { pub struct ItemId  = u32; }
index_vec::define_index_type! { pub struct ExprId  = u32; }
index_vec::define_index_type! { pub struct BlockId = u32; }
index_vec::define_index_type! { pub struct TypeId  = u32; }

pub struct Module {
    pub items:  IndexVec<ItemId,  Item>,
    pub exprs:  IndexVec<ExprId,  Expr>,
    pub blocks: IndexVec<BlockId, Block>,
    pub types:  IndexVec<TypeId,  Type>,
    pub root_items: Vec<ItemId>,           // top-level items in source order
    pub span: Span,
}

pub struct Item   { pub kind: ItemKind, pub span: Span }
pub enum   ItemKind { Fn(FnDecl) }
pub struct FnDecl {
    pub name: Ident,
    pub params: Vec<Param>,
    pub ret_ty: Option<TypeId>,            // None ⇒ unit
    pub body: BlockId,
}
pub struct Param  { pub mutable: bool, pub name: Ident, pub ty: TypeId, pub span: Span }
pub struct Ident  { pub name: String, pub span: Span }

pub struct Block {
    pub items: Vec<ExprId>,                // evaluated in order; values discarded
    pub tail:  Option<ExprId>,             // optional value-producing expression
    pub span:  Span,
}

pub struct Expr { pub kind: ExprKind, pub span: Span }
pub enum ExprKind {
    IntLit  (u64),
    BoolLit (bool),
    CharLit (char),
    StrLit  (String),
    Ident   (Ident),
    Paren   (ExprId),
    Unary   { op: UnOp,     expr: ExprId },
    Binary  { op: BinOp,    lhs: ExprId, rhs: ExprId },
    Assign  { op: AssignOp, lhs: ExprId, rhs: ExprId },
    Call    { callee: ExprId, args: Vec<ExprId> },
    Index   { base: ExprId, index: ExprId },
    Field   { base: ExprId, name: Ident },
    Cast    { expr: ExprId, ty: TypeId },
    If      { cond: ExprId, then_block: BlockId, else_arm: Option<ElseArm> },
    Block   (BlockId),
    Return  (Option<ExprId>),              // type `!`
    Let     { mutable: bool, name: Ident,
              ty: Option<TypeId>, init: Option<ExprId> },   // type `()`
    Poison,                                // produced on recovery
}

pub enum ElseArm {
    Block (BlockId),
    If    (ExprId),                        // ExprId whose kind is ExprKind::If
}

pub enum UnOp     { Neg, Not, BitNot }
pub enum BinOp    { Add, Sub, Mul, Div, Rem,
                    Eq, Ne, Lt, Le, Gt, Ge,
                    And, Or,
                    BitAnd, BitOr, BitXor, Shl, Shr }
pub enum AssignOp { Eq, Add, Sub, Mul, Div, Rem,
                    BitAnd, BitOr, BitXor, Shl, Shr }

pub struct Type     { pub kind: TypeKind, pub span: Span }
pub enum   TypeKind { Named(Ident) }       // v0: only named types
```

### No `Stmt` type

v0 has no `Stmt` enum. A `Block` holds a list of expressions evaluated in
order (`items: Vec<ExprId>`), plus an optional tail expression. `let`,
`if`, `{block}`, and `return` are all expression kinds. "Used as a
statement" is positional — being an item in a block — not a separate
variant.

This is closer to ML/OCaml-style ASTs than Rust's parse tree, but rustc's
HIR converges in the same direction. The advantage is uniformity:

- `let b: i32 = return 1;` parses cleanly. `return e` has type `!`, and
  `!` is a subtype of every type, so the binding is well-typed (it just
  never executes).
- `if`/`{block}` need no statement-form duplicate variants. The pretty
  printer and downstream passes dispatch on `ExprKind`.

The grammar (not the AST) restricts `let` to block-item position. `1 + let
x = 5` is a parse error because `expr_parser` doesn't include `let_form`.

### What's arena'd vs inline

- **Arena'd**: `Item`, `Expr`, `Block`, `Type`. These need stable handles
  for typeck side-tables, cross-module refs, and IR lowering.
- **Inline**: `Param`, `Ident`. `Param` lives only inside
  `FnDecl::params: Vec<Param>` and is never referenced from elsewhere.
  `Ident` is a trivial `String + Span`; later, when name resolution lands,
  identifier *uses* will get their own resolved handle (`LocalId`/`DefId`)
  produced by the resolver — interning `Ident` itself in the parser is
  premature.
- `IfExpr` is **not** a separate type. `If` is one node kind on `Expr`.
  `else if` chains stay uniform via `ElseArm::If(ExprId)` pointing at an
  `If`-kinded `Expr`.

### Spans & identity

Every node carries a `lexer::Span`, computed as the union of the first and
last consumed token spans. **Identity is the typed-index handle** issued
when the node is pushed into its arena (e.g. `ExprId`); nothing else (no
`NodeId`, no pointer identity, no span-as-key). Reuse `lexer::Span` — do
not redefine.

## Grammar

The grammar below is informal EBNF; the chumsky combinators are the source
of truth. Italicized non-terminals (`*Expr*`) refer to AST nodes via their
`*Id*` handle.

```
Module     ::= Item*
Item       ::= FnDecl
FnDecl     ::= 'fn' Ident '(' (Param (',' Param)*)? ','? ')' ('->' Type)? Block
Param      ::= 'mut'? Ident ':' Type
Type       ::= Ident                                  // v0: named only

Block      ::= '{' BlockItem* Expr? '}'               // optional tail expr
BlockItem  ::= LetItem
             | IfExpr                                 // no trailing ';'
             | BlockExpr                              // no trailing ';'
             | Expr ';'
LetItem    ::= 'let' 'mut'? Ident (':' Type)? ('=' Expr)? ';'
IfExpr     ::= 'if' Expr Block ('else' (Block | IfExpr))?
BlockExpr  ::= '{' BlockItem* Expr? '}'
ReturnExpr ::= 'return' Expr?                         // an expression of type `!`
```

`return` is parseable in any expression position — it's at the top of the
expression parser, ahead of the Pratt machinery. So `let b: i32 = return 1;`,
`1 + return 2`, and `if cond { return 0 } else { 1 }` all work.

Block items are tried in this order (first match wins): `let_form`,
`if_form`, `block_form`, `expr ';'`. The first three need no trailing `;`;
the `expr ';'` fallback is for everything else.

### Operator precedence

Lower number = lower precedence. All binary ops are left-associative
**except** assignment, which is right-associative. Each row corresponds to
one rung of the Pratt builder.

| Lvl | Operators                                              | Notes                                                            |
|----:|--------------------------------------------------------|------------------------------------------------------------------|
|  1  | `=  +=  -=  *=  /=  %=  &=  \|=  ^=  <<=  >>=`         | right-assoc; LHS must be a place expression (validated post-parse) |
|  2  | `\|\|`                                                 |                                                                  |
|  3  | `&&`                                                   |                                                                  |
|  4  | `==  !=`                                               | chains accepted; typeck may later reject                         |
|  5  | `<  <=  >  >=`                                         |                                                                  |
|  6  | `\|`                                                   | bitwise OR                                                       |
|  7  | `^`                                                    | bitwise XOR                                                      |
|  8  | `&`                                                    | binary bitwise AND (no `&` prefix in v0)                         |
|  9  | `<<  >>`                                               |                                                                  |
| 10  | `+  -` (binary)                                        |                                                                  |
| 11  | `*  /  %`                                              | binary `*` (no deref in v0)                                      |
| 12  | `as`                                                   | postfix; `e as T as U` chains left                               |
| 13  | unary prefix: `-  !  ~`                                | right-assoc                                                      |
| 14  | postfix: `f(args)`, `e[i]`, `e.field`                  | left-assoc                                                       |
| 15  | atoms: literal, ident, `(expr)`, `{ block }`, `if … else …` |                                                             |

`*` and `&` are **only binary** in v0. There is no unary deref (`*p`),
addr-of (`&p`), or `&mut`. The reserved-keyword machinery (E0104) catches
`&mut` if it appears.

Place-expression check (for assignment LHS) is not enforced at parse
time; the parser accepts any expression on the LHS, and HIR lower
emits `InvalidAssignTarget` (E0207) when the lowered target's
`is_place` bit is false. See spec/08_ADT.md "Place expressions and
`is_place`".

## API

```rust
// src/parser/mod.rs
pub fn parse(tokens: &[Token]) -> (Module, Vec<ParseError>);
```

- Always returns a `Module`. Empty input ⇒ `Module` with empty arenas and
  `root_items = []`. All-error input does the same plus errors in the
  `Vec<ParseError>`.
- Callers walk top-level items via
  `module.root_items.iter().map(|id| &module.items[*id])`.
- Errors are independent of the tree: a partial AST and a non-empty error
  vector both occur on recovered parses.

## Errors

```rust
pub enum ParseError {
    UnexpectedToken     { expected: Vec<&'static str>, found: TokenKind, span: Span }, // E0101
    UnexpectedEof       { expected: Vec<&'static str>, span: Span },                   // E0102
    BadStatement        { span: Span },                                                // E0103
    ReservedKeyword     { kw: &'static str, span: Span },                              // E0104
    LexErrorToken       { err: LexError, span: Span },                                 // E0105
}
```

Code namespace: parser owns **E0101–E0199** (lexer owns E0001–E0008; typeck
will take E0201+). `expected` carries human labels (`"`)`"`, `"expression"`,
`"type"`) rather than `TokenKind`s so messages stay readable.

### Reserved keywords

`match`, `impl`, `trait`, `pub`, `use`, `mod` are lexed as keyword tokens but
have no syntactic role in v0. The parser emits `ReservedKeyword` (E0104)
when it sees one and recovers (skip-to-`;`/`}` at statement level, or scan
to next `fn`/EOF at item level). They become real syntax in a later
revision.

### Lexer errors entering the parser

`TokenKind::Error(_)` tokens from the lexer are filtered out of the chumsky
input stream by the input adapter and re-emitted as `LexErrorToken` (E0105).
The parser never sees them, which avoids cascade "unexpected token"
diagnostics. The actual user-facing message reuses `from_lex_error` so the
output is the same as if the lexer ran standalone.

### Conversion to `Diagnostic`

Conversion lives in `src/reporter/from_parse.rs`, mirroring `from_lex.rs`:

```rust
pub fn from_parse_error(err: &ParseError, file: FileId) -> Diagnostic;
```

The parser stays presentation-free, like the lexer.

## Recovery

Three recovery points, in increasing scope:

- **Bracket-balanced** — chumsky's `nested_delimiters` skips across
  balanced `()`, `[]`, `{}` when call-arg lists, indexing, or paren
  expressions fail. Substitute `Expr::Poison` and continue.
- **Block-item level** — on a block-item failure, skip tokens until the
  next `;` or `}` and resume the next item. Substitute an `ExprKind::Poison`
  node into the items list.
- **Item level** — at the top of `Module`, on `Item` failure scan to the
  next `fn` keyword or EOF; drop the bad item.

The load-bearing property: a syntax error inside one function does not
prevent parsing the next one. A dedicated test enforces this.

## chumsky integration

We use `chumsky = "0.10"` and `index_vec = "0.1"`. The combinators emit
arena indices rather than owned nodes; the arenas live in a `ModuleBuilder`
that chumsky carries as parser state.

### Parser state

```rust
pub struct ModuleBuilder {
    pub items:  IndexVec<ItemId,  Item>,
    pub exprs:  IndexVec<ExprId,  Expr>,
    pub blocks: IndexVec<BlockId, Block>,
    pub types:  IndexVec<TypeId,  Type>,
    pub errors: Vec<ParseError>,           // diverted lex errors land here
}
```

Wired in via `chumsky::extra::Full<Rich<…>, ModuleBuilder, ()>`. Inside any
combinator, `e.state_mut()` gives `&mut ModuleBuilder`.

### Parser type alias

```rust
type Extra<'src> = extra::Full<Rich<'src, &'src TokenKind, Span>, ModuleBuilder, ()>;
type P<'src, O>  = impl Parser<'src, TokenStream<'src>, O, Extra<'src>>;
```

(`TokenStream` is whatever concrete `Stream`-based input we end up with; the
exact name is implementation-detail.)

### Pushing nodes

The standard combinator shape is "parse the kind, push an `Expr`":

```rust
expr_kind_parser
    .map_with(|kind, e| -> ExprId {
        let span = e.span();
        e.state_mut().exprs.push(Expr { kind, span })
    })
```

`IndexVec::push` returns the freshly issued index, which becomes the
combinator's output. Statements, blocks, items, and types follow the same
pattern with their respective arenas.

### Pratt builder

Each row in §"Operator precedence" maps to one chumsky `infix`/`prefix`/
`postfix` rung. The fold closure receives the children's arena indices and
pushes the parent node into the `exprs` arena, returning the parent's
`ExprId` — so the Pratt builder produces an `ExprId` directly.

### Input adapter

```rust
let eoi: Span = tokens.last().unwrap().span.clone();
let stream = Stream::from_iter(
    tokens.iter()
        .filter_map(|t| match &t.kind {
            TokenKind::Eof          => None,             // chumsky models EOF itself
            TokenKind::Error(err)   => {
                builder.errors.push(ParseError::LexErrorToken {
                    err: err.clone(), span: t.span.clone(),
                });
                None
            }
            _ => Some((&t.kind, t.span.clone())),
        }),
).map(eoi.clone(), |(kind, span)| (kind, span));
```

The trailing `Eof` token's span is used as the end-of-input span, so
`UnexpectedEof` diagnostics point at the right location.

### Top-level flow

```rust
let mut builder = ModuleBuilder::new();
let result = module_parser.parse_with_state(&stream, &mut builder);
let (output, rich_errs) = result.into_output_errors();

let module = builder.finish(output);   // moves arenas + root_items into Module
let parse_errors = builder.errors      // diverted lex errors (E0105)
    .into_iter()
    .chain(rich_errs.into_iter().map(rich_to_parse_error))
    .collect();
(module, parse_errors)
```

## Worked example

Source:

```
fn add(a: i32, b: i32) { a + b }
```

Resulting `Module` (spans elided for readability):

```text
types  = [
    TypeId(0) → Named("i32"),
    TypeId(1) → Named("i32"),
]
exprs  = [
    ExprId(0) → Ident("a"),
    ExprId(1) → Ident("b"),
    ExprId(2) → Binary { op: Add, lhs: ExprId(0), rhs: ExprId(1) },
]
blocks = [
    BlockId(0) → Block { items: [], tail: Some(ExprId(2)) },
]
items  = [
    ItemId(0) → Item::Fn(FnDecl {
        name: "add",
        params: [
            Param { name: "a", ty: TypeId(0) },
            Param { name: "b", ty: TypeId(1) },
        ],
        ret_ty: None,
        body: BlockId(0),
    }),
]
root_items = [ItemId(0)]
```

Edge graph (FnDecl → Block → Expr):

```
ItemId(0) ─body──▶ BlockId(0) ─tail─▶ ExprId(2) ┬─lhs─▶ ExprId(0)  (Ident "a")
                                                └─rhs─▶ ExprId(1)  (Ident "b")
ItemId(0) ─params[0].ty─▶ TypeId(0)  (Named "i32")
ItemId(0) ─params[1].ty─▶ TypeId(1)  (Named "i32")
```

Every node also carries a `Span`; arenas are dense `IndexVec`s in
source-encounter order.

## Debug & pretty-printing

A derived `Debug` on `Module` produces a flat dump of every arena
(`exprs = [...], items = [...], ...`), which is unreadable for non-trivial
programs — to follow the tree you'd have to chase IDs across arenas by
hand.

For snapshot tests and the example CLI we expose a tree-shaped pretty-printer
that walks from `root_items` and resolves IDs inline:

```rust
// src/parser/pretty.rs
pub fn pretty_print(module: &Module) -> String;
```

Output for the §Worked-example program:

```
Module
  Fn add(a: i32, b: i32)
    Block
      tail: Binary Add
        Ident "a"
        Ident "b"
```

Spans are omitted by default (toggleable via a future `pretty_print_with`
variant if needed). `Debug` is left untouched on the AST types so it remains
available for parser-internal debugging when you do want the raw arena
layout.

## Testing

Tests are not part of v0 spec implementation, but the strategy is:

- **Unit tests** in `src/parser/parse.rs` (`#[cfg(test)] mod tests`) for
  individual combinators: literals, identifiers, simple precedence
  (`a + b * c` parses as `a + (b * c)`), `as` chains, paren grouping.
  Assert on `ExprKind` after looking up the returned `ExprId`.
- **Integration tests** in `tests/parser.rs` for full programs (the worked
  example, `let`/`return`/`if`-chains, recovery scenarios).
- **Snapshot tests** via `expect_test`, mirroring `tests/reporter.rs`:
  `expect![[...]]` over `pretty_print(&module)` for AST shape (not raw
  `{:#?}` — see "Debug & pretty-printing" above), and over rendered
  diagnostics (parse → `from_parse_error` → `emit`) for user-visible error
  output.
- **Recovery tests** that assert: a deliberate error inside one function
  does not prevent parsing a *second* well-formed function. This is the
  load-bearing property and the easiest way for recovery to silently
  regress.
- **Lex-error passthrough test**: feeding `let x = 'ab';` (a lexer
  `BadEscape`) yields exactly one E0006 diagnostic and lets the rest of
  the program parse.

## Out of scope (v0)

- Structs, enums.
- Pointers: `*T` types, unary `*` deref, unary `&` / `&mut` addr-of, `null`,
  `sizeof`.
- Loops: `while`, `for`, `break`, `continue`.
- Generics, traits / `impl`, modules / `use`, macros, pattern matching,
  lifetimes, attributes, visibility (`pub`).
- Float literals, suffix-typed integer literals (`123u32`).
- Array types `[T; N]` and tuple types/expressions.
- Identifier interning, `IdentId` arena.
- Reserved keywords (`match impl trait pub use mod`) parse-error as E0104
  until they get real syntax.
