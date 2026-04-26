# Address-of operator (`&` / `&mut`)

## Requirements

We have pointer *types* (`*const T` / `*mut T`) and pointer *passing*
through fn signatures, but no source-level way to construct a pointer
to a local. Without that, every C-ABI function whose interface takes
`T*` (which is the vast majority of `<unistd.h>`, `<sys/socket.h>`,
`<fcntl.h>`, …) is unreachable from Oxide except via a C glue shim.

This spec adds the **address-of operator**: `&place` produces
`*const T`, `&mut place` produces `*mut T`, where `T` is the type of
the operand and `place` is a place expression.

This iteration covers **AST → HIR → typeck**. Codegen is a one-liner
on top of the existing `lvalue` infrastructure (place expressions
already produce a `PointerValue` for assignment); the spec includes
the codegen note for completeness but it's mechanical.

Pointer **deref** (`*p` rvalue and `*p = v` lvalue) remains deferred
per `07_POINTER.md` §5. `&` and `*` are the two pointer operators;
`&` lands first because it's the producer side and unblocks more
real programs (any FFI call that takes `T*`).

## Subset-of-Rust constraint

Anything we accept must also parse in Rust with the same meaning.
We accept:

- `&expr` — produces a shared reference in Rust (`&T`); we type it as
  `*const T`.
- `&mut expr` — produces an exclusive mutable reference in Rust
  (`&mut T`); we type it as `*mut T`.

This is **not** a strict mirror of Rust's reference semantics —
Rust's `&T` is a borrow with lifetime, ours is a raw pointer. But
the **syntax** is a subset of Rust's, and the meaning of the
syntactic shape (`&x` produces "a pointer to x"; `&mut x` requires
`x` to be mutable) lines up with what a reader expects. We're
choosing C semantics under Rust syntax — matching the precedent set
by `*const T` / `*mut T` in `07_POINTER.md`.

The narrow gotcha: a Rust user might expect `&` to give them a
borrow that the borrow checker will police. They get a raw pointer
instead. This is documented in the language overview, not enforced
in the type system.

## Acceptance

```rust
struct Counter { value: i32 }

extern "C" {
    fn read_counter(p: *const Counter) -> i32;
    fn reset_counter(p: *mut Counter);
}

fn snapshot_then_reset() -> i32 {
    let mut c = Counter { value: 42 };
    let v = read_counter(&c);          // *const Counter
    reset_counter(&mut c);             // *mut Counter
    v
}
```

This program parses, lowers, and typechecks:

- `&c` (operand: `Local(c)`, mut place since `c` was `let mut`)
  produces `*const Counter` — coerces into `read_counter`'s
  `*const Counter` slot via the existing `mut → const` outer rule.
- `&mut c` produces `*mut Counter` — exact-match into
  `reset_counter`'s slot.

`&mut` on an immutable place would error at typeck:

```rust
fn bad() -> i32 {
    let c = Counter { value: 42 };     // not `mut`
    reset_counter(&mut c);             // E0263 MutateImmutable { op: BorrowMut }
    0
}
```

A more end-to-end follow-up: pure-Oxide socket-server stub calling
`bind`/`accept` directly without C glue. Recursion substitutes for
the still-missing `while`. Lands as `example-projects/socket-server/`
once this spec is implemented.

## Position in the pipeline

```
Source ─▶ tokens ─▶ AST ─▶ HIR ─▶ typeck ─▶ codegen
                              ╰─── `&` operator added in this spec ───╯
```

## AST changes (`src/parser/`)

### New expression kind

```rust
pub enum ExprKind {
    ...
    AddrOf {
        mutability: Mutability,                 // reused from `*mut T` / `*const T` work
        expr: ExprId,
    },
}
```

A dedicated variant rather than overloading `UnOp::Neg`/`Not`/`BitNot`
because the operator carries a `Mutability` payload. Same shape as
`TypeKind::Ptr { mutability, pointee }`.

### Grammar

```
AddrOfExpr ::= '&' 'mut'? UnaryExpr
```

Slots into the prefix-unary level of the Pratt builder (level 13,
alongside `-` / `!` / `~`). Right-associative.

### Token disambiguation

`&` lexes as `TokenKind::Amp`. Today it's used only for binary bitwise
AND (level 8). The Pratt builder distinguishes prefix from infix by
position — at the start of an atom slot, `Amp` parses as `AddrOf`'s
prefix; in the middle of an expression, as `BitAnd`. Chumsky handles
this naturally.

`&mut` is two tokens: `Amp` followed by `KwMut`. The mut-token is
optional — absence means `Mutability::Const`. Whitespace between
`&` and `mut` is allowed (`& mut x` parses identically to `&mut x`),
matching Rust.

**`&&` collision.** The lexer greedily tokenizes `&&` as
`TokenKind::AndAnd` (logical AND). So `&&x` does *not* parse as
"address of address of `x`" — it lexes as `AndAnd` followed by `x`,
which the parser rejects (binary op with no LHS).

To take the address of an address, intermediate-bind:
`let p = &x; let pp = &p;`. Or use a space: `& &x` works because
`& ` (with whitespace) lexes as `Amp` then `Amp` separately. We
don't add special parser handling — the workaround is fine and
matches Rust's behavior verbatim.

### What the AST does *not* add

- `&raw const` / `&raw mut` (Rust's raw-pointer borrow). Not needed:
  our `&` already produces raw pointers.
- `&place as *const T`-style coercion through `as`. The type of `&p`
  is `*const T` directly; no cast required.
- Reference types in type position (`&T`, `&mut T`). We have only
  `*const T` / `*mut T`. `&` produces those, doesn't introduce a
  new type.

## HIR changes (`src/hir/`)

### New expression kind

```rust
pub enum HirExprKind {
    ...
    AddrOf {
        mutability: Mutability,
        expr: HExprId,
    },
}
```

### Place rule

`AddrOf` is **not** a place — its result is a fresh pointer value
(specifically: a `ptr` SSA value in LLVM, not a slot or a GEP that
identifies one). `compute_is_place` falls through its catch-all
arm and returns `false` for `AddrOf`; do **not** add an explicit
`is_place: true` arm for `AddrOf`.

The operand, however, **must** be a place. The check fires at
lowering time:

```rust
ast::ExprKind::AddrOf { mutability, expr } => {
    let inner = self.lower_expr(expr);
    if !self.exprs[inner].is_place {
        self.errors.push(HirError::AddrOfNonPlace {
            span: self.exprs[inner].span.clone(),
        });
    }
    HirExprKind::AddrOf { mutability, expr: inner }
}
```

The check is purely syntactic — same `is_place` cache used by
`InvalidAssignTarget`. No typeck information needed.

### New error

```rust
pub enum HirError {
    ...
    AddrOfNonPlace { span: Span },                  // E0208
}
```

The `span` points at the **operand** (the not-a-place expression),
not the `&` token, so the diagnostic underline lands where the user
needs to look.

`from_hir.rs` grows an arm. Diagnostic message: "cannot take the
address of a non-place expression — only locals, fields, and
deref-of-pointer are addressable."

## Typeck changes (`src/typeck/`)

### Type rule

```text
infer_addr_of(mutability, expr) -> TyId:
    inner_ty = infer_expr(expr)
    intern(TyKind::Ptr(inner_ty, mutability))
```

That's it. The result type is `*α T` where `α` is the operator's
mutability and `T` is the operand's inferred type. No unification
needed beyond what infer_expr does for the operand.

### Mutability check for `&mut`

`&mut place` requires `place` to be a mutable place. `place_mutability`
returns `Option<Mutability>` — `None` for non-places (HIR has already
emitted `AddrOfNonPlace`, so typeck deliberately doesn't double-report).
The walk is the natural recursion through projections:

```text
place_mutability(eid) -> Option<Mutability>:
    Local(lid)            -> Some(if hir.locals[lid].mutable { Mut } else { Const })
    Field { base, _ }     -> place_mutability(base)        // inherit from owner
    Index { base, _ }     -> place_mutability(base)        // same
    Deref { e }           -> Some(per e's pointer mutability — *mut T → Mut, *const T → Const)
                                                            // (TBD per 07_POINTER)
    _                     -> None                          // not a place;
                                                            // HIR already errored
```

`infer_addr_of` for the `&mut` arm:

```text
infer_addr_of(mutability, expr):
    inner_ty = infer_expr(expr)                            // always type the operand
    if mutability == Mut:
        match place_mutability(expr):
            Some(Mut)   -> ok
            Some(Const) -> emit MutateImmutable { op: BorrowMut, span }
            None        -> /* HIR emitted AddrOfNonPlace; don't double-report */
    intern(Ptr(inner_ty, mutability))                       // result type regardless
```

Three points worth pinning down:

- **Result type stays typed even on error.** `&mut bad_thing` still
  produces `*mut T` so cascades downstream stay structurally typed
  (no `Error` poisoning unrelated checks).
- **`&` (immutable) has no mutability constraint.** Taking a `*const T`
  of any place is always fine; we only consult `place_mutability`
  on the `&mut` path.
- **`None` is the silent path.** When the operand isn't a place,
  HIR has already filed `AddrOfNonPlace`. Typeck's mutability check
  doesn't fire, and typeck doesn't add its own error — the user sees
  one error from one layer, not two.

### Companion fix: `=`-side mutability

The same `place_mutability` walk applies to assignment targets:
`x = 5` should error if `x` isn't `mut`. Currently it doesn't —
that's a pre-existing gap not closed by the place rule (`is_place`)
alone, which is purely structural.

Bundle the fix into this PR — same check, same `None`-suppression
rule, same error variant:

```text
infer_assign(op, target, rhs):
    target_ty = infer_expr(target)
    rhs_ty    = infer_expr(rhs)
    coerce(rhs_ty, target_ty, span)                        // existing
    match place_mutability(target):                        // new
        Some(Mut)   -> ok
        Some(Const) -> emit MutateImmutable { op: Assign, span }
        None        -> /* HIR emitted InvalidAssignTarget; don't double-report */
    Unit
```

The asymmetry of fixing `&mut` strictly while leaving `x = v` loose
would be more confusing than the cost of bundling. One PR, one
error variant, both call sites.

### Errors

```rust
pub enum TypeError {
    ...
    /// `&mut place` or `place = rhs` where the place's root is an
    /// immutable Local (or, future, a `*const T` deref). E0263.
    MutateImmutable {
        op: MutateOp,                                // BorrowMut | Assign
        span: Span,
    },
}

pub enum MutateOp { BorrowMut, Assign }
```

One variant for both `&mut` and `=`-on-immutable, with an `op`
discriminator so the diagnostic can render appropriately
("cannot take a mutable pointer to immutable `x`" vs "cannot
assign to immutable `x`").

The `span` points at the **place expression** (the LHS of `=` /
the operand of `&mut`), not the operator — so for `&mut p.x`, the
span covers `p.x`, and for `p.x = 5`, it covers `p.x`.

### `coerce` and `unify` — no changes

`AddrOf`'s result is `Ptr(T, mutability)` — already handled by the
existing pointer rules in `unify` (shape) and `coerce` (mutability
subtype). Nothing new at the type-arithmetic layer.

## Codegen (mechanical, included for completeness)

The existing `lvalue(eid) -> PointerValue` already produces the
slot/GEP pointer for any place expression (`Local`, `Field`,
eventually `Deref` and `Index`). `AddrOf` is a one-arm addition
that returns this pointer directly:

```rust
HirExprKind::AddrOf { mutability: _, expr } => {
    let ptr = self.lvalue(fx, expr);
    Some(ptr.into())
}
```

LLVM doesn't distinguish `*mut` from `*const` — both lower to
`ptr`. The mutability tag is purely a typeck concept; codegen
ignores it.

No new IR patterns. The existing alloca-for-locals + GEP-for-fields
infrastructure produces the right pointer; `AddrOf` just exposes it
at the value level instead of consuming it inline (as `Assign`'s
`lvalue` call does).

### Worked LLVM IR

For:

```rust
struct Point { x: i32, y: i32 }

fn make_ptr() {
    let mut p = Point { x: 1, y: 2 };
    let q = &mut p.x;          // q: *mut i32
}
```

The IR after the `let q = &mut p.x` line:

```llvm
%p.0.slot = alloca %Point, align 8
%q.1.slot = alloca ptr, align 8                ; q is itself in a slot

; let mut p = Point { ... }
store %Point { i32 1, i32 2 }, ptr %p.0.slot, align 4

; let q = &mut p.x
%fld.gep = getelementptr inbounds %Point, ptr %p.0.slot, i32 0, i32 0
store ptr %fld.gep, ptr %q.1.slot, align 8     ; store the GEP pointer into q's slot
```

The GEP that `lvalue(p.x)` already produces is exactly the value
we want from `&mut p.x`. It flows into `q`'s slot like any other
pointer value would. `&p` (instead of `&p.x`) would simply skip the
GEP and store `%p.0.slot` directly.

## Worked examples

### `&mut` on a Local

```rust
struct Point { x: i32, y: i32 }

fn write_through(p: *mut Point) {}

fn caller() {
    let mut p = Point { x: 1, y: 2 };
    write_through(&mut p);
}
```

After lowering + typeck (spans elided):

```text
caller body:
  Let p[Local(0), mut] = StructLit(Point, x=1, y=2)        : Adt(0)
  Call write_through(AddrOf(Mut, Local(p)))                : ()
                     ^^^^^^^^^^^^^^^^^^^^^^                : *mut Adt(0)
```

Codegen for `AddrOf(Mut, Local(p))` is `lvalue(Local(p))` →
`%p.0.slot` (the alloca pointer). That's what gets passed to
`write_through` as the `ptr` argument.

### `&mut` on a chained Field

```rust
struct Inner { v: i32 }
struct Outer { i: Inner }

fn rest(p: *mut i32) {}

fn caller() {
    let mut o = Outer { i: Inner { v: 99 } };
    rest(&mut o.i.v);
}
```

`place_mutability(o.i.v)` walks: `Field(o.i)` → `Field(o)` →
`Local(o)` → `Some(Mut)`. OK to borrow mutably.

Codegen: `lvalue(o.i.v)` produces the chained GEP into the field;
`&mut o.i.v` exposes that pointer as a value. Same pattern as the
"Worked LLVM IR" section above, just one extra GEP layer.

### Error: `&mut` on immutable Local

```rust
fn bad() {
    let q = Point { x: 1, y: 2 };       // not `mut`
    write_through(&mut q);
}
```

`place_mutability(q)` returns `Some(Const)`. Typeck emits
`MutateImmutable { op: BorrowMut, span: <span of q> }`.

Diagnostic: "cannot take a mutable pointer to immutable local `q`;
declare it as `let mut q` to allow mutable references."

### Error: `&` on a non-place

```rust
fn bad2() {
    let p = &(1 + 2);                   // (1+2) is not a place
}
```

HIR's lower walks the operand, finds `is_place == false`, emits
`AddrOfNonPlace { span: <span of (1+2)> }`. Typeck then runs over
the resulting `AddrOf` expr but the place-mutability arm only
fires for `&mut`; since this is `&` (Const), no further error
emerges. One diagnostic from one layer.

## Out of scope (this round)

- Pointer **deref** (`*p` rvalue, `*p = v` lvalue). Still deferred
  per `07_POINTER.md` §5. Lands in a separate spec; together with
  this work it completes the basic pointer-operator pair.
- `&` over arbitrary expressions (only places). Rust's `&temp` for
  non-place expressions extends the temporary's lifetime; we don't
  have that machinery and it's not load-bearing for FFI use.
- Reference types `&T` / `&mut T` in type position. We continue to
  use `*const T` / `*mut T` for the type-level vocabulary.
- Borrow checking. Raw-pointer semantics; the user owns aliasing
  discipline.
- `&place as *const U` cross-type pointer casts. `as` between
  pointer types remains TBD.

### Known subset-of-Rust gap: `&` on string literals

In Rust, `"hello"` has type `&'static str` (a borrow that is also
a place by virtue of static lifetime), so `&"hello"` is legal and
produces `&&'static str`. Our model is different — per
`07_POINTER.md`, `"hello"` *is* a `*const u8`, the pointer itself
rather than a borrow of bytes. Treating `StrLit` as a place would
make `&"hello"` produce `*const *const u8`, which is rarely useful
and complicates the pointer model for a marginal case.

Consequence: `&"hello"` errors as `E0208 AddrOfNonPlace`. Workaround:
bind to a local first, then take its address.

```rust
let s: *const u8 = "hello";
let pp = &s;                   // *const *const u8
```

This is the one non-trivial place where we accept *fewer* programs
than Rust. The subset-of-Rust constraint is preserved (we don't
accept anything Rust rejects), but the asymmetry is real.

A future `static` item / `&raw const NAME` would naturally cover
this case — codegen's existing `emit_str_lit` already creates a
private global, so the storage is there; only the surface needs
to materialize. Out of scope for this round.

## Errors summary

| Code | Variant | Layer |
|---|---|---|
| E0208 | `HirError::AddrOfNonPlace` | HIR |
| E0263 | `TypeError::MutateImmutable { op: BorrowMut \| Assign }` | typeck |

E0263 is shared between `&mut x` and `x = v` because both require
the same place-mutability check. The `op` discriminator lets the
diagnostic phrase the message naturally for each.

## What this unblocks

Once `&` lands, the realistic FFI surface widens substantially. Any
C function taking `T*` becomes callable (`socket.h` / `unistd.h` /
`fcntl.h` / `time.h` essentially in full, modulo struct-by-value
which we still forbid at `extern "C"` boundaries). The pure-Oxide
HTTP-server example becomes possible — combined with recursion
substituting for the still-missing `while`, we can write the
acceptance program for `07_POINTER`'s eventual "real FFI" goal.
