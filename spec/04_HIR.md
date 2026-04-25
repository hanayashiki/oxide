# HIR Spec

The HIR (High-level IR) is the AST with **value-namespace name resolution
applied**: every `Ident` use becomes a typed-index handle into a
flat-arena module. Types stay syntactic — just primitive name strings —
because real type derivation is typeck's job.

Requirements: single file scope, resolve block-scoped `let`,
function-scoped parameters, and module-scoped functions.

Acceptance:

```
fn add(a: i32, b: i32) { a +     b }
//                       ^ Local(0)  ^ Local(1)
```

## Goals

- **One job**: resolve value names. Locals (`let` bindings, fn params)
  → `LocalId`. Module-level functions → `FnId`. Misses → `Unresolved`
  with an `E0201` diagnostic.
- **Recoverable**: name lookup failures don't poison the tree. The HIR
  is always a (possibly partial) valid representation typeck can walk.
- **Same arena style as AST** — `IndexVec<Id, T>` per kind, owned by a
  single `HirModule`.
- **Single AST traversal**: one external call (`lower(&ast::Module)`)
  produces a `HirModule`. Internally a brief pre-scan registers fn names
  so functions can be forward-referenced.

## What HIR is *not* responsible for

- **Type derivation**. HIR doesn't know that `"i32"` is a primitive.
  It carries `HirTyKind::Named("i32")` and lets typeck resolve.
- **Type interning**. The hash-cons `TyArena` lives in
  `src/typeck/`. HIR has no concept of `TyId`.
- **Type errors**. Unknown type-position names like `"blarg"` are *not*
  HIR errors — typeck catches them when it tries to resolve to a real
  type. HIR's only type-related obligation is to faithfully carry the
  written name.

## Data shape

```rust
use index_vec::IndexVec;
use crate::lexer::Span;
use crate::parser::ast::{UnOp, BinOp, AssignOp};   // reused verbatim

index_vec::define_index_type! { pub struct FnId     = u32; }
index_vec::define_index_type! { pub struct LocalId  = u32; }
index_vec::define_index_type! { pub struct HExprId  = u32; }
index_vec::define_index_type! { pub struct HBlockId = u32; }

pub struct HirModule {
    pub fns:    IndexVec<FnId,    HirFn>,
    pub locals: IndexVec<LocalId, HirLocal>,
    pub exprs:  IndexVec<HExprId, HirExpr>,
    pub blocks: IndexVec<HBlockId, HirBlock>,
    pub root_fns: Vec<FnId>,             // module-order
    pub span: Span,
}

pub struct HirFn {
    pub name: String,
    pub params: Vec<LocalId>,            // params are locals; order matters
    /// `None` when source omits `-> T`. Typeck defaults to unit.
    pub ret_ty: Option<HirTy>,
    pub body: HBlockId,
    pub span: Span,
}

pub struct HirLocal {
    pub name: String,
    pub mutable: bool,
    /// `None` ⇒ no annotation in source; typeck creates an inference var.
    pub ty: Option<HirTy>,
    pub span: Span,
}

pub struct HirBlock {
    pub items: Vec<HExprId>,             // evaluated in order; values discarded
    pub tail:  Option<HExprId>,          // optional value-producing expression
    pub span:  Span,
}

pub struct HirExpr { pub kind: HirExprKind, pub span: Span }

pub enum HirExprKind {
    IntLit(u64),                          // typed by typeck (default i32)
    BoolLit(bool),
    CharLit(u8),                          // C-style: a byte
    StrLit(String),                       // typeck rejects in v0 (no string type yet)
    Local(LocalId),                       // resolved use of a let/param
    Fn(FnId),                             // resolved use of a fn name
    Unresolved(String),                   // name lookup failed; preserved for diagnostics
    Unary  { op: UnOp,     expr: HExprId },
    Binary { op: BinOp,    lhs: HExprId, rhs: HExprId },
    Assign { op: AssignOp, target: HExprId, rhs: HExprId },
    Call   { callee: HExprId, args: Vec<HExprId> },
    Index  { base: HExprId, index: HExprId },
    Field  { base: HExprId, name: String },
    Cast   { expr: HExprId, ty: HirTy },
    If     { cond: HExprId, then_block: HBlockId, else_arm: Option<HElseArm> },
    Block  (HBlockId),
    Return (Option<HExprId>),             // type `!` (assigned by typeck)
    Let    { local: LocalId, init: Option<HExprId> },   // type `()`
    Poison,                               // recovery placeholder
}

pub enum HElseArm { Block(HBlockId), If(HExprId) }

pub struct HirTy { pub kind: HirTyKind, pub span: Span }

pub enum HirTyKind {
    /// Type-position name as written in source. Typeck resolves it.
    Named(String),
    /// Recovery placeholder for malformed type positions.
    Error,
}
```

### Notable simplifications vs AST

- **`Paren` is gone** — `(e)` has no semantic content; we drop the
  wrapper and reuse the inner `HExprId`.
- **`Ident(Ident)` splits into three** — `Local(LocalId)` for resolved
  bindings, `Fn(FnId)` for resolved functions, `Unresolved(String)` on
  miss (with an `E0201` filed alongside).
- **`CharLit(char)` → `CharLit(u8)`** — char literals are bytes
  (C-style). Out-of-range chars (`'😀'`) lower to `Poison` with `E0203`.
- **Type names pass through unchanged** — `TypeKind::Named(ident)` from
  AST → `HirTyKind::Named(ident.name)` in HIR. Typeck does the
  primitive-name lookup.

## Lowering algorithm

```rust
pub fn lower(ast: &Module) -> (HirModule, Vec<HirError>) {
    // Pass 1: prescan module items, allocate FnIds, populate
    //         module-level scope so fns can be forward-referenced.
    //         Push stub HirFn entries; bodies are filled in pass 2.
    // Pass 2: walk each fn body, lowering expressions and resolving
    //         names against the scope stack.
}
```

Resolution rules:

1. **Lookup is innermost-first across `scopes`** (the LIFO stack of
   block scopes). First hit wins → `Local(LocalId)`.
2. On block-scope miss, check `module_scope` → `Fn(FnId)`.
3. On final miss, push `HirError::UnresolvedName` and produce
   `HirExprKind::Unresolved(name)`.

Scope discipline:

- Entering a block: `scopes.push(HashMap::new())`. Leaving: pop.
- Function parameters are pushed into a scope opened *before* the body
  block, so the body-block scope nests inside the param scope.
- `let x = init`: lower `init` first, *then* push the binding into the
  current scope. This matches Rust: `let x = x;` reads the outer `x`,
  not the binding being introduced.

## Error model

```rust
pub enum HirError {
    UnresolvedName  { name: String, span: Span },                       // E0201
    DuplicateFn     { name: String, first: Span, dup: Span },           // E0202
    CharOutOfRange  { ch: char, span: Span },                           // E0203
}
```

Code namespace: HIR owns **E0201–E0249**; typeck owns E0250+.

Conversion to `Diagnostic` lives in `src/reporter/from_hir.rs`,
mirroring `from_lex.rs` and `from_parse.rs`. Re-exported as
`reporter::from_hir_error`.

## Public API

```rust
// src/hir/mod.rs
pub fn lower(module: &crate::parser::ast::Module) -> (HirModule, Vec<HirError>);
```

Always returns a (possibly partial) `HirModule` plus collected errors.

## Worked example

`fn add(a: i32, b: i32) { a + b }` lowers to:

```text
locals = [
    LocalId(0) → HirLocal { name: "a", mutable: false, ty: Some(Named("i32")) },
    LocalId(1) → HirLocal { name: "b", mutable: false, ty: Some(Named("i32")) },
]
exprs = [
    HExprId(0) → Local(LocalId(0)),
    HExprId(1) → Local(LocalId(1)),
    HExprId(2) → Binary { op: Add, lhs: HExprId(0), rhs: HExprId(1) },
]
blocks = [
    HBlockId(0) → HirBlock { items: [], tail: Some(HExprId(2)) },
]
fns = [
    FnId(0) → HirFn {
        name: "add",
        params: [LocalId(0), LocalId(1)],
        ret_ty: None,                    // source omitted `-> T`
        body: HBlockId(0),
    },
]
root_fns = [FnId(0)]
```

The `Ident("a")`/`Ident("b")` from AST resolved to
`Local(LocalId(0))` / `Local(LocalId(1))`. Type-position `i32`s pass
through as `Named("i32")` for typeck to interpret.

## Out of scope (v0)

- **Type-namespace resolution.** `HirTy` is just a syntactic name —
  there is no `Local(LocalId)`-style "resolved" form for types in HIR.
  Typeck's primitive table handles `i32`/`bool`/etc. directly; once
  user-defined types (struct/enum) land we'll need a type-namespace
  prescan analogous to the fn prescan, likely living in typeck rather
  than HIR.
- Multi-file modules / cross-module name resolution.
- Pattern bindings — `let` only binds a single identifier.
- Closure captures (no closures yet).
- Visibility, traits, generics, lifetimes.
