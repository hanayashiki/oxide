# Array

## Requirements

We have primitives, pointers, and ADTs (record structs). The natural
next building block is **arrays** — fixed-size contiguous runs of a
single element type. Arrays unblock buffers, lookup tables,
fixed-size message structs, and (eventually) the migration of
`StrLit` from "magic `*const u8`" scaffolding to a proper
`[u8; N]` place expression per `07_POINTER.md`.

This spec adds **two array kinds living together**:

- `[T; N]` — **sized array.** N is part of the type. Bounds-checked
  on every `Index`. Value-type — passable, returnable, copyable.
  The safe path.

- `[T]` — **unsized array,** modeled as `[T; ∞]`. No length carried
  in the type. Used **only** behind a thin pointer (`*const [T]`
  / `*mut [T]`). Indexing is unchecked because the type literally
  says "no upper bound." The unsafe escape hatch.

Two array kinds, **one `TyKind` variant**: `Array { elem, len:
Option<ConstId> }`. `None` is the unsized form; `Some(c)` is the
sized form. The `[T] ≡ [T; ∞]` mental model is reflected directly
in the type representation.

### Why keep `[T]` when it's so restrictive?

`[T]` looks anemic on its own: it can't be a value, can't be
allocated, can't be assigned, can't be passed or returned. Two
reasons we keep it anyway — one practical, one language-design.

**Practical: `[T]` is the v0 escape hatch for runtime-sized
buffer access.** Per `07_POINTER.md`, pointer arithmetic
(`p + n`) and pointer deref (`*p`) are deferred. Without `[T]`,
the typical C-FFI buffer pattern is unreachable in pure Oxide:

```rust
// Without [T]:
extern "C" {
    fn malloc(n: usize) -> *mut u8;
    fn read(fd: i32, buf: *mut u8, count: usize) -> i64;
}

fn read_first(fd: i32) -> u8 {
    let buf: *mut u8 = malloc(64);
    read(fd, buf, 64);
    // buf[0]    — E0258 (*mut T is not indexable; T isn't an array)
    // *buf      — deferred per 07
    // *(buf+0)  — both deref AND ptr-arith deferred
    // The buffer is genuinely unreachable.
}
```

With `[T]`, indexing through `*mut [T]` is the v0 syntax for
"element at offset i in this buffer" — no pointer arithmetic, no
deref operator needed:

```rust
extern "C" {
    fn malloc(n: usize) -> *mut [u8];      // pointer to unsized array
}

fn read_first(fd: i32) -> u8 {
    let buf: *mut [u8] = malloc(64);
    buf[0]                                  // works (Index auto-deref + GEP)
}
```

This means `*const [T]` / `*mut [T]` is the natural type for FFI
returns of dynamically-sized buffers (`malloc`, `read`, `mmap`,
`fread`, …). Without it, the C-buffer interop story for v0 would
need to wait on pointer-arith + deref, which carry their own
design surface (provenance, alignment, bounds, overflow rules)
and are deliberately deferred.

**Language design: even C differentiates `[T]` from `*T` at the
type level.** Collapsing `[T]` into `*T` would lose a semantic
distinction that C — the language we most directly mirror — keeps:

- `int arr[]` (incomplete array type) and `int *p` participate in
  different rules. `sizeof arr` in the defining translation unit
  yields the full array's byte size; `sizeof *p` is a single
  element. The two types print differently in error messages,
  declare differently in headers, and document intent
  differently.
- C arrays *decay* to pointers in many contexts, but the decay is
  a one-way coercion at use sites — not an identity. A C compiler
  that erased the distinction at the type level (treating `[T]`
  as a synonym for `*T` everywhere) would be a worse C compiler.
- "Pointer to many T" vs "pointer to one T" is information the
  API designer wants to convey to the API consumer. Erasing it
  is dishonest.

We adopt the same stance: `*const [T]` means "pointer to a
contiguous run of T elements, length unknown to the type
system"; `*const T` means "pointer to one T." The two types are
distinct in our type system, distinct in our diagnostics, and
participate in distinct coercions. Specifically:

- `*const [T; N] → *const [T]` is a coercion (drop the length,
  stay an array).
- `*const [T; N] → *const T` is **not** a coercion (would be the
  C "decay to first element" wart) — it requires an explicit
  `as` cast.

The restrictiveness of `[T]` (cannot be a value, cannot be
allocated, only behind a pointer) is exactly the property that
makes it correct as an escape hatch and as a type-level signal:
it captures "I refer to many of these but don't know how many"
without forcing a runtime length representation (Rust slice fat
pointer) or eroding the distinction from "I refer to one of
these" (C-style decay).

Three adjacent items land in this spec because they fall out of
the array work and are unsound to defer:

- **`usize` / `isize` as distinct primitives** (not aliases of
  `u64`/`i64`). Fixed at 64-bit width on the current target. Future
  target awareness flips only the codegen width.

- **`ConstArena` / `ConstId` / `ConstKind`** — a hash-cons interner
  parallel to `TyArena`, holding type-level constant values. Today
  it carries `Value(u64)` and `Error`. Future const-generics
  (`ConstKind::Param(idx)`) is purely additive.

- **Length must be exactly an integer literal** (`ExprKind::IntLit`).
  No const-expression evaluation in v0: parens, casts, char lits,
  binary ops, idents, calls — all rejected at **parse time** by the
  grammar's length-slot rule (the slot only matches a single `Int`
  token). HIR's "evaluator" is then a structural pattern match with
  no error path. Future work (a real ICE evaluator, `const` items,
  or const generics) relaxes the parser and extends the HIR match;
  staying with literals for now keeps the initial cut tractable and
  avoids any constexpr/const-fn slippery slope.

This iteration covers **AST → HIR → typeck → codegen** end-to-end
for both array kinds.

## Subset-of-Rust constraint

Anything we accept must also parse in Rust with the same meaning.
The grammar below is a strict subset of Rust's array/slice syntax:

- `[T; N]` matches Rust verbatim (with N restricted to an integer literal token in v0; Rust accepts any const expression — that's a tighter subset, not a divergence).
- `[T]` is Rust's slice DST — we use the same syntax but without
  fat pointers; pointers to `[T]` are thin. This is a **semantic
  divergence**, not a syntactic one (Rust's `&[T]` is fat;
  `*const [T]` in Rust is also fat). We accept the same syntax;
  the underlying ABI differs.
- Array literals `[a, b, c]` and `[init; N]` mirror Rust.

Out-of-scope features (deferred, not deviations):

<!-- Empty `[]` is supported (parses as zero-length Elems); typeck handles inference. -->

- Length-inference sugar `[init; _]` / `[init; ..]` (not in Rust
  anyway).
- ICE-style length expressions (e.g. `[T; 1+1]`, `[T; SIZE]`,
  `[T; sizeof(T)]`). Deferred — would land alongside an ICE
  evaluator (or const items / const fns) in its own spec.
- Const generics (`fn f<const N: usize>(a: [T; N])`).
- Const items (`const N: usize = 10`) — would extend the (future)
  ICE evaluator with name lookup.
- `sizeof T` — would extend the (future) ICE evaluator with
  type-size queries.
- Slice fat pointers (`&[T]` carrying length). Rust has them; we
  intentionally don't. C-ish stance.
- Pointer arithmetic `p + n` and deref `*p` (deferred per
  `07_POINTER.md`); Index auto-deref for pointer-to-array
  sidesteps the need.
- Per-expression unsafe opt-out (`unsafe { arr[i] }`) — needs
  `unsafe` blocks first.
- Repeat with non-trivially-copyable init (irrelevant in v0; all
  our types are trivially copyable).
- Build flag `--no-bounds-check` for global guard suppression.

## Acceptance

```rust
fn first(a: [i32; 3]) -> i32 { a[0] }

fn at(p: *const [i32], i: usize) -> i32 { p[i] }

fn main() -> i32 {
    let a: [i32; 3] = [1, 2, 3];
    let b: [u8; 1024] = [0; 1024];
    first(a) + (b[0] as i32)
}
```

This program parses, lowers cleanly to HIR with array types
resolved to `Array(elem, Some(_))` everywhere, typechecks (with
`a` flowing into `first`'s sized parameter via the internal ABI;
`b[0]` indexed and cast), and codegens to LLVM IR with
bounds-check guards.

End-to-end run target: compile, link, execute, exit code 1
(`first(a)` returns 1, `b[0]` is 0).

Slice-side acceptance is exercised via FFI for now (until `&` lands
per `10_ADDRESS_OF.md`):

```rust
extern "C" {
    fn make_buf(n: usize) -> *mut [i32];
    fn buf_len(p: *const [i32]) -> usize;
}
```

`*mut [i32]` and `*const [i32]` are valid pointer types; indexing
through them is unchecked.

## Position in the pipeline

```
Source ─▶ tokens ─▶ AST ─▶ HIR ─▶ typeck ─▶ codegen
                              ╰── this spec adds:
                                    • TypeKind::Array { elem, len: Option<ExprId> }
                                    • ExprKind::ArrayLit (Elems | Repeat)
                                    • HirTyKind::Array(_, Option<HirConst>)
                                    • HirConst { Lit(u64), Error }
                                    • HirExprKind::ArrayLit
                                    • PrimTy::Usize | PrimTy::Isize
                                    • ConstArena + ConstId + ConstKind
                                    • TyKind::Array(_, Option<ConstId>)
                                    • Codegen for Array, Index, ArrayLit (incl. llvm.trap guard)
```

## AST changes (`src/parser/`)

### New / changed shape

```rust
pub enum TypeKind {
    Named(Ident),
    Ptr { mutability: Mutability, pointee: TypeId },
    Array { elem: TypeId, len: Option<ExprId> },     // NEW
}

pub enum ExprKind {
    ...
    ArrayLit(ArrayLit),                              // NEW
}

pub enum ArrayLit {
    Elems(Vec<ExprId>),                              // [a, b, c]
    Repeat { init: ExprId, len: ExprId },            // [init; N]
}
```

`TypeKind::Array { len: None }` is `[T]`; `len: Some(expr)` is
`[T; expr]`. The expression form is preserved in AST for fidelity;
HIR const-evaluates it to a `u64` at lower time.

`ExprKind::Index { base, index }` already exists from prior work
— no change needed at the expression level for indexing. Field
access composes by accident through the existing place machinery.

### Grammar

```
ArrayType    ::= '[' Type ';' Expr ']'                  # sized   [T; N]
              | '[' Type ']'                            # unsized [T]
ArrayLit     ::= '[' (Expr (',' Expr)* ','?)? ']'       # elems   [a, b, c]
              | '[' Expr ';' Expr ']'                   # repeat  [init; N]
```

`ArrayType` slots into the Type production at the same level as
`Ptr`. `ArrayLit` slots into the atom level of the expression
parser, alongside `StructLit`. Disambiguation with `Index` is
positional: `[...]` at the start of an atom is `ArrayLit`; `[...]`
after a place is `Index`.

### Lit/Type disambiguation

`[T; N]` (type) and `[a; n]` (repeat literal) share punctuation but
appear in different contexts (type vs expression). The parser
distinguishes them by context.

`[a]` (one-elem list literal) and `[T]` (unsized type) both end
after one item. Same — type vs expression context distinguishes.

Empty `[]` parses as `ArrayLit::Elems(vec![])` — a zero-length
element list. The parser does **not** reject it. The "needs
context type to infer the element type" question is a *semantic*
issue (typeck has to fall back to the let-annotation / fn-arg
slot type to know `T` in `[T; 0]`), not a syntactic one. Typeck
handles it: with a context type like `let a: [i32; 0] = [];`,
the literal types as `[i32; 0]`; without a context type
(`let a = [];`), typeck emits its standard `cannot infer a type`
diagnostic, the same as for any unannotated empty container.

### Parser ambiguity: array literal in cond position

Like `08_ADT.md`'s struct-lit-in-cond rule, `if [1, 2, 3] { ... }`
is rejected — the cond slot uses "expression-no-arraylit" mode.
Rust treats this similarly via the matched-pair-with-block heuristic;
we adopt the same rule by parser context.

### What the AST does *not* add

- `[T; _]` length-inference sugar.
- Slice patterns `[a, b, c]` (pattern position).
- Range syntax `arr[1..5]` — slicing. Out of scope.
- Generic types in the elem slot: `[T<U>; N]` — no generics yet.

## HIR changes (`src/hir/`)

### New IDs and shapes

```rust
pub enum HirTyKind {
    Named(String),
    Adt(HAdtId),
    Ptr { mutability: Mutability, pointee: Box<HirTy> },
    Array(Box<HirTy>, Option<HirConst>),         // NEW
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum HirConst {                              // NEW
    Lit(u64),
    Error,
}

pub enum HirExprKind {
    ...
    ArrayLit(HirArrayLit),                       // NEW
}

pub enum HirArrayLit {
    Elems(Vec<HExprId>),
    Repeat { init: HExprId, len: HirConst },
}
```

`HirTyKind::Array(elem, None)` is the unsized variant. `Some(_)`
is sized; the inner `HirConst` is already const-evaluated at lower
time (`Lit(u64)` or `Error`).

`HirArrayLit` covers both literal forms. The `Repeat` variant
stores its length as `HirConst` (already evaluated), parallel to
how the type's length lives on `HirTyKind::Array`.

### Length literal extraction

No const-expression evaluator in v0. The length slot in `[T; N]`
and `[init; N]` is **a parser rule**, not an expression rule:
the grammar matches exactly one `Int(n)` token there. Any other
expression shape (parens, casts, char lits, unary/binary ops,
idents, calls, etc.) fails to parse at the offending token —
chumsky reports a generic "unexpected token" diagnostic
(rendered as the existing `E0101` parse-error code), with a span
on the first non-`Int` token.

The captured `n` is wrapped in an `ExprKind::IntLit(n)` AST node
so the AST shape stays unchanged: type lengths remain
`Option<ExprId>`, repeat-literal lengths remain `ExprId`. What
changes is what tokens reach the AST.

HIR-side "evaluation" is a structural pattern match with no
error path:

```rust
fn extract_length_const(ast_expr: &Expr) -> HirConst {
    match &ast_expr.kind {
        ExprKind::IntLit(n) => HirConst::Lit(*n),
        other => unreachable!(
            "parser ensures length slot is IntLit; got {other:?}"
        ),
    }
}
```

That's the entire "evaluator" — a one-arm match. No new module;
this lives directly in `src/hir/lower.rs`. Future work (a real
ICE evaluator, `const` items, const generics) relaxes the parser
rule and extends this match; the storage shape
(`HirConst::Lit(u64)`) and the typeck-side `ConstArena` /
`ConstKind` layer are unchanged.

Why so strict (no parens, no casts):

- **Simplicity.** Zero const-eval semantics in v0 means zero
  questions about overflow, division-by-zero, sign extension,
  evaluation order. We simply don't have any of that surface.
- **Easy to relax later.** Adding `Paren(IntLit) → IntLit` peeling
  is a one-line change. Adding full ICE is a separate spec.
  Strict-now is forward-compatible with anything we'd want next.
- **No silent meaning.** The user writes `5`, gets `5`. They write
  `(5)`, get a clear parse error telling them an integer literal
  was expected here.
- **One source of truth.** Rejection lives in the grammar — the
  HIR doesn't carry a redundant reporter arm, and the AST never
  holds a length expression that downstream layers would have to
  second-guess.

### Lowering algorithm changes

Pass-1 (prescan items): no change. Arrays don't introduce any
module-level name.

Pass-2 (resolve ADT field types) gains a recursive walker that
descends into `Array(elem, Option<HirConst>)` and `Ptr` types,
calling `extract_length_const` on the length expression where
present. Same machinery applies to fn signatures' types in pass 1
(`fn sig` resolution already walks types — extend the walker).

Pass-3 (lower fn bodies) gains:

- `ast::ExprKind::ArrayLit(ArrayLit::Elems(es))` →
  `HirArrayLit::Elems` after lowering each element expr.
- `ast::ExprKind::ArrayLit(ArrayLit::Repeat { init, len })` →
  lower `init`; run `extract_length_const(len)`; result is
  `HirArrayLit::Repeat { init: lowered_init, len: HirConst::Lit(n) }`.
  The match is total — non-`IntLit` was rejected at parse time —
  so there is no `HirConst::Error` failure path here.

### Place rule

`HirExprKind::Index { base, index }` is a **place expression** when:

- `base` is a place expression of array type (`Array(_, _)`), OR
- `base` is a value of pointer type whose pointee is array type
  (`Ptr(Array(_, _), _)`).

The latter is the **auto-deref through Index** rule — required in
v0 because `*p` deref is deferred (`07_POINTER.md` §5). Once `*p`
lands, `p[i]` becomes equivalent to `(*p)[i]` and the auto-deref
falls out of the standard place rule.

Mutability of the resulting place inherits from the base:

- Place from `let arr` → read-only place (per the upcoming
  mut-enforcement in `11_MUTABILITY.md`).
- Place from `let mut arr` → read-write place.
- Auto-deref through `*const [T; N]` / `*const [T]` → read-only.
- Auto-deref through `*mut [T; N]` / `*mut [T]` → read-write.

`ArrayLit` is **not** a place — it's a fresh value (allocated to
a temporary slot at codegen time).

### New errors

HIR adds **no new error variants** for arrays. The two checks the
spec previously slotted into HIR have moved:

- **Non-`IntLit` length** is rejected at parse time, not HIR. The
  parser's grammar rule for the length slot only matches an `Int`
  token, so anything richer fails with the existing generic
  parse-error code (`E0101`, "unexpected token") with a span on
  the first non-`Int` token.

- **Unsized array `[T]` in a value-type position** is rejected at
  **typeck** (E0261 `UnsizedArrayAsValue`), not HIR. HIR doesn't
  fully resolve types: a future `type Buf = [i32]` alias would be
  `Named("Buf")` at HIR with the unsized shape only visible after
  typeck resolves the alias. Putting the check at HIR would catch
  the syntactic case but miss aliased ones — and typeck has to
  run the check anyway. Single-source-of-truth wins.

(E0207, E0208 are claimed by `InvalidAssignTarget` and
`AddrOfNonPlace` from prior specs. E0209/E0210/E0211 are
unallocated — array lowering produces no diagnostics of its own.)

DivByZero and Overflow codes are **not allocated in v0** since
there is no const-evaluator to produce those failures. They will
be assigned in the next free HIR slot when a real ICE evaluator
lands.

`E0209` was previously assigned to `ArrayLenNotIntLit` and is now
free — left unallocated for now to avoid renumbering existing
codes. Future ICE-evaluator errors should claim it (or any later
free slot).

`E0210` is unallocated. (An earlier draft reserved it for "empty
`[]` without context-type"; today's design accepts `[]` at parse
time and lets typeck handle the inference question alongside any
other `cannot infer a type` case, so no dedicated code is needed.)

HIR adds no new diagnostic arms for arrays — `from_hir_error` in
`src/reporter/from_hir.rs` is unchanged by this spec.

### What HIR doesn't catch

- **Bounds violations on literal indices.** A literal index past
  the array's known length is a runtime trap (or compile-time fold
  of the guard). Typeck/codegen emits the guard; we don't try to
  reject at HIR.
- **Length-mismatch in `[T; N]` unify.** Two array types with
  different lengths flowing into the same slot is a typeck error
  (E0257), not HIR.

## Type system changes (`src/typeck/ty.rs`)

### New primitives

```rust
pub enum PrimTy {
    I8, I16, I32, I64,
    U8, U16, U32, U64,
    Bool,
    Usize,                                       // NEW — 64-bit on current target
    Isize,                                       // NEW — 64-bit on current target
}
```

`PrimTy::name` adds `"usize"` / `"isize"`. `from_prim_name` adds
the same. `is_integer` returns `true` for both.

`TyArena` pre-interns `usize` and `isize` alongside the other
primitives:

```rust
pub struct TyArena {
    ...
    pub usize: TyId,
    pub isize: TyId,
}
```

**Rationale for distinct primitives** (not aliases of `u64`/`i64`):
the type system carries the semantic distinction "this is a
target-pointer-sized integer used for sizes/indices" from day one.
Aliases would erase that distinction and make later target-aware
work a breaking change. We're target-fixed at 64-bit for now;
codegen maps both to LLVM `i64`. The day we add 32-bit support,
codegen flips to `i32` for those targets — type-system-side
nothing changes.

`usize` and `u64` are **NOT** interconvertible without an explicit
`as` cast. `let n: u64 = some_usize` is an error
(`E0250 TypeMismatch`); user must write `some_usize as u64`. Same
for the other direction.

### ConstArena

A new hash-cons interner parallel to `TyArena`:

```rust
index_vec::define_index_type! { pub struct ConstId = u32; }

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum ConstKind {
    Value(u64),
    Error,
}

pub struct ConstArena {
    arena: IndexVec<ConstId, ConstKind>,
    interner: HashMap<ConstKind, ConstId>,
    pub error: ConstId,
}

impl ConstArena {
    pub fn new() -> Self;
    pub fn intern(&mut self, kind: ConstKind) -> ConstId;
    pub fn kind(&self, id: ConstId) -> &ConstKind;
    pub fn value_of(&self, id: ConstId) -> Option<u64>;  // None for Error
    pub fn render(&self, id: ConstId) -> String;
}
```

The `u64` stored in `ConstKind::Value` is the **physical bag** for
type-level integers regardless of whether the source-level slot
type is `usize`, `u64`, or anything else. Today every length slot
uses `usize` semantically; the `u64` storage is an implementation
detail.

Future extension: `ConstKind::Param(ConstParamIdx)` for const
generics. Adding it is purely additive — the v0 code's pattern
matches stay correct, just gain a new arm to handle.

### TyKind extension

```rust
pub enum TyKind {
    Prim(PrimTy),
    Unit,
    Never,
    Fn(Vec<TyId>, TyId),
    Ptr(TyId, Mutability),
    Array(TyId, Option<ConstId>),                // NEW — None = [T]; Some(c) = [T; c]
    Infer(InferId),
    Error,
}
```

`Array(elem, None)` is unified-array-kind for `[T]`; `Array(elem,
Some(cid))` is `[T; consts.kind(cid)]`. Hash-cons interns by the
pair, so `[i32; 3]` and `[i32; 3]` collapse to the same `TyId`
once both `i32`'s `TyId` and the `ConstId` for `Value(3)` are
themselves interned.

### TypeckResults extension

```rust
pub struct TypeckResults {
    pub tys: TyArena,
    pub consts: ConstArena,                      // NEW
    pub adts: IndexVec<HAdtId, AdtDef>,
    pub fn_sigs: IndexVec<FnId, FnSig>,
    pub local_tys: IndexVec<LocalId, TyId>,
    pub expr_tys: IndexVec<HExprId, TyId>,
}
```

`consts` lives next to `adts` for the same reason `adts` lives
on `TypeckResults` rather than `TyArena`: structural per-id data
keyed by an interner, distinct from the hash-cons identity layer.

### Render

`TyArena::render` for `Array(elem, len_opt)`:

- If `len_opt == None`: `[<elem>]`
- If `len_opt == Some(cid)` and `ConstArena` is in scope:
  `[<elem>; <consts.render(cid)>]`
- Fallback (no `ConstArena` in scope): `[<elem>; ?N]`

The `?N` fallback mirrors `08_ADT.md`'s `Adt(N)` fallback. Full
rendering goes through a `TypeckResults::render(ty)` helper.

## Typeck rules (`src/typeck/check.rs`)

### `resolve_named_ty` extension

One arm covers both array variants via the inner `Option`:

```rust
HirTyKind::Array(elem, hconst_opt) => {
    let elem_id = resolve_named_ty(elem, ...);
    let cid_opt = hconst_opt.as_ref().map(|hc| match hc {
        HirConst::Lit(n) => consts.intern(ConstKind::Value(*n)),
        HirConst::Error  => consts.error,
    });
    tys.intern(TyKind::Array(elem_id, cid_opt))
}
```

### Index typing rule

```text
infer_index(base, idx) -> TyId:
    base_ty = infer_expr(base)
    idx_ty  = infer_expr(idx)
    coerce_or_unify(idx_ty, tys.usize)              // strict usize, see note below
    match base_ty:
        Array(elem, _)                                  -> elem
        Ptr(inner, _) where kind(inner) is Array(_,_)   -> elem of inner    // auto-deref
        _                                                -> emit E0258 NotIndexable; Error
```

Two arms handle four call shapes (sized vs unsized × direct vs
pointer-base) — the `Some(N)` vs `None` distinction defers to
codegen.

**Note on "strict usize":** the index type must be exactly `usize`
(no implicit widening from `i32` etc.). Users write
`some_i32 as usize` to convert. This mirrors Rust verbatim and
keeps the typeck rule trivial. Diagnostic on mismatch is
`E0259 IndexNotUsize`.

### Array literal typing rule

```text
infer_array_lit(lit) -> TyId:
    match lit:
        Elems([])       -> /* parser already rejected; defensive Error */
        Elems(es) where es.len() == n:
            t = infer_expr(es[0])
            for e in es[1..]:
                unify(infer_expr(e), t) or emit E0260 ArrayLitElementMismatch
            cid = consts.intern(ConstKind::Value(n as u64))
            tys.intern(Array(t, Some(cid)))
        Repeat { init, len: HirConst::Lit(n) }:
            t = infer_expr(init)
            cid = consts.intern(ConstKind::Value(n))
            tys.intern(Array(t, Some(cid)))
        Repeat { init, len: HirConst::Error }:
            t = infer_expr(init)
            tys.intern(Array(t, Some(consts.error)))
```

### Coercions (extending `07_POINTER.md`)

Three new array-related coercions, all on the **pointer outer
layer**:

| From | To | Direction |
|---|---|---|
| `*const [T; N]` | `*const [T]` | Drop length |
| `*mut [T; N]` | `*mut [T]` | Drop length (mut preserved) |
| `*mut [T; N]` | `*const [T]` | Drop length AND mutability (composes with 07's `*mut → *const`) |

Implementation: extend `coerce` to allow, when the outer is `Ptr`
and the existing mutability subtype passes, the inner step
`Array(T, Some(_)) → Array(T, None)` as a one-way length-erasure.
Reverse direction is **not** offered as a coerce — users must use
an explicit `as` cast.

`unify` is unchanged. Equality of array types is structural:
`Array(T1, c1) ~ Array(T2, c2)` succeeds iff `T1 ~ T2` and
`c1 == c2` (interned ConstId equality). Length mismatch is
`E0257 ArrayLengthMismatch`.

### ABI: array-by-value across `extern "C"` boundaries

C does not allow arrays as parameter or return types by value
(`int[10] f();` is a syntax error in C; `void f(int arr[10])`
silently decays to a pointer). There is no C ABI for
array-by-value. This spec follows the precedent of `08_ADT.md`
(struct-by-value rejected at the C boundary, codegen-time error,
TBD-T7) and applies the **same rule to arrays**:

| Position | `Array(_, Some(N))` | `Array(_, None)` |
|---|---|---|
| Internal fn parameter | ✓ — LLVM internal ABI | ✗ — not a value type anywhere (E0261) |
| Internal fn return | ✓ — LLVM internal ABI (memcpy / sret) | ✗ |
| `extern "C"` fn parameter | ✗ — E0264 ArrayByValueAtExternC | ✗ — already E0261 |
| `extern "C"` fn return | ✗ — E0264 ArrayByValueAtExternC | ✗ — already E0261 |
| `let` binding init / `=` rhs | ✓ — Model 2 copy via memcpy | n/a |

Internal-ABI rationale: we have value semantics for whole-array
assignment (Model 2 — copy via `llvm.memcpy`). Forbidding
fn-by-value while allowing let-by-value would be asymmetric. LLVM's
`[N x T]` parameters and returns lower correctly under its
internal calling convention; we emit straightforward IR and let
LLVM handle the platform-specific pieces.

Boundary rationale: at `extern "C"`, the only honest position is
"cross via a pointer." Same as struct-by-value in 08.

To cross the boundary, wrap in a pointer:

```rust
extern "C" {
    // forbidden:
    // fn make() -> [i32; 3];
    // fn first(a: [i32; 3]) -> i32;

    fn make() -> *const [i32; 3];           // OK — pointer to sized array
    fn make_dyn() -> *const [i32];          // OK — pointer to unsized array
    fn first(a: *const [i32; 3]) -> i32;    // OK
}
```

E0264 fires at typeck-time; the check walks every `extern "C"`
fn signature and rejects `Array(_, Some(_))` at parameter or
return positions. Unsized arrays are already rejected at value
positions everywhere (E0261), so E0264 only specifically
targets the sized form at the C boundary.

### Mutability composition

Local mutability (`let` vs `let mut`, per upcoming `11_MUTABILITY.md`)
and pointer mutability (`*const` vs `*mut`, per `07_POINTER.md`)
are **orthogonal axes**. Array semantics compose with both via
standard place-expression rules — no array-specific machinery:

- `arr[i]` is a place when its base is a place or pointer; the
  mutability of the resulting place inherits from the base.
- Whole-array assignment `arr = expr` requires the binding to be
  `let mut`, exactly the same as for any local.
- The pointer mutability rules (`*mut → *const` only, never
  reverse) compose with the array→slice coerces: every coerce
  drops capabilities (length, mutability, or both); none grants.

The combined system is internally consistent: no chain of
operations can grant a mutating capability that wasn't already
present at the source.

### New typeck errors

```rust
pub enum TypeError {
    ...
    ArrayLengthMismatch       { expected: u64, got: u64, span: Span },     // E0257
    NotIndexable              { ty: TyId, span: Span },                     // E0258
    IndexNotUsize             { got: TyId, span: Span },                    // E0259
    ArrayLitElementMismatch   { i: usize, expected: TyId, got: TyId, span: Span }, // E0260
    UnsizedArrayAsValue       { span: Span },                               // E0261
    ArrayLenZeroForbidden     { span: Span },                               // E0262 (reserved)
    /* E0263 reserved by 10_ADDRESS_OF for MutateImmutable */
    ArrayByValueAtExternC     { which: ParamOrReturn, span: Span },         // E0264
}

pub enum ParamOrReturn { Param, Return }
```

E0261 is the **single source of truth** for "unsized array in
value position." HIR doesn't reject this — it can't, because
type aliases (a future feature) would produce `Named(_)` at HIR
and only typeck would see through them to `[T]`. Typeck checks
the resolved type at every value-type slot (let-binding
annotation, fn parameter, fn return, struct field).

E0262 is **reserved** (defer flag). v0 allows zero-length arrays
(`[i32; 0]`); the code is reserved in case a future iteration
forbids them.

E0264 specifically fires for sized arrays at the C boundary;
unsized arrays at the boundary are already caught by E0261
since they aren't value types.

## Codegen (`src/codegen/`)

### LLVM type lowering (`ty.rs`)

```rust
match ty.kind() {
    ...
    TyKind::Array(elem, Some(cid)) => {
        let n = consts.value_of(cid).expect("array length must be Value(_)");
        elem_ll.array_type(n as u32).into()
    }
    TyKind::Array(_, None) => {
        unreachable!("[T] (Array(_, None)) is not a value type; \
                      typeck E0261 should have rejected before codegen")
    }
    TyKind::Prim(PrimTy::Usize) | TyKind::Prim(PrimTy::Isize) => {
        ctx.i64_type().into()         // target-fixed at 64-bit; future target awareness
                                       // changes only this line
    }
}
```

### Index lowering (`lower.rs`)

```text
codegen_index(base_id, idx_id) -> BasicValueEnum:
    match base_ty:
        Array(T, Some(cid)):
            base_ptr = lvalue(base_id)              # alloca for the array
            idx_v    = codegen_expr(idx_id)         # i64 (usize == i64)
            n        = consts.value_of(cid).unwrap()
            emit_bounds_guard(idx_v, n)             # llvm.trap on OOB
            elt_ptr  = GEP [N x T] base_ptr, 0, idx_v
            load T from elt_ptr

        Ptr(Array(T, Some(cid)), _):
            base_ptr = codegen_expr(base_id)        # the pointer value
            idx_v    = codegen_expr(idx_id)
            n        = consts.value_of(cid).unwrap()
            emit_bounds_guard(idx_v, n)
            elt_ptr  = GEP [N x T] base_ptr, 0, idx_v
            load T from elt_ptr

        Ptr(Array(T, None), _):
            base_ptr = codegen_expr(base_id)
            idx_v    = codegen_expr(idx_id)
            elt_ptr  = GEP T base_ptr, idx_v        # no bound — flat element-stride GEP
            load T from elt_ptr

        Array(T, None):
            unreachable!("rejected by typeck E0261")
```

`emit_bounds_guard`:

```llvm
%cmp = icmp uge i64 %idx, N
br i1 %cmp, label %trap, label %ok
trap:
  call void @llvm.trap()
  unreachable
ok:
  ; ...continue with GEP...
```

For statically-known indices where `idx < N` is decidable, fold
the guard at codegen time (skip emitting it). LLVM's optimizer
also folds when both `idx` and `N` are constants.

`llvm.trap` declaration: declared once per module via inkwell's
`Module::add_function` for the intrinsic. Inkwell exposes
`Intrinsic::find("llvm.trap")` for the lookup. The call site is
`call void @llvm.trap()` immediately followed by `unreachable`.

### Place codegen for Index

`Index { base, idx }` as an lvalue (i.e., on the LHS of `=` or as
the operand of `&mut`):

```text
lvalue_index(base_id, idx_id) -> PointerValue:
    # same dispatch as codegen_index, but stop before the load
    # — return the GEP'd pointer, not the loaded value.
```

`Assign { target: Index{..}, rhs }` becomes: compute lvalue of
the index, evaluate rhs, store.

### Array literal lowering

**`HirArrayLit::Elems`:**

```text
codegen_array_lit_elems(es: &[HExprId]) -> PointerValue:
    n        = es.len() as u64
    elem_ll  = lower(elem_ty)
    arr_ll   = elem_ll.array_type(n as u32)
    slot     = builder.alloca(arr_ll)
    for (i, e) in es.iter().enumerate():
        v   = codegen_expr(e)
        gep = GEP arr_ll slot, 0, i as u64
        store v at gep
    slot
```

Returns the alloca pointer; consumed by the caller (`let`-init,
fn-arg passing, etc.) via load or memcpy depending on context.

**`HirArrayLit::Repeat`:**

```text
codegen_array_lit_repeat(init: HExprId, len: HirConst) -> PointerValue:
    n        = const_u64(len)                      # already known
    elem_ll  = lower(elem_ty)
    arr_ll   = elem_ll.array_type(n as u32)
    slot     = builder.alloca(arr_ll)
    init_v   = codegen_expr(init)

    # Fast path: integer zero of any width → llvm.memset
    if is_integer_zero(init_v):
        size_bytes = data_layout.size_of(arr_ll)
        builder.build_memset(slot, ctx.i8_type().const_zero(), size_bytes, align)
    else:
        # General path: explicit loop
        for i in 0..n:
            gep = GEP arr_ll slot, 0, i
            store init_v at gep

    slot
```

For very large `n` with a non-zero init, the loop is preferred over
unrolled stores. LLVM's optimizer may unroll for small `n`. We
don't need to make that call ourselves.

### Whole-array copy

`let arr2 = arr;` where `arr: [T; N]`:

```text
target_slot = alloca [N x T]
src_slot    = lvalue(arr)
size_bytes  = data_layout.size_of([N x T])
build_memcpy(target_slot, target_align, src_slot, src_align, size_bytes)
```

Inkwell's `builder.build_memcpy` handles intrinsic lookup and
declaration internally. LLVM lowers small fixed sizes to inline
load/store sequences; large sizes lower to a libc `memcpy` call
(linker resolves against libc).

This is **byte-level**: the element type doesn't appear in the
`memcpy` call. One declaration of `llvm.memcpy.p0.p0.i64` per
module covers every aggregate copy regardless of T. Soundness
rests on the v0 invariant that **all our types are trivially
copyable** (primitives, pointers, ADTs in v0 with no destructors,
nested arrays of the same). When non-trivially-copyable types
land (e.g., types with `Drop`), aggregate copy will need to lower
to element-by-element move; revisit then.

### Codegen-side abort for E0264

For `extern "C"` fn signatures with `Array(_, Some(_))` parameter
or return slots, codegen emits a hard error and aborts compilation
of that fn. This mirrors `08_ADT.md`'s TBD-T7 plan for struct-by-
value at the C boundary. (Typeck E0264 should already have
rejected; codegen abort is a backstop that produces a useful
internal-error message if the typeck rule was somehow bypassed.)

## Worked examples

### Sized array, indexing, and copy

```rust
fn first(a: [i32; 3]) -> i32 {
    a[0]
}

fn main() -> i32 {
    let a: [i32; 3] = [1, 2, 3];
    let b = a;            // Model 2: copy via memcpy
    first(b)
}
```

After lowering (spans elided):

```text
exprs:
  E0  IntLit(1)
  E1  IntLit(2)
  E2  IntLit(3)
  E3  ArrayLit(Elems([E0, E1, E2]))            : [i32; 3]
  E4  Local(a)                                  : [i32; 3]
  E5  Local(b)                                  : [i32; 3]
  E6  Call(first, [E5])                         : i32
  E7  Local(a)                                  : [i32; 3]
  E8  IntLit(0u64)                              : usize
  E9  Index { base: E7, idx: E8 }               : i32      # in `first`'s body

main locals: a (LocalId 0), b (LocalId 1)
first locals: a (LocalId 0)

Types:
  [i32; 3] = TyKind::Array(i32_id, Some(consts.intern(Value(3))))
```

LLVM IR sketch (main):

```llvm
%a.slot = alloca [3 x i32], align 4
%b.slot = alloca [3 x i32], align 4

; let a = [1, 2, 3]
%a.gep0 = getelementptr [3 x i32], ptr %a.slot, i32 0, i32 0
store i32 1, ptr %a.gep0
%a.gep1 = getelementptr [3 x i32], ptr %a.slot, i32 0, i32 1
store i32 2, ptr %a.gep1
%a.gep2 = getelementptr [3 x i32], ptr %a.slot, i32 0, i32 2
store i32 3, ptr %a.gep2

; let b = a   (memcpy)
call void @llvm.memcpy.p0.p0.i64(ptr %b.slot, ptr %a.slot, i64 12, i1 false)

; first(b)
%first.arg.slot = alloca [3 x i32], align 4
call void @llvm.memcpy.p0.p0.i64(ptr %first.arg.slot, ptr %b.slot, i64 12, i1 false)
%ret = call i32 @first(ptr %first.arg.slot)        ; LLVM internal ABI rewrites
                                                    ; [3 x i32]-by-value to ptr
ret i32 %ret
```

(LLVM's internal calling convention may pass the array directly in
registers for small sizes; the ptr-rewrite shown is the general
case. The user-visible behavior is the same.)

LLVM IR for `first` (with bounds-check guard):

```llvm
define i32 @first(ptr %a.byref) {
entry:
  %idx = i64 0                                  ; from IntLit(0)
  %cmp = icmp uge i64 %idx, 3
  br i1 %cmp, label %trap, label %ok

trap:
  call void @llvm.trap()
  unreachable

ok:
  %elt.gep = getelementptr [3 x i32], ptr %a.byref, i64 0, i64 %idx
  %v = load i32, ptr %elt.gep, align 4
  ret i32 %v
}
```

Optimizer folds the guard (`%cmp` is constant-false) and the load
becomes `i32 1`. The runtime program prints "1" effectively.

### Repeat literal with memset fast-path

```rust
fn make_buf() -> [u8; 1024] {
    [0; 1024]
}
```

LLVM IR:

```llvm
define void @make_buf(ptr sret([1024 x i8]) %retslot) {
entry:
  call void @llvm.memset.p0.i64(ptr %retslot, i8 0, i64 1024, i1 false)
  ret void
}
```

Sret because `[1024 x i8]` exceeds the register-return budget on
typical platforms; LLVM's internal ABI handles the rewrite.

### Unsized slice via FFI

```rust
extern "C" {
    fn make_buf(n: usize) -> *mut [i32];
}

fn third_elem(p: *const [i32]) -> i32 {
    p[2]
}
```

After typeck:

```text
extern fn make_buf has signature (usize) -> *mut Array(i32_id, None)
fn third_elem param p: *const Array(i32_id, None)

Index { base: p, idx: IntLit(2) } : i32
  base_ty = Ptr(Array(i32, None), Const)
  idx_ty  = i32 (default) → user must write `2usize`; or coerce

  (assume `2usize`)
  → matches Ptr(Array(_, None), _) arm
  → result = i32 (the elem)
  → no bounds guard (length unknown)
```

LLVM IR for `third_elem`:

```llvm
define i32 @third_elem(ptr %p) {
entry:
  %elt.gep = getelementptr i32, ptr %p, i64 2     ; flat element-stride GEP
  %v = load i32, ptr %elt.gep, align 4
  ret i32 %v
}
```

No `icmp uge` / `br` / `llvm.trap` — the unsized form is the
deliberate opt-out from bounds checking.

### Length mismatch (typeck error)

```rust
fn want3(a: [i32; 3]) -> i32 { a[0] }

fn caller() -> i32 {
    let a: [i32; 4] = [1, 2, 3, 4];
    want3(a)                                     // E0257 ArrayLengthMismatch
}
```

`want3` expects `Array(i32, Some(intern(Value(3))))`; the actual
arg has `Array(i32, Some(intern(Value(4))))`. Different ConstId,
different TyId, unify fails — diagnostic:
*"expected `[i32; 3]`, found `[i32; 4]`"*.

### Pointer-to-sized → pointer-to-unsized coercion

```rust
fn at_dyn(p: *const [i32], i: usize) -> i32 { p[i] }

fn caller(a: *const [i32; 10]) -> i32 {
    at_dyn(a, 5)                                 // *const [i32; 10] coerces to *const [i32]
}
```

Coerce check at the call site: outer mutability (`*const →
*const`) is OK; inner pointee transitions
`Array(i32, Some(_)) → Array(i32, None)` via the new length-erasure
arm. No diagnostic. Inside `at_dyn`, indexing through `*const [i32]`
emits no bounds guard.

### `extern "C"` rejection

```rust
extern "C" {
    fn bad(a: [i32; 3]) -> [u8; 4];              // E0264 (twice — param and return)
}
```

Typeck walks the signature, finds `Array(i32, Some(_))` at param
position and `Array(u8, Some(_))` at return position. Emits
`ArrayByValueAtExternC` for each, with `which: Param` and
`which: Return` respectively.

Diagnostic: *"sized array `[i32; 3]` cannot appear by value at an
`extern \"C\"` boundary; wrap in a pointer (`*const [i32; 3]` /
`*mut [i32; 3]`) or an unsized-array pointer (`*const [i32]`)."*

## Errors summary

| Code | Variant | Layer |
|---|---|---|
| E0101 | `UnexpectedToken` — non-`IntLit` in `[T; N]` / `[init; N]` length slot lands on the existing generic parse-error code | parser |
| E0257 | `ArrayLengthMismatch` | typeck |
| E0258 | `NotIndexable` | typeck |
| E0259 | `IndexNotUsize` | typeck |
| E0260 | `ArrayLitElementMismatch` | typeck |
| E0261 | `UnsizedArrayAsValue` | typeck |
| E0262 | `ArrayLenZeroForbidden` (reserved) | typeck |
| E0264 | `ArrayByValueAtExternC` | typeck |

E0263 is reserved by `10_ADDRESS_OF.md` for `MutateImmutable`.

## Out of scope

- **Length-inference sugar `[init; _]` / `[init; ..]`.** Match
  Rust verbatim; users rely on RHS inference for the `let` case.
- **Compiler flag `--no-bounds-check`.** Type-level opt-out via
  `[T]` is the v0 mechanism. A global flag for `[T; N]` is
  deferred to a later flag/build-mode spec.
- **Per-expression unsafe opt-out.** `unsafe { arr[i] }` requires
  `unsafe` blocks first.
- **Const generics:** `fn f<const N: usize>(a: [T; N])`. Own spec
  (parser, name resolution, monomorphization). Today's
  `ConstArena` layer leaves a clean extension point.
- **ICE-style length expressions** like `[T; 1+1]`, `[T; (5)]`,
  `[T; 5 as u64]`. Own spec; reuses the existing `HirConst::Lit`
  storage and just relaxes `extract_length_const`'s match.
- **Const items:** `const N: usize = 10;`. Own spec; pairs with
  ICE expressions for name lookup.
- **`sizeof T`.** Own spec; pairs with ICE expressions for
  type-size queries.
- **Slice fat pointers** (`&[T]` carrying length). Rust has them;
  we don't. C-ish stance.
- **Pointer arithmetic** `p + n` and pointer **deref** `*p` —
  deferred per `07_POINTER.md`. Index auto-deref on
  pointer-to-array sidesteps the need.
- **Array slicing** `arr[1..5]`. Needs slice fat pointers and
  range syntax; both out of scope.
- **Repeat with non-trivially-copyable init.** All v0 types are
  trivially copyable; the question doesn't arise.
- **Array-to-pointer decay** `[T; N] → *const T` (C-style,
  proposed in `07_POINTER.md` for StrLit migration). Not adopted
  in v0 — to reach a `*const T` from a `[T; N]`, users go through
  `&arr` (per `10_ADDRESS_OF.md`) and then `as` cast, or use
  pointer-to-array directly. Re-evaluate when StrLit migration
  lands.

## TBDs and future evolution

- **Bidirectional length inference** — would enable both `[init; _]`
  and `[]` literals by reading the expected type. Useful but
  non-trivial; defer.
- **Sized-trait-ification.** Today `is_sized(ty)` is an internal
  predicate (`Array(_, None)` is the sole `false` case). When
  generics land, `Sized` becomes a real trait with an implicit
  bound on type parameters. Migration is purely additive — every
  v0 use of `is_sized` becomes `T: Sized` enforcement.
- **Const generics over N.** Adds `ConstKind::Param(idx)` and a
  `ConstKind::Infer(_)` for unification at call sites.
  Monomorphization in codegen (one specialization per concrete N).
  Independent spec; no v0 surface change beyond what `ConstArena`
  already affords.
- **StrLit migration.** Once arrays land, `"hello"` becomes
  `[u8; 6]` per the migration plan in `07_POINTER.md`. Implies
  StrLit becomes a place expression, and `&"hello"` becomes legit
  per `10_ADDRESS_OF.md`. The path between this spec and StrLit
  migration also needs to address whether array-to-pointer decay
  (`[u8; N] → *const u8` at fn-arg / let-init position) is added
  to keep existing FFI use sites compiling. Re-evaluated as part
  of the StrLit migration spec.
- **Struct-of-array layout.** Today nested arrays compose
  naturally — `[[T; M]; N]` is `Array(Array(T, Some(M)), Some(N))`
  and lowers to `[N x [M x T]]` in LLVM. No special spec needed
  unless we ever want SoA/AoS reshaping primitives.

## What this unblocks

- Buffers as first-class values (`[u8; 1024]` for I/O scratch,
  `[i32; 256]` for lookup tables, etc.).
- Fixed-size message structs via `[u8; N]` payloads.
- StrLit migration from "magic `*const u8`" scaffolding to
  proper `[u8; N]` per `07_POINTER.md`'s long-promised plan.
- Pointer-to-unsized-array `*const [T]` / `*mut [T]` as the
  unsafe array view, which is the natural type for FFI returns
  like `malloc`-shaped routines.
- The `usize`/`isize` distinction, which all future indexing /
  size / count APIs build on (and which avoids the alias trap of
  retro-fitting later).

## Note: `04_HIR.md` and `08_ADT.md` need a one-line update

`04_HIR.md`'s "Out of scope (v0)" section already states that
type-namespace resolution is deferred to typeck. `08_ADT.md`
flagged this as stale. This spec adds another layer of
HIR-resolved types (arrays). Update `04_HIR.md` to note that
**user-defined types AND array/slice types** are HIR-resolved;
primitives remain typeck-resolved.

`08_ADT.md`'s TBD list mentions struct-by-value at `extern "C"` is
TBD-T7. Cross-reference with this spec's E0264: the rule for
arrays is identical; the rule for structs (TBD-T7) lands by the
same machinery.
