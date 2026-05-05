# Const items (v0)

Top-level `const NAME: Type = LITERAL;` items. Driven by
`spec/BACKLOG/B022_NO_CONST_ITEMS_BOILERPLATE.md` — the bootstrap
implementer's #1 friction point. Every enum-shaped tag table in
stage-1 today is a row of `fn TK_KW_FN() -> u8 { 10 }` declarations,
forcing call syntax (`TK_KW_FN()`) at every dispatch site. Const
items collapse the row to `const TK_KW_FN: u8 = 10;` and the use
site to `TK_KW_FN`.

This spec is deliberately scoped to *literal-only* RHS. Operators
(`const N: i32 = 1 + 2;`) require a const-expression evaluator
which is its own future spec; supporting them here would bleed
scope. Mirrors the precedent at `spec/09_ARRAY.md` "Length literal
extraction": the parser accepts exactly one literal token in the
slot.

## Surface form

```
ConstItem ::= 'const' Ident ':' Type '=' Literal ';'
Literal   ::= IntLit | BoolLit | CharLit | StrLit
```

- **Top-level only.** Module scope. Inside `extern "C" { ... }` is
  rejected (same posture as `struct`/`import` today via
  `UnsupportedExternItem`).
- **No nested const items inside fn bodies.** `BlockItem` does not
  admit items today; extending it is its own refactor. A future
  spec can lift this without breaking forward compatibility.
- **Type annotation mandatory.** No inference. The annotation
  *is* the literal's type; typeck verifies the literal kind
  matches the annotation.
- **RHS is one token.** The grammar matches exactly one literal
  token. Anything else (`1 + 2`, parens, idents, calls, casts)
  fails to parse with chumsky's "expected `;`" diagnostic.

## Examples

```oxide
const TK_KW_FN: u8 = 10;
const TK_KW_LET: u8 = 11;
const MAX_DEPTH: usize = 256;
const DEBUG: bool = false;
const NUL: u8 = '\0';
const GREETING: *const [u8; 6] = "hello";

fn dispatch(k: u8) {
    if k == TK_KW_FN { /* ... */ }
}
```

## Parser

The literal RHS uses a `choice((int_lit, bool_lit, char_lit,
str_lit))` parser. Same shape as the array-length slot's
`int_lit_length_parser`: no Pratt machinery, no expression
recursion. The captured literal is wrapped in an
`ExprKind::IntLit/BoolLit/CharLit/StrLit` so the AST shape stays
uniform with body expressions.

AST:

```rust
pub enum ItemKind {
    Fn(FnDecl),
    ExternBlock(ExternBlock),
    Struct(StructDecl),
    Import(ImportItem),
    Const(ConstDecl),                  // NEW
}

pub struct ConstDecl {
    pub name: Ident,
    pub ty:   TypeId,
    pub value: ExprId,                 // pinned to a literal
    pub span: Span,
}
```

`ItemKind::label()` adds `Const(_) => "const"`.

## HIR

```rust
index_vec::define_index_type! { pub struct ConstId = u32; }

pub struct HirProgram {
    pub fns: IndexVec<FnId, HirFn>,
    pub adts: IndexVec<HAdtId, HirAdt>,
    pub consts: IndexVec<ConstId, HirConstItem>,    // NEW
    // ...
}

pub struct HirModule {
    pub fns: Vec<FnId>,
    pub adts: Vec<HAdtId>,
    pub consts: Vec<ConstId>,                       // NEW
    pub root_fns: Vec<FnId>,
    pub root_adts: Vec<HAdtId>,
    pub root_consts: Vec<ConstId>,                  // NEW
    // ...
}

pub struct HirConstItem {
    pub name: String,
    pub ty: HirTy,
    pub value: HirConstValue,
    pub span: Span,
}

pub enum HirConstValue {
    Int(u64),
    Bool(bool),
    Char(u8),                          // mirrors HirExprKind::CharLit
    Str(String),
}

pub enum HirExprKind {
    // ... existing variants ...
    Const(ConstId),                    // NEW — resolved use site
}
```

`Const(_)` is **not a place** — `is_place == false`. Consts are
not assignable; the value materializes inline at codegen.

### Value namespace widens

`ModuleScope.values` is widened from `HashMap<String, FnId>` to
`HashMap<String, ValueId>` where `ValueId` is:

```rust
pub enum ValueId {
    Fn(FnId),
    Const(ConstId),
}
```

Both fns and consts share the value namespace — `fn FOO` and
`const FOO` collide as `DuplicateValueSymbol`.

### Lowering

Const items are lowered at scanner prescan time (alongside fns
and ADTs). The annotation's `HirTy` lowers via `ty::lower_ty`
under an empty type-param scope (consts have no generic params
in v0). The literal extracts to `HirConstValue` via a structural
match on `ExprKind` — no error path, since the parser already
pinned the RHS to a literal.

`resolve_ident` returns `HirExprKind::Const(cid)` when the value
namespace resolves to `ValueId::Const(cid)`.

### HIR errors

| Error | Code | Notes |
|---|---|---|
| `DuplicateValueSymbol { name, first, dup }` | E0210 | Replaces `DuplicateFn` for the cross-kind case (fn-vs-const). The same-kind cases (fn-vs-fn, const-vs-const) also route through this variant; the renderer can branch on the kinds via the spans alone if it cares. |
| `UnsupportedExternItem { kind: "const", ... }` | E0210-ish | Reuses the existing `UnsupportedExternItem` arm. `ItemKind::Const(_).label()` returns `"const"`. |

## Typeck

Add `const_tys: IndexVec<ConstId, TyId>` to `TypeckResults`.

In `decl::resolve_decls`, after fn-sig resolution, walk every
const:

```rust
for (cid, hc) in cx.hir.consts.iter_enumerated() {
    let annotated = Checker::resolve_ty(&mut cx.tys, &mut cx.errors, &hc.ty);
    let ok = match &hc.value {
        HirConstValue::Int(_)  => is_integer_prim(&cx.tys, annotated),
        HirConstValue::Bool(_) => annotated == cx.tys.bool,
        HirConstValue::Char(_) => annotated == cx.tys.u8,
        HirConstValue::Str(s)  => is_str_lit_ty(&cx.tys, annotated, s.len()),
    };
    if !ok {
        cx.errors.push(TypeError::TypeMismatch { ... });
    }
    cx.const_tys[cid] = annotated;
}
```

- **Int**: annotation must be an integer primitive (any width,
  any signedness). Same posture as `infer_expr`'s `IntLit` arm —
  the annotation pins the width directly, no inference variable.
- **Bool**: annotation must be `bool`.
- **Char**: annotation must be `u8`.
- **Str**: annotation must be `*const [u8; N+1]` where `N` is the
  source byte length. Mirrors `infer_expr`'s `StrLit` arm.

`infer_expr` adds:

```rust
HirExprKind::Const(cid) => self.const_tys[cid],
```

## Codegen

In `emit_expr`:

```rust
HirExprKind::Const(cid) => {
    let hc = &self.hir.consts[cid];
    Some(match &hc.value {
        HirConstValue::Int(n) => {
            let ty = self.typeck_results.const_tys[cid];
            // Same shape as emit_int_lit but read ty from const_tys[cid]
            // instead of expr_tys[eid].
            ...
        }
        HirConstValue::Bool(b) => Operand::Value(self.ctx.bool_type().const_int(*b as u64, false).into()),
        HirConstValue::Char(c) => Operand::Value(self.ctx.i8_type().const_int(*c as u64, false).into()),
        HirConstValue::Str(s)  => self.emit_str_lit(s),
    })
}
```

No per-`ConstId` cache. The arm just dispatches on the value
variant and re-emits the literal at each use site, the same way
the existing `IntLit/BoolLit/CharLit/StrLit` arms do.

### Side fix: cache `emit_str_lit` by content

Today `emit_str_lit` mints a fresh `@.str.N` global per call. Two
`"hi"` literals in the source — whether from a const or from
inline code — produce two distinct globals. With const items
each use of `const HELLO = "hi";` would spray new globals.

Fix at the source: `Codegen.str_lit_cache: HashMap<String,
PointerValue<'ctx>>` (alongside `str_counter`). On entry to
`emit_str_lit`, look up `s` first; cache hit → return; cache miss
→ mint as today and insert.

## Mono

No changes. Const items are not generic; they bypass mono
entirely.

## Out of scope

- Const-expression evaluation (`const N: usize = M + 1`).
- Const generics.
- `const fn` items.
- Nested const items inside fn bodies.

These are independent future specs. The HIR shape for
`HirConstValue` is forward-compatible: a future const-eval pass
would produce richer variants (`Computed { ... }`) without
disturbing `Int/Bool/Char/Str`.
