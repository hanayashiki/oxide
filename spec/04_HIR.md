# HIR Spec

The HIR (High-level IR) is the AST with **name resolution applied** and
**place-ness cached**: every `Ident` use in value position becomes a
typed-index handle (`LocalId` / `FnId`) and every type-position name
that refers to a user-defined ADT becomes an `HAdtId`. Primitive type
names stay syntactic (`HirTyKind::Named("i32")`) — real type derivation
is typeck's job.

Requirements: single-file module scope, resolve block-scoped `let`,
function-scoped parameters, module-scoped functions, and module-scoped
ADTs (struct).

Acceptance:

```
fn add(a: i32, b: i32) { a +     b }
//                       ^ Local(0)  ^ Local(1)
```

## Goals

- **Resolve names.**
  - Value namespace: locals (`let` bindings, fn params) → `LocalId`,
    module-level fns → `FnId`. Miss → `Unresolved` + `E0201`.
  - Type namespace: user-defined ADTs (struct/enum/union) → `HAdtId`.
    Miss in struct-literal position → `UnresolvedAdt` + `E0206`.
    Miss in type position → carried as `HirTyKind::Named(_)` and
    surfaced by typeck (`UnknownType` E0251).
- **Cache place-ness.** Every `HirExpr` carries an `is_place: bool`
  flag derived structurally at lower time. Used by typeck (mutability
  walk) and by the HIR errors `InvalidAssignTarget` / `AddrOfNonPlace`.
- **Recoverable.** Name lookup failures don't poison the tree. The HIR
  is always a (possibly partial) valid representation typeck can walk.
- **Same arena style as AST** — `IndexVec<Id, T>` per kind, owned by
  a single `HirModule`.
- **Multi-pass internally.** A pre-scan registers all module-level
  names (fns + ADTs) so forward references work in both namespaces; a
  second pass fills in ADT field types; a final pass lowers fn bodies.
  The external entry point remains a single `lower(&ast::Module)` call.

## What HIR is *not* responsible for

- **Type derivation.** HIR doesn't know that `"i32"` is a primitive.
  It carries `HirTyKind::Named("i32")` and lets typeck resolve. Pointer,
  array, and ADT type *constructors* are recognised structurally (because
  the parser already distinguishes them syntactically) but their
  components are still purely syntactic.
- **Type interning.** The hash-cons `TyArena` / `ConstArena` lives in
  `src/typeck/`. HIR has no concept of `TyId` or `ConstId`.
- **Primitive-name errors.** Unknown type-position names like `"blarg"`
  are *not* HIR errors — typeck catches them via `UnknownType` (E0251).
- **Mutability and field-set checks.** Whether `s.f = v` is allowed
  (mutability) and whether `f` exists on `s` (field set) are typeck's
  jobs. HIR only carries the `name: String` and the resolved `aid` for
  struct literals; field access is `Field { base, name }` with the
  string preserved.

## Data shape

```rust
use index_vec::IndexVec;
use crate::lexer::Span;
use crate::parser::ast::{UnOp, BinOp, AssignOp, Mutability};

index_vec::define_index_type! { pub struct FnId       = u32; }
index_vec::define_index_type! { pub struct LocalId    = u32; }
index_vec::define_index_type! { pub struct HExprId    = u32; }
index_vec::define_index_type! { pub struct HBlockId   = u32; }
index_vec::define_index_type! { pub struct HAdtId     = u32; }
index_vec::define_index_type! { pub struct VariantIdx = u32; }
index_vec::define_index_type! { pub struct FieldIdx   = u32; }

pub struct HirModule {
    pub fns:       IndexVec<FnId,     HirFn>,
    pub adts:      IndexVec<HAdtId,   HirAdt>,
    pub locals:    IndexVec<LocalId,  HirLocal>,
    pub exprs:     IndexVec<HExprId,  HirExpr>,
    pub blocks:    IndexVec<HBlockId, HirBlock>,
    pub root_fns:  Vec<FnId>,            // module-order
    pub root_adts: Vec<HAdtId>,
    pub span: Span,
}

/// Algebraic data type definition. v0 is record-struct only; the
/// variants-list shape is rustc-style umbrella so enums and unions
/// fit by adding variants/AdtKind without reshaping. See spec/08_ADT.md.
pub struct HirAdt {
    pub name: String,
    pub kind: AdtKind,
    pub variants: IndexVec<VariantIdx, HirVariant>,
    pub span: Span,
}

pub enum AdtKind { Struct /* Enum, Union — future */ }

pub struct HirVariant {
    /// `None` for the implicit unnamed variant of a struct.
    pub name: Option<String>,
    pub fields: IndexVec<FieldIdx, HirField>,
    pub span: Span,
}

pub struct HirField { pub name: String, pub ty: HirTy, pub span: Span }

pub struct HirFn {
    pub name: String,
    pub params: Vec<LocalId>,            // params are locals; order matters
    /// `None` when source omits `-> T`. Typeck defaults to unit.
    pub ret_ty: Option<HirTy>,
    /// `Some(_)` for defined fns; `None` for foreign fns declared in
    /// an `extern "C"` block. Correlated with `is_extern` today, kept
    /// distinct so future no-body cases (trait method defaults, etc.)
    /// don't require a refactor.
    pub body: Option<HBlockId>,
    pub is_extern: bool,
    pub span: Span,
}

pub struct HirLocal {
    pub name: String,
    pub mutable: bool,                   // see spec/11_MUTABILITY.md
    /// `None` ⇒ no annotation in source; typeck creates an inference var.
    pub ty: Option<HirTy>,
    pub span: Span,
}

pub struct HirBlock {
    /// Items in source order. The block's *value* comes from the last
    /// item if `has_semi == false`; otherwise the block has type `()`.
    /// Mid-block items with `has_semi == false` are validated by typeck
    /// (must coerce to `()` or `!`).
    pub items: Vec<HBlockItem>,
    pub span: Span,
}

pub struct HBlockItem { pub expr: HExprId, pub has_semi: bool }

pub struct HirExpr {
    pub kind: HirExprKind,
    pub span: Span,
    /// Place-ness derived structurally at lower time:
    ///   - `Local(_)` → place
    ///   - `Field { base, .. }` → place iff `base` is place
    ///   - `Unresolved(_) | Poison` → place (suppress cascading errors)
    ///   - everything else → not place
    /// `Unary { Deref, .. }` and `Index { .. }` will gain producer /
    /// projection arms when their feature specs land. See spec/08_ADT.md
    /// "Place expressions and `is_place`" and spec/11_MUTABILITY.md.
    pub is_place: bool,
}

pub enum HirExprKind {
    IntLit(u64),                              // typed by typeck (default i32)
    BoolLit(bool),
    CharLit(u8),                              // C-style: a byte
    StrLit(String),                           // typeck: `*const [u8; N]`, N = bytes + 1 (NUL-terminated)
    Local(LocalId),                           // resolved use of a let/param
    Fn(FnId),                                 // resolved use of a fn name
    Unresolved(String),                       // value-name lookup failed
    Unary    { op: UnOp,        expr: HExprId },
    Binary   { op: BinOp,       lhs: HExprId, rhs: HExprId },
    Assign   { op: AssignOp,    target: HExprId, rhs: HExprId },
    Call     { callee: HExprId, args: Vec<HExprId> },
    Index    { base: HExprId,   index: HExprId },
    Field    { base: HExprId,   name: String },         // typeck resolves `name`
    StructLit{ adt: HAdtId,     fields: Vec<HirStructLitField> },
    AddrOf   { mutability: Mutability, expr: HExprId }, // see spec/10_ADDRESS_OF.md
    ArrayLit (HirArrayLit),                             // see spec/09_ARRAY.md
    Cast     { expr: HExprId,   ty: HirTy },
    If       { cond: HExprId,   then_block: HBlockId, else_arm: Option<HElseArm> },
    Block    (HBlockId),
    Return   (Option<HExprId>),                         // type `!`
    Let      { local: LocalId,  init: Option<HExprId> }, // type `()`
    Poison,                                              // recovery placeholder
}

pub enum HElseArm { Block(HBlockId), If(HExprId) }

pub struct HirStructLitField { pub name: String, pub value: HExprId, pub span: Span }

/// Array literal — element list or repeat-with-length form. See spec/09_ARRAY.md.
pub enum HirArrayLit {
    Elems(Vec<HExprId>),                      // `[a, b, c]`
    Repeat { init: HExprId, len: HirConst },  // `[init; N]`
}

/// Type-level constant value extracted at HIR-lower time. v0 carries
/// only `Lit(u64)` (from a bare `IntLit`) or `Error`; future const-
/// generics work adds variants without reshaping callers.
pub enum HirConst { Lit(u64), Error }

pub struct HirTy { pub kind: HirTyKind, pub span: Span }

pub enum HirTyKind {
    /// Type-position name as written. Typeck resolves it (primitives
    /// or `UnknownType` E0251).
    Named(String),
    /// Resolved use of a user-defined ADT.
    Adt(HAdtId),
    /// `*const T` / `*mut T`.
    Ptr { mutability: Mutability, pointee: Box<HirTy> },
    /// `[T; N]` (sized) / `[T]` (unsized). See spec/09_ARRAY.md.
    Array(Box<HirTy>, Option<HirConst>),
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
- **Type-namespace names split into two** — `HirTyKind::Adt(HAdtId)`
  for resolved user-defined types; `HirTyKind::Named(_)` for everything
  else (including primitives, which typeck resolves later).
- **`CharLit(char)` → `CharLit(u8)`** — char literals are bytes
  (C-style). Out-of-range chars (`'😀'`) lower to `Poison` with `E0203`.
- **Place-ness is cached** — `HirExpr.is_place` is computed at lower
  time, not re-derived per lookup.

## Lowering algorithm

```rust
pub fn lower(ast: &Module) -> (HirModule, Vec<HirError>) {
    // Pass 1 — prescan all module-level items.
    //   ItemKind::Fn          → allocate FnId,   register in value scope
    //   ItemKind::ExternBlock → allocate FnIds for each child decl,
    //                           register names; mark `is_extern = true`,
    //                           `body = None`
    //   ItemKind::Struct      → allocate HAdtId, register in type scope
    //   Push stub HirFn / HirAdt entries; bodies and field types are
    //   filled in later passes. Duplicates → DuplicateFn / DuplicateAdt.
    //
    // Pass 2 — resolve ADT field types.
    //   For each HirAdt: resolve each field's HirTy. Unknown user-type
    //   names stay as HirTyKind::Named(_) for typeck. Duplicate field
    //   names → DuplicateField.
    //
    // Pass 3 — lower fn bodies.
    //   Walk each fn body, lowering expressions and resolving names
    //   against the scope stack. Compute `is_place` per expression.
}
```

Value-namespace resolution rules:

1. **Lookup is innermost-first across `scopes`** (LIFO stack of block
   scopes). First hit wins → `Local(LocalId)`.
2. On block-scope miss, check `module_scope` → `Fn(FnId)`.
3. On final miss, push `HirError::UnresolvedName` and produce
   `HirExprKind::Unresolved(name)`.

Type-namespace resolution rules:

1. In a `TypeKind::Named(ident)` position, look up `ident` in the
   module's type scope. Hit → `HirTyKind::Adt(haid)`. Miss →
   `HirTyKind::Named(name)` (typeck distinguishes "primitive" vs
   "unknown" via `UnknownType` E0251).
2. In a struct-literal `Ident { f: v, ... }` position, the type must
   resolve at HIR. Miss → `HirError::UnresolvedAdt` + the literal
   lowers to `Poison`.

Scope discipline:

- Entering a block: `scopes.push(HashMap::new())`. Leaving: pop.
- Function parameters are pushed into a scope opened *before* the body
  block, so the body-block scope nests inside the param scope.
- `let x = init`: lower `init` first, *then* push the binding into the
  current scope. This matches Rust: `let x = x;` reads the outer `x`,
  not the binding being introduced.

Place validation:

- LHS of `Assign` whose `target.is_place == false` → `InvalidAssignTarget`.
- Operand of `&` / `&mut` whose `expr.is_place == false` → `AddrOfNonPlace`.
- The expression is still lowered structurally so cascading errors
  remain suppressed (`Unresolved`/`Poison` are place-treated for this
  reason).

## Error model

```rust
pub enum HirError {
    UnresolvedName      { name: String, span: Span },                              // E0201
    DuplicateFn         { name: String, first: Span, dup: Span },                  // E0202
    CharOutOfRange      { ch: char, span: Span },                                  // E0203
    DuplicateAdt        { name: String, first: Span, dup: Span },                  // E0204
    DuplicateField      { adt: String, name: String, first: Span, dup: Span },     // E0205
    UnresolvedAdt       { name: String, span: Span },                              // E0206
    InvalidAssignTarget { span: Span },                                            // E0207
    AddrOfNonPlace      { span: Span },                                            // E0208
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
    HExprId(0) → Local(LocalId(0))   { is_place: true  },
    HExprId(1) → Local(LocalId(1))   { is_place: true  },
    HExprId(2) → Binary { op: Add, lhs: HExprId(0), rhs: HExprId(1) }
                                      { is_place: false },
]
blocks = [
    HBlockId(0) → HirBlock {
        items: [HBlockItem { expr: HExprId(2), has_semi: false }],
    },
]
fns = [
    FnId(0) → HirFn {
        name: "add",
        params: [LocalId(0), LocalId(1)],
        ret_ty: None,                          // source omitted `-> T`
        body: Some(HBlockId(0)),
        is_extern: false,
    },
]
root_fns = [FnId(0)]
```

The `Ident("a")`/`Ident("b")` from AST resolved to
`Local(LocalId(0))` / `Local(LocalId(1))`. Type-position `i32`s pass
through as `Named("i32")` for typeck to interpret.

## Out of scope (v0)

- **Multi-file modules** / cross-module name resolution.
- **Pattern bindings** — `let` only binds a single identifier.
- **Closure captures** — no closures.
- **Visibility, traits, generics, lifetimes.**
- **`Unary { Deref, .. }` / `Index { .. }` as place producers** —
  gated on the deferred deref work (spec/07_POINTER.md) and array
  index work (spec/09_ARRAY.md "Phase A Step 4/5"). Today both arms
  produce `is_place == false`.
