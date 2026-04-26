# ADT (Algebraic Data Types)

## Requirements

We currently have only primitive types and pointers. To enable more
complex programs (multi-return, grouped state) and — eventually — FFI
struct interop, we add **algebraic data types**.

v0 covers only **record structs**:

```rust
struct Point { x: i32, y: i32 }
```

The `Adt` umbrella name is chosen for forward compatibility — `enum`,
`union`, tuple structs, and unit structs all slot in by adding
`AdtKind` variants and/or grammar rules without reshaping the data
model. Today the only `AdtKind` is `Struct`.

This iteration covers **AST → HIR only**. Typeck and codegen for
ADTs are deferred (see *TBDs* below).

## Subset-of-Rust constraint

Anything we accept must also parse in Rust with the same meaning.
The grammar below is a strict subset of Rust's struct/struct-literal
grammar — we don't add anything Rust doesn't have, and we don't
diverge in semantics from Rust's interpretation of the syntax we do
accept. Out-of-scope features (`pub`, shorthand, `..rest`, tuple
structs, unit structs) are *omissions*, not deviations.

## Acceptance

```rust
struct Point { x: i32, y: i32 }

fn make(a: i32, b: i32) -> Point {
    Point { x: a, y: b }
}
```

This program parses, lowers cleanly to HIR with `Point` resolved to
`HirTyKind::Adt(HAdtId(0))` everywhere it appears in type position,
and the literal `Point { x: a, y: b }` lowers to
`HirExprKind::StructLit { adt: HAdtId(0), fields: [..] }`. Field
access (`p.x`) lowers to the existing `HirExprKind::Field { base,
name: String }` — typeck will resolve the field name later.

## Position in the pipeline

```
Source ─▶ tokens ─▶ AST ─▶ HIR  (this spec adds ADT support)
                                ─▶ typeck (TBD) ─▶ codegen (TBD)
```

## AST changes (`src/parser/`)

### New item kind

```rust
pub enum ItemKind {
    Fn(FnDecl),
    ExternBlock(ExternBlock),
    Struct(StructDecl),                              // new
}

pub struct StructDecl {
    pub name: Ident,
    pub fields: Vec<FieldDecl>,
    pub span: Span,
}

pub struct FieldDecl {
    pub name: Ident,
    pub ty: TypeId,
    pub span: Span,
}
```

### New expression kind

```rust
pub enum ExprKind {
    ...
    StructLit { name: Ident, fields: Vec<StructLitField> },
}

pub struct StructLitField {
    pub name: Ident,
    pub value: ExprId,
    pub span: Span,
}
```

`ExprKind::Field { base, name: Ident }` already exists; no AST
change needed for field access.

### Grammar

```
StructDecl     ::= 'struct' Ident '{' (FieldDecl (',' FieldDecl)* ','?)? '}'
FieldDecl      ::= Ident ':' Type
StructLitExpr  ::= Ident '{' (StructLitField (',' StructLitField)* ','?)? '}'
StructLitField ::= Ident ':' Expr
```

`StructDecl` is added at the item level, alongside `FnDecl` and
`ExternBlock`. `StructLitExpr` is added at the atom level of the
expression parser, ahead of bare `Ident` (the parser commits on
seeing `Ident '{' Ident ':'`).

### Parser ambiguity: struct literal in cond position

`if Foo { x: 1 } { ... }` is grammatically ambiguous between
"if-with-cond-`Foo`-then-block" and "struct-literal-followed-by-
then-block." Rust resolves this by forbidding struct literals in
cond/control-flow expression positions; we follow the same rule.

The parser distinguishes "expression context" from "expression-no-
struct context." `if`'s cond slot uses the latter; a struct literal
in that position is a parse error (new code, e.g. `E0107`). This
matches Rust exactly — `if Foo { x: 1 } {}` is a parse error there
too, with the workaround being `if (Foo { x: 1 }) {}`.

### What the AST does *not* add

- `pub` visibility on items or fields.
- Field shorthand (`Foo { x }` instead of `Foo { x: x }`).
- Update syntax (`Foo { x: 1, ..rest }`).
- Tuple structs (`struct Foo(i32, i32)`).
- Unit structs (`struct Foo;`).
- Generic parameters / lifetimes.
- Block-level item declarations (Rust allows nested `struct` in fn
  bodies; v0 keeps items module-only).

## HIR changes (`src/hir/`)

### New IDs and arenas

```rust
index_vec::define_index_type! { pub struct HAdtId      = u32; }
index_vec::define_index_type! { pub struct VariantIdx  = u32; }
index_vec::define_index_type! { pub struct FieldIdx    = u32; }

pub struct HirModule {
    pub fns:        IndexVec<FnId,    HirFn>,
    pub adts:       IndexVec<HAdtId,  HirAdt>,       // new
    pub locals:     IndexVec<LocalId, HirLocal>,
    pub exprs:      IndexVec<HExprId, HirExpr>,
    pub blocks:     IndexVec<HBlockId, HirBlock>,
    pub root_fns:   Vec<FnId>,
    pub root_adts:  Vec<HAdtId>,                     // new
    pub span:       Span,
}
```

`FieldIdx` is the typed-index newtype used by `IndexVec` for
positional access into `HirVariant.fields`. **It is not stored in
any side-table**; HIR/typeck/codegen all look up fields by name on
demand. The newtype exists for arena ergonomics, not as a resolution
cache.

### Adt shape — variants list from day one

We use the rustc-style "list of variants" shape even though structs
always have exactly one (unnamed) variant. This costs one
`IndexVec` cell per struct today and saves the rename + match-site
churn the day enums land — adding `AdtKind::Enum` then becomes
"variants has N entries" rather than "reshape `HirAdt`."

```rust
pub struct HirAdt {
    pub name:     String,
    pub kind:     AdtKind,
    pub variants: IndexVec<VariantIdx, HirVariant>,
    pub span:     Span,
}

pub enum AdtKind {
    Struct,
    // Enum, Union — future
}

pub struct HirVariant {
    /// `None` for the implicit unnamed variant of a struct.
    /// `Some(name)` for enum variants.
    pub name:   Option<String>,
    pub fields: IndexVec<FieldIdx, HirField>,
    pub span:   Span,
}

pub struct HirField {
    pub name: String,
    pub ty:   HirTy,
    pub span: Span,
}
```

For a struct, `variants` always has length 1, with
`variants[VariantIdx(0)].name == None`.

### Type-namespace resolution

```rust
pub enum HirTyKind {
    Named(String),                                                    // existing
    Ptr { mutability: Mutability, pointee: Box<HirTy> },              // existing
    Adt(HAdtId),                                                      // new
    Error,                                                            // existing
}
```

`Adt(haid)` means HIR resolved the source-text name against the
type scope. `Named(s)` is the fall-through — the name didn't match
any user-defined adt. Typeck consults its primitive table; if `s`
matches a primitive, it resolves; otherwise it's an unknown-type
error.

**Why the asymmetry between user types (resolved at HIR) and
primitives (resolved at typeck)**: user types are *definitions*
introduced by source declarations — pure lexical lookup, which is
HIR's job. Primitives are built-in vocabulary of the type system —
they have no source-level decl, no span, no body. Treating them as
if they did would be ceremonial. Typeck is the layer that knows the
type system; the primitive table lives there.

### Struct literal expression

```rust
pub enum HirExprKind {
    ...
    StructLit { adt: HAdtId, fields: Vec<HirStructLitField> },
}

pub struct HirStructLitField {
    pub name:  String,
    pub value: HExprId,
    pub span:  Span,
}
```

The `adt` is HIR-resolved. Field names stay as strings — typeck
walks them against `adts[adt].variants[VariantIdx(0)].fields` to
validate field-set membership and to type-check each expression.

If the source name doesn't resolve to any user-defined adt, lower
emits `HirExprKind::Poison` plus a `HirError::UnresolvedAdt`. We do
**not** produce a `StructLit` with a placeholder `adt` — typeck
would have to special-case that, and the existing `Poison` recovery
already absorbs cleanly downstream.

### Field access

`HirExprKind::Field { base: HExprId, name: String }` is unchanged
(the variant already exists from the original HIR shape). HIR
cannot resolve `name` because it doesn't know the type of `base`;
typeck does the lookup once `base`'s type is inferred.

## Lowering algorithm

```rust
pub fn lower(ast: &Module) -> (HirModule, Vec<HirError>) {
    // Pass 1 — prescan all items.
    //   For each ItemKind::Fn:          allocate FnId,    register name in module-level value scope.
    //   For each ItemKind::ExternBlock: allocate FnIds for each child decl, register names.
    //   For each ItemKind::Struct:      allocate HAdtId,  register name in module-level ty scope.
    //   Stub HirAdt and HirFn entries; field types and bodies filled in later.
    //
    // Pass 2 — resolve adt field types.
    //   For each HirAdt:
    //     for each declared field:
    //       lower its HirTy against ty_scopes (same lookup machinery as
    //       fn signatures use in pass 3).
    //     check duplicate field names within this adt.
    //
    // Pass 3 — lower fn bodies (the existing pass-2 of the original algorithm).
    //   Resolve type-position names (param/return/let-annotation, struct
    //   literal type names) against ty_scopes.
    //   Resolve value-position names against value scopes.
    //   Field names in `Field { base, name }` and in struct literals stay
    //   as raw strings.
}
```

Passes 2 and 3 are independent — they could run in either order or
in parallel — but Pass 1 must complete before either begins.
Sequencing 2 → 3 matches the existing fn-prescan pattern and is the
simplest implementation.

### Scopes

```rust
struct LowerCx {
    scopes:    Vec<HashMap<String, ValueRes>>,       // existing — value namespace
    ty_scopes: Vec<HashMap<String, HAdtId>>,         // new      — type namespace
    ...
}
```

Both stacks are LIFO. The bottom frame of each is the module-level
scope, populated in Pass 1.

In v0:
- Function bodies push a scope for parameters.
- Blocks push a scope for `let` bindings.
- Adts only ever live in the module-level frame of `ty_scopes` (no
  nested struct decls).

### Resolution rules

Type-position lookup (used in field types in Pass 2; in
param/return/let-annotation types in Pass 3):

1. Walk `ty_scopes` innermost-first. Hit → `HirTyKind::Adt(haid)`.
2. Miss → `HirTyKind::Named(name)`. (Typeck will resolve as a
   primitive or emit an unknown-type error.)

Struct-literal name lookup (Pass 3):

1. Walk `ty_scopes` innermost-first. Hit → emit
   `HirExprKind::StructLit { adt, fields }`.
2. Miss → emit `HirExprKind::Poison` plus
   `HirError::UnresolvedAdt { name, span }`.

Field-name resolution (in `Field` expressions and in
`StructLit`-field names): **does not happen at HIR**. Names stay
as strings; typeck resolves once base/literal type is known.

## Errors

Add to `HirError`:

```rust
pub enum HirError {
    UnresolvedName     { name: String, span: Span },                            // E0201 (existing)
    DuplicateFn        { name: String, first: Span, dup: Span },                // E0202 (existing)
    CharOutOfRange     { ch: char, span: Span },                                // E0203 (existing)
    DuplicateAdt       { name: String, first: Span, dup: Span },                // E0204 (new)
    DuplicateField     { adt: String, name: String, first: Span, dup: Span },   // E0205 (new)
    UnresolvedAdt      { name: String, span: Span },                            // E0206 (new)
}
```

Codes E0207–E0249 reserved for future HIR diagnostics.

`from_hir_error` in `src/reporter/from_hir.rs` grows arms for the
new codes.

### What HIR doesn't catch

These are real errors but live in TBD layers:

- **Recursive type with infinite size** (`struct A { x: A }`). HIR
  happily resolves `A` against its own freshly-registered HAdtId.
  Typeck (TBD-T2) catches the infinite-size case and accepts the
  pointer-broken case (`struct A { x: *const A }`).

- **Unknown type name in field position** (`struct Foo { x: Blarg }`).
  HIR produces `HirField { name: "x", ty: HirTyKind::Named("Blarg"), .. }`
  and lets typeck error in TBD-T4.

- **Field-set mismatch in struct literal** (missing/extra/duplicate
  field name in a literal vs the decl). Typeck (TBD-T6) catches —
  it has to walk the literal anyway to type-check.

- **Mutability** for `s.f = 1` and similar — TBD-T6.

## Worked example

Source:

```rust
struct Point { x: i32, y: i32 }

fn make(a: i32, b: i32) -> Point {
    Point { x: a, y: b }
}
```

After lowering (spans elided):

```text
adts = [
    HAdtId(0) → HirAdt {
        name: "Point",
        kind: Struct,
        variants: [
            VariantIdx(0) → HirVariant {
                name: None,
                fields: [
                    FieldIdx(0) → HirField { name: "x", ty: Named("i32") },
                    FieldIdx(1) → HirField { name: "y", ty: Named("i32") },
                ],
            },
        ],
    },
]

locals = [
    LocalId(0) → HirLocal { name: "a", mutable: false, ty: Some(Named("i32")) },
    LocalId(1) → HirLocal { name: "b", mutable: false, ty: Some(Named("i32")) },
]

exprs = [
    HExprId(0) → Local(LocalId(0)),                  // a
    HExprId(1) → Local(LocalId(1)),                  // b
    HExprId(2) → StructLit {
        adt: HAdtId(0),
        fields: [
            HirStructLitField { name: "x", value: HExprId(0) },
            HirStructLitField { name: "y", value: HExprId(1) },
        ],
    },
]

blocks = [
    HBlockId(0) → HirBlock { items: [HBlockItem { expr: HExprId(2), has_semi: false }] },
]

fns = [
    FnId(0) → HirFn {
        name: "make",
        params: [LocalId(0), LocalId(1)],
        ret_ty: Some(Adt(HAdtId(0))),                // resolved
        body: Some(HBlockId(0)),
        is_extern: false,
    },
]

root_adts = [HAdtId(0)]
root_fns  = [FnId(0)]
```

Note the asymmetry of `Named("i32")` (in field types and local
types) versus `Adt(HAdtId(0))` (in `ret_ty`) — both are correct
per the resolution rules. Primitives stay as strings; user types
become `HAdtId` handles.

## TBDs (typeck and codegen, future iterations)

- **TBD-T1**: typeck phase ordering — when does adt info land vs fn
  signatures vs body checking?
- **TBD-T2**: recursive type cycle detection (`struct A { x: A }`
  rejected; `struct A { x: *const A }` accepted because pointers
  are pointer-sized).
- **TBD-T3**: mutual reference between adts (forward/back across
  decls); should fall out naturally from phase ordering.
- **TBD-T4**: field type resolution against typeck's type vocabulary
  (`HirTyKind::Named("i32")` → `TyKind::Prim(I32)`,
  `HirTyKind::Named("Blarg")` → unknown-type error).
- **TBD-T5**: AdtDef storage and interning location (TyArena vs a
  sibling arena on `TypeckResults`).
- **TBD-T6**: typing rules for `StructLit` (field-set validation,
  per-field unification) and `Field` (place-expression rule, field
  name lookup, mutability for assignment-through-field). Rule for
  `Field`-as-place-when-base-is-place; composes with the existing
  `Local`-is-place rule and the (TBD) `Deref`-is-place rule.
- **TBD-T7**: codegen for struct values (LLVM `struct_type`, GEP
  for field access, lvalue extension for `Field`, struct-by-value
  parameter/return for in-language calls only — `extern "C"` boundary
  forbidden initially per the C-ABI discussion in 07_POINTER.md).

## Out of scope (forever-ish for v0)

- `pub` visibility.
- Field shorthand, update syntax (`..rest`).
- Tuple structs, unit structs.
- Enums, unions.
- Generic parameters, lifetimes.
- Methods, `impl` blocks, traits.
- `repr(...)` attributes. Default LLVM struct layout matches the
  C ABI on common targets, so `repr(C)` would be a no-op cosmetic
  attribute today; it becomes meaningful when `repr(packed)` /
  `repr(Rust)`-reordering / `repr(align(N))` land.
- Block-level item declarations (`fn outer() { struct Inner {} }`).
- Struct-by-value across `extern "C"` boundaries — see C-ABI
  discussion in 07_POINTER.md. Codegen-time error in TBD-T7.

## Note: 04_HIR.md needs an update

The "Out of scope (v0)" section of `04_HIR.md` currently states
that type-namespace resolution is deferred and "likely living in
typeck rather than HIR." This spec contradicts that — we resolve
user-defined types at HIR. `04_HIR.md` should be updated to
reflect the decision once this spec is implemented.
