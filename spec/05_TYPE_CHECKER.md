# Type Checker

## Requirements

Goal: minimal HM-style algorithm — scoped per-function, no
let-generalization, no generics. Just enough to typecheck primitive
types and direct function calls so we can get to LLVM fast.

API style: query-based — `results.type_of_expr(eid)`,
`results.type_of_local(lid)`, `results.fn_sig(fid)`. Internally these
are O(1) lookups into `IndexVec` side-tables; the queries are the
public surface so callers don't reach into field structure.

Acceptance:

```
fn add(a: i32, b: i32) -> i32 { a + b }
//      ^ i32   ^ i32          ^ i32
```

## Position in the pipeline

Source ─▶ tokens ─▶ AST ─▶ HIR (name-resolved) ─▶ **typeck (real types)** ─▶ codegen.

Typeck is the layer that finally derives real types: it owns the
hash-cons `TyArena`, resolves primitive type names (`"i32"` → `Prim(I32)`),
runs HM unification per function body, and produces side-tables that
codegen consumes through query methods.

## Type vocabulary

```rust
pub enum TyKind {
    Prim(PrimTy),
    Unit,                    // ()
    Never,                   // ! — subtype of every type during unify
    Fn(Vec<TyId>, TyId),
    Ptr(TyId),               // reserved for future pointer support
    Infer(InferId),           // unification variable
    Error,                   // poison; absorbs without further errors
}

pub enum PrimTy {
    I8, I16, I32, I64,
    U8, U16, U32, U64,
    Bool,
}

pub struct TyArena { /* hash-cons: equal types share TyId */ }
pub struct FnSig { pub params: Vec<TyId>, pub ret: TyId }
```

`TyArena` pre-interns all primitives plus `Unit`, `Never`, and `Error`,
exposed as fields (`tcx.i32`, `tcx.bool`, `tcx.unit`, etc.). Every
`TyKind` constructed elsewhere goes through `intern` for dedup.

## Inference is per-function (Rust-style)

Function signatures are **not** inferred — they come from source
annotations only. Type inference happens *inside* a single function
body via a transient `Inferer` that holds union-find bindings. After a
function is checked, its `Inferer` is dropped; no inference state
leaks across functions.

```rust
struct Inferer {
    bindings: IndexVec<InferId, Option<TyId>>,
    int_default: IndexVec<InferId, bool>,
}
```

`int_default[α]` flags vars created from integer literals so finalization
can default them to `i32`. The flag also constrains unification: an
int-flagged var being unified with a non-integer concrete primitive
(e.g., `bool`, `Unit`) is rejected as a type mismatch rather than
silently bound — without this check, programs like
`fn f() { if true { 1 } }` would silently accept the literal `1` as
unit.

## Algorithm

```rust
pub fn check(hir: &HirModule) -> (TypeckResults, Vec<TypeError>) {
    // Phase 1 — resolve every fn signature from annotations. No
    //           inference. Forward calls work because all sigs land
    //           before any body is checked.
    // Phase 2 — for each fn, fresh Inferer + walk body + unify body
    //           tail with declared ret + finalize.
}
```

`Checker::check_fn`:

1. Spin up a fresh `Inferer`; record `cur_ret = fn_sigs[fid].ret`.
2. `infer_block(body)` — walks items (results discarded), then tail
   (or `Unit` if absent). Returns the block's value type.
3. `unify(body_ty, cur_ret, body_span)` — body must produce the
   declared return type.
4. `finalize_fn` — defaults unconstrained int vars to `i32`, replaces
   any other still-unresolved `Infer` with `Error`, walks
   `expr_tys`/`local_tys` and substitutes through resolved binding
   chains so no `Infer(_)` leaks into module-level results.

## Per-expression rules

`infer_expr(eid) -> TyId` returns the inferred type. Callers are
responsible for unifying with whatever they expected.

| `HirExprKind` | Inferred type |
|---|---|
| `IntLit(_)` | fresh `Infer` flagged for int default |
| `BoolLit(_)` | `bool` |
| `CharLit(_)` | `u8` |
| `StrLit(_)` | `Error`; emit `UnsupportedStrLit` (E0254) |
| `Local(lid)` | `local_tys[lid]` (set in Phase 1 for params; in `infer_let` for bindings) |
| `Fn(fid)` | intern `Fn(fn_sigs[fid].params, fn_sigs[fid].ret)` |
| `Unresolved(_)` | `Error` (already errored at HIR) |
| `Unary { Neg, e }` | type of `e` |
| `Unary { Not, e }` | unify `e` with `bool`; result `bool` |
| `Unary { BitNot, e }` | type of `e` |
| `Binary { arith/bitwise, l, r }` | unify `l` & `r`; result = unified type |
| `Binary { cmp, l, r }` | unify `l` & `r`; result = `bool` |
| `Binary { logical, l, r }` | unify both with `bool`; result = `bool` |
| `Binary { shift, l, r }` | result = `l`'s type |
| `Assign { _, target, rhs }` | unify; result = `Unit` |
| `Call { callee, args }` | callee must be `Fn(...)`; arity + arg types check; result = sig ret |
| `Index/Field` | `Error`; emit `UnsupportedFeature` (E0255) — no arrays/structs in v0 |
| `Cast { expr, ty }` | result = resolved `ty` (no compat check in v0) |
| `If { cond, then, else? }` | cond unified with `bool`; then/else unified together (or with `Unit` if no else) |
| `Block(bid)` | recurse `infer_block` |
| `Return(val)` | val unified with `cur_ret`; result = `Never` |
| `Let { local, init }` | local's annotated ty (or fresh `Infer`); unify init; result = `Unit` |
| `Poison` | `Error` |

## Unification rules

`unify` is **symmetric** Hindley-Milner unification — there is no
subtyping in this layer. The two type arguments are algebraically
interchangeable; we keep the parameter names `found` / `expected`
only because the emitted diagnostic renders them with those labels,
which is a presentation artifact (most call sites have no semantic
notion of which side is "expected").

After resolving both sides through any `Infer` chains:

- `found == expected` → ok.
- Either is `Error` → ok (poison absorbs).
- Both `Never` → ok. Anything else against `Never` is a mismatch
  here. The "`!` flows into any context" rule lives in `coerce`,
  not in `unify`.
- One side is `Infer(α)` → bind `α := other` via `bind_infer_checked`,
  which rejects int-flagged vars being unified with non-integer
  concrete types.
- Both `Prim(p)`, `Prim(q)` → ok iff `p == q`.
- Both `Unit` → ok.
- Both `Fn` → arity match, recurse pairwise on args + on rets.
- Both `Ptr` → recurse on inner.
- Otherwise → `TypeMismatch { expected, found, span }` (E0250).

## Coercion rules

`coerce(actual, expected, span)` is the **directional** check used
where the context dictates a slot type that an expression's value
must fit (fn body vs declared return, `Return(val)`, call args, let
init, assignment rhs). It runs in two steps:

1. If `actual` is `Never`, accept unconditionally. A divergent
   expression produces no value, so the type it doesn't produce
   cannot conflict with the slot. The reverse (`expected` is
   `Never`, `actual` is some concrete `T`) is *not* accepted —
   that is exactly the case "this fn declared `-> never` but
   returns a value", which must error.
2. Otherwise, `unify(actual, expected)` plus the pointer outer-layer
   subtype check (`*mut T` → `*const T` allowed; inner positions
   must match exactly).

Branch unification in `if` (then vs else) is symmetric, not a
coercion — but it shares the Never-absorbs spirit. Implemented as
`unify_arms`: if either arm is `Never`, skip unify entirely; the
non-divergent arm decides the if-expr's type via `join_never`.

## Errors

```rust
pub enum TypeError {
    TypeMismatch       { expected: TyId, found: TyId, span: Span },     // E0250
    UnknownType        { name: String, span: Span },                     // E0251
    NotCallable        { found: TyId, span: Span },                      // E0252
    WrongArgCount      { expected: usize, found: usize, span: Span },    // E0253
    UnsupportedStrLit  { span: Span },                                   // E0254
    UnsupportedFeature { feature: &'static str, span: Span },            // E0255
    CannotInfer        { span: Span },                                   // E0256
}
```

Code namespace: typeck owns **E0250–E0299**.

`from_typeck_error(err, file, &TyArena)` lives in
`src/reporter/from_typeck.rs` and needs the arena to render type names
for `expected: i32, found: bool`-style messages.

## Output / public API

```rust
// src/typeck/mod.rs
pub fn check(hir: &HirModule) -> (TypeckResults, Vec<TypeError>);

pub struct TypeckResults { /* tys + fn_sigs + local_tys + expr_tys */ }

impl TypeckResults {
    pub fn type_of_expr(&self, eid: HExprId) -> TyId;
    pub fn type_of_local(&self, lid: LocalId) -> TyId;
    pub fn fn_sig(&self, fid: FnId) -> &FnSig;
    pub fn tys(&self) -> &TyArena;
}
```

The query methods are O(1) reads against pre-computed `IndexVec`
caches. After `check` returns, every cached `TyId` is fully resolved
(no `Infer` left).

## Worked example

`fn add(a: i32, b: i32) -> i32 { a + b }`:

```text
fn_sig(FnId(0))      = FnSig { params: [i32, i32], ret: i32 }
type_of_local(0)     = i32       // a
type_of_local(1)     = i32       // b
type_of_expr(0)      = i32       // Local(a)
type_of_expr(1)      = i32       // Local(b)
type_of_expr(2)      = i32       // Binary(Add, a, b)
```

Body's tail (`a + b`, `i32`) unifies with the fn's `ret` (`i32`). No
errors.

## Out of scope (v0)

- Let-generalization / polymorphism / generics.
- User-defined types (struct/enum) — no type-namespace resolution
  beyond primitive lookup.
- Pointer types, address-of/deref operators.
- Arrays, slices, strings (E0254/E0255 placeholders).
- Implicit numeric widening (Rust-strict: explicit `as` required).
- Cast compatibility check (loose in v0; codegen will refuse invalid
  casts).
- Trait/method dispatch.
- Incremental / on-demand recomputation. The query API is implemented
  via an eager IndexVec cache; rebuilding requires re-running `check`.
