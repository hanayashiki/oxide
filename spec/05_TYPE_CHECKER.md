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
    Unit,                          // ()
    Never,                         // ! — bottom type; absorbs in coerce
    Fn(Vec<TyId>, TyId),
    Ptr(TyId, Mutability),         // *const T / *mut T; mutability interned alongside pointee
    Adt(AdtId),                    // identity handle for user-defined types — see spec/08_ADT.md
    Array(TyId, Option<ConstId>),  // [T; N] (Some) / [T] (None) — see spec/09_ARRAY.md
    Infer(InferId),                // unification variable
    Error,                         // poison; absorbs without further errors
}

pub enum PrimTy {
    I8, I16, I32, I64,
    U8, U16, U32, U64,
    Usize, Isize,                  // target-pointer-sized; v0 lowers both to i64 (see spec/09_ARRAY.md)
    Bool,
}

pub struct TyArena  { /* hash-cons: equal types share TyId */ }
pub struct ConstArena { /* hash-cons for type-level u64 lengths — see spec/09_ARRAY.md */ }
pub struct FnSig { pub params: Vec<TyId>, pub ret: TyId, pub partial: bool }
```

`TyArena` pre-interns all primitives plus `Unit`, `Never`, and `Error`,
exposed as fields (`tcx.i32`, `tcx.bool`, `tcx.unit`, etc.). Every
`TyKind` constructed elsewhere goes through `intern` for dedup. The
parallel `ConstArena` interns `ConstKind::Value(u64)` (and a recovery
`Error` constant) used by `Array(_, Some(cid))`. Per-ADT structural
data lives in a separate `adts: IndexVec<AdtId, AdtDef>` table; equality
on `Adt(_)` is identity-only (`aid == aid`).

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
    /// Errors emitted while this fn body was being inferred. TyId fields
    /// inside may still reference unresolved Infer vars; finalize resolves
    /// them post-defaulting before flushing into Checker.errors.
    errors: Vec<TypeError>,
    /// Declared return type of the fn whose body is being inferred. Read
    /// by the `Return` arm of `infer_expr`.
    cur_ret: TyId,
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
3. `coerce(body_ty, cur_ret, body_span)` — body must produce the
   declared return type. Coerce (not unify) so a divergent body
   (`!`) vacuously satisfies any declared return.
4. `finalize_fn` — defaults unconstrained int vars to `i32`, replaces
   any other still-unresolved `Infer` with `Error`, walks
   `expr_tys`/`local_tys` and substitutes through resolved binding
   chains so no `Infer(_)` leaks into module-level results, and
   **discharges this fn's body-phase check-only obligations** against
   the just-finalized Inferer (each captured TyId is resolved through
   the live bindings; non-trivial diagnostics are emitted in place).

Decl-phase obligations (Sized at param / return / struct field) carry
concrete TyIds from the start. They live in `Checker.decl_obligations`
and discharge once at the end of `check`, after every fn body has been
processed. Discharge is **pure observation** — it never unifies, binds,
or otherwise touches inference state. By the time any obligation runs,
the relevant types are frozen.

## Per-expression rules

`infer_expr(eid) -> TyId` returns the inferred type. Callers are
responsible for unifying with whatever they expected.

| `HirExprKind` | Inferred type |
|---|---|
| `IntLit(_)` | fresh `Infer` flagged for int default |
| `BoolLit(_)` | `bool` |
| `CharLit(_)` | `u8` |
| `StrLit(_)` | intern `Ptr(u8, Const)` (C-style, NUL-terminated at codegen — see spec/07_POINTER.md) |
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
| `Assign { _, target, rhs }` | `coerce(rhs, target)` (directional — pointer-mut subtype applies); result = `Unit` |
| `Call { callee, args }` | callee must be `Fn(...)`; arity + per-arg `coerce` check; result = sig ret |
| `Field { base, name }` | recurse on `base`; if its type is `Adt(aid)`, look up `name` → field type. `Adt` with no such field → E0261 `NoFieldOnAdt`. Non-ADT base → E0262 `TypeNotFieldable`. See spec/08_ADT.md. |
| `Index { base, index }` | `Error`; emit `UnsupportedFeature` (E0255). Real typing rule (auto-deref through `Ptr<Array>`) is deferred per spec/09_ARRAY.md "Phase A Step 4/5". |
| `StructLit { adt, fields }` | resolve `aid` from the HIR-level `HAdtId`; walk each field's value expr; per-field validation against the declared field set: unknown → E0258, missing → E0259, duplicate → E0260; per-value `coerce` against declared field type; result = `Adt(aid)`. See spec/08_ADT.md. |
| `AddrOf { mutability, expr }` | infer `expr` (type `T`); validate `expr` is a place (HIR already filed `AddrOfNonPlace` if not); for `&mut`, `place_mutability(expr)` must be `Mut` else E0263; result = `Ptr(T, mutability)`. See spec/10_ADDRESS_OF.md. |
| `ArrayLit(_)` | walk sub-exprs for inner diagnostics; emit `UnsupportedFeature` (E0255). Real typing rule deferred per spec/09_ARRAY.md "Phase A Step 4/5". |
| `Cast { expr, ty }` | result = resolved `ty` (no compat check in v0; `InvalidCast` E0264 lands per spec/12_AS.md) |
| `If { cond, then, else? }` | cond unified with `bool`; then/else go through `unify_arms` + `join_never` (or `coerce(then, Unit)` on then-arm if no else — a degenerate coercion) |
| `Block(bid)` | recurse `infer_block` |
| `Return(val)` | `coerce(val, cur_ret)` (or `coerce(Unit, cur_ret)` if no val); result = `Never` |
| `Let { local, init }` | local's annotated ty (or fresh `Infer`); `coerce(init, local_ty)`; result = `Unit` |
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
init, assignment rhs, mid-block expression-statement, else-less `if`
then-arm). It splits into two halves — one eager, one deferred:

**Eager half (runs at the call site):**

1. If `actual` is `Never` or `Error`, accept unconditionally. A
   divergent expression produces no value, so the type it doesn't
   produce cannot conflict with the slot. The reverse (`expected` is
   `Never`, `actual` is some concrete `T`) is *not* accepted — that
   is exactly the case "this fn declared `-> never` but returns a
   value", which must error.
2. Otherwise, `unify(actual, expected)` immediately. This propagates
   type information through the union-find as the walk continues,
   binding any Infer vars on either side.
3. Enqueue `Obligation::Coerce { actual, expected, span }`.

**Deferred half (runs at module-level discharge after all inference
and integer-defaulting):** the obligation's discharge handler
re-resolves both sides with `resolve_fully` and runs the pointer
outer-layer subtype check (`*mut T` → `*const T` allowed at the
outermost layer; inner positions must match exactly). Non-Ptr-Ptr
inputs are no-ops here — the `unify` from the eager half has already
fired any shape-mismatch diagnostic.

**Why split?** `unify` is structurally permissive on outer Ptr
mutability (it discards the mut bits and recurses on inner types),
so the directional `*mut → *const` rule cannot live inside `unify`;
it's a separate post-hoc validation. Deferring that validation to a
frozen type universe means every check sees fully-resolved outer
constructors — the check is honest by construction. See
`obligation.rs`.

**On `expect_unit`:** there is no separate `expect_unit` rule — the
unit-position constraint at mid-block expression statements and
else-less `if` then-arms is exactly `coerce(expr_ty, Unit)`. The
Ptr-Ptr branch never fires (Unit isn't Ptr), so the obligation
discharge is a no-op and the eager `unify(_, Unit)` enforces the
constraint. Int-flagged Infer being unified with Unit is rejected by
`bind_infer_checked` with the same diagnostic shape as before.

Branch unification in `if` (then vs else) is symmetric, not a
coercion — but it shares the Never-absorbs spirit. Implemented as
`unify_arms`: if either arm is `Never`, skip unify entirely; the
non-divergent arm decides the if-expr's type via `join_never`.

## Obligations

Some validations are **directional** or **layout-sensitive** — they
cannot be folded into HM unification (which is symmetric and
shape-only) without breaking unification's algebraic properties. We
defer these to a check-only post-pass via a queue of obligations.

Two obligation kinds today:

- **`Coerce { actual, expected, span }`** — pointer mut-compat at
  every level (outer subtype, inner strict equality). Enqueued from
  every `coerce` call site after the eager `unify` body runs.
- **`Sized { ty, pos, span }`** — `TyKind::Array(_, None)` (the
  unsized form, see spec/09_ARRAY.md) is rejected at every value
  position (fn parameter, fn return, struct field, let-binding).
  `pos` discriminates the position for diagnostics. Enqueued during
  HirTy resolution at decl phase (param/return/field) and at
  `infer_let` (let-binding).

**Discharge is pure observation.** Each obligation calls
`resolve_fully` on its captured TyIds, inspects the resolved kind,
and emits a diagnostic if the rule fails. **It never unifies, binds,
or introduces new type variables.** All inference happens in the
eager half during the AST walk; obligations only validate.

**Two queues, two timings:**

- **Body-phase** obligations live in `Inferer.obligations` and
  discharge inside `Checker::finalize` while the Inferer is still
  alive. Captured TyIds may carry Infer references that need
  resolution against this fn's bindings; the live Inferer makes
  that direct.
- **Decl-phase** Sized obligations live in `Checker.decl_obligations`
  and discharge once at the end of `check`. They carry concrete
  TyIds (decl resolution never produces Infer), so no Inferer is
  needed.

Both feed the same `Checker::discharge_obligation` handler, which
takes `Option<&Inferer>` for the resolution step. Because discharge
is read-only, the order across obligations doesn't affect
acceptance/rejection — push order is preserved for deterministic
diagnostic ordering within each queue.

**Future-proofing.** The framework extends without redesign for
generics: a `T: Sized` obligation will be enqueued at instantiation
sites and discharged the same way. See spec/09_ARRAY.md "Sized
trait-ification".

## Errors

```rust
pub enum TypeError {
    TypeMismatch              { expected: TyId, found: TyId, span: Span },        // E0250
    UnknownType               { name: String, span: Span },                       // E0251
    NotCallable               { found: TyId, span: Span },                        // E0252
    WrongArgCount             { expected: usize, found: usize, span: Span },      // E0253
    // E0254 retired — string literals are typed `*const [u8; N]` (see StrLit row above)
    UnsupportedFeature        { feature: &'static str, span: Span },              // E0255
    CannotInfer               { span: Span },                                     // E0256
    PointerMutabilityMismatch { expected: TyId, actual: TyId, span: Span },       // E0257
    StructLitUnknownField     { field: String, adt: String, span: Span },         // E0258 — see spec/08_ADT.md
    StructLitMissingField     { field: String, adt: String, lit_span: Span },     // E0259 — see spec/08_ADT.md
    StructLitDuplicateField   { field: String, first: Span, dup: Span },          // E0260 — see spec/08_ADT.md
    NoFieldOnAdt              { field: String, adt: String, span: Span },         // E0261 — see spec/08_ADT.md
    TypeNotFieldable          { ty: TyId, span: Span },                           // E0262 — see spec/08_ADT.md
    MutateImmutable           { op: MutateOp, span: Span },                       // E0263 — see spec/10_ADDRESS_OF.md, spec/11_MUTABILITY.md
    UnsizedArrayAsValue       { pos: SizedPos, span: Span },                      // E0261 (collision — see note) — spec/09_ARRAY.md
    // E0264 InvalidCast reserved per spec/12_AS.md
}
```

Code namespace: typeck owns **E0250–E0299**.

**E0261 collision (known, tracked):** the code currently uses E0261
for both `NoFieldOnAdt` and `UnsizedArrayAsValue`. The rendered text
is unambiguous, but the numeric code is double-booked. Renumbering
is a separate code-and-snapshot task; the spec lists both at their
intended-but-conflicting codes so the discrepancy is discoverable.

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
- Pointer **deref** operator (`*p` rvalue, `*p = v` lvalue). Pointer
  types and `&` / `&mut` are wired (see spec/10_ADDRESS_OF.md);
  consuming a pointer back into a place is the still-deferred half
  per spec/07_POINTER.md.
- Array typing rules — `HirTyKind::Array` resolves to `Error` in
  `resolve_ty` today, and `ArrayLit` / `Index` emit E0255. The
  vocabulary (`TyKind::Array`, `ConstArena`, `Sized` obligation,
  `UnsizedArrayAsValue`) is in place; rules land per spec/09_ARRAY.md
  Phase A Step 4/5.
- Implicit numeric widening (Rust-strict: explicit `as` required).
- Cast compatibility check — loose in v0; `InvalidCast` (E0264) lands
  per spec/12_AS.md.
- Trait/method dispatch.
- Recursive-type cycle detection — see spec/08_ADT.md TBD-T2.
- Struct-by-value across `extern "C"` boundaries — see spec/08_ADT.md
  TBD-T7.
- Incremental / on-demand recomputation. The query API is implemented
  via an eager IndexVec cache; rebuilding requires re-running `check`.
