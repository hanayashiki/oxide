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

fn make() -> Point {
    Point { x: 1, y: 2 }
}

fn x_of_make() -> i32 {
    let p = make();
    p.x
}
```

This program parses, lowers cleanly to HIR (`Point` resolves to
`HirTyKind::Adt(HAdtId(0))` everywhere it appears in type position)
and typechecks end-to-end: phase 0 allocates `AdtId(0)` for `Point`,
phase 0.5 resolves both fields' `i32` types, phase 1 resolves the
two fn signatures, and phase 2 types each body — `Point { x: 1,
y: 2 }` as `Adt(0)`, `make()` as `Adt(0)`, and `p.x` as `i32`.

Codegen lowers it to runnable LLVM IR: the struct literal becomes
an `insertvalue` chain, the local binding gets an alloca + store of
the aggregate, and the field read becomes a `getelementptr` + `load`.
JIT-executing `x_of_make` yields `1`.

What stays out of scope is *writes through* fields (`s.f = 1`) and
struct-by-value across the FFI boundary; those are tracked in the
remaining TBDs below.

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

### Place expressions and `is_place`

Whether an expression refers to a memory location ("place" in rustc
terminology, "lvalue" in C) is a purely syntactic property —
derivable from `HirExprKind` and the place-ness of children. Because
it never depends on type information, it lives at the HIR layer:

```rust
pub struct HirExpr {
    pub kind: HirExprKind,
    pub span: Span,
    pub is_place: bool,
}
```

`is_place` is cached on every `HirExpr`, populated once per node at
lower time when the parent is constructed (children lower first, so
child `is_place` bits are already available).

#### Rules

```text
Local(_)                       → place
Field { base, .. }             → exprs[base].is_place
Index { base, .. }             → exprs[base].is_place
Unresolved(_) | Poison         → place  (suppress cascading errors;
                                          underlying issue already filed)
everything else                → not place
```

The cache is computed by a small helper inside `lower.rs`:

```rust
fn compute_is_place(kind: &HirExprKind, exprs: &IndexVec<HExprId, HirExpr>) -> bool {
    match kind {
        HirExprKind::Local(_) => true,
        HirExprKind::Field { base, .. } => exprs[*base].is_place,
        HirExprKind::Index { base, .. } => exprs[*base].is_place,
        HirExprKind::Unresolved(_) | HirExprKind::Poison => true,
        _ => false,
    }
}
```

`Index` is included even though typeck still rejects all indexing
as `UnsupportedFeature`: the place rule is purely structural (HIR-
level), independent of typeck's array support, so wiring it now
keeps the answer right when a future array spec lights up indexing.

`Unary { Deref, .. }` will be place-shaped under 07_POINTER §5;
its arm gets added when the deferred deref work lands.

#### Validation: assignment-target check at lower time

When `lower_expr` produces an `Assign { target, .. }`, it inspects
the lowered target's `is_place` bit (already populated). If false,
HIR emits `HirError::InvalidAssignTarget` (E0207). The `Assign`
itself is still constructed — downstream `Error` propagation absorbs
cleanly — but the diagnostic surfaces at the HIR layer, which is
where the rule is structurally definable.

#### Parser cleanup: `ParseError::InvalidAssignTarget` is removed

`spec/03_PARSER.md` declared `ParseError::InvalidAssignTarget`
(E0106) for "post-parse validation that the LHS is a place." The
variant exists in `parser/error.rs` and is rendered in
`reporter/from_parse.rs`, but is **never constructed** — dead code.
Place validation belongs at HIR (where `is_place` is computed
anyway), not at the parser layer. This round:

- Remove the `ParseError::InvalidAssignTarget` variant.
- Remove the `from_parse_error` arm.
- Remove the E0106 description from `spec/03_PARSER.md`.

#### What stays typeck's job

**Mutability of places.** Whether a place is *writable* (e.g.,
`s.f = 1` requires `s` to be `mut`) lives at typeck because the
eventual `Unary { Deref, expr }` arm requires reading the pointer's
type to decide `*mut T` (writable) vs `*const T` (not). For
`Local`/`Field` only the decision is purely structural, but
splitting `place_mutability` across layers would be uglier than
keeping it together. The current `Local`/`Field` walk is implemented
in `src/typeck/check.rs`; the `Deref` arm is the remaining piece of
TBD-T6. See `spec/11_MUTABILITY.md` for the consolidated rules.

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
    UnresolvedName       { name: String, span: Span },                            // E0201
    DuplicateFn          { name: String, first: Span, dup: Span },                // E0202
    CharOutOfRange       { ch: char, span: Span },                                // E0203
    DuplicateAdt         { name: String, first: Span, dup: Span },                // E0204
    DuplicateField       { adt: String, name: String, first: Span, dup: Span },   // E0205
    UnresolvedAdt        { name: String, span: Span },                            // E0206
    InvalidAssignTarget  { span: Span },                                          // E0207
}
```

Codes E0208–E0249 reserved for future HIR diagnostics.

`from_hir_error` in `src/reporter/from_hir.rs` grows arms for the
new codes.

### What HIR doesn't catch

These are real errors that live in typeck (or remain genuinely
deferred):

- **Recursive type with infinite size** (`struct A { x: A }`). HIR
  happily resolves `A` against its own freshly-registered HAdtId.
  Typeck (TBD-T2) catches the infinite-size case and accepts the
  pointer-broken case (`struct A { x: *const A }`).

- **Unknown type name in field position** (`struct Foo { x: Blarg }`).
  HIR produces `HirField { name: "x", ty: HirTyKind::Named("Blarg"), .. }`
  and lets typeck error via `UnknownType` (E0251).

- **Field-set mismatch in struct literal** (missing/extra/duplicate
  field name in a literal vs the decl). Typeck catches — E0258/
  E0259/E0260 — since it has to walk the literal anyway to type-check.

- **Mutability** for `s.f = 1` and similar. Typeck's `place_mutability`
  walk + `MutateImmutable` (E0263). See spec/11_MUTABILITY.md.

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

## Typeck phase ordering and ADT vocabulary

(This section resolves TBD-T1, TBD-T3, TBD-T4, TBD-T5.)

The graph-shape problem: structs can reference each other (or
themselves), so a fn signature mentioning `Foo` may need `Foo`'s
type representation before `Foo`'s field types have been resolved
(since one of those fields might be `Bar`, which mentions `Foo`).

We solve it the way pyright (and most graph-building algorithms)
do: **partial construction**. Allocate the per-ADT identity first,
mutably backfill the structural details in a second pass.

### Type vocabulary

```rust
// Typeck has its own AdtId — HAdtId is HIR's identifier and stays in HIR.
// 1:1 mapping with HAdtId today; the indirection leaves room for future
// generic-instantiation many-to-one (e.g., Vec<i32> and Vec<u8> both
// reference the same HAdtId but produce distinct AdtIds).
index_vec::define_index_type! { pub struct AdtId = u32; }

pub enum TyKind {
    ...
    /// Identity-only handle. Structural data lives in
    /// `TypeckResults.adts[aid]`; equality is `aid == aid`.
    Adt(AdtId),
}
```

The structural data:

```rust
pub struct AdtDef {
    pub name: String,
    pub kind: AdtKind,                                // mirror HIR's
    pub variants: IndexVec<VariantIdx, VariantDef>,
    /// `true` while mid-construction (phase 0 stub, before 0.5 backfill).
    /// Flipped to `false` once variant/field types are resolved. Reading
    /// a partial AdtDef from outside the build phases is a typeck bug.
    pub partial: bool,
}

pub struct VariantDef {
    pub name: Option<String>,
    pub fields: IndexVec<FieldIdx, FieldDef>,
}

pub struct FieldDef {
    pub name: String,
    pub ty: TyId,                                     // resolved
    pub span: Span,
}
```

`FnSig` gains the same flag for symmetry — placeholder sigs at
`Checker::new` time have `partial: true`, phase 1 flips them:

```rust
pub struct FnSig {
    pub params: Vec<TyId>,
    pub ret: TyId,
    pub partial: bool,
}
```

Note: today the FnSig flag is mostly ceremonial. Phase 1 is single-
pass and nothing reads `fn_sig` between `Checker::new` (where the
sig is placeholder-shaped with `partial: true`) and the flip at
the end of phase 1, so there's no observable partial-FnSig state
in our pipeline. The flag carries its weight only on `AdtDef`,
where phase 0 and phase 0.5 are split. We keep `FnSig::partial`
for symmetry and as a hook for any future case where signature
resolution itself becomes multi-pass — generics, trait method
default impls, where-clause resolution. For our C-ish language
that's likely never; the flag may eventually go away if it stays
ceremonial.

Both `partial` flags should be `false` for every entry by the time
`finish()` produces `TypeckResults`. A `debug_assert!` at finalize
and a panic in any reader path enforce this — partial state is
an internal-construction concept that must not leak.

`TypeckResults`:

```rust
pub struct TypeckResults {
    pub tys: TyArena,
    pub adts: IndexVec<AdtId, AdtDef>,                // new
    pub fn_sigs: IndexVec<FnId, FnSig>,
    pub local_tys: IndexVec<LocalId, TyId>,
    pub expr_tys: IndexVec<HExprId, TyId>,
}
```

`adts` lives alongside `tys` on `Checker` during construction and
moves into `TypeckResults` at finalize, exactly mirroring how
`tys`/`fn_sigs` already work. `TyArena` keeps doing hash-cons
interning (just adds the `Adt(aid)` variant); `AdtDef` storage is
identity-keyed and mutably built — separate concerns, separate
field on the struct.

`AdtKind`, `VariantIdx`, `FieldIdx` are reused verbatim from
`crate::hir`. `HAdtId` is *not* — typeck uses its own `AdtId`
(today: `AdtId(N)` always corresponds to `HAdtId(N)`; the explicit
mapping site lives in `decl.rs`'s phase 0 to make the
correspondence visible).

### Phase ordering and module layout

The multi-pass type-building (phases 0, 0.5, 1) lives in a child
submodule of `check.rs` so it can access `Checker`'s private fields
directly without leaking visibility to `ty.rs` / `error.rs`.
Phase 2 (body inference) stays in `check.rs`.

```
src/typeck/
    mod.rs              — re-exports
    error.rs            — TypeError
    ty.rs               — TyArena, TyKind, AdtDef, FnSig (vocab + partial flags)
    check.rs            — Checker, Inferer, phase 2, entry point;
                          declares `mod decl;` at the top
    check/
        decl.rs         — phases 0, 0.5, 1;
                          pub(super) fn resolve_decls(cx: &mut Checker)
```

```rust
// check.rs
mod decl;

pub fn check(hir: &HirModule) -> (TypeckResults, Vec<TypeError>) {
    let mut cx = Checker::new(hir);
    decl::resolve_decls(&mut cx);    // Phase 0 + 0.5 + 1
    for (fid, _) in hir.fns.iter_enumerated() {
        cx.check_fn(fid);            // Phase 2
    }
    cx.finish()
}

// check/decl.rs
pub(super) fn resolve_decls(cx: &mut Checker) {
    // Phase 0  — alloc AdtDef stubs (partial: true), intern TyKind::Adt(aid)
    //            for each. Stubs have name+kind copied from HIR but empty
    //            variants.
    // Phase 0.5 — walk hir.adts, resolve each field's HirTy → TyId, mutably
    //             backfill AdtDef.variants, set partial: false. The shared
    //             resolve_named_ty handles HirTyKind::Adt(haid) → the
    //             pre-interned TyKind::Adt(aid).
    // Phase 1   — fn signatures: resolve params + ret, set partial: false.
}
```

Why this works for graph-shaped types:

- **Forward / backward references** between ADTs (`struct A { b: B }`
  / `struct B { x: i32 }`, in either source order): both `HAdtId`s
  exist after phase 0. Phase 0.5 resolves `b: B` to
  `TyKind::Adt(B_haid)` cleanly.
- **Mutual references** (`struct A { b: B }` / `struct B { a: A }`):
  same as above. (TBD-T3 falls out — no separate work needed.)
- **Self-reference via pointer** (`struct Node { next: *const Node }`):
  resolves to `TyKind::Ptr(TyKind::Adt(Node_haid), Const)`. Pointer-
  sized, no cycle problem.
- **Self-reference without indirection** (`struct A { x: A }`):
  resolves cleanly at the TyId level, but produces a structurally
  infinite type. TBD-T2 catches this with a separate size check
  (cycle detection over the field-type graph).

### Unification

Pure nominal:

```text
unify(Adt(a), Adt(b), span):
    a == b   →  ok
    a != b   →  TypeMismatch
```

Three things this rule does *not* do:

- **No structural recursion into fields.** `Adt(a)` and `Adt(a)` have
  identical fields by construction (we allocate exactly one `AdtDef`
  per HIR struct decl), so walking is pointless. And recursing on
  cyclic types (`struct A { x: A }`) would loop forever.
- **No coercion or subtyping.** Distinct AdtIds with identical field
  shapes are distinct types. The standard nominal rule.
- **No partial-state read.** Unify only inspects `aid`, never
  `cx.adts[aid]`. The identity is set in phase 0 before any
  inference runs, so even if an `AdtDef` were still `partial: true`
  at unify time (it shouldn't be — phase 0.5 finishes before
  phase 2), unify wouldn't care.

Adt-vs-anything-else (Prim, Unit, Fn, Ptr) → `TypeMismatch`. Adt-
vs-`Infer(?)` falls through to the existing Infer rule (binds
`?T := Adt(a)`). Adt-vs-`Error` and Adt-vs-`Never` fall through to
the existing absorb rules.

### `resolve_named_ty` extension

```rust
fn resolve_named_ty(...) -> TyId {
    match &ty.kind {
        HirTyKind::Named(name) => /* primitive lookup, existing */,
        HirTyKind::Adt(haid)   => tys.intern(TyKind::Adt(*haid)),
        HirTyKind::Ptr { .. }  => /* recurse, existing */,
        HirTyKind::Error       => tys.error,
    }
}
```

Phase 0 has already populated `adts[haid]` with at least a stub, so
the `intern` is sound; readers can defer to `adts[haid]` for
field-level structural info when they need it (e.g., `Field` lookup
in `infer_field`, and `lower_ty` once TBD-T7 lands).

### Render

`TyArena.render(TyKind::Adt(haid))` needs the AdtDef to print the
name. The arena alone doesn't have it. Two options:

- (chosen) `TypeckResults` exposes a `render_with_adts(ty)` helper
  that has both the arena and the adts. The plain `tys.render(ty)`
  for `Adt` falls back to `Adt(N)` (where N is the raw HAdtId) when
  the adts table isn't in scope. This keeps `TyArena` self-contained.
- Pass `&adts` into `TyArena::render`. Couples the two more tightly
  but rendering is always-correct.

We pick option 1 — render falls back to `Adt(N)` if the caller
only has `TyArena`; full rendering happens through a `TypeckResults`
helper.

## Operations on ADTs

This section documents two operations on ADTs: **construction**
(`StructLit`) and **field read** (`Field` as rvalue). Both are
fully implemented in typeck and codegen. The third operation,
**field write** (`Field` as place + `Assign`), is in the remaining
TBDs.

### Construction (`StructLit`)

Typeck rule for `HirExprKind::StructLit { adt: HAdtId, fields }`:

- Result type: `tys.intern(TyKind::Adt(AdtId::from_raw(adt.raw())))`.
- Field-set validation, per-field:
  - Provided name occurs more than once in the literal →
    `E0260 StructLitDuplicateField { field, first, dup }`.
  - Provided name not declared on the adt →
    `E0258 StructLitUnknownField { field, adt, span }`.
  - Provided value is `coerce`d to the declared field's `ty`.
- After the per-field walk, missing fields → `E0259
  StructLitMissingField { field, adt, lit_span }`.
- Result is `Adt(aid)` regardless of any per-field error so cascades
  stay typed (`Error` absorbs at the per-field level).

Codegen emits an SSA aggregate via `insertvalue`:

```text
%agg = undef of <struct ty>
%agg = insertvalue <T> %agg, <fld_0_value>, 0
%agg = insertvalue <T> %agg, <fld_1_value>, 1
...
```

The walk goes in *declared* field order. The literal's provided
order may differ; we look up each declared field's matching value
by name. Typeck has already validated completeness, so the lookup
is infallible at codegen time.

LLVM at `-O0` typically constant-folds the chain when all field
values are constants (`store %T { i32 3, i32 4 }, ptr %slot`); at
higher opt levels both forms collapse to the same code.

### Field read (`Field` as rvalue)

Typeck rule for `HirExprKind::Field { base, name }`:

- Infer `base`, resolve through `Infer` chains.
- Dispatch on the resolved kind:
  - `Adt(aid)`: look up `name` in `adts[aid].variants[0].fields`.
    Hit → matched field's `ty`. Miss →
    `E0261 NoFieldOnAdt { field, adt, span }`.
  - `Infer(_)`: the receiver type is unresolved at this point. Emit
    `E0256 CannotInfer { span }`. (We don't have an obligation
    queue; field access can't make progress without the receiver
    type, see spec/05_TYPE_CHECKER.md "Inference is per-function".)
  - `Never`: propagate `Never`.
  - `Error`: propagate `Error` silently.
  - `Prim`/`Unit`/`Fn`/`Ptr`: emit
    `E0262 TypeNotFieldable { ty, span }`. No auto-deref for `Ptr`
    in v0.

Codegen dispatches on `base.is_place` (the cached HIR bit):

- **Place path** (`is_place == true`): `lvalue(base)` returns a
  pointer to the base's storage. `getelementptr inbounds <T>, ptr,
  i32 0, i32 <field_idx>` projects to the field's slot, then
  `load <field_ty>` reads just that slot. Single-field memory
  traffic.
- **Value path** (`is_place == false`): `emit_expr(base)` returns
  the SSA aggregate; `extractvalue <T>, <field_idx>` pulls the
  field out. No memory traffic.

Both paths produce the same observable value; the choice mirrors
how the base lives (in memory vs in SSA). `lvalue` recurses through
`Field` for chained access (`a.b.c`), emitting one `getelementptr`
per layer and a single trailing `load` at the outermost rvalue use.

The `lvalue`-of-`Field` machinery is shared with the
assignment-through-field path: `s.f = v` reuses the same GEP chain
and emits a `store` instead of a `load` (`emit_assign` →
`lvalue` → `Field` arm in `src/codegen/lower.rs`).

### LLVM type construction (`prepare_adt_types`)

ADT type lowering mirrors typeck's phase 0 / 0.5 split.

- **Phase A** — `ctx.opaque_struct_type(&adt.name)` for every
  `AdtDef`, producing one stable `StructType<'ctx>` handle per
  `AdtId` *before* any field types are resolved.
- **Phase B** — for each AdtDef, lower each field's `TyId` via
  `lower_ty` (which now hits the cache for any `TyKind::Adt(aid)`
  field) and `set_body` on the opaque struct.

The cache lives on `Codegen.adt_ll: IndexVec<AdtId, StructType<'ctx>>`
and is threaded into every `lower_ty` / `lower_fn_type` call.

Self-reference via `Ptr` resolves cleanly because pointers lower
to opaque `ptr` (no recursion into the pointee). Direct
self-reference (`struct A { x: A }`) is rejected by typeck before
codegen — see "Recursive type rejection" below (E0273).

`set_body(_, packed: false)` — natural alignment, padding inserted
per LLVM's target data layout. Matches the C ABI on common targets;
the day `repr(packed)` lands, the second arg toggles.

## Recursive type rejection

After phase 0.5 (when every `AdtDef` has its fields resolved to
`TyId`s), `decl::check_recursive_adts` runs a tri-color DFS over
the ADT field-type graph:

- **White / Gray / Black** — unvisited / on the current DFS stack /
  fully explored cycle-free.
- An edge `A → B` exists for each field of `A` whose type, after
  walking through any `Array(T, Some(_))` layers, lands on
  `Adt(B)`. A `Ptr(_, _)` layer is the cycle-breaker: pointers
  lower to opaque `ptr`, so we don't enter the pointee
  structurally.
- Hitting a **Gray** child during DFS is a back-edge — the cycle
  closes there. Emit `RecursiveAdt { adt, span }` (E0273) with the
  offending field's span and don't descend into that subtree.

Result: `struct A { x: A }`, `struct A { b: B } / struct B { a: A }`,
and `struct A { xs: [A; 3] }` are all rejected with E0273;
`struct Node { next: *const Node }` (and any `Ptr`-mediated cycle)
is accepted, since codegen lowers the pointer to opaque `ptr` and
the recursion never bottoms out into infinite size.

Complexity: `O(V + E)` over the ADT graph, V = `|adts|`, E = sum
of "ADT-typed leaves" across all field types after walking through
sized arrays.

## Remaining TBDs

- **TBD-T6**: mutability and field assignment.
  Most of this is now resolved:
  - `place_mutability` walks `Local` and `Field` in
    `src/typeck/check.rs`; `MutateImmutable` (E0263) fires for both
    `Assign` and `&mut`, closing the plain-`Local` gap. See
    `spec/11_MUTABILITY.md`.
  - Codegen for `Field`-as-place in `Assign` works through the same
    `lvalue` GEP chain as the rvalue-place path (`emit_assign` →
    `lvalue` → `Field` arm in `src/codegen/lower.rs`).

  Still open: the `Deref` arm in `place_mutability`, gated on the
  pointer-deref work deferred per `spec/07_POINTER.md`.
- **TBD-T7**: struct-by-value across `extern "C"` boundaries. Today
  Oxide-internal calls work (LLVM picks a default lowering, both
  ends agree because both ends are us). FFI calls that pass or
  return structs by value fall off the C-ABI cliff documented in
  07_POINTER.md "ABI considerations" — needs a per-target classifier
  (rustc's `rustc_target/src/callconv/<arch>.rs` is the reference
  shape). Mitigation while deferred: emit a typeck error on
  struct-by-value at any `extern "C"` signature.

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

